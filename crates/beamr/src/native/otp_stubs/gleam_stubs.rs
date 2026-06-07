//! Gleam standard library stub BIFs for OTP import resolution.
//!
//! These stubs satisfy imports from gleam_otp .beam files for modules
//! that have no corresponding .beam fixture:
//! - `gleam@dynamic` — dynamic type checking
//! - `gleam@string` — string inspection and concatenation
//! - `gleam@option` — Option type combinators
//! - `gleam@result` — Result type combinators
//! - `gleam@otp@intensity_tracker` — supervisor restart intensity

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;

/// Atom sentinel for "None" (Gleam Option type).
static NONE_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

pub fn init_gleam_atoms(atom_table: &AtomTable) {
    let _ = NONE_ATOM.set(atom_table.intern("None"));
}

fn none_atom_index() -> u32 {
    NONE_ATOM.get().map_or(u32::MAX, |a| a.index())
}

// ── gleam@dynamic ─────────────────────────────────────────────────────────

/// `gleam@dynamic:classify/1` -- returns a binary describing the term type.
pub fn bif_dynamic_classify(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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
    context
        .alloc_binary(description.as_bytes())
        .or_else(|_| context.alloc_tuple(&[Term::atom(Atom::OK), Term::atom(Atom::NIL)]))
}

/// `gleam@dynamic:int/1` -- returns `{ok, Value}` for integers, `{error, []}` otherwise.
pub fn bif_dynamic_int(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    if term.as_small_int().is_some() {
        context.alloc_tuple(&[Term::atom(Atom::OK), *term])
    } else {
        context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::NIL])
    }
}

/// `gleam@dynamic:string/1` -- returns `{ok, Value}` for binaries, `{error, []}` otherwise.
pub fn bif_dynamic_string(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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

/// `gleam@string:inspect/1` -- returns a binary with a debug representation.
pub fn bif_string_inspect(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let repr = format!("{term:?}");
    context.alloc_binary(repr.as_bytes())
}

/// `gleam@string:append/2` -- concatenates two binary strings.
pub fn bif_string_append(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
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
    context.alloc_binary(&combined)
}

// ── gleam@option ──────────────────────────────────────────────────────────

/// `gleam@option:map/2` -- identity stub (cannot call BEAM closures from BIFs).
pub fn bif_option_map(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [option, _fun] = args else {
        return Err(badarg());
    };
    Ok(*option)
}

/// `gleam@option:unwrap/2` -- unwraps `{some, Value}` or returns default for `none`.
pub fn bif_option_unwrap(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [option, default] = args else {
        return Err(badarg());
    };
    if let Some(atom) = option.as_atom()
        && (atom == Atom::NIL || atom.index() == none_atom_index())
    {
        return Ok(*default);
    }
    if let Some(tuple) = crate::term::boxed::Tuple::new(*option)
        && tuple.arity() == 2
    {
        return tuple.get(1).ok_or_else(badarg);
    }
    Ok(*option)
}

// ── gleam@result ──────────────────────────────────────────────────────────

/// `gleam@result:map_error/2` -- identity stub (cannot call BEAM closures).
pub fn bif_result_map_error(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, _fun] = args else {
        return Err(badarg());
    };
    Ok(*result)
}

/// `gleam@result:then/2` -- identity stub (cannot call BEAM closures).
pub fn bif_result_then(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, _fun] = args else {
        return Err(badarg());
    };
    Ok(*result)
}

// ── gleam@otp@intensity_tracker ───────────────────────────────────────────

/// `gleam@otp@intensity_tracker:new/2` -- returns a stub tracker tuple.
pub fn bif_intensity_tracker_new(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [limit, period] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::small_int(0), *limit, *period, Term::NIL])
}

/// `gleam@otp@intensity_tracker:add_event/1` -- always succeeds with `{ok, tracker}`.
pub fn bif_intensity_tracker_add_event(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [tracker] = args else {
        return Err(badarg());
    };
    context.alloc_tuple(&[Term::atom(Atom::OK), *tracker])
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
