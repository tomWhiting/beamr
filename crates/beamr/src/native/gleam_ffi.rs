//! Process utility BIFs for gleam_erlang_ffi.
//!
//! These are thin wrappers around existing facility traits, registered under
//! the `gleam_erlang_ffi` module atom alongside the selector BIFs from
//! `selector_ffi.rs`.
//!
//! Gleam's `Nil` type compiles to the atom `nil` in BEAM bytecode, so all
//! functions that return Gleam Nil use `Term::atom(Atom::NIL)` (the atom `nil`)
//! rather than `Term::NIL` (the empty list `[]`).

use std::time::Duration;

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::scheduler::dirty::DirtySchedulerKind;
use crate::term::Term;

/// The Gleam `nil` atom, distinct from the BEAM empty list (`Term::NIL`).
const GLEAM_NIL: Term = Term::atom(Atom::NIL);

type GleamBif = (
    &'static str,
    u8,
    Capability,
    Option<DirtySchedulerKind>,
    NativeFn,
);

const GLEAM_PROCESS_BIFS: &[GleamBif] = &[
    // R1: Process flag and link wrappers
    ("trap_exits", 1, Capability::Pure, None, bif_trap_exits),
    ("link", 1, Capability::Pure, None, bif_gleam_link),
    ("demonitor", 1, Capability::Pure, None, bif_gleam_demonitor),
    // R2: Sleep, flush, registry wrappers
    (
        "sleep",
        1,
        Capability::Clock,
        Some(DirtySchedulerKind::Io),
        bif_sleep,
    ),
    (
        "sleep_forever",
        0,
        Capability::Clock,
        None,
        bif_sleep_forever,
    ),
    (
        "flush_messages",
        0,
        Capability::Pure,
        None,
        bif_flush_messages,
    ),
    (
        "register_process",
        2,
        Capability::Pure,
        None,
        bif_register_process,
    ),
    (
        "unregister_process",
        1,
        Capability::Pure,
        None,
        bif_unregister_process,
    ),
    (
        "process_named",
        1,
        Capability::Pure,
        None,
        bif_process_named,
    ),
    // R3: pid_from_dynamic
    (
        "pid_from_dynamic",
        1,
        Capability::Pure,
        None,
        bif_pid_from_dynamic,
    ),
];

/// Registers all gleam_erlang_ffi process utility BIFs.
///
/// These are registered under the same `gleam_erlang_ffi` module as the
/// selector BIFs; the module atom must already exist in the atom table.
pub fn register_gleam_ffi_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let module = atom_table.intern("gleam_erlang_ffi");

    for &(function_name, arity, capability, dirty_kind, native_function) in GLEAM_PROCESS_BIFS {
        let function = atom_table.intern(function_name);
        if let Some(kind) = dirty_kind {
            registry.register_dirty(module, function, arity, native_function, kind, capability)?;
        } else {
            registry.register(module, function, arity, native_function, capability)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// R1: Process flag and link wrappers
// ---------------------------------------------------------------------------

/// `gleam_erlang_ffi:trap_exits/1` -- set the trap_exit process flag.
///
/// Accepts a boolean term. Delegates to the link facility's `set_trap_exit`.
/// Returns the Gleam `nil` atom.
pub fn bif_trap_exits(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [bool_term] = args else {
        return Err(badarg());
    };
    let value = atom_to_bool(*bool_term).ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.link_facility().ok_or_else(badarg)?;
    facility
        .set_trap_exit(caller_pid, value)
        .map_err(|_| badarg())?;
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:link/1` -- establish a bidirectional link.
///
/// Accepts a Pid term. Delegates to the link facility's `link`.
/// Returns the Gleam `nil` atom.
pub fn bif_gleam_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let target_pid = pid_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.link_facility().ok_or_else(badarg)?;
    facility
        .link(caller_pid, target_pid)
        .map_err(|_| badarg())?;
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:demonitor/1` -- remove a monitor by reference.
///
/// Accepts a small integer reference. Delegates to the supervision
/// facility's `demonitor`. Returns the Gleam `nil` atom.
pub fn bif_gleam_demonitor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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
    Ok(GLEAM_NIL)
}

// ---------------------------------------------------------------------------
// R2: Sleep, flush, registry wrappers
// ---------------------------------------------------------------------------

/// `gleam_erlang_ffi:sleep/1` -- suspend the calling thread for N ms.
///
/// Accepts an integer (milliseconds). Uses `std::thread::sleep`.
/// Returns the Gleam `nil` atom.
pub fn bif_sleep(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [ms_term] = args else {
        return Err(badarg());
    };
    let ms = ms_term.as_small_int().ok_or_else(badarg)?;
    if ms < 0 {
        return Err(badarg());
    }
    std::thread::sleep(Duration::from_millis(ms as u64));
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:sleep_forever/0` -- block the calling thread forever.
///
/// Loops with `Duration::MAX`. Never returns under normal conditions.
pub fn bif_sleep_forever(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    loop {
        std::thread::sleep(Duration::MAX);
    }
}

/// `gleam_erlang_ffi:flush_messages/0` -- stub for mailbox flush.
///
/// Returns the Gleam `nil` atom. Full mailbox flush is complex and
/// gleam_otp does not rely on it critically, so this is a no-op stub.
pub fn bif_flush_messages(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:register_process/2` -- register a name for a PID.
///
/// Accepts (atom Name, Pid). Delegates to the registry facility.
/// Returns the Gleam `nil` atom.
pub fn bif_register_process(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term, pid_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    facility.register(name, pid).map_err(|_| badarg())?;
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:unregister_process/1` -- remove a name registration.
///
/// Accepts an atom name. Delegates to the registry facility.
/// Returns the Gleam `nil` atom.
pub fn bif_unregister_process(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    facility.unregister(name).map_err(|_| badarg())?;
    Ok(GLEAM_NIL)
}

/// `gleam_erlang_ffi:process_named/1` -- look up a registered name.
///
/// Accepts an atom name. Returns `{ok, Pid}` if registered, or
/// `{error, nil}` if not found.
pub fn bif_process_named(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    match facility.whereis(name) {
        Some(pid) => {
            let pid_term = Term::try_pid(pid).ok_or_else(badarg)?;
            context.alloc_tuple(&[Term::atom(Atom::OK), pid_term])
        }
        None => context.alloc_tuple(&[Term::atom(Atom::ERROR), GLEAM_NIL]),
    }
}

// ---------------------------------------------------------------------------
// R3: pid_from_dynamic/1
// ---------------------------------------------------------------------------

/// `gleam_erlang_ffi:pid_from_dynamic/1` -- type-check a dynamic term as Pid.
///
/// If the term is a Pid, returns `{ok, Pid}`. Otherwise returns
/// `{error, nil}` (simplified from the full DecodeError list).
pub fn bif_pid_from_dynamic(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    if term.is_pid() {
        context.alloc_tuple(&[Term::atom(Atom::OK), *term])
    } else {
        context.alloc_tuple(&[Term::atom(Atom::ERROR), GLEAM_NIL])
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "gleam_ffi_tests.rs"]
mod tests;
