//! Term ordering and equality — `==` (number coercion) and `=:=` (exact).
//! BEAM order: number < atom < reference < fun < port < pid <
//! tuple < map < nil < list < binary.

use std::cmp::Ordering;

use super::{
    Term,
    binary::Binary,
    boxed::{BigInt, Closure, Cons, Float, Map, Reference, Tuple},
};

/// Compares two terms using Erlang `=:=` exact equality semantics.
#[must_use]
pub fn exact_eq(left: Term, right: Term) -> bool {
    compare_exact(left, right) == Ordering::Equal
}

/// Compares two terms using Erlang `==` semantics.
///
/// Integer/float pairs compare after converting the integer to `f64`; all
/// non-numeric pairs use exact equality.
#[must_use]
pub fn numeric_eq(left: Term, right: Term) -> bool {
    match (number_value(left), number_value(right)) {
        (Some(NumberValue::SmallInt(left)), Some(NumberValue::Float(right))) => {
            left as f64 == right
        }
        (Some(NumberValue::Float(left)), Some(NumberValue::SmallInt(right))) => {
            left == right as f64
        }
        (Some(NumberValue::SmallInt(left)), Some(NumberValue::SmallInt(right))) => left == right,
        (Some(NumberValue::Float(left)), Some(NumberValue::Float(right))) => left == right,
        _ => exact_eq(left, right),
    }
}

/// Compares two terms using the BEAM term order.
#[must_use]
pub fn cmp(left: Term, right: Term) -> Ordering {
    let left_rank = rank(left);
    let right_rank = rank(right);
    match left_rank.cmp(&right_rank) {
        Ordering::Equal => compare_same_rank(left, right, left_rank),
        order => order,
    }
}

