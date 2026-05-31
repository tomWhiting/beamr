//! maps module stdlib BIFs, including continuation-backed higher-order calls.

use crate::atom::Atom;
use crate::native::{NativeContinuation, ProcessContext};
use crate::term::Term;
use crate::term::boxed::{Closure, Cons, Map, write_map};

#[derive(Clone, Debug)]
pub enum MapsHofState {
    Fold {
        fun: Term,
        entries: Vec<(Term, Term)>,
        index: usize,
    },
    Filter {
        fun: Term,
        entries: Vec<(Term, Term)>,
        index: usize,
        kept: Vec<(Term, Term)>,
    },
    MergeWith {
        fun: Term,
        pending: Vec<(Term, Term, Term)>,
        entries: Vec<(Term, Term)>,
        index: usize,
    },
    UpdateWith {
        remaining: Vec<(Term, Term)>,
        updated: Vec<(Term, Term)>,
    },
}

pub fn bif_maps_put(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [key, value, map_term] = args else {
        return Err(badarg());
    };
    let mut entries = map_entries(*map_term)?;
    set_entry(&mut entries, *key, *value);
    make_sorted_map(&entries)
}

pub fn bif_maps_find(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    match map.get(*key) {
        Some(value) => context.alloc_tuple(&[Term::atom(Atom::OK), value]),
        None => Ok(Term::atom(Atom::ERROR)),
    }
}

pub fn bif_maps_keys(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    let keys: Result<Vec<_>, _> = (0..map.len())
        .map(|index| map.key(index).ok_or_else(badarg))
        .collect();
    list_from_vec(&keys?, context)
}

pub fn bif_maps_values(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [map_term] = args else {
        return Err(badarg());
    };
    let map = Map::new(*map_term).ok_or_else(badarg)?;
    let values: Result<Vec<_>, _> = (0..map.len())
        .map(|index| map.value(index).ok_or_else(badarg))
        .collect();
    list_from_vec(&values?, context)
}

pub fn bif_maps_to_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [map_term] = args else {
        return Err(badarg());
    };
    let pairs = map_entries(*map_term)?;
    let mut tuples = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        tuples.push(context.alloc_tuple(&[key, value])?);
    }
    list_from_vec(&tuples, context)
}

