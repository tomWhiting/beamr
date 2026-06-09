//! Map runtime helpers callable from JIT-generated code.

use crate::gc;
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Map, write_map};
use crate::term::compare;

use super::ir_exceptions::JitReturn;
use super::runtime::process_from_abi;

pub(crate) extern "C" fn jit_map_new(
    process: *mut Process,
    source: u64,
    pairs: *const u64,
    pair_count: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    map_update(process, Term::from_raw(source), pairs, pair_count, false)
}

pub(crate) extern "C" fn jit_map_update(
    process: *mut Process,
    source: u64,
    pairs: *const u64,
    pair_count: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    map_update(process, Term::from_raw(source), pairs, pair_count, true)
}

pub(crate) extern "C" fn jit_map_get(map: u64, key: u64) -> JitReturn {
    let Some(map) = Map::new(Term::from_raw(map)) else {
        return map_get_return(0, 0);
    };
    map.get(Term::from_raw(key)).map_or_else(
        || map_get_return(0, 0),
        |value| map_get_return(1, value.raw()),
    )
}

pub(crate) extern "C" fn jit_map_has_key(map: u64, key: u64) -> u8 {
    Map::new(Term::from_raw(map))
        .and_then(|map| map.get(Term::from_raw(key)))
        .map_or(0, |_| 1)
}

fn map_update(
    process: &mut Process,
    source: Term,
    pairs: *const u64,
    pair_count: u64,
    exact: bool,
) -> u64 {
    let Some(source_map) = Map::new(source) else {
        return 0;
    };
    let Some(updates) = map_pair_terms(pairs, pair_count) else {
        return 0;
    };
    let Some(mut entries) = map_entries(source_map) else {
        return 0;
    };
    if exact
        && updates.iter().any(|(key, _value)| {
            entries
                .iter()
                .all(|(existing_key, _value)| *existing_key != *key)
        })
    {
        return 0;
    }
    for (key, value) in updates {
        if let Some((_existing_key, existing_value)) = entries
            .iter_mut()
            .find(|(existing_key, _value)| *existing_key == key)
        {
            *existing_value = value;
        } else {
            entries.push((key, value));
        }
    }
    entries.sort_by(|(left, _), (right, _)| compare::raw_cmp(*left, *right));
    write_map_entries(process, &entries).map_or(0, Term::raw)
}

fn map_pair_terms(pairs: *const u64, pair_count: u64) -> Option<Vec<(Term, Term)>> {
    let pair_count = usize::try_from(pair_count).ok()?;
    let term_count = pair_count.checked_mul(2)?;
    if term_count > 0 && pairs.is_null() {
        return None;
    }
    let raw_pairs = if term_count == 0 {
        &[]
    } else {
        // SAFETY: Generated code passes a stack slot containing exactly
        // `pair_count * 2` raw term words for the duration of this helper call.
        unsafe { std::slice::from_raw_parts(pairs, term_count) }
    };
    Some(
        raw_pairs
            .chunks_exact(2)
            .map(|pair| (Term::from_raw(pair[0]), Term::from_raw(pair[1])))
            .collect(),
    )
}

const fn map_get_return(status: u8, value: u64) -> JitReturn {
    JitReturn {
        status,
        _padding: [0; 7],
        value,
    }
}

fn map_entries(map: Map) -> Option<Vec<(Term, Term)>> {
    let mut entries = Vec::with_capacity(map.len());
    for index in 0..map.len() {
        entries.push((map.key(index)?, map.value(index)?));
    }
    Some(entries)
}

fn write_map_entries(process: &mut Process, entries: &[(Term, Term)]) -> Option<Term> {
    let words = map_word_count(entries.len())?;
    let keys = entries.iter().map(|(key, _value)| *key).collect::<Vec<_>>();
    let values = entries
        .iter()
        .map(|(_key, value)| *value)
        .collect::<Vec<_>>();
    if gc::ensure_space(process, words, 256).is_err() {
        return None;
    }
    let heap = process.heap_mut().alloc_slice(words).ok()?;
    write_map(heap, &keys, &values)
}

fn map_word_count(entries: usize) -> Option<usize> {
    entries.checked_mul(2)?.checked_add(2)
}
