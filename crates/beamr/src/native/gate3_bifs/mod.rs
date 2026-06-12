//! Gate 3 erlang BIFs — element, send, make_ref, spawn/1, type queries,
//! type conversion, process registry, and demonitor/2.
//!
//! These BIFs are required by gleam_erlang and gleam_otp before OTP modules
//! can execute. They follow the same registration pattern as Gate 1
//! (arithmetic) and Gate 2 (process lifecycle).

mod additional;
mod registry_bifs;
mod type_conversion;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::atom::{Atom, AtomTable};
use crate::distribution::DEFAULT_NODE_NAME;
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Tuple};
use crate::term::pid_ref::PidRef;
use crate::term::reference_ref::ReferenceRef;

pub use additional::{
    bif_binary_part, bif_bit_size, bif_is_bitstring, bif_is_function_1, bif_is_function_2,
    bif_is_map_key, bif_map_size, bif_round, bif_trunc, bif_unary_minus,
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
    ("node", 0, Capability::Pure, bif_node_0),
    ("node", 1, Capability::Pure, bif_node_1),
    ("is_alive", 0, Capability::Pure, bif_is_alive_0),
    ("nodes", 0, Capability::ExternalIo, bif_nodes_0),
    (
        "disconnect_node",
        1,
        Capability::ExternalIo,
        bif_disconnect_node_1,
    ),
    (
        "is_process_alive",
        1,
        Capability::Pure,
        bif_is_process_alive,
    ),
    ("spawn", 1, Capability::Spawn, bif_spawn_1),
    ("spawn_link", 1, Capability::Spawn, bif_spawn_link_1),
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
    ("is_function", 1, Capability::Pure, bif_is_function_1),
    ("is_function", 2, Capability::Pure, bif_is_function_2),
    ("is_map_key", 2, Capability::Pure, bif_is_map_key),
    ("map_size", 1, Capability::Pure, bif_map_size),
    ("binary_part", 3, Capability::Pure, bif_binary_part),
    ("bit_size", 1, Capability::Pure, bif_bit_size),
    ("-", 1, Capability::Pure, bif_unary_minus),
    // Hashing BIFs (B-129)
    ("phash2", 1, Capability::Pure, bif_phash2_1),
    ("phash2", 2, Capability::Pure, bif_phash2_2),
    // Time BIFs (B-129)
    ("monotonic_time", 0, Capability::Clock, bif_monotonic_time_0),
    ("system_time", 0, Capability::Clock, bif_system_time_0),
    ("monotonic_time", 1, Capability::Clock, bif_monotonic_time_1),
    ("system_time", 1, Capability::Clock, bif_system_time_1),
    ("time_offset", 0, Capability::Clock, bif_time_offset_0),
    // Unique-value BIFs (B-129)
    ("unique_integer", 0, Capability::Pure, bif_unique_integer_0),
    ("unique_integer", 1, Capability::Pure, bif_unique_integer_1),
    // Misc utility BIFs (B-129). ETF term_to_binary/1 and binary_to_term/1
    // are registered by Gate 1's ETF registration path and intentionally not
    // duplicated here.
    ("min", 2, Capability::Pure, bif_min_2),
    ("max", 2, Capability::Pure, bif_max_2),
    ("abs", 1, Capability::Pure, bif_abs_1),
];

/// Global monotonic counter for make_ref/0.
static REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Global monotonic counter for unique_integer/0,1.
static UNIQUE_INTEGER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// VM-local epoch for native monotonic time units.
///
/// Native time units are microseconds so current UNIX wall time fits in the
/// runtime's immediate small-integer representation.
static MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

const PHASH2_DEFAULT_RANGE: u64 = 1 << 27;

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
    let target = PidRef::new(*pid_term).ok_or_else(badarg)?;
    if target.is_local() {
        let _ = context.send_to_attached_self(target.pid_number(), *message_term);
    } else {
        let noconnection = Term::atom(
            context
                .atom_table()
                .map_or(Atom::ERROR, |table| table.intern("noconnection")),
        );
        let facility = context.distribution_send_facility().ok_or(noconnection)?;
        facility
            .send_remote(*pid_term, *message_term)
            .map_err(|_| noconnection)?;
    }
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

/// erlang:make_ref/0 — returns a unique local reference.
pub fn bif_make_ref(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let id = REF_COUNTER.fetch_add(1, Ordering::Relaxed);
    context.alloc_reference(id)
}

/// erlang:node/0 — returns the local node name atom.
pub fn bif_node_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    let local_node = context.local_node().ok_or_else(badarg)?;
    Ok(Term::atom(local_node.name))
}

/// erlang:node/1 — returns the originating node of a PID or reference term.
pub fn bif_node_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let local_node = context.local_node().ok_or_else(badarg)?;
    if let Some(pid) = PidRef::new(*term) {
        return Ok(Term::atom(pid.node().unwrap_or(local_node.name)));
    }
    if let Some(reference) = ReferenceRef::new(*term) {
        return Ok(Term::atom(reference.node().unwrap_or(local_node.name)));
    }
    Err(badarg())
}

