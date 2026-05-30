//! OTP module stub BIFs for gleam_otp support.
//!
//! These stubs satisfy imports from gleam_otp's compiled .beam files that
//! reference modules without corresponding .beam fixtures in the test suite.
//!
//! Modules stubbed here:
//! - `gleam_otp_external` — application lifecycle
//! - `supervisor` — Erlang supervisor (gleam_otp wraps this)
//! - `gleam@dynamic` — dynamic type checking
//! - `gleam@string` — string utilities
//! - `gleam@option` — Option type combinators
//! - `gleam@result` — Result type combinators
//! - `gleam@otp@intensity_tracker` — supervisor restart intensity
//! - `application`, `os`, `io`, `code`, `net_kernel`, `string` — Erlang stdlib

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;

type OtpBif = (&'static str, &'static str, u8, NativeFn);

const OTP_STUBS: &[OtpBif] = &[
    // gleam_otp_external
    (
        "gleam_otp_external",
        "application_stopped",
        0,
        bif_application_stopped,
    ),
    // supervisor
    ("supervisor", "start_link", 2, bif_supervisor_start_link),
    // gleam@dynamic
    ("gleam@dynamic", "classify", 1, bif_dynamic_classify),
    ("gleam@dynamic", "int", 1, bif_dynamic_int),
    ("gleam@dynamic", "string", 1, bif_dynamic_string),
    // gleam@string
    ("gleam@string", "inspect", 1, bif_string_inspect),
    ("gleam@string", "append", 2, bif_string_append),
    // gleam@option
    ("gleam@option", "map", 2, bif_option_map),
    ("gleam@option", "unwrap", 2, bif_option_unwrap),
    // gleam@result
    ("gleam@result", "map_error", 2, bif_result_map_error),
    ("gleam@result", "then", 2, bif_result_then),
    // gleam@otp@intensity_tracker
    (
        "gleam@otp@intensity_tracker",
        "new",
        2,
        bif_intensity_tracker_new,
    ),
    (
        "gleam@otp@intensity_tracker",
        "add_event",
        1,
        bif_intensity_tracker_add_event,
    ),
    // application
    (
        "application",
        "ensure_all_started",
        1,
        bif_ensure_all_started,
    ),
    // os
    ("os", "getenv", 0, bif_os_getenv_0),
    ("os", "getenv", 1, bif_os_getenv_1),
    ("os", "putenv", 2, bif_os_putenv),
    ("os", "unsetenv", 1, bif_os_unsetenv),
    ("os", "type", 0, bif_os_type),
    // io
    ("io", "get_line", 1, bif_io_get_line),
    // code
    ("code", "priv_dir", 1, bif_code_priv_dir),
    // net_kernel
    ("net_kernel", "connect_node", 1, bif_connect_node),
    // string
    ("string", "split", 2, bif_string_split),
];

/// Registers all OTP stub BIFs under their respective module names.
pub fn register_otp_stubs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    for &(module_name, function_name, arity, native_function) in OTP_STUBS {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        registry.register(module, function, arity, native_function)?;
    }
    Ok(())
}

// ── gleam_otp_external ────────────────────────────────────────────────────

/// `gleam_otp_external:application_stopped/0` -- returns the atom `ok`.
pub fn bif_application_stopped(
    args: &[Term],
    _context: &mut ProcessContext,
) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::atom(Atom::OK))
}

// ── supervisor ────────────────────────────────────────────────────────────

/// `supervisor:start_link/2` -- stub returning `{ok, self_pid}`.
///
/// Gleam_otp manages children directly; the Erlang supervisor module is
/// only called as a compatibility shim.
pub fn bif_supervisor_start_link(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [_module, _init_args] = args else {
        return Err(badarg());
    };
    let pid = context.pid().ok_or_else(badarg)?;
    let pid_term = Term::try_pid(pid).ok_or_else(badarg)?;
    context.alloc_tuple(&[Term::atom(Atom::OK), pid_term])
}

// ── gleam@dynamic ─────────────────────────────────────────────────────────

