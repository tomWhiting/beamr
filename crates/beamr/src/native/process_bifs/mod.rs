//! Process lifecycle BIFs — self, spawn, spawn_link, spawn_monitor, link,
//! unlink, process_flag, monitor, demonitor, exit.
//!
//! Registered as Gate 2 BIFs alongside the Gate 1 arithmetic, comparison, and
//! utility functions.

use crate::atom::{Atom, AtomTable};
use crate::native::links::LinkError;
use crate::native::{
    BifRegistryImpl, Capability, ExceptionClass, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::process::ExitReason;
use crate::term::Term;
use crate::term::boxed::{Closure, Cons};

type Gate2Bif = (&'static str, u8, Capability, NativeFn);

const GATE2_BIFS: &[Gate2Bif] = &[
    ("self", 0, Capability::Pure, bif_self),
    ("spawn", 3, Capability::Pure, bif_spawn),
    ("spawn_link", 3, Capability::Pure, bif_spawn_link),
    ("spawn_monitor", 1, Capability::Pure, bif_spawn_monitor_1),
    ("spawn_monitor", 3, Capability::Pure, bif_spawn_monitor_3),
    ("link", 1, Capability::Pure, bif_link),
    ("unlink", 1, Capability::Pure, bif_unlink),
    ("process_flag", 2, Capability::Pure, bif_process_flag),
    ("monitor", 2, Capability::Pure, bif_monitor),
    ("demonitor", 1, Capability::Pure, bif_demonitor),
    ("exit", 1, Capability::Pure, bif_exit_1),
    ("exit", 2, Capability::Pure, bif_exit),
];

/// Registers all Gate 2 (process lifecycle) BIFs into the VM-owned BIF registry.
pub fn register_gate2_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in GATE2_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }

    Ok(())
}

/// erlang:self/0 — returns the calling process's PID.
pub fn bif_self(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let pid = context.pid().ok_or_else(badarg)?;
    Term::try_pid(pid).ok_or_else(badarg)
}

/// erlang:spawn/3 — creates a new process executing Module:Function(Args).
pub fn bif_spawn(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_impl(args, context, false)
}

/// erlang:spawn_link/3 — creates a new linked process.
pub fn bif_spawn_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_impl(args, context, true)
}

/// erlang:spawn_monitor/1 — creates and monitors a process from a zero-arity fun.
pub fn bif_spawn_monitor_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_monitor_fun_impl(args, context)
}

/// erlang:spawn_monitor/3 — creates and monitors a new process atomically.
pub fn bif_spawn_monitor_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_monitor_mfa_impl(args, context)
}

/// erlang:link/1 — establishes a bidirectional link to the target process.
pub fn bif_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [target_term] = args else {
        return Err(badarg());
    };
    let target_pid = target_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    if caller_pid == target_pid {
        return Ok(Term::atom(Atom::TRUE));
    }
    let facility = context.link_facility().ok_or_else(badarg)?;
    match facility.link(caller_pid, target_pid) {
        Ok(()) => Ok(Term::atom(Atom::TRUE)),
        Err(LinkError::NoProc) => Err(Term::atom(Atom::NOPROC)),
        Err(LinkError::NoCaller) => Err(badarg()),
    }
}

/// erlang:unlink/1 — removes the bidirectional link to the target process.
pub fn bif_unlink(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [target_term] = args else {
        return Err(badarg());
    };
    let target_pid = target_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    if caller_pid == target_pid {
        return Ok(Term::atom(Atom::TRUE));
    }
    let facility = context.link_facility().ok_or_else(badarg)?;
    facility
        .unlink(caller_pid, target_pid)
        .map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

/// erlang:process_flag/2 — sets a process flag, returns the previous value.
pub fn bif_process_flag(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [flag_term, value_term] = args else {
        return Err(badarg());
    };
    let flag = flag_term.as_atom().ok_or_else(badarg)?;
    if flag == Atom::TRAP_EXIT {
        let new_value = atom_to_bool(*value_term).ok_or_else(badarg)?;
        let caller_pid = context.pid().ok_or_else(badarg)?;
        let facility = context.link_facility().ok_or_else(badarg)?;
        let old_value = facility
            .set_trap_exit(caller_pid, new_value)
            .map_err(|_| badarg())?;
        Ok(bool_to_atom(old_value))
    } else {
        Err(badarg())
    }
}

/// erlang:monitor/2 — establish a unidirectional monitor from caller to target.
///
/// Note: the returned reference is currently a small integer, not a boxed
/// reference term, because BIFs cannot allocate boxed terms on the process heap.
pub fn bif_monitor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [type_term, pid_term] = args else {
        return Err(badarg());
    };
    let type_atom = type_term.as_atom().ok_or_else(badarg)?;
    if type_atom != Atom::PROCESS {
        return Err(badarg());
    }
    let target_pid = pid_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.supervision_facility().ok_or_else(badarg)?;
    let result = facility
        .monitor(caller_pid, target_pid)
        .map_err(|_| badarg())?;
    Term::try_small_int(result.reference as i64).ok_or_else(badarg)
}

