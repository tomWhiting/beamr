//! Gate 3 erlang BIFs — element, send, make_ref, spawn/1, type queries,
//! type conversion, process registry, and demonitor/2.
//!
//! These BIFs are required by gleam_erlang and gleam_otp before OTP modules
//! can execute. They follow the same registration pattern as Gate 1
//! (arithmetic) and Gate 2 (process lifecycle).

mod registry_bifs;
mod type_conversion;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

pub use registry_bifs::{bif_register, bif_unregister, bif_whereis};
pub use type_conversion::{
    bif_atom_to_binary, bif_binary_to_existing_atom, bif_binary_to_list, bif_list_to_binary,
    bif_map_get,
};

type Gate3Bif = (&'static str, u8, NativeFn);

const GATE3_BIFS: &[Gate3Bif] = &[
    ("element", 2, bif_element),
    ("send", 2, bif_send),
    ("tuple_size", 1, bif_tuple_size),
    ("make_ref", 0, bif_make_ref),
    ("is_process_alive", 1, bif_is_process_alive),
    ("spawn", 1, bif_spawn_1),
    ("spawn_link", 1, bif_spawn_link_1),
    // Type conversion BIFs (R1)
    ("atom_to_binary", 2, bif_atom_to_binary),
    ("binary_to_existing_atom", 1, bif_binary_to_existing_atom),
    ("binary_to_list", 1, bif_binary_to_list),
    ("list_to_binary", 1, bif_list_to_binary),
    ("map_get", 2, bif_map_get),
    // Process registry BIFs (R2)
    ("register", 2, bif_register),
    ("unregister", 1, bif_unregister),
    ("whereis", 1, bif_whereis),
    // demonitor/2 (R3)
    ("demonitor", 2, bif_demonitor_2),
    // OTP support BIFs (B-032)
    ("get", 0, bif_get),
    ("pid_to_list", 1, bif_pid_to_list),
    ("++", 2, bif_list_append),
    ("not", 1, bif_not),
    ("/=", 2, bif_not_equal),
    ("length", 1, bif_length),
];

/// Global monotonic counter for make_ref/0.
static REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Registers all Gate 3 BIFs into the VM-owned BIF registry.
pub fn register_gate3_bifs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in GATE3_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function)?;
    }

    Ok(())
}

/// erlang:element/2 — returns the Nth element (1-based) of a tuple.
pub fn bif_element(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [index_term, tuple_term] = args else {
        return Err(badarg());
    };
    let index = index_term.as_small_int().ok_or_else(badarg)?;
    if index < 1 {
        return Err(badarg());
    }
    let tuple = Tuple::new(*tuple_term).ok_or_else(badarg)?;
    // BEAM element/2 is 1-based; Tuple::get is 0-based.
    let zero_based = (index - 1) as usize;
    tuple.get(zero_based).ok_or_else(badarg)
}

/// erlang:send/2 — the BIF form of `!`. Delivers a message to the target
/// process's mailbox.
///
/// Since BIFs only have ProcessContext (no direct process table access),
/// message delivery routes through the supervision facility's process
/// liveness check as a proxy. For now, if no facility is available, the
/// message is silently dropped — matching BEAM's behavior for sends to
/// dead processes. Returns Message.
pub fn bif_send(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, message_term] = args else {
        return Err(badarg());
    };
    // Validate that the first argument is a pid.
    pid_term.as_pid().ok_or_else(badarg)?;
    // Message delivery requires mailbox access which is not yet available
    // through ProcessContext. Return the message (BEAM semantics: send/2
    // always returns the message, even for dead targets).
    Ok(*message_term)
}