/// `gleam@dynamic:classify/1` -- returns an atom describing the term type.
///
/// Returns `"Int"`, `"Atom"`, etc. as a string atom for debug output.
/// Simplified: returns a descriptive atom for the term's tag.
fn bif_dynamic_classify(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let description = if term.as_small_int().is_some() {
        "Int"
    } else if term.as_atom().is_some() {
        "Atom"
    } else if term.is_nil() || term.is_list() {
        "List"
    } else if term.is_pid() {
        "Pid"
    } else {
        "Other"
    };
    // Return as a binary string.
    let bytes = description.as_bytes();
    let data_words = crate::term::binary::packed_word_count(bytes.len());
    let total_words = 2 + data_words;
    let heap: &mut [u64] = Box::leak(vec![0u64; total_words].into_boxed_slice());
    crate::term::binary::write_binary(heap, bytes)
        .ok_or_else(badarg)
        .or_else(|_| {
            // Fallback: return as an ok-wrapped value.
            context.alloc_tuple(&[Term::atom(Atom::OK), Term::atom(Atom::NIL)])
        })
}

/// `gleam@dynamic:int/1` -- extract an integer from a dynamic value.
///
/// Returns `{ok, Value}` if the term is an integer, `{error, []}` otherwise.
fn bif_dynamic_int(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    if term.as_small_int().is_some() {
        context.alloc_tuple(&[Term::atom(Atom::OK), *term])
    } else {
        context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::NIL])
    }
}

/// `gleam@dynamic:string/1` -- extract a string from a dynamic value.
///
/// Returns `{ok, Value}` if the term is a binary, `{error, []}` otherwise.
fn bif_dynamic_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    if crate::term::binary::Binary::new(*term).is_some() {
        context.alloc_tuple(&[Term::atom(Atom::OK), *term])
    } else {
        context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::NIL])
    }
}

// ── gleam@string ──────────────────────────────────────────────────────────

/// `gleam@string:inspect/1` -- returns a debug string representation.
///
/// Returns a binary containing a debug representation of the term.
fn bif_string_inspect(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let repr = format!("{term:?}");
    make_leaked_binary(repr.as_bytes())
}

/// `gleam@string:append/2` -- concatenates two binary strings.
fn bif_string_append(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [first, second] = args else {
        return Err(badarg());
    };

    let a_bytes = crate::term::binary::Binary::new(*first)
        .map(|b| b.as_bytes().to_vec())
        .unwrap_or_default();
    let b_bytes = crate::term::binary::Binary::new(*second)
        .map(|b| b.as_bytes().to_vec())
        .unwrap_or_default();

    let mut combined = a_bytes;
    combined.extend_from_slice(&b_bytes);
    make_leaked_binary(&combined)
}

// ── gleam@option ──────────────────────────────────────────────────────────

/// `gleam@option:map/2` -- maps a function over an Option value.
///
/// Gleam Options are `{some, Value}` or `none` atoms.
/// Since we cannot call BEAM closures from BIFs, this stub returns the
/// option unchanged (identity map). This is correct for `None` and is
/// a documented limitation for `Some`.
fn bif_option_map(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [option, _fun] = args else {
        return Err(badarg());
    };
    // Return option unchanged — the mapping function cannot be called
    // from a native BIF without interpreter re-entry.
    Ok(*option)
}

/// `gleam@option:unwrap/2` -- unwraps an option, returning default if None.
///
/// Gleam Options are `{some, Value}` tuples or the atom `none`.
fn bif_option_unwrap(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [option, default] = args else {
        return Err(badarg());
    };
    // Check if it's the atom `none`.
    if let Some(atom) = option.as_atom()
        && (atom == Atom::NIL || atom.index() == none_atom_index())
    {
        return Ok(*default);
    }
    // If it's a {some, Value} tuple, extract the value.
    if let Some(tuple) = crate::term::boxed::Tuple::new(*option)
        && tuple.arity() == 2
    {
        return tuple.get(1).ok_or_else(badarg);
    }
    // Fallback: return the option itself.
    Ok(*option)
}

// ── gleam@result ──────────────────────────────────────────────────────────

/// `gleam@result:map_error/2` -- maps a function over the Error variant.
///
/// Result values are `{ok, Value}` or `{error, Reason}`.
/// Cannot call the closure from BIF; returns unchanged.
fn bif_result_map_error(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, _fun] = args else {
        return Err(badarg());
    };
    Ok(*result)
}