/// erlang:demonitor/1 — remove a monitor identified by its reference.
pub fn bif_demonitor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [ref_term] = args else {
        return Err(badarg());
    };
    let reference = ref_term.as_small_int().ok_or_else(badarg)?;
    if reference < 0 {
        return Err(badarg());
    }
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.supervision_facility().ok_or_else(badarg)?;
    facility
        .demonitor(caller_pid, reference as u64)
        .map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

/// erlang:exit/1 — raises an exit-class exception in the calling process.
pub fn bif_exit_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [reason] = args else {
        return Err(badarg());
    };

    context.set_exception_class(ExceptionClass::Exit);
    Err(*reason)
}

/// erlang:exit/2 — send an exit signal to a target process.
pub fn bif_exit(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, reason_term] = args else {
        return Err(badarg());
    };
    let target_pid = pid_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let reason = exit_reason_from_term(*reason_term)?;
    let facility = context.supervision_facility().ok_or_else(badarg)?;
    facility
        .exit_signal(caller_pid, target_pid, reason)
        .map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

fn exit_reason_from_term(term: Term) -> Result<ExitReason, Term> {
    let atom = term.as_atom().ok_or_else(badarg)?;
    match atom {
        Atom::NORMAL => Ok(ExitReason::Normal),
        Atom::KILL => Ok(ExitReason::Kill),
        Atom::KILLED => Ok(ExitReason::Killed),
        Atom::ERROR => Ok(ExitReason::Error),
        _ => Err(badarg()),
    }
}

fn spawn_impl(args: &[Term], context: &mut ProcessContext, link: bool) -> Result<Term, Term> {
    let [module_term, function_term, args_term] = args else {
        return Err(badarg());
    };
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let function = function_term.as_atom().ok_or_else(badarg)?;
    let spawn_args = list_to_vec(*args_term)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let link_to = if link { Some(caller_pid) } else { None };
    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let new_pid = facility
        .spawn(caller_pid, module, function, spawn_args, link_to)
        .map_err(|_| badarg())?;
    Term::try_pid(new_pid).ok_or_else(badarg)
}

fn spawn_monitor_mfa_impl(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [module_term, function_term, args_term] = args else {
        return Err(badarg());
    };
    let module = module_term.as_atom().ok_or_else(badarg)?;
    let function = function_term.as_atom().ok_or_else(badarg)?;
    let spawn_args = list_to_vec(*args_term)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let result = facility
        .spawn_monitor(caller_pid, module, function, spawn_args)
        .map_err(|_| badarg())?;
    spawn_monitor_tuple(result.pid, result.reference, context)
}

fn spawn_monitor_fun_impl(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun_term] = args else {
        return Err(badarg());
    };
    let closure = Closure::new(*fun_term).ok_or_else(badarg)?;
    if closure.arity() != 0 || closure.num_free() != 0 {
        return Err(badarg());
    }
    let module = closure.module().ok_or_else(badarg)?;
    let lambda_index = closure.function_index() as u32;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let result = facility
        .spawn_lambda_monitor(caller_pid, module, lambda_index)
        .map_err(|_| badarg())?;
    spawn_monitor_tuple(result.pid, result.reference, context)
}

fn spawn_monitor_tuple(
    child_pid: u64,
    reference: u64,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let pid_term = Term::try_pid(child_pid).ok_or_else(badarg)?;
    let reference_term = context.alloc_reference(reference)?;
    context.alloc_tuple(&[pid_term, reference_term])
}

fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(elements);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
}

fn atom_to_bool(term: Term) -> Option<bool> {
    let atom = term.as_atom()?;
    if atom == Atom::TRUE {
        Some(true)
    } else if atom == Atom::FALSE {
        Some(false)
    } else {
        None
    }
}

const fn bool_to_atom(value: bool) -> Term {
    if value {
        Term::atom(Atom::TRUE)
    } else {
        Term::atom(Atom::FALSE)
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests;