/// erlang:is_alive/0 — true when distribution is configured with a non-default node name.
pub fn bif_is_alive_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    let alive = context
        .local_node()
        .and_then(|node| {
            context
                .atom_table()
                .and_then(|table| table.resolve(node.name))
        })
        .is_some_and(|name| name != DEFAULT_NODE_NAME);
    Ok(Term::atom(if alive { Atom::TRUE } else { Atom::FALSE }))
}

/// erlang:nodes/0 — returns connected visible node names.
pub fn bif_nodes_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    let elements: Vec<Term> = context
        .net_kernel()
        .map(|net_kernel| net_kernel.nodes().into_iter().map(Term::atom).collect())
        .unwrap_or_default();
    context.alloc_list(&elements)
}

/// erlang:disconnect_node/1 — manually closes a distribution connection.
pub fn bif_disconnect_node_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [node_term] = args else {
        return Err(badarg());
    };
    let node = node_term.as_atom().ok_or_else(badarg)?;
    let disconnected = context
        .net_kernel()
        .is_some_and(|net_kernel| net_kernel.disconnect_node(node));
    Ok(Term::atom(if disconnected {
        Atom::TRUE
    } else {
        Atom::FALSE
    }))
}

/// erlang:phash2/1 — deterministic portable hash in the default range.
pub fn bif_phash2_1(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    phash2(*term, PHASH2_DEFAULT_RANGE)
}

/// erlang:phash2/2 — deterministic portable hash modulo Range.
pub fn bif_phash2_2(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term, range_term] = args else {
        return Err(badarg());
    };
    let range = range_term
        .as_small_int()
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(badarg)?;
    phash2(*term, range)
}

fn phash2(term: Term, range: u64) -> Result<Term, Term> {
    let value = crate::term::hash::term_hash(term) % range;
    i64::try_from(value)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:monotonic_time/0 — returns elapsed VM-local native time units.
pub fn bif_monotonic_time_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    if let Some(value) = replay_native_result(context, "monotonic_time", 0)? {
        return Ok(value);
    }
    native_time_term(monotonic_time_native()?)
}

/// erlang:system_time/0 — returns wall-clock native time units since UNIX epoch.
pub fn bif_system_time_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    if let Some(value) = replay_native_result(context, "system_time", 0)? {
        return Ok(value);
    }
    native_time_term(system_time_native()?)
}

/// erlang:monotonic_time/1 — returns monotonic time converted to Unit.
pub fn bif_monotonic_time_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [unit_term] = args else {
        return Err(badarg());
    };
    let unit = parse_time_unit(*unit_term, context)?;
    if let Some(value) = replay_native_result(context, "monotonic_time", 1)? {
        return Ok(value);
    }
    native_time_term(convert_native_time(monotonic_time_native()?, unit)?)
}

/// erlang:system_time/1 — returns wall-clock time converted to Unit.
pub fn bif_system_time_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [unit_term] = args else {
        return Err(badarg());
    };
    let unit = parse_time_unit(*unit_term, context)?;
    if let Some(value) = replay_native_result(context, "system_time", 1)? {
        return Ok(value);
    }
    native_time_term(convert_native_time(system_time_native()?, unit)?)
}

/// erlang:time_offset/0 — returns system_time(native) - monotonic_time(native).
pub fn bif_time_offset_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    if let Some(value) = replay_native_result(context, "time_offset", 0)? {
        return Ok(value);
    }
    let offset = i128::from(system_time_native()?) - i128::from(monotonic_time_native()?);
    i64::try_from(offset)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

/// erlang:unique_integer/0 — returns a globally unique integer.
pub fn bif_unique_integer_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    if let Some(value) = replay_native_result(context, "unique_integer", 0)? {
        return Ok(value);
    }
    next_unique_integer()
}

/// erlang:unique_integer/1 — returns a globally unique integer with options.
pub fn bif_unique_integer_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [options_term] = args else {
        return Err(badarg());
    };
    parse_unique_integer_options(*options_term, context)?;
    if let Some(value) = replay_native_result(context, "unique_integer", 1)? {
        return Ok(value);
    }
    next_unique_integer()
}

/// erlang:min/2 — returns the smaller term by BEAM term ordering.
pub fn bif_min_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    if crate::term::compare::cmp(*left, *right, atom_table).is_gt() {
        Ok(*right)
    } else {
        Ok(*left)
    }
}

/// erlang:max/2 — returns the larger term by BEAM term ordering.
pub fn bif_max_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    if crate::term::compare::cmp(*left, *right, atom_table).is_lt() {
        Ok(*right)
    } else {
        Ok(*left)
    }
}

