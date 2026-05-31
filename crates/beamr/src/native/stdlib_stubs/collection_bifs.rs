//! Non-higher-order stdlib BIFs for maps, lists, and timer modules.
//!
//! These functions do NOT take closure arguments and can be implemented as
//! simple native Rust BIFs without interpreter re-entry.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::boxed::{Cons, Map, Tuple, write_cons, write_map};

/// maps:from_list/1 — builds a map from a list of `{Key, Value}` 2-tuples.
///
/// Duplicate keys are resolved by last-occurrence-wins (the last tuple in the
/// list with a given key determines the value), matching OTP semantics.
pub fn bif_maps_from_list(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    let pairs = list_of_2tuples(*input)?;

    // Deduplicate: last occurrence wins. Build a sorted, deduplicated list.
    let mut entries: Vec<(Term, Term)> = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        if let Some(existing) = entries.iter_mut().find(|(k, _)| *k == key) {
            existing.1 = value;
        } else {
            entries.push((key, value));
        }
    }

    // Sort by key for flatmap ordering.
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let keys: Vec<Term> = entries.iter().map(|(k, _)| *k).collect();
    let values: Vec<Term> = entries.iter().map(|(_, v)| *v).collect();

    make_leaked_map(&keys, &values)
}

/// maps:merge/2 — merges two maps (second overrides first on collision).
pub fn bif_maps_merge(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [map1_term, map2_term] = args else {
        return Err(badarg());
    };

    let map1 = Map::new(*map1_term).ok_or_else(badarg)?;
    let map2 = Map::new(*map2_term).ok_or_else(badarg)?;

    // Collect all entries from map1.
    let mut entries: Vec<(Term, Term)> = Vec::with_capacity(map1.len() + map2.len());
    for i in 0..map1.len() {
        if let (Some(k), Some(v)) = (map1.key(i), map1.value(i)) {
            entries.push((k, v));
        }
    }

    // Merge entries from map2 (overriding map1 on collision).
    for i in 0..map2.len() {
        if let (Some(k), Some(v)) = (map2.key(i), map2.value(i)) {
            if let Some(existing) = entries.iter_mut().find(|(ek, _)| *ek == k) {
                existing.1 = v;
            } else {
                entries.push((k, v));
            }
        }
    }

    // Sort by key for flatmap ordering.
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let keys: Vec<Term> = entries.iter().map(|(k, _)| *k).collect();
    let values: Vec<Term> = entries.iter().map(|(_, v)| *v).collect();

    make_leaked_map(&keys, &values)
}

/// maps:remove/2 — removes a key from a map, returning a new map.
///
/// If the key is not present, returns the same map structure (as a new
/// allocation for simplicity).
pub fn bif_maps_remove(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [key_term, map_term] = args else {
        return Err(badarg());
    };

    let map = Map::new(*map_term).ok_or_else(badarg)?;

    // Collect entries excluding the target key.
    let mut keys = Vec::with_capacity(map.len());
    let mut values = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let (Some(k), Some(v)) = (map.key(i), map.value(i))
            && k != *key_term
        {
            keys.push(k);
            values.push(v);
        }
    }

    make_leaked_map(&keys, &values)
}

/// lists:reverse/1 — reverses a proper list.
pub fn bif_lists_reverse(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    // Collect all elements from the proper list.
    let elements = list_to_vec(*input)?;

    // Build the reversed list from the end.
    let mut tail = Term::NIL;
    for element in elements {
        let cell = Box::leak(Box::new([0u64; 2]));
        tail = write_cons(cell, element, tail).ok_or_else(badarg)?;
    }

    Ok(tail)
}

/// maps:map/2 — stub for higher-order map transformation.
///
/// This function requires interpreter re-entry to call the closure argument,
/// which the native BIF signature does not support. Returns `badarg` as a
/// documented limitation. The real implementation should be loaded from
/// compiled BEAM bytecode (see `fixtures/stdlib/`) once maps:to_list/1 and
/// maps:from_list/1 are available within Erlang-level code.
pub fn bif_maps_map(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [_fun, _map] = args else {
        return Err(badarg());
    };

    // TODO: Implement via compiled BEAM bytecode once cross-module calls from
    // stdlib .beam modules to native BIFs (maps:to_list, maps:from_list) work.
    Err(badarg())
}

/// timer:sleep/1 — sleeps the current thread for the given milliseconds.
///
/// Returns the atom `ok`. For now this uses `std::thread::sleep` which blocks
/// the thread. This is acceptable for the single-process CLI path; a future
/// scheduler integration should yield the process instead.
pub fn bif_timer_sleep(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [ms_term] = args else {
        return Err(badarg());
    };

    let ms = ms_term
        .as_small_int()
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(badarg)?;

    std::thread::sleep(std::time::Duration::from_millis(ms));

    Ok(Term::atom(Atom::OK))
}

/// Extracts a proper list of 2-tuples into a `Vec<(Term, Term)>`.
fn list_of_2tuples(term: Term) -> Result<Vec<(Term, Term)>, Term> {
    let mut pairs = Vec::new();
    let mut current = term;

    loop {
        if current.is_nil() {
            return Ok(pairs);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let head = cons.head();

        let tuple = Tuple::new(head).ok_or_else(badarg)?;
        if tuple.arity() != 2 {
            return Err(badarg());
        }
        let key = tuple.get(0).ok_or_else(badarg)?;
        let value = tuple.get(1).ok_or_else(badarg)?;
        pairs.push((key, value));

        current = cons.tail();
    }
}

/// Collects a proper list into a `Vec<Term>`.
fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(elements);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
}

/// Creates a map term via leaked heap allocation.
fn make_leaked_map(keys: &[Term], values: &[Term]) -> Result<Term, Term> {
    let total_words = 2 + keys.len() + values.len();
    let heap: &mut [u64] = Box::leak(vec![0u64; total_words].into_boxed_slice());
    write_map(heap, keys, values).ok_or_else(badarg)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
