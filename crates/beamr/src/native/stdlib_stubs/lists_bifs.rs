//! lists module stdlib BIFs, including continuation-backed higher-order BIFs.

use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Closure, Cons, Tuple};
use crate::term::compare;

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

pub fn bif_lists_nth(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [index, list] = args else {
        return Err(badarg());
    };
    let index = positive_position(*index)?;
    let elements = list_to_vec(*list)?;
    elements.get(index - 1).copied().ok_or_else(badarg)
}

pub fn bif_lists_member(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [element, list] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    Ok(boolean_atom(elements.iter().any(|candidate| {
        compare::numeric_eq(*element, *candidate)
    })))
}

pub fn bif_lists_keyfind(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, position, list] = args else {
        return Err(badarg());
    };
    let index = positive_position(*position)? - 1;
    for element in list_to_vec(*list)? {
        let tuple = tuple_with_position(element, index)?;
        let field = tuple.get(index).ok_or_else(badarg)?;
        if compare::numeric_eq(*key, field) {
            return Ok(element);
        }
    }
    Ok(Term::atom(Atom::FALSE))
}

pub fn bif_lists_last(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [list] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    elements.last().copied().ok_or_else(badarg)
}

pub fn bif_lists_sort(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list] = args else {
        return Err(badarg());
    };
    let mut elements = list_to_vec(*list)?;
    let atom_table = ordering_atom_table(context);
    elements.sort_by(|left, right| compare::cmp(*left, *right, &atom_table));
    list_from_vec(&elements, context)
}

pub fn bif_lists_flatten(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list] = args else {
        return Err(badarg());
    };
    let mut flattened = Vec::new();
    flatten_term(*list, &mut flattened)?;
    list_from_vec(&flattened, context)
}

pub fn bif_lists_zip(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(badarg());
    };
    let left_elements = list_to_vec(*left)?;
    let right_elements = list_to_vec(*right)?;
    if left_elements.len() != right_elements.len() {
        return Err(badarg());
    }
    let mut pairs = Vec::with_capacity(left_elements.len());
    for (left_element, right_element) in left_elements.iter().zip(right_elements.iter()) {
        pairs.push(context.alloc_tuple(&[*left_element, *right_element])?);
    }
    list_from_vec(&pairs, context)
}

pub fn bif_lists_unzip(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    let mut left = Vec::with_capacity(elements.len());
    let mut right = Vec::with_capacity(elements.len());
    for element in elements {
        let tuple = Tuple::new(element).ok_or_else(badarg)?;
        if tuple.arity() != 2 {
            return Err(badarg());
        }
        left.push(tuple.get(0).ok_or_else(badarg)?);
        right.push(tuple.get(1).ok_or_else(badarg)?);
    }
    let left_list = list_from_vec(&left, context)?;
    let right_list = list_from_vec(&right, context)?;
    context.alloc_tuple(&[left_list, right_list])
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

pub fn bif_lists_keystore(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, position, list, new_tuple] = args else {
        return Err(badarg());
    };
    let index = positive_position(*position)? - 1;
    let new_tuple_access = tuple_with_position(*new_tuple, index)?;
    let _ = new_tuple_access.get(index).ok_or_else(badarg)?;
    let mut replaced = false;
    let mut results = Vec::new();
    for element in list_to_vec(*list)? {
        let tuple = tuple_with_position(element, index)?;
        let field = tuple.get(index).ok_or_else(badarg)?;
        if !replaced && compare::numeric_eq(*key, field) {
            results.push(*new_tuple);
            replaced = true;
        } else {
            results.push(element);
        }
    }
    if !replaced {
        results.push(*new_tuple);
    }
    list_from_vec(&results, context)
}

pub fn bif_lists_keysort(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [position, list] = args else {
        return Err(badarg());
    };
    let index = positive_position(*position)? - 1;
    let mut elements = list_to_vec(*list)?;
    for element in &elements {
        let tuple = tuple_with_position(*element, index)?;
        let _ = tuple.get(index).ok_or_else(badarg)?;
    }
    let atom_table = ordering_atom_table(context);
    elements.sort_by(|left, right| {
        let left_key = tuple_key(*left, index);
        let right_key = tuple_key(*right, index);
        compare::cmp(left_key, right_key, &atom_table)
    });
    list_from_vec(&elements, context)
}

pub fn bif_lists_keydelete(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, position, list] = args else {
        return Err(badarg());
    };
    let index = positive_position(*position)? - 1;
    let mut results = Vec::new();
    for element in list_to_vec(*list)? {
        let tuple = tuple_with_position(element, index)?;
        let field = tuple.get(index).ok_or_else(badarg)?;
        if !compare::numeric_eq(*key, field) {
            results.push(element);
        }
    }
    list_from_vec(&results, context)
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

fn positive_position(term: Term) -> Result<usize, Term> {
    let value = term.as_small_int().ok_or_else(badarg)?;
    if value <= 0 {
        return Err(badarg());
    }
    usize::try_from(value).map_err(|_| badarg())
}

fn tuple_with_position(term: Term, index: usize) -> Result<Tuple, Term> {
    let tuple = Tuple::new(term).ok_or_else(badarg)?;
    if index < tuple.arity() {
        Ok(tuple)
    } else {
        Err(badarg())
    }
}

fn tuple_key(term: Term, index: usize) -> Term {
    Tuple::new(term)
        .and_then(|tuple| tuple.get(index))
        .unwrap_or(Term::NIL)
}

fn flatten_term(term: Term, flattened: &mut Vec<Term>) -> Result<(), Term> {
    if term.is_nil() {
        return Ok(());
    }
    if Cons::new(term).is_some() {
        for element in list_to_vec(term)? {
            flatten_term(element, flattened)?;
        }
    } else {
        flattened.push(term);
    }
    Ok(())
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

fn ordering_atom_table(context: &ProcessContext) -> Arc<AtomTable> {
    context
        .atom_table_arc()
        .unwrap_or_else(|| Arc::new(AtomTable::with_common_atoms()))
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

fn boolean_atom(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