/// erlang:abs/1 — returns the absolute value of an integer or float.
///
/// Integer results promote to a bignum when they leave the small-integer
/// range and demote back to a small immediate when they fit.
pub fn bif_abs_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    if let Some(integer) = value.as_small_int()
        && let Some(absolute) = integer.checked_abs().and_then(Term::try_small_int)
    {
        return Ok(absolute);
    }
    // Small integers whose absolute value overflows fall through here too.
    if let Some(integer) = crate::term::bigint_math::BigIntValue::from_term(*value) {
        return crate::native::bifs::integer_result(integer.abs(), context);
    }
    let value = Float::new(*value).ok_or_else(badarg)?.value();
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value.abs())
}

#[derive(Copy, Clone)]
enum TimeUnit {
    Native,
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
}

fn replay_native_result(
    context: &mut ProcessContext,
    function_name: &str,
    arity: u8,
) -> Result<Option<Term>, Term> {
    let Some(driver) = context.replay_driver().cloned() else {
        return Ok(None);
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let module = atom_table.intern("erlang");
    let function = atom_table.intern(function_name);
    let pid = context.pid().ok_or_else(badarg)?;
    let recorded = match driver.lock() {
        Ok(mut guard) => guard.next_native_call(pid, module, function, arity),
        Err(error) => error
            .into_inner()
            .next_native_call(pid, module, function, arity),
    }
    .map_err(|_| badarg())?;
    match recorded.outcome.result {
        Ok(value) => Ok(Some(value)),
        Err(reason) => {
            context.set_exception_class(recorded.outcome.exception_class);
            context.set_exception_stacktrace(recorded.outcome.exception_stacktrace);
            Err(reason)
        }
    }
}

fn monotonic_time_native() -> Result<i64, Term> {
    let epoch = MONOTONIC_EPOCH.get_or_init(Instant::now);
    duration_to_native(epoch.elapsed())
}

fn system_time_native() -> Result<i64, Term> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| badarg())?;
    duration_to_native(elapsed)
}

fn duration_to_native(duration: Duration) -> Result<i64, Term> {
    i64::try_from(duration.as_micros()).map_err(|_| badarg())
}

fn native_time_term(value: i64) -> Result<Term, Term> {
    Term::try_small_int(value).ok_or_else(badarg)
}

fn parse_time_unit(unit_term: Term, context: &mut ProcessContext) -> Result<TimeUnit, Term> {
    let unit_atom = unit_term.as_atom().ok_or_else(badarg)?;
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    match atom_table.resolve(unit_atom) {
        Some("native") => Ok(TimeUnit::Native),
        Some("nanosecond") => Ok(TimeUnit::Nanosecond),
        Some("microsecond") => Ok(TimeUnit::Microsecond),
        Some("millisecond") => Ok(TimeUnit::Millisecond),
        Some("second") => Ok(TimeUnit::Second),
        _ => Err(badarg()),
    }
}

fn convert_native_time(value: i64, unit: TimeUnit) -> Result<i64, Term> {
    match unit {
        TimeUnit::Native | TimeUnit::Microsecond => Ok(value),
        TimeUnit::Nanosecond => value.checked_mul(1_000).ok_or_else(badarg),
        TimeUnit::Millisecond => Ok(value / 1_000),
        TimeUnit::Second => Ok(value / 1_000_000),
    }
}

fn parse_unique_integer_options(
    options_term: Term,
    context: &mut ProcessContext,
) -> Result<(), Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let mut current = options_term;
    loop {
        if current.is_nil() {
            return Ok(());
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let option = cons.head().as_atom().ok_or_else(badarg)?;
        match atom_table.resolve(option) {
            Some("positive" | "monotonic") => {}
            _ => return Err(badarg()),
        }
        current = cons.tail();
    }
}

fn next_unique_integer() -> Result<Term, Term> {
    let id = UNIQUE_INTEGER_COUNTER.fetch_add(1, Ordering::Relaxed);
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
    let reference = if let Some(reference) = ReferenceRef::new(*ref_term) {
        reference.id()
    } else {
        let legacy_reference = ref_term.as_small_int().ok_or_else(badarg)?;
        if legacy_reference < 0 {
            return Err(badarg());
        }
        legacy_reference as u64
    };

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
    let result = facility.demonitor(caller_pid, reference);

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
    let bytes = match BinaryRef::new(term) {
        Some(binary) => binary.len(),
        // Compiler-reused match contexts: `byte_size` of a match tail is
        // emitted on the context register (see
        // `match_context_remaining_bits`).
        None => crate::interpreter::opcodes::binary::match_context_remaining_bits(term)
            .ok_or_else(badarg)?
            .div_ceil(u8::BITS as usize),
    };
    i64::try_from(bytes)
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

    if list_b.is_nil() {
        if let Some(binary) = BinaryRef::new(*list_a) {
            return context.alloc_binary(binary.as_bytes());
        }
        if Cons::new(*list_a).is_none() {
            return Ok(*list_a);
        }
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
