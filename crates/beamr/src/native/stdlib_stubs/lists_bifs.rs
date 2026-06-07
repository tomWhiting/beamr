//! lists module stdlib BIFs, including continuation-backed lists:map/2.

use crate::atom::Atom;
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Closure, Cons};

use super::maps_bifs::ContinuationStep;

#[derive(Clone, Debug)]
pub struct ListsMapState {
    pub fun: Term,
    pub remaining: Vec<Term>,
    pub results: Vec<Term>,
}

pub fn bif_lists_append_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [lists] = args else {
        return Err(badarg());
    };
    let parts = list_to_vec(*lists)?;
    let mut flattened = Vec::new();
    for part in parts {
        flattened.extend(list_to_vec(part)?);
    }
    list_from_vec(&flattened, context)
}

pub fn bif_lists_append_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*left)?;
    let mut tail = *right;
    for element in elements.iter().rev() {
        tail = context.alloc_cons(*element, tail)?;
    }
    Ok(tail)
}

pub fn bif_lists_join(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [separator, list] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    if elements.is_empty() {
        return Ok(Term::NIL);
    }
    let mut joined = Vec::with_capacity(elements.len().saturating_mul(2).saturating_sub(1));
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            joined.push(*separator);
        }
        joined.push(*element);
    }
    list_from_vec(&joined, context)
}

pub fn bif_lists_map(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, list] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let elements = list_to_vec(*list)?;
    if elements.is_empty() {
        return Ok(Term::NIL);
    }
    let first = elements[0];
    context.set_continuation_trampoline(
        *fun,
        vec![first],
        NativeContinuation::ListsMap(ListsMapState {
            fun: *fun,
            remaining: elements[1..].to_vec(),
            results: Vec::new(),
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_lists_reverse_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list, tail] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    let mut result = *tail;
    for element in elements {
        result = context.alloc_cons(element, result)?;
    }
    Ok(result)
}

pub fn bif_lists_seq(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [from, to] = args else {
        return Err(badarg());
    };
    let from = from.as_small_int().ok_or_else(badarg)?;
    let to = to.as_small_int().ok_or_else(badarg)?;
    if from > to {
        return Ok(Term::NIL);
    }
    let mut elements = Vec::new();
    let mut value = from;
    while value <= to {
        elements.push(Term::try_small_int(value).ok_or_else(badarg)?);
        value = value.checked_add(1).ok_or_else(badarg)?;
    }
    list_from_vec(&elements, context)
}

pub fn resume_lists_map(
    state: ListsMapState,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<ContinuationStep, Term> {
    let mut results = state.results;
    results.push(closure_result);
    if let Some((first, rest)) = state.remaining.split_first() {
        Ok(ContinuationStep::Call {
            fun: state.fun,
            args: vec![*first],
            continuation: NativeContinuation::ListsMap(ListsMapState {
                fun: state.fun,
                remaining: rest.to_vec(),
                results,
            }),
        })
    } else {
        Ok(ContinuationStep::Done(list_from_vec(&results, context)?))
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

fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
    Ok(elements)
}

fn list_from_vec(elements: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let mut tail = Term::NIL;
    for element in elements.iter().rev() {
        tail = context.alloc_cons(*element, tail)?;
    }
    Ok(tail)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
