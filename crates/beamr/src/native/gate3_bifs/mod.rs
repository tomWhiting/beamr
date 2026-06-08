//! Gate 3 erlang BIFs — element, send, make_ref, spawn/1, type queries,
//! type conversion, process registry, and demonitor/2.
//!
//! These BIFs are required by gleam_erlang and gleam_otp before OTP modules
//! can execute. They follow the same registration pattern as Gate 1
//! (arithmetic) and Gate 2 (process lifecycle).

mod additional;
mod registry_bifs;
mod type_conversion;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Tuple};

pub use additional::{
    bif_binary_part, bif_bit_size, bif_is_bitstring, bif_is_map_key, bif_map_size, bif_round,
    bif_trunc, bif_unary_minus,
};
pub use registry_bifs::{bif_register, bif_unregister, bif_whereis};
pub use type_conversion::{
    bif_atom_to_binary, bif_atom_to_binary_1, bif_atom_to_binary_2, bif_atom_to_list,
    bif_binary_to_atom, bif_binary_to_existing_atom, bif_binary_to_existing_atom_2,
    bif_binary_to_list, bif_float_to_binary_2, bif_float_to_list, bif_list_to_atom,
    bif_list_to_binary, bif_list_to_existing_atom, bif_list_to_float, bif_list_to_integer,
    bif_map_get,
};

type Gate3Bif = (&'static str, u8, Capability, NativeFn);

const GATE3_BIFS: &[Gate3Bif] = &[
    ("element", 2, Capability::Pure, bif_element),
    ("send", 2, Capability::Pure, bif_send),
    ("tuple_size", 1, Capability::Pure, bif_tuple_size),
    ("make_ref", 0, Capability::Pure, bif_make_ref),
    (
        "is_process_alive",
        1,
        Capability::Pure,
        bif_is_process_alive,
    ),
    ("spawn", 1, Capability::Pure, bif_spawn_1),
    ("spawn_link", 1, Capability::Pure, bif_spawn_link_1),
    // Type conversion BIFs (R1)
    ("list_to_atom", 1, Capability::Pure, bif_list_to_atom),
    ("atom_to_list", 1, Capability::Pure, bif_atom_to_list),
    (
        "list_to_existing_atom",
        1,
        Capability::Pure,
        bif_list_to_existing_atom,
    ),
    ("list_to_integer", 1, Capability::Pure, bif_list_to_integer),
    ("list_to_float", 1, Capability::Pure, bif_list_to_float),
    ("float_to_list", 1, Capability::Pure, bif_float_to_list),
    (
        "float_to_binary",
        2,
        Capability::Pure,
        bif_float_to_binary_2,
    ),
    ("binary_to_atom", 1, Capability::Pure, bif_binary_to_atom),
    ("atom_to_binary", 1, Capability::Pure, bif_atom_to_binary_1),
    ("atom_to_binary", 2, Capability::Pure, bif_atom_to_binary_2),
    (
        "binary_to_existing_atom",
        1,
        Capability::Pure,
        bif_binary_to_existing_atom,
    ),
    (
        "binary_to_existing_atom",
        2,
        Capability::Pure,
        bif_binary_to_existing_atom_2,
    ),
    ("binary_to_list", 1, Capability::Pure, bif_binary_to_list),
    ("list_to_binary", 1, Capability::Pure, bif_list_to_binary),
    ("map_get", 2, Capability::Pure, bif_map_get),
    // Process registry BIFs (R2)
    ("register", 2, Capability::Pure, bif_register),
    ("unregister", 1, Capability::Pure, bif_unregister),
    ("whereis", 1, Capability::Pure, bif_whereis),
    // demonitor/2 (R3)
    ("demonitor", 2, Capability::Pure, bif_demonitor_2),
    // OTP support BIFs (B-032)
    ("pid_to_list", 1, Capability::Pure, bif_pid_to_list),
    ("byte_size", 1, Capability::Pure, bif_byte_size),
    ("iolist_size", 1, Capability::Pure, bif_iolist_size),
    ("++", 2, Capability::Pure, bif_list_append),
    ("not", 1, Capability::Pure, bif_not),
    ("length", 1, Capability::Pure, bif_length),
    ("round", 1, Capability::Pure, bif_round),
    ("trunc", 1, Capability::Pure, bif_trunc),
    ("is_bitstring", 1, Capability::Pure, bif_is_bitstring),
    ("is_map_key", 2, Capability::Pure, bif_is_map_key),
    ("map_size", 1, Capability::Pure, bif_map_size),
    ("binary_part", 3, Capability::Pure, bif_binary_part),
    ("bit_size", 1, Capability::Pure, bif_bit_size),
    ("-", 1, Capability::Pure, bif_unary_minus),
];

/// Global monotonic counter for make_ref/0.
static REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Registers all Gate 3 BIFs into the VM-owned BIF registry.
pub fn register_gate3_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in GATE3_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
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
/// Delivery to the attached current process is supported for self-send;
/// sends to other processes are otherwise silently dropped for now. Returns
/// Message, matching BEAM's return-value semantics.
pub fn bif_send(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term, message_term] = args else {
        return Err(badarg());
    };
    let target = pid_term.as_pid().ok_or_else(badarg)?;
    let _ = context.send_to_attached_self(target, *message_term);
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

    if closure.arity() != 0 || closure.num_free() != 0 {
        return Err(badarg());
    }

    let module = closure.module().ok_or_else(badarg)?;
    let lambda_index = closure.function_index() as u32;

    let caller_pid = context.pid().ok_or_else(badarg)?;
    let link_to = if link { Some(caller_pid) } else { None };

    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let new_pid = facility
        .spawn_lambda(caller_pid, module, lambda_index, link_to)
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

/// erlang:pid_to_list/1 — converts a PID to its string representation.
///
/// Returns a binary containing `"<0.N.0>"` for PID N, matching the Erlang
/// convention for local PIDs.
pub fn bif_pid_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [pid_term] = args else {
        return Err(badarg());
    };
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let repr = format!("<0.{pid}.0>");
    let bytes = repr.as_bytes();

    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    context.alloc_list(&elements)
}

/// erlang:byte_size/1 — returns the byte length of a binary.
pub fn bif_byte_size(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [binary_term] = args else {
        return Err(badarg());
    };
    binary_size(*binary_term)
}

/// erlang:iolist_size/1 — returns the byte length of a binary iolist stub.
///
/// This stub intentionally accepts binaries only; proper nested iolists are
/// outside B-033 scope and return `badarg`.
pub fn bif_iolist_size(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [binary_term] = args else {
        return Err(badarg());
    };
    binary_size(*binary_term)
}

fn binary_size(term: Term) -> Result<Term, Term> {
    let binary = Binary::new(term).ok_or_else(badarg)?;
    i64::try_from(binary.len())
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:++/2 — appends two proper lists.
///
/// Returns a new list that is the concatenation of `ListA ++ ListB`.
pub fn bif_list_append(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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

    context.alloc_list_with_tail(&elements, *list_b)
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
#[path = "additional_tests.rs"]
mod additional_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "demonitor2_tests.rs"]
mod demonitor2_tests;