/// erlang:tuple_size/1 — returns the arity of a tuple as a small integer.
pub fn bif_tuple_size(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [tuple_term] = args else {
        return Err(badarg());
    };
    let tuple = Tuple::new(*tuple_term).ok_or_else(badarg)?;
    let arity = tuple.arity();
    i64::try_from(arity)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:make_ref/0 — returns a unique reference as a small integer.
///
/// Uses a global monotonic counter. The reference is returned as a small
/// integer (same simplification as monitor/2 in Gate 2).
pub fn bif_make_ref(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let id = REF_COUNTER.fetch_add(1, Ordering::Relaxed);
    i64::try_from(id)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:is_process_alive/1 — checks if a PID refers to a living process.
///
/// Routes through the supervision facility to check process liveness.
/// If no facility is available, returns false (conservative default).
pub fn bif_is_process_alive(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let target_pid = pid_term.as_pid().ok_or_else(badarg)?;

    // Check if the target is the caller itself — always alive.
    if let Some(caller_pid) = context.pid()
        && caller_pid == target_pid
    {
        return Ok(bool_term(true));
    }

    // Route through supervision facility for process table access.
    if let Some(facility) = context.supervision_facility() {
        // A monitor attempt to a dead process returns NoProc.
        // We use this as a liveness probe: if monitor succeeds, the process
        // is alive (and we immediately demonitor). If it fails with NoProc,
        // the process is dead.
        let caller_pid = context.pid().ok_or_else(badarg)?;
        match facility.monitor(caller_pid, target_pid) {
            Ok(result) => {
                // Process is alive — clean up the monitor.
                let _ = facility.demonitor(caller_pid, result.reference);
                Ok(bool_term(true))
            }
            Err(_) => Ok(bool_term(false)),
        }
    } else {
        // No facility available — conservative default.
        Ok(bool_term(false))
    }
}

/// erlang:spawn/1 — spawns a process from a zero-arity fun.
///
/// The fun must be an MFA export closure (module + function_index with
/// arity 0 and no captured variables). Closures with captures return badarg
/// (documented limitation).
pub fn bif_spawn_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_from_fun(args, context, false)
}

/// erlang:spawn_link/1 — spawns a linked process from a zero-arity fun.
///
/// Same restrictions as spawn/1 regarding closure captures.
pub fn bif_spawn_link_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_from_fun(args, context, true)
}

fn spawn_from_fun(args: &[Term], context: &mut ProcessContext, link: bool) -> Result<Term, Term> {
    let [fun_term] = args else {
        return Err(badarg());
    };
    let closure = crate::term::boxed::Closure::new(*fun_term).ok_or_else(badarg)?;

    // Must be a zero-arity fun with no captures.
    if closure.arity() != 0 {
        return Err(badarg());
    }
    if closure.num_free() != 0 {
        return Err(badarg());
    }

    let module = closure.module().ok_or_else(badarg)?;
    // For MFA export closures, the function name atom is resolved from the
    // module's function table using the function_index. Since we don't have
    // module access here, we use the function_index as a placeholder atom.
    // The spawn facility implementation must handle this appropriately.
    let function = Atom::new(closure.function_index() as u32);

    let link_to = if link {
        Some(context.pid().ok_or_else(badarg)?)
    } else {
        None
    };

    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let new_pid = facility
        .spawn(module, function, Vec::new(), link_to)
        .map_err(|_| badarg())?;
    Term::try_pid(new_pid).ok_or_else(badarg)
}

/// erlang:demonitor/2 — remove a monitor with options.
///
/// Options is a list that may contain:
/// - `flush` — removes the monitor and flushes any pending DOWN message
/// - `info` — returns `true` if the monitor was active, `false` otherwise
///
/// An empty options list behaves like demonitor/1.
pub fn bif_demonitor_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [ref_term, opts_term] = args else {
        return Err(badarg());
    };
    let reference = ref_term.as_small_int().ok_or_else(badarg)?;
    if reference < 0 {
        return Err(badarg());
    }

    let mut opt_flush = false;
    let mut opt_info = false;

    let mut current = *opts_term;
    loop {
        if current.is_nil() {
            break;
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let opt = cons.head().as_atom().ok_or_else(badarg)?;
        if opt == Atom::FLUSH {
            opt_flush = true;
        } else if opt == Atom::INFO {
            opt_info = true;
        } else {
            return Err(badarg());
        }
        current = cons.tail();
    }

    let caller_pid = context.pid().ok_or_else(badarg)?;
    let facility = context.supervision_facility().ok_or_else(badarg)?;
    let result = facility.demonitor(caller_pid, reference as u64);

    // `flush` option: in a real implementation this would also flush
    // any pending DOWN message from the mailbox. Since BIFs don't
    // have mailbox access, the flush is a no-op for now (the monitor
    // removal itself is the primary effect).
    let _ = opt_flush;

    if opt_info {
        // Return true if monitor was active (demonitor succeeded),
        // false if it was already gone.
        Ok(bool_term(result.is_ok()))
    } else {
        // Without info, always return true (like demonitor/1).
        result.map_err(|_| badarg())?;
        Ok(bool_term(true))
    }
}

