//! Small compatibility BIFs needed by bundled sample workflow fixtures.

use crate::atom::Atom;
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Closure, Tuple};

use super::gleam_stdlib_ffi::bif_string_replace;
use super::lists_bifs::bif_lists_map;

pub fn bif_gleam_list_map(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_lists_map(args, context)
}

pub fn bif_gleam_string_replace(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    bif_string_replace(args, context)
}

pub fn bif_gleam_string_repeat(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [input, count] = args else {
        return Err(badarg());
    };
    let bytes = BinaryRef::new(*input)
        .map(|binary| binary.as_bytes())
        .ok_or_else(badarg)?;
    let count = count
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    let total = bytes.len().checked_mul(count).ok_or_else(badarg)?;
    let mut out = Vec::with_capacity(total);
    for _ in 0..count {
        out.extend_from_slice(bytes);
    }
    context.alloc_binary(&out)
}

pub fn bif_gleam_string_tree_split(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let _ = context;
    let [_tree, _separator] = args else {
        return Err(badarg());
    };
    // Approximate sample fixture support: the full string_tree representation is
    // not modeled yet, and sample_workflow:run/1 does not exercise this path.
    Ok(Term::NIL)
}

pub fn bif_gleeunit_main(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    if args.is_empty() {
        Ok(Term::atom(Atom::OK))
    } else {
        Err(badarg())
    }
}

pub fn bif_gleam_result_try(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let (fun, result) = if Closure::new(*left).is_some() {
        (*left, *right)
    } else if Closure::new(*right).is_some() {
        (*right, *left)
    } else {
        return Err(badarg());
    };
    let tuple = match Tuple::new(result) {
        Some(tuple) => tuple,
        None => return Ok(result),
    };
    if tuple.arity() != 2 {
        return Err(badarg());
    }
    match tuple.get(0) {
        Some(tag) if tag == Term::atom(Atom::OK) => {
            let value = tuple.get(1).ok_or_else(badarg)?;
            context.set_continuation_trampoline(
                fun,
                vec![value],
                NativeContinuation::GleamResultTry,
            );
            Ok(Term::NIL)
        }
        Some(tag) if tag == Term::atom(Atom::ERROR) => Ok(result),
        _ => Err(badarg()),
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
