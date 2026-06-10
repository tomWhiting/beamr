//! Non-higher-order stdlib BIFs for maps, lists, and timer modules.
//!
//! These functions do NOT take closure arguments and can be implemented as
//! simple native Rust BIFs without interpreter re-entry.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::boxed::{Cons, Map, Tuple};
use crate::term::compare;

/// maps:from_list/1 — builds a map from a list of `{Key, Value}` 2-tuples.
///
/// Duplicate keys are resolved by last-occurrence-wins (the last tuple in the
/// list with a given key determines the value), matching OTP semantics.
pub fn bif_maps_from_list(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };

    let pairs = list_of_2tuples(*input)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *input);
    }
    let map_words = 2 + pairs.len() * 2;
    context.ensure_heap_space(map_words)?;
    let input = context.process_mut().ok_or_else(badarg)?.x_reg(0);
    let pairs = list_of_2tuples(input)?;

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
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    entries.sort_by(|(a, _), (b, _)| compare::cmp(*a, *b, atom_table));

    let keys: Vec<Term> = entries.iter().map(|(k, _)| *k).collect();
    let values: Vec<Term> = entries.iter().map(|(_, v)| *v).collect();

    context.alloc_map_prereserved(&keys, &values)
}

/// maps:merge/2 — merges two maps (second overrides first on collision).
pub fn bif_maps_merge(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [map1_term, map2_term] = args else {
        return Err(badarg());
    };

    let map1 = Map::new(*map1_term).ok_or_else(badarg)?;
    let map2 = Map::new(*map2_term).ok_or_else(badarg)?;
    let entry_count = map1.len() + map2.len();
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *map1_term);
        process.set_x_reg(1, *map2_term);
    }
    context.ensure_heap_space(2 + entry_count * 2)?;
    let (map1_term, map2_term) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let map1 = Map::new(map1_term).ok_or_else(badarg)?;
    let map2 = Map::new(map2_term).ok_or_else(badarg)?;

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
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    entries.sort_by(|(a, _), (b, _)| compare::cmp(*a, *b, atom_table));

    let keys: Vec<Term> = entries.iter().map(|(k, _)| *k).collect();
    let values: Vec<Term> = entries.iter().map(|(_, v)| *v).collect();

    context.alloc_map_prereserved(&keys, &values)
}

/// maps:remove/2 — removes a key from a map, returning a new map.
///
/// If the key is not present, returns the same map structure (as a new
/// allocation for simplicity).
pub fn bif_maps_remove(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key_term, map_term] = args else {
        return Err(badarg());
    };

    let map = Map::new(*map_term).ok_or_else(badarg)?;
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *key_term);
        process.set_x_reg(1, *map_term);
    }
    context.ensure_heap_space(2 + map.len() * 2)?;
    let (key_term, map_term) = {
        let process = context.process_mut().ok_or_else(badarg)?;
        (process.x_reg(0), process.x_reg(1))
    };
    let map = Map::new(map_term).ok_or_else(badarg)?;

    // Collect entries excluding the target key.
    let mut keys = Vec::with_capacity(map.len());
    let mut values = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let (Some(k), Some(v)) = (map.key(i), map.value(i))
            && k != key_term
        {
            keys.push(k);
            values.push(v);
        }
    }

    context.alloc_map_prereserved(&keys, &values)
}

/// lists:reverse/1 — reverses a proper list.
///
/// Uses a GC-safe two-pass approach: count first (no allocation), save the
/// input to x-register 0 so GC can trace it, reserve heap space (GC may run
/// and update the x-register), then re-read and build the reversed result.
pub fn bif_lists_reverse(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [input] = args else {
        return Err(badarg());
    };
    let count = list_length(*input)?;
    if count == 0 {
        return Ok(Term::NIL);
    }
    {
        let process = context.process_mut().ok_or_else(badarg)?;
        process.set_x_reg(0, *input);
    }
    context.ensure_heap_space(count * 2)?;
    let input = context.process_mut().ok_or_else(badarg)?.x_reg(0);
    build_reversed_list(context, input, count)
}

/// maps:map/2 — trampoline-backed higher-order map transformation.
pub fn bif_maps_map(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    super::maps_bifs::bif_maps_map(args, context)
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

/// Counts elements in a proper list without allocating.
pub(super) fn list_length(term: Term) -> Result<usize, Term> {
    let mut count = 0;
    let mut current = term;
    loop {
        if current.is_nil() {
            return Ok(count);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        count += 1;
        current = cons.tail();
    }
}

/// Builds a reversed list from a live heap list using pre-reserved space.
/// Caller must have already called `ensure_heap_space(count * 2)`.
fn build_reversed_list(
    context: &mut ProcessContext,
    list: Term,
    count: usize,
) -> Result<Term, Term> {
    use crate::term::boxed::write_cons;
    let mut result = Term::NIL;
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

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
