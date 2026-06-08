//! Continuation-backed higher-order BIFs for the `lists` module.

use crate::atom::Atom;
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Closure, Tuple};

use super::lists_bifs::{badarg, list_from_vec, list_to_vec};
use super::maps_bifs::ContinuationStep;

#[derive(Clone, Debug)]
pub enum ListsHofState {
    Map {
        fun: Term,
        remaining: Vec<Term>,
        results: Vec<Term>,
    },
    Filter {
        fun: Term,
        current: Term,
        remaining: Vec<Term>,
        results: Vec<Term>,
    },
    FilterMap {
        fun: Term,
        current: Term,
        remaining: Vec<Term>,
        results: Vec<Term>,
    },
    Foreach {
        fun: Term,
        remaining: Vec<Term>,
    },
}

pub fn bif_lists_filter(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, list] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let elements = list_to_vec(*list)?;
    let Some((first, rest)) = elements.split_first() else {
        return Ok(Term::NIL);
    };
    context.set_continuation_trampoline(
        *fun,
        vec![*first],
        NativeContinuation::Lists(ListsHofState::Filter {
            fun: *fun,
            current: *first,
            remaining: rest.to_vec(),
            results: Vec::new(),
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_lists_filtermap(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, list] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let elements = list_to_vec(*list)?;
    let Some((first, rest)) = elements.split_first() else {
        return Ok(Term::NIL);
    };
    context.set_continuation_trampoline(
        *fun,
        vec![*first],
        NativeContinuation::Lists(ListsHofState::FilterMap {
            fun: *fun,
            current: *first,
            remaining: rest.to_vec(),
            results: Vec::new(),
        }),
    );
    Ok(Term::NIL)
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
        NativeContinuation::Lists(ListsHofState::Map {
            fun: *fun,
            remaining: elements[1..].to_vec(),
            results: Vec::new(),
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_lists_foreach(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, list] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let elements = list_to_vec(*list)?;
    let Some((first, rest)) = elements.split_first() else {
        return Ok(Term::atom(Atom::OK));
    };
    context.set_continuation_trampoline(
        *fun,
        vec![*first],
        NativeContinuation::Lists(ListsHofState::Foreach {
            fun: *fun,
            remaining: rest.to_vec(),
        }),
    );
    Ok(Term::NIL)
}

pub fn resume_lists_continuation(
    state: ListsHofState,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<ContinuationStep, Term> {
    match state {
        ListsHofState::Map {
            fun,
            remaining,
            mut results,
        } => {
            results.push(closure_result);
            continue_or_finish_list(
                fun,
                remaining,
                results,
                context,
                |fun, _current, rest, results| ListsHofState::Map {
                    fun,
                    remaining: rest,
                    results,
                },
            )
        }
        ListsHofState::Filter {
            fun,
            current,
            remaining,
            mut results,
        } => {
            if is_true(closure_result)? {
                results.push(current);
            }
            continue_or_finish_list(
                fun,
                remaining,
                results,
                context,
                |fun, current, rest, results| ListsHofState::Filter {
                    fun,
                    current,
                    remaining: rest,
                    results,
                },
            )
        }
        ListsHofState::FilterMap {
            fun,
            current,
            remaining,
            mut results,
        } => {
            if let Some(mapped) = filtermap_value(closure_result)? {
                results.push(mapped.unwrap_or(current));
            }
            continue_or_finish_list(
                fun,
                remaining,
                results,
                context,
                |fun, current, rest, results| ListsHofState::FilterMap {
                    fun,
                    current,
                    remaining: rest,
                    results,
                },
            )
        }
        ListsHofState::Foreach { fun, remaining } => {
            if let Some((first, rest)) = remaining.split_first() {
                Ok(ContinuationStep::Call {
                    fun,
                    args: vec![*first],
                    continuation: NativeContinuation::Lists(ListsHofState::Foreach {
                        fun,
                        remaining: rest.to_vec(),
                    }),
                })
            } else {
                Ok(ContinuationStep::Done(Term::atom(Atom::OK)))
            }
        }
    }
}

fn continue_or_finish_list(
    fun: Term,
    remaining: Vec<Term>,
    results: Vec<Term>,
    context: &mut ProcessContext,
    next_state: impl FnOnce(Term, Term, Vec<Term>, Vec<Term>) -> ListsHofState,
) -> Result<ContinuationStep, Term> {
    if let Some((first, rest)) = remaining.split_first() {
        Ok(ContinuationStep::Call {
            fun,
            args: vec![*first],
            continuation: NativeContinuation::Lists(next_state(
                fun,
                *first,
                rest.to_vec(),
                results,
            )),
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

fn filtermap_value(term: Term) -> Result<Option<Option<Term>>, Term> {
    if term == Term::atom(Atom::FALSE) {
        return Ok(None);
    }
    if term == Term::atom(Atom::TRUE) {
        return Ok(Some(None));
    }
    let tuple = Tuple::new(term).ok_or_else(badarg)?;
    if tuple.arity() != 2 || tuple.get(0) != Some(Term::atom(Atom::TRUE)) {
        return Err(badarg());
    }
    Ok(Some(tuple.get(1)))
}

fn is_true(term: Term) -> Result<bool, Term> {
    if term == Term::atom(Atom::TRUE) {
        Ok(true)
    } else if term == Term::atom(Atom::FALSE) {
        Ok(false)
    } else {
        Err(badarg())
    }
}