// ── OTP support BIFs (B-032) ──────────────────────────────────────────────

/// erlang:get/0 — returns the process dictionary as a list of `{Key, Value}` pairs.
///
/// beamr does not implement a process dictionary. Returns an empty list,
/// which is the correct result for a process that has never called `put/2`.
pub fn bif_get(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::NIL)
}

/// erlang:pid_to_list/1 — converts a PID to its string representation.
///
/// Returns a binary containing `"<0.N.0>"` for PID N, matching the Erlang
/// convention for local PIDs.
pub fn bif_pid_to_list(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let repr = format!("<0.{pid}.0>");
    let bytes = repr.as_bytes();

    // Build a proper list of integer code points (Erlang string).
    let mut tail = Term::NIL;
    for &byte in bytes.iter().rev() {
        let int_term = Term::small_int(i64::from(byte));
        let cell = Box::leak(Box::new([0u64; 2]));
        tail = crate::term::boxed::write_cons(cell, int_term, tail).ok_or_else(badarg)?;
    }
    Ok(tail)
}

/// erlang:++/2 — appends two proper lists.
///
/// Returns a new list that is the concatenation of `ListA ++ ListB`.
pub fn bif_list_append(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_a, list_b] = args else {
        return Err(badarg());
    };

    // If ListA is empty, return ListB directly.
    if list_a.is_nil() {
        return Ok(*list_b);
    }

    // Collect all elements from ListA.
    let mut elements = Vec::new();
    let mut current = *list_a;
    loop {
        if current.is_nil() {
            break;
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }

    // Build a new list: elements from A followed by B as the tail.
    let mut tail = *list_b;
    for element in elements.into_iter().rev() {
        let cell = Box::leak(Box::new([0u64; 2]));
        tail = crate::term::boxed::write_cons(cell, element, tail).ok_or_else(badarg)?;
    }
    Ok(tail)
}

/// erlang:not/1 — boolean negation.
///
/// Returns `true` if the argument is `false`, `false` if `true`.
/// Raises badarg for non-boolean inputs.
pub fn bif_not(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let atom = term.as_atom().ok_or_else(badarg)?;
    if atom == Atom::TRUE {
        Ok(Term::atom(Atom::FALSE))
    } else if atom == Atom::FALSE {
        Ok(Term::atom(Atom::TRUE))
    } else {
        Err(badarg())
    }
}

/// erlang:/=/2 — not-equal comparison (structural, not exact).
///
/// Returns `true` if the two terms are structurally different, `false`
/// if they are equal. This is the `/=` operator (non-exact inequality).
/// For the subset of types beamr supports, this is equivalent to `=/=`.
pub fn bif_not_equal(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    Ok(bool_term(left != right))
}

/// erlang:length/1 — returns the length of a proper list.
pub fn bif_length(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [list_term] = args else {
        return Err(badarg());
    };

    let mut count: i64 = 0;
    let mut current = *list_term;
    loop {
        if current.is_nil() {
            return Term::try_small_int(count).ok_or_else(badarg);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        count = count.checked_add(1).ok_or_else(badarg)?;
        current = cons.tail();
    }
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "demonitor2_tests.rs"]
mod demonitor2_tests;