pub fn bif_maps_fold(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, acc, map_term] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 3)?;
    let entries = map_entries(*map_term)?;
    if entries.is_empty() {
        return Ok(*acc);
    }
    let (key, value) = entries[0];
    context.set_continuation_trampoline(
        *fun,
        vec![key, value, *acc],
        NativeContinuation::Maps(MapsHofState::Fold {
            fun: *fun,
            entries,
            index: 1,
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_maps_filter(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, map_term] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 2)?;
    let entries = map_entries(*map_term)?;
    if entries.is_empty() {
        return make_sorted_map(&[]);
    }
    let (key, value) = entries[0];
    context.set_continuation_trampoline(
        *fun,
        vec![key, value],
        NativeContinuation::Maps(MapsHofState::Filter {
            fun: *fun,
            entries,
            index: 1,
            kept: Vec::new(),
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_maps_merge_with(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [fun, map1_term, map2_term] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 3)?;
    let map1_entries = map_entries(*map1_term)?;
    let map2_entries = map_entries(*map2_term)?;
    let mut entries = map1_entries.clone();
    let mut collisions = Vec::new();
    for (key, value2) in map2_entries {
        if let Some((_, value1)) = entries
            .iter()
            .find(|(existing_key, _)| *existing_key == key)
        {
            collisions.push((key, *value1, value2));
        } else {
            entries.push((key, value2));
        }
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    if collisions.is_empty() {
        return make_sorted_map(&entries);
    }
    let (key, value1, value2) = collisions[0];
    context.set_continuation_trampoline(
        *fun,
        vec![key, value1, value2],
        NativeContinuation::Maps(MapsHofState::MergeWith {
            fun: *fun,
            pending: collisions,
            entries,
            index: 1,
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_maps_update_with(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, fun, init, map_term] = args else {
        return Err(badarg());
    };
    ensure_fun_arity(*fun, 1)?;
    let entries = map_entries(*map_term)?;
    let Some(position) = entries.iter().position(|(entry_key, _)| entry_key == key) else {
        let mut with_init = entries;
        with_init.push((*key, *init));
        return make_sorted_map(&with_init);
    };
    let (existing_key, existing_value) = entries[position];
    let remaining = entries[position + 1..].to_vec();
    let updated = entries[..position].to_vec();
    context.set_continuation_trampoline(
        *fun,
        vec![existing_value],
        NativeContinuation::Maps(MapsHofState::UpdateWith {
            remaining,
            updated: [updated, vec![(existing_key, Term::NIL)]].concat(),
        }),
    );
    Ok(Term::NIL)
}

pub fn bif_maps_with(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [keys_term, map_term] = args else {
        return Err(badarg());
    };
    let keys = list_to_vec(*keys_term)?;
    let entries = map_entries(*map_term)?;
    let kept: Vec<_> = entries
        .into_iter()
        .filter(|(key, _)| keys.iter().any(|wanted| wanted == key))
        .collect();
    make_sorted_map(&kept)
}

pub fn bif_maps_without(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [keys_term, map_term] = args else {
        return Err(badarg());
    };
    let keys = list_to_vec(*keys_term)?;
    let entries = map_entries(*map_term)?;
    let kept: Vec<_> = entries
        .into_iter()
        .filter(|(key, _)| !keys.iter().any(|removed| removed == key))
        .collect();
    make_sorted_map(&kept)
}

pub fn resume_maps_continuation(
    state: MapsHofState,
    closure_result: Term,
) -> Result<ContinuationStep, Term> {
    match state {
        MapsHofState::Fold {
            fun,
            entries,
            index,
        } => {
            if let Some((key, value)) = entries.get(index).copied() {
                Ok(ContinuationStep::Call {
                    fun,
                    args: vec![key, value, closure_result],
                    continuation: NativeContinuation::Maps(MapsHofState::Fold {
                        fun,
                        entries,
                        index: index + 1,
                    }),
                })
            } else {
                Ok(ContinuationStep::Done(closure_result))
            }
        }
        MapsHofState::Filter {
            fun,
            entries,
            index,
            mut kept,
        } => {
            if is_true(closure_result)? {
                let previous = index.checked_sub(1).ok_or_else(badarg)?;
                kept.push(entries[previous]);
            }
            if let Some((key, value)) = entries.get(index).copied() {
                Ok(ContinuationStep::Call {
                    fun,
                    args: vec![key, value],
                    continuation: NativeContinuation::Maps(MapsHofState::Filter {
                        fun,
                        entries,
                        index: index + 1,
                        kept,
                    }),
                })
            } else {
                Ok(ContinuationStep::Done(make_sorted_map(&kept)?))
            }
        }
        MapsHofState::MergeWith {
            fun,
            pending,
            mut entries,
            index,
        } => {
            let current_key = pending
                .get(index - 1)
                .map(|(key, _, _)| *key)
                .ok_or_else(badarg)?;
            set_entry(&mut entries, current_key, closure_result);
            if let Some((key, value1, value2)) = pending.get(index).copied() {
                Ok(ContinuationStep::Call {
                    fun,
                    args: vec![key, value1, value2],
                    continuation: NativeContinuation::Maps(MapsHofState::MergeWith {
                        fun,
                        pending,
                        entries,
                        index: index + 1,
                    }),
                })
            } else {
                Ok(ContinuationStep::Done(make_sorted_map(&entries)?))
            }
        }
        MapsHofState::UpdateWith {
            mut remaining,
            mut updated,
        } => {
            if let Some(last) = updated.last_mut() {
                last.1 = closure_result;
            } else {
                return Err(badarg());
            }
            updated.append(&mut remaining);
            Ok(ContinuationStep::Done(make_sorted_map(&updated)?))
        }
    }
}

pub enum ContinuationStep {
    Call {
        fun: Term,
        args: Vec<Term>,
        continuation: NativeContinuation,
    },
    Done(Term),
}

fn ensure_fun_arity(fun: Term, arity: u8) -> Result<(), Term> {
    let closure = Closure::new(fun).ok_or_else(badarg)?;
    if closure.arity() == arity {
        Ok(())
    } else {
        Err(Term::atom(Atom::BADARITY))
    }
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

fn map_entries(term: Term) -> Result<Vec<(Term, Term)>, Term> {
    let map = Map::new(term).ok_or_else(badarg)?;
    (0..map.len())
        .map(|index| {
            Ok((
                map.key(index).ok_or_else(badarg)?,
                map.value(index).ok_or_else(badarg)?,
            ))
        })
        .collect()
}

fn set_entry(entries: &mut Vec<(Term, Term)>, key: Term, value: Term) {
    if let Some(existing) = entries.iter_mut().find(|(entry_key, _)| *entry_key == key) {
        existing.1 = value;
    } else {
        entries.push((key, value));
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    }
}

fn make_sorted_map(entries: &[(Term, Term)]) -> Result<Term, Term> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|(left, _), (right, _)| left.cmp(right));
    let keys: Vec<_> = sorted.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = sorted.iter().map(|(_, value)| *value).collect();
    make_leaked_map(&keys, &values)
}

fn make_leaked_map(keys: &[Term], values: &[Term]) -> Result<Term, Term> {
    let total_words = 2 + keys.len() + values.len();
    let heap: &mut [u64] = Box::leak(vec![0u64; total_words].into_boxed_slice());
    write_map(heap, keys, values).ok_or_else(badarg)
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