pub(crate) fn partial_eq(left: &Term, right: &Term) -> bool {
    exact_eq(*left, *right)
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
#[allow(dead_code)]
enum TermRank {
    Number,
    Atom,
    Reference,
    Fun,
    // No port representation exists yet; keep the BEAM rank slot reserved so
    // future port terms sort between fun and pid without renumbering ranks.
    Port,
    Pid,
    Tuple,
    Map,
    Nil,
    List,
    Binary,
    OtherBoxed,
}

#[derive(Copy, Clone)]
enum NumberValue {
    SmallInt(i64),
    Float(f64),
}

fn rank(term: Term) -> TermRank {
    if term.is_small_int() || Float::new(term).is_some() || BigInt::new(term).is_some() {
        TermRank::Number
    } else if term.is_atom() {
        TermRank::Atom
    } else if Reference::new(term).is_some() {
        TermRank::Reference
    } else if Closure::new(term).is_some() {
        TermRank::Fun
    } else if term.is_pid() {
        TermRank::Pid
    } else if Tuple::new(term).is_some() {
        TermRank::Tuple
    } else if Map::new(term).is_some() {
        TermRank::Map
    } else if term.is_nil() {
        TermRank::Nil
    } else if term.is_list() {
        TermRank::List
    } else if Binary::new(term).is_some() {
        TermRank::Binary
    } else {
        TermRank::OtherBoxed
    }
}

fn number_value(term: Term) -> Option<NumberValue> {
    if let Some(value) = term.as_small_int() {
        Some(NumberValue::SmallInt(value))
    } else {
        Float::new(term).map(|float| NumberValue::Float(float.value()))
    }
}

fn compare_same_rank(left: Term, right: Term, term_rank: TermRank) -> Ordering {
    match term_rank {
        TermRank::Number => compare_numbers(left, right),
        TermRank::Atom => left.raw().cmp(&right.raw()),
        TermRank::Reference => reference_id(left).cmp(&reference_id(right)),
        TermRank::Fun => compare_closures(left, right),
        TermRank::Port => Ordering::Equal,
        TermRank::Pid => left.as_pid().cmp(&right.as_pid()),
        TermRank::Tuple => compare_tuples(left, right),
        TermRank::Map => compare_maps(left, right),
        TermRank::Nil => Ordering::Equal,
        TermRank::List => compare_lists(left, right),
        TermRank::Binary => binary_bytes(left).cmp(binary_bytes(right)),
        TermRank::OtherBoxed => left.raw().cmp(&right.raw()),
    }
}

fn compare_exact(left: Term, right: Term) -> Ordering {
    let left_kind = exact_kind(left);
    let right_kind = exact_kind(right);
    match left_kind.cmp(&right_kind) {
        Ordering::Equal => compare_same_exact_kind(left, right, left_kind),
        order => order,
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
enum ExactKind {
    SmallInt,
    Atom,
    Pid,
    Nil,
    Tuple,
    Float,
    BigInt,
    Closure,
    Map,
    Reference,
    Binary,
    List,
    Other,
}

fn exact_kind(term: Term) -> ExactKind {
    if term.is_small_int() {
        ExactKind::SmallInt
    } else if term.is_atom() {
        ExactKind::Atom
    } else if term.is_pid() {
        ExactKind::Pid
    } else if term.is_nil() {
        ExactKind::Nil
    } else if Tuple::new(term).is_some() {
        ExactKind::Tuple
    } else if Float::new(term).is_some() {
        ExactKind::Float
    } else if BigInt::new(term).is_some() {
        ExactKind::BigInt
    } else if Closure::new(term).is_some() {
        ExactKind::Closure
    } else if Map::new(term).is_some() {
        ExactKind::Map
    } else if Reference::new(term).is_some() {
        ExactKind::Reference
    } else if Binary::new(term).is_some() {
        ExactKind::Binary
    } else if term.is_list() {
        ExactKind::List
    } else {
        ExactKind::Other
    }
}

fn compare_same_exact_kind(left: Term, right: Term, kind: ExactKind) -> Ordering {
    match kind {
        ExactKind::SmallInt => left.as_small_int().cmp(&right.as_small_int()),
        ExactKind::Atom => left.raw().cmp(&right.raw()),
        ExactKind::Pid => left.as_pid().cmp(&right.as_pid()),
        ExactKind::Nil => Ordering::Equal,
        ExactKind::Tuple => compare_tuples_exact(left, right),
        ExactKind::Float => float_bits(left).cmp(&float_bits(right)),
        ExactKind::BigInt => compare_bigints(left, right),
        ExactKind::Closure => compare_closures_exact(left, right),
        ExactKind::Map => compare_maps_exact(left, right),
        ExactKind::Reference => reference_id(left).cmp(&reference_id(right)),
        ExactKind::Binary => binary_bytes(left).cmp(binary_bytes(right)),
        ExactKind::List => compare_lists_exact(left, right),
        ExactKind::Other => left.raw().cmp(&right.raw()),
    }
}

fn compare_numbers(left: Term, right: Term) -> Ordering {
    match (number_value(left), number_value(right)) {
        (Some(NumberValue::SmallInt(left)), Some(NumberValue::SmallInt(right))) => left.cmp(&right),
        (Some(NumberValue::SmallInt(left)), Some(NumberValue::Float(right))) => {
            compare_f64(left as f64, right)
        }
        (Some(NumberValue::Float(left)), Some(NumberValue::SmallInt(right))) => {
            compare_f64(left, right as f64)
        }
        (Some(NumberValue::Float(left)), Some(NumberValue::Float(right))) => {
            compare_f64(left, right)
        }
        (Some(NumberValue::SmallInt(left)), None) => compare_small_int_to_bigint(left, right),
        (None, Some(NumberValue::SmallInt(right))) => {
            compare_small_int_to_bigint(right, left).reverse()
        }
        (Some(NumberValue::Float(left)), None) => compare_f64(left, bigint_to_f64(right)),
        (None, Some(NumberValue::Float(right))) => compare_f64(bigint_to_f64(left), right),
        (None, None) => compare_bigints(left, right),
    }
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.total_cmp(&right)
}

fn float_bits(term: Term) -> Option<u64> {
    let float = Float::new(term)?;
    Some(float.value().to_bits())
}

fn reference_id(term: Term) -> Option<u64> {
    Reference::new(term).map(Reference::id)
}

fn binary_bytes(term: Term) -> &'static [u8] {
    Binary::new(term).map_or(&[], Binary::as_bytes)
}

fn compare_bigints(left: Term, right: Term) -> Ordering {
    match (BigInt::new(left), BigInt::new(right)) {
        (Some(left), Some(right)) => compare_bigint_values(left, right),
        _ => left.raw().cmp(&right.raw()),
    }
}

fn compare_bigint_values(left: BigInt, right: BigInt) -> Ordering {
    let left_limbs = normalized_limbs(left);
    let right_limbs = normalized_limbs(right);
    let left_negative = left.is_negative() && !left_limbs.is_empty();
    let right_negative = right.is_negative() && !right_limbs.is_empty();

    match (left_negative, right_negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => compare_magnitude(left_limbs, right_limbs),
        (true, true) => compare_magnitude(left_limbs, right_limbs).reverse(),
    }
}

fn compare_small_int_to_bigint(left: i64, right: Term) -> Ordering {
    let Some(right) = BigInt::new(right) else {
        return Ordering::Less;
    };
    let right_limbs = normalized_limbs(right);
    let right_negative = right.is_negative() && !right_limbs.is_empty();

    match (left.is_negative(), right_negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => compare_small_magnitude(left.unsigned_abs(), right_limbs),
        (true, true) => compare_small_magnitude(left.unsigned_abs(), right_limbs).reverse(),
    }
}

fn compare_small_magnitude(left: u64, right_limbs: &[u64]) -> Ordering {
    match right_limbs.len().cmp(&1) {
        Ordering::Less => left.cmp(&0),
        Ordering::Equal => left.cmp(&right_limbs[0]),
        Ordering::Greater => Ordering::Less,
    }
}

fn compare_magnitude(left: &[u64], right: &[u64]) -> Ordering {
    match left.len().cmp(&right.len()) {
        Ordering::Equal => left.iter().rev().cmp(right.iter().rev()),
        order => order,
    }
}

fn normalized_limbs(bigint: BigInt) -> &'static [u64] {
    let limbs = bigint.limbs();
    let significant_len = limbs
        .iter()
        .rposition(|limb| *limb != 0)
        .map_or(0, |index| index + 1);
    &limbs[..significant_len]
}

fn bigint_to_f64(term: Term) -> f64 {
    let Some(bigint) = BigInt::new(term) else {
        return f64::NAN;
    };

    let mut value = 0.0_f64;
    for limb in normalized_limbs(bigint).iter().rev() {
        value = value.mul_add(18_446_744_073_709_551_616.0, *limb as f64);
    }

    if bigint.is_negative() && value != 0.0 {
        -value
    } else {
        value
    }
}

fn compare_tuples(left: Term, right: Term) -> Ordering {
    match (Tuple::new(left), Tuple::new(right)) {
        (Some(left), Some(right)) => match left.arity().cmp(&right.arity()) {
            Ordering::Equal => compare_tuple_elements(left, right),
            order => order,
        },
        _ => left.raw().cmp(&right.raw()),
    }
}

fn compare_tuples_exact(left: Term, right: Term) -> Ordering {
    match (Tuple::new(left), Tuple::new(right)) {
        (Some(left), Some(right)) => match left.arity().cmp(&right.arity()) {
            Ordering::Equal => compare_tuple_elements_exact(left, right),
            order => order,
        },
        _ => left.raw().cmp(&right.raw()),
    }
}

fn compare_tuple_elements(left: Tuple, right: Tuple) -> Ordering {
    for index in 0..left.arity() {
        if let (Some(left_element), Some(right_element)) = (left.get(index), right.get(index)) {
            match cmp(left_element, right_element) {
                Ordering::Equal => {}
                order => return order,
            }
        }
    }
    Ordering::Equal
}

fn compare_tuple_elements_exact(left: Tuple, right: Tuple) -> Ordering {
    for index in 0..left.arity() {
        if let (Some(left_element), Some(right_element)) = (left.get(index), right.get(index)) {
            match compare_exact(left_element, right_element) {
                Ordering::Equal => {}
                order => return order,
            }
        }
    }
    Ordering::Equal
}

fn compare_lists(left: Term, right: Term) -> Ordering {
    compare_lists_with(left, right, cmp)
}

fn compare_lists_exact(left: Term, right: Term) -> Ordering {
    compare_lists_with(left, right, compare_exact)
}

fn compare_lists_with(
    mut left: Term,
    mut right: Term,
    element_cmp: fn(Term, Term) -> Ordering,
) -> Ordering {
    loop {
        match (Cons::new(left), Cons::new(right)) {
            (Some(left_cons), Some(right_cons)) => {
                match element_cmp(left_cons.head(), right_cons.head()) {
                    Ordering::Equal => {
                        left = left_cons.tail();
                        right = right_cons.tail();
                    }
                    order => return order,
                }
            }
            _ => return element_cmp(left, right),
        }
    }
}

fn compare_maps(left: Term, right: Term) -> Ordering {
    compare_maps_with(left, right, cmp)
}

fn compare_maps_exact(left: Term, right: Term) -> Ordering {
    compare_maps_with(left, right, compare_exact)
}

fn compare_maps_with(left: Term, right: Term, element_cmp: fn(Term, Term) -> Ordering) -> Ordering {
    match (Map::new(left), Map::new(right)) {
        (Some(left), Some(right)) => {
            let left_entries = sorted_map_entries(left, element_cmp);
            let right_entries = sorted_map_entries(right, element_cmp);
            match left_entries.len().cmp(&right_entries.len()) {
                Ordering::Equal => compare_map_entries(&left_entries, &right_entries, element_cmp),
                order => order,
            }
        }
        _ => left.raw().cmp(&right.raw()),
    }
}

#[derive(Copy, Clone)]
struct MapEntry {
    key: Term,
    value: Term,
}

fn sorted_map_entries(map: Map, element_cmp: fn(Term, Term) -> Ordering) -> Vec<MapEntry> {
    let mut entries = Vec::with_capacity(map.len());
    for index in 0..map.len() {
        if let (Some(key), Some(value)) = (map.key(index), map.value(index)) {
            entries.push(MapEntry { key, value });
        }
    }
    entries.sort_by(|left, right| element_cmp(left.key, right.key));
    entries
}

fn compare_map_entries(
    left_entries: &[MapEntry],
    right_entries: &[MapEntry],
    element_cmp: fn(Term, Term) -> Ordering,
) -> Ordering {
    for (left, right) in left_entries.iter().zip(right_entries.iter()) {
        match element_cmp(left.key, right.key) {
            Ordering::Equal => match element_cmp(left.value, right.value) {
                Ordering::Equal => {}
                order => return order,
            },
            order => return order,
        }
    }
    Ordering::Equal
}

fn compare_closures(left: Term, right: Term) -> Ordering {
    compare_closures_with(left, right, cmp)
}

fn compare_closures_exact(left: Term, right: Term) -> Ordering {
    compare_closures_with(left, right, compare_exact)
}

fn compare_closures_with(
    left: Term,
    right: Term,
    element_cmp: fn(Term, Term) -> Ordering,
) -> Ordering {
    match (Closure::new(left), Closure::new(right)) {
        (Some(left), Some(right)) => {
            match left
                .module()
                .map(|module| Term::atom(module).raw())
                .cmp(&right.module().map(|module| Term::atom(module).raw()))
            {
                Ordering::Equal => {}
                order => return order,
            }
            match left.function_index().cmp(&right.function_index()) {
                Ordering::Equal => {}
                order => return order,
            }
            match left.arity().cmp(&right.arity()) {
                Ordering::Equal => {}
                order => return order,
            }
            match left.generation().cmp(&right.generation()) {
                Ordering::Equal => {}
                order => return order,
            }
            match left.unique_id().cmp(&right.unique_id()) {
                Ordering::Equal => {}
                order => return order,
            }
            match left.num_free().cmp(&right.num_free()) {
                Ordering::Equal => {}
                order => return order,
            }
            for index in 0..left.num_free() {
                if let (Some(left_free), Some(right_free)) =
                    (left.free_var(index), right.free_var(index))
                {
                    match element_cmp(left_free, right_free) {
                        Ordering::Equal => {}
                        order => return order,
                    }
                }
            }
            Ordering::Equal
        }
        _ => left.raw().cmp(&right.raw()),
    }
}

#[cfg(test)]
mod tests;
