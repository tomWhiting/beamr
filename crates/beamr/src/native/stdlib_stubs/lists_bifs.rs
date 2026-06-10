//! Non-higher-order BIFs for the `lists` module.

use std::sync::Arc;

use super::collection_bifs::list_length;
use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple, write_cons};
use crate::term::compare;

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
    let count = list_length(*left)?;
    if count == 0 {
        return Ok(*right);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *left);
        process.set_x_reg(1, *right);
    }
    context.ensure_heap_space(count * 2)?;
    let (left, mut tail) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let elements = list_to_vec(left)?;
    for element in elements.iter().rev() {
        tail = context.alloc_cons_prereserved(*element, tail)?;
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
    if !list.is_nil() && Cons::new(*list).is_none() {
        return Err(badarg());
    }
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
    if left_elements.is_empty() {
        return Ok(Term::NIL);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *left);
        process.set_x_reg(1, *right);
    }
    context.ensure_heap_space(left_elements.len() * 5)?;
    let (left, right) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let left_elements = list_to_vec(left)?;
    let right_elements = list_to_vec(right)?;
    let mut tail = Term::NIL;
    for (left_element, right_element) in left_elements.iter().zip(right_elements.iter()).rev() {
        let pair = context.alloc_tuple_prereserved(&[*left_element, *right_element])?;
        tail = context.alloc_cons_prereserved(pair, tail)?;
    }
    Ok(tail)
}

pub fn bif_lists_unzip(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list] = args else {
        return Err(badarg());
    };
    let elements = list_to_vec(*list)?;
    for element in &elements {
        let tuple = Tuple::new(*element).ok_or_else(badarg)?;
        if tuple.arity() != 2 {
            return Err(badarg());
        }
        let _ = tuple.get(0).ok_or_else(badarg)?;
        let _ = tuple.get(1).ok_or_else(badarg)?;
    }
    if elements.is_empty() {
        return context.alloc_tuple(&[Term::NIL, Term::NIL]);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *list);
    }
    context.ensure_heap_space(elements.len() * 4 + 3)?;
    let list = context.process_mut().ok_or_else(badarg)?.x_reg(0);
    let elements = list_to_vec(list)?;
    let mut left_tail = Term::NIL;
    let mut right_tail = Term::NIL;
    for element in elements.iter().rev() {
        let tuple = Tuple::new(*element).ok_or_else(badarg)?;
        left_tail = context.alloc_cons_prereserved(tuple.get(0).ok_or_else(badarg)?, left_tail)?;
        right_tail =
            context.alloc_cons_prereserved(tuple.get(1).ok_or_else(badarg)?, right_tail)?;
    }
    context.alloc_tuple_prereserved(&[left_tail, right_tail])
}

pub fn bif_lists_reverse_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [list, tail] = args else {
        return Err(badarg());
    };
    let count = list_length(*list)?;
    if count == 0 {
        return Ok(*tail);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *list);
        process.set_x_reg(1, *tail);
    }
    context.ensure_heap_space(count * 2)?;
    let (list, mut result) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let mut current = list;
    for _ in 0..count {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let head = cons.head();
        current = cons.tail();
        let process = context.process_mut().ok_or_else(badarg)?;
        let heap = process.heap_mut().alloc_slice(2).map_err(|_| badarg())?;
        result = write_cons(heap, head, result).ok_or_else(badarg)?;
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

fn ordering_atom_table(context: &ProcessContext) -> Arc<AtomTable> {
    context
        .atom_table_arc()
        .unwrap_or_else(|| Arc::new(AtomTable::with_common_atoms()))
}

pub(super) fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
    Ok(elements)
}

pub(super) fn list_from_vec(elements: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        for (index, element) in elements.iter().enumerate() {
            let register = u16::try_from(index).map_err(|_| badarg())?;
            process.set_x_reg(register, *element);
        }
    }
    context.ensure_heap_space(elements.len() * 2)?;
    let mut tail = Term::NIL;
    for index in (0..elements.len()).rev() {
        let register = u16::try_from(index).map_err(|_| badarg())?;
        let element = context.process_mut().ok_or_else(badarg)?.x_reg(register);
        tail = context.alloc_cons_prereserved(element, tail)?;
    }
    Ok(tail)
}

fn boolean_atom(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

pub(super) fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