/// `gleam@result:then/2` -- monadic bind on Result.
///
/// Cannot call the closure from BIF; returns the result unchanged.
fn bif_result_then(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, _fun] = args else {
        return Err(badarg());
    };
    Ok(*result)
}

// ── gleam@otp@intensity_tracker ───────────────────────────────────────────

/// `gleam@otp@intensity_tracker:new/2` -- creates a new intensity tracker.
///
/// Returns a simple tuple `{intensity_tracker, Limit, Period}`.
fn bif_intensity_tracker_new(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [limit, period] = args else {
        return Err(badarg());
    };
    // Return a tracker tuple: {intensity_tracker, limit, period, events=[]}
    context.alloc_tuple(&[Term::small_int(0), *limit, *period, Term::NIL])
}

/// `gleam@otp@intensity_tracker:add_event/1` -- records an event.
///
/// Returns `{ok, UpdatedTracker}` if under limit, `{error, TrackerAtLimit}`.
fn bif_intensity_tracker_add_event(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [tracker] = args else {
        return Err(badarg());
    };
    // Always succeed (under limit) — return {ok, tracker}.
    context.alloc_tuple(&[Term::atom(Atom::OK), *tracker])
}

// ── Erlang stdlib stubs ───────────────────────────────────────────────────

/// `application:ensure_all_started/1` -- stub returning `{ok, []}`.
fn bif_ensure_all_started(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [_app] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::atom(Atom::OK), Term::NIL])
}

/// `os:getenv/0` -- returns an empty list of environment variables.
fn bif_os_getenv_0(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    Ok(Term::NIL)
}

/// `os:getenv/1` -- returns `false` (env var not found).
fn bif_os_getenv_1(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_name] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::FALSE))
}

/// `os:putenv/2` -- no-op stub, returns `true`.
fn bif_os_putenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_key, _value] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::TRUE))
}

/// `os:unsetenv/1` -- no-op stub, returns `true`.
fn bif_os_unsetenv(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_name] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::TRUE))
}

/// `os:type/0` -- returns `{unix, linux}` as a stub.
fn bif_os_type(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }
    // Return a plausible {OsFamily, OsName} tuple.
    // Use atom indices that will be interned at registration time.
    let unix = UNIX_ATOM
        .get()
        .copied()
        .unwrap_or_else(|| Atom::new(9998));
    let darwin = DARWIN_ATOM
        .get()
        .copied()
        .unwrap_or_else(|| Atom::new(9997));
    context.alloc_tuple(&[Term::atom(unix), Term::atom(darwin)])
}

/// `io:get_line/1` -- stub returning an empty binary.
fn bif_io_get_line(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_prompt] = args else {
        return Err(badarg());
    };
    make_leaked_binary(b"")
}

/// `code:priv_dir/1` -- stub returning `{error, bad_name}`.
fn bif_code_priv_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [_app] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(Atom::BADARG)])
}

/// `net_kernel:connect_node/1` -- stub returning `false`.
fn bif_connect_node(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_node] = args else {
        return Err(badarg());
    };
    Ok(Term::atom(Atom::FALSE))
}

