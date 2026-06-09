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
use crate::native::stdlib_stubs::maps_bifs::ContinuationStep;
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Closure, Tuple};

/// Atom sentinel for "None" (Gleam Option type).
static NONE_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();
/// Atom tag for "Some" (Gleam Option type).
static SOME_ATOM: std::sync::OnceLock<Atom> = std::sync::OnceLock::new();

#[derive(Clone, Debug)]
pub enum GleamOptionState {
    Map,
}

#[derive(Clone, Debug)]
pub enum GleamResultState {
    MapError,
    Then,
}

pub fn init_gleam_atoms(atom_table: &AtomTable) {
    let _ = NONE_ATOM.set(atom_table.intern("None"));
    let _ = SOME_ATOM.set(atom_table.intern("Some"));
}

fn none_atom_index() -> u32 {
    NONE_ATOM.get().map_or(u32::MAX, |a| a.index())
}

fn some_atom_index() -> u32 {
    SOME_ATOM.get().map_or(u32::MAX, |a| a.index())
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

/// `gleam@option:map/2` -- maps over `{some, Value}` via a continuation trampoline.
pub fn bif_option_map(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [option, fun] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    if is_option_none(*option) {
        return Ok(*option);
    }
    let value = option_some_value(*option, context)?;
    context.set_continuation_trampoline(
        *fun,
        vec![value],
        NativeContinuation::GleamOption(GleamOptionState::Map),
    );
    Ok(Term::NIL)
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

/// `gleam@result:map_error/2` -- maps over `{error, Reason}` via a continuation trampoline.
pub fn bif_result_map_error(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, fun] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let (tag, value) = result_tuple(*result)?;
    if tag == Atom::OK {
        return Ok(*result);
    }
    if tag != Atom::ERROR {
        return Err(badarg());
    }
    context.set_continuation_trampoline(
        *fun,
        vec![value],
        NativeContinuation::GleamResult(GleamResultState::MapError),
    );
    Ok(Term::NIL)
}

/// `gleam@result:then/2` -- chains over `{ok, Value}` via a continuation trampoline.
pub fn bif_result_then(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [result, fun] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let (tag, value) = result_tuple(*result)?;
    if tag == Atom::ERROR {
        return Ok(*result);
    }
    if tag != Atom::OK {
        return Err(badarg());
    }
    context.set_continuation_trampoline(
        *fun,
        vec![value],
        NativeContinuation::GleamResult(GleamResultState::Then),
    );
    Ok(Term::NIL)
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

/// `gleam@otp@intensity_tracker:add_event/1` -- increments count and tags by limit.
pub fn bif_intensity_tracker_add_event(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [tracker] = args else {
        return Err(badarg());
    };
    let tuple = Tuple::new(*tracker).ok_or_else(badarg)?;
    if tuple.arity() != 4 {
        return Err(badarg());
    }
    let count = tuple
        .get(0)
        .and_then(Term::as_small_int)
        .ok_or_else(badarg)?;
    let limit = tuple
        .get(1)
        .and_then(Term::as_small_int)
        .ok_or_else(badarg)?;
    let period = tuple.get(2).ok_or_else(badarg)?;
    let events = tuple.get(3).ok_or_else(badarg)?;
    let new_count = count.checked_add(1).ok_or_else(badarg)?;
    let new_count_term = Term::try_small_int(new_count).ok_or_else(badarg)?;
    let updated = context.alloc_tuple(&[new_count_term, Term::small_int(limit), period, events])?;
    let status = if count < limit { Atom::OK } else { Atom::ERROR };
    context.alloc_tuple(&[Term::atom(status), updated])
}

// ── Helpers ───────────────────────────────────────────────────────────────

pub fn resume_gleam_option_continuation(
    state: GleamOptionState,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<ContinuationStep, Term> {
    match state {
        GleamOptionState::Map => Ok(ContinuationStep::Done(some_tuple(closure_result, context)?)),
    }
}

pub fn resume_gleam_result_continuation(
    state: GleamResultState,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<ContinuationStep, Term> {
    match state {
        GleamResultState::MapError => Ok(ContinuationStep::Done(
            context.alloc_tuple(&[Term::atom(Atom::ERROR), closure_result])?,
        )),
        GleamResultState::Then => Ok(ContinuationStep::Done(closure_result)),
    }
}

fn ensure_fun_arity(fun: Term, arity: u8) -> Result<(), Term> {
    let closure = Closure::new(fun).ok_or_else(badarg)?;
    if closure.arity() == arity {
        Ok(())
    } else {
        Err(Term::atom(Atom::BADARITY))
    }
}

fn is_option_none(option: Term) -> bool {
    option
        .as_atom()
        .is_some_and(|atom| atom == Atom::NIL || atom.index() == none_atom_index())
}

fn option_some_value(option: Term, context: &ProcessContext) -> Result<Term, Term> {
    let tuple = Tuple::new(option).ok_or_else(badarg)?;
    if tuple.arity() != 2 {
        return Err(badarg());
    }
    let tag = tuple.get(0).and_then(Term::as_atom).ok_or_else(badarg)?;
    if is_some_tag(tag, context) {
        tuple.get(1).ok_or_else(badarg)
    } else {
        Err(badarg())
    }
}

fn is_some_tag(tag: Atom, context: &ProcessContext) -> bool {
    tag == Atom::OK
        || tag.index() == some_atom_index()
        || context.atom_table().is_some_and(|atom_table| {
            atom_table
                .lookup("Some")
                .or_else(|| atom_table.lookup("some"))
                .is_some_and(|some| some == tag)
        })
}

fn some_tag(context: &ProcessContext) -> Atom {
    context.atom_table().map_or(Atom::OK, |atom_table| {
        atom_table
            .lookup("Some")
            .or_else(|| atom_table.lookup("some"))
            .unwrap_or(Atom::OK)
    })
}

fn result_tuple(result: Term) -> Result<(Atom, Term), Term> {
    let tuple = Tuple::new(result).ok_or_else(badarg)?;
    if tuple.arity() != 2 {
        return Err(badarg());
    }
    let tag = tuple.get(0).and_then(Term::as_atom).ok_or_else(badarg)?;
    let value = tuple.get(1).ok_or_else(badarg)?;
    Ok((tag, value))
}

fn some_tuple(value: Term, context: &mut ProcessContext) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(some_tag(context)), value])
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