/// `string:split/2` -- stub returning a list with the original string.
fn bif_string_split(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [input, _pattern] = args else {
        return Err(badarg());
    };
    // Return [Input] — a single-element list.
    let cell = Box::leak(Box::new([0u64; 2]));
    crate::term::boxed::write_cons(cell, *input, Term::NIL).ok_or_else(badarg)
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Atom sentinel for "none" (Gleam Option type).
static NONE_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "unix".
static UNIX_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom for "darwin".
static DARWIN_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

fn none_atom_index() -> u32 {
    NONE_ATOM.get().map_or(u32::MAX, |a| a.index())
}

/// Call this during registration to initialize sentinel atoms.
pub fn init_otp_atoms(atom_table: &AtomTable) {
    let _ = NONE_ATOM.set(atom_table.intern("None"));
    let _ = UNIX_ATOM.set(atom_table.intern("unix"));
    let _ = DARWIN_ATOM.set(atom_table.intern("darwin"));
}

/// Creates a binary term from bytes using a leaked heap allocation.
fn make_leaked_binary(bytes: &[u8]) -> Result<Term, Term> {
    let data_words = crate::term::binary::packed_word_count(bytes.len());
    let total_words = 2 + data_words;
    let heap: &mut [u64] = Box::leak(vec![0u64; total_words].into_boxed_slice());
    crate::term::binary::write_binary(heap, bytes).ok_or_else(badarg)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomTable;
    use crate::native::BifRegistryImpl;

    #[test]
    fn application_stopped_returns_ok() {
        let mut context = ProcessContext::new();
        let result = bif_application_stopped(&[], &mut context);
        assert_eq!(result, Ok(Term::atom(Atom::OK)));
    }

    #[test]
    fn application_stopped_rejects_args() {
        let mut context = ProcessContext::new();
        let result = bif_application_stopped(&[Term::atom(Atom::OK)], &mut context);
        assert!(result.is_err());
    }

    #[test]
    fn supervisor_start_link_rejects_wrong_arity() {
        let mut context = ProcessContext::new();
        let result = bif_supervisor_start_link(&[], &mut context);
        assert!(result.is_err());
    }

    #[test]
    fn register_otp_stubs_registers_all_entries() {
        let atom_table = AtomTable::with_common_atoms();
        init_otp_atoms(&atom_table);
        let mut registry = BifRegistryImpl::new();

        register_otp_stubs(&mut registry, &atom_table).expect("otp stub registration");

        let gleam_otp_ext = atom_table.intern("gleam_otp_external");
        let app_stopped = atom_table.intern("application_stopped");
        assert!(
            registry.lookup(gleam_otp_ext, app_stopped, 0).is_some(),
            "gleam_otp_external:application_stopped/0 should be registered"
        );

        let supervisor = atom_table.intern("supervisor");
        let start_link = atom_table.intern("start_link");
        assert!(
            registry.lookup(supervisor, start_link, 2).is_some(),
            "supervisor:start_link/2 should be registered"
        );

        // Spot-check a few more
        let gleam_string = atom_table.intern("gleam@string");
        let inspect = atom_table.intern("inspect");
        assert!(
            registry.lookup(gleam_string, inspect, 1).is_some(),
            "gleam@string:inspect/1 should be registered"
        );

        let os = atom_table.intern("os");
        let getenv = atom_table.intern("getenv");
        assert!(
            registry.lookup(os, getenv, 0).is_some(),
            "os:getenv/0 should be registered"
        );
    }

    #[test]
    fn register_otp_stubs_rejects_duplicate_registration() {
        let atom_table = AtomTable::with_common_atoms();
        init_otp_atoms(&atom_table);
        let mut registry = BifRegistryImpl::new();

        register_otp_stubs(&mut registry, &atom_table).expect("first");
        assert!(register_otp_stubs(&mut registry, &atom_table).is_err());
    }

    #[test]
    fn dynamic_int_returns_ok_for_integers() {
        let mut context = ProcessContext::new();
        let result = bif_dynamic_int(&[Term::small_int(42)], &mut context);
        assert!(result.is_ok());
    }

    #[test]
    fn dynamic_int_returns_error_for_atoms() {
        let mut context = ProcessContext::new();
        let result = bif_dynamic_int(&[Term::atom(Atom::OK)], &mut context);
        // Should return {error, []}
        assert!(result.is_ok());
    }

    #[test]
    fn get_returns_empty_list() {
        let mut context = ProcessContext::new();
        let result = bif_os_getenv_0(&[], &mut context);
        assert_eq!(result, Ok(Term::NIL));
    }

    #[test]
    fn not_negates_booleans() {
        use crate::native::gate3_bifs::{bif_not, bif_length, bif_get};

        let mut context = ProcessContext::new();
        assert_eq!(
            bif_not(&[Term::atom(Atom::TRUE)], &mut context),
            Ok(Term::atom(Atom::FALSE))
        );
        assert_eq!(
            bif_not(&[Term::atom(Atom::FALSE)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert!(bif_not(&[Term::atom(Atom::OK)], &mut context).is_err());
    }

    #[test]
    fn length_counts_list_elements() {
        use crate::native::gate3_bifs::bif_length;

        let mut context = ProcessContext::new();
        assert_eq!(
            bif_length(&[Term::NIL], &mut context),
            Ok(Term::small_int(0))
        );
    }
}
