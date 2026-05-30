//! Term ordering and equality.
//!
//! Implements both `==` semantics (number coercion: 1 == 1.0) and
//! `=:=` semantics (exact: 1 =/= 1.0). Term ordering follows the
//! BEAM order: number < atom < reference < fun < port < pid <
//! tuple < map < nil < list < binary. Structural comparison for
//! boxed terms recurses into elements.

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
enum TermRank {
    Number,
    Atom,
    Reference,
    Fun,
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
        // No port representation exists yet; unknown boxed terms sort after the
        // implemented BEAM term kinds until the term layout grows a port value.
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
        (None, None) => compare_bigints(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
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
        (Some(left), Some(right)) => match left.is_negative().cmp(&right.is_negative()) {
            Ordering::Equal => match left.limb_count().cmp(&right.limb_count()) {
                Ordering::Equal => left.limbs().cmp(right.limbs()),
                order => order,
            },
            // Positive numbers sort after negative numbers.
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        },
        _ => left.raw().cmp(&right.raw()),
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
mod tests {
    use super::*;
    use crate::{
        atom::Atom,
        term::{
            binary::write_binary,
            boxed::{
                write_bigint, write_closure, write_cons, write_float, write_map, write_reference,
                write_tuple,
            },
        },
    };

    #[test]
    fn exact_equality_compares_immediates_and_distinguishes_numeric_types() {
        let mut float_heap = [0_u64; 2];
        let float_one = write_float(&mut float_heap, 1.0).unwrap();

        assert_eq!(Term::small_int(1), Term::small_int(1));
        assert_ne!(Term::small_int(1), Term::small_int(2));
        assert_eq!(Term::atom(Atom::OK), Term::atom(Atom::OK));
        assert_ne!(Term::atom(Atom::OK), Term::atom(Atom::ERROR));
        assert_ne!(Term::small_int(1), float_one);
        assert_eq!(Term::pid(1), Term::pid(1));
        assert_ne!(Term::NIL, Term::atom(Atom::NIL));
    }

    #[test]
    fn exact_equality_compares_boxed_terms_structurally() {
        let mut left_heap = [0_u64; 3];
        let mut right_heap = [0_u64; 3];
        let mut different_heap = [0_u64; 3];
        let left = write_tuple(&mut left_heap, &[Term::small_int(1), Term::small_int(2)]).unwrap();
        let right =
            write_tuple(&mut right_heap, &[Term::small_int(1), Term::small_int(2)]).unwrap();
        let different = write_tuple(
            &mut different_heap,
            &[Term::small_int(1), Term::small_int(3)],
        )
        .unwrap();

        assert_eq!(left, right);
        assert_ne!(left, different);
    }

    #[test]
    fn numeric_equality_coerces_integer_float_pairs() {
        let mut one_heap = [0_u64; 2];
        let mut one_point_five_heap = [0_u64; 2];
        let float_one = write_float(&mut one_heap, 1.0).unwrap();
        let float_one_point_five = write_float(&mut one_point_five_heap, 1.5).unwrap();

        assert!(numeric_eq(Term::small_int(1), float_one));
        assert!(!numeric_eq(Term::small_int(1), float_one_point_five));
        assert!(numeric_eq(Term::small_int(1), Term::small_int(1)));
        assert!(numeric_eq(Term::atom(Atom::OK), Term::atom(Atom::OK)));
        assert!(!numeric_eq(Term::small_int(1), Term::atom(Atom::OK)));
    }

    #[test]
    fn beam_ordering_across_available_types_follows_rank_order() {
        let mut ref_heap = [0_u64; 2];
        let mut closure_heap = [0_u64; 5];
        let mut tuple_heap = [0_u64; 1];
        let mut map_heap = [0_u64; 2];
        let mut list_heap = [0_u64; 2];
        let mut binary_heap = [0_u64; 3];

        let terms = [
            Term::small_int(1),
            Term::atom(Atom::OK),
            write_reference(&mut ref_heap, 1).unwrap(),
            write_closure(&mut closure_heap, Atom::OK, 0, 0, &[]).unwrap(),
            // Port rank is reserved in the comparator but no term encoding exists yet.
            Term::pid(1),
            write_tuple(&mut tuple_heap, &[]).unwrap(),
            write_map(&mut map_heap, &[], &[]).unwrap(),
            Term::NIL,
            write_cons(&mut list_heap, Term::small_int(1), Term::NIL).unwrap(),
            write_binary(&mut binary_heap, b"a").unwrap(),
        ];

        for window in terms.windows(2) {
            assert!(window[0] < window[1]);
        }
        assert!(Term::small_int(1) < Term::atom(Atom::OK));
        assert!(Term::atom(Atom::OK) < Term::pid(1));
    }

    #[test]
    fn beam_ordering_compares_within_types() {
        let mut tuple_one_heap = [0_u64; 2];
        let mut tuple_two_heap = [0_u64; 3];
        let mut tuple_a_heap = [0_u64; 3];
        let mut tuple_b_heap = [0_u64; 3];
        let tuple_one = write_tuple(&mut tuple_one_heap, &[Term::small_int(1)]).unwrap();
        let tuple_two = write_tuple(
            &mut tuple_two_heap,
            &[Term::small_int(1), Term::small_int(2)],
        )
        .unwrap();
        let tuple_a =
            write_tuple(&mut tuple_a_heap, &[Term::small_int(1), Term::small_int(2)]).unwrap();
        let tuple_b =
            write_tuple(&mut tuple_b_heap, &[Term::small_int(1), Term::small_int(3)]).unwrap();

        assert!(Term::small_int(1) < Term::small_int(2));
        assert!(!(Term::small_int(2) < Term::small_int(1)));
        assert!(tuple_a < tuple_b);
        assert!(tuple_one < tuple_two);
    }

    #[test]
    fn nested_structural_comparison_recurses_into_tuples_and_lists() {
        let mut inner_left_heap = [0_u64; 3];
        let mut inner_right_heap = [0_u64; 3];
        let mut inner_diff_heap = [0_u64; 3];
        let inner_left = write_tuple(
            &mut inner_left_heap,
            &[Term::small_int(1), Term::small_int(2)],
        )
        .unwrap();
        let inner_right = write_tuple(
            &mut inner_right_heap,
            &[Term::small_int(1), Term::small_int(2)],
        )
        .unwrap();
        let inner_diff = write_tuple(
            &mut inner_diff_heap,
            &[Term::small_int(1), Term::small_int(3)],
        )
        .unwrap();
        let mut outer_left_heap = [0_u64; 3];
        let mut outer_right_heap = [0_u64; 3];
        let mut outer_diff_heap = [0_u64; 3];
        let outer_left =
            write_tuple(&mut outer_left_heap, &[inner_left, Term::small_int(3)]).unwrap();
        let outer_right =
            write_tuple(&mut outer_right_heap, &[inner_right, Term::small_int(3)]).unwrap();
        let outer_diff =
            write_tuple(&mut outer_diff_heap, &[inner_diff, Term::small_int(3)]).unwrap();

        assert_eq!(outer_left, outer_right);
        assert_ne!(outer_left, outer_diff);

        let mut right_tail_left_heap = [0_u64; 2];
        let mut right_tail_right_heap = [0_u64; 2];
        let mut right_nested_left_heap = [0_u64; 2];
        let mut right_nested_right_heap = [0_u64; 2];
        let left_nested_tail =
            write_cons(&mut right_tail_left_heap, Term::small_int(3), Term::NIL).unwrap();
        let right_nested_tail =
            write_cons(&mut right_tail_right_heap, Term::small_int(4), Term::NIL).unwrap();
        let left_nested = write_cons(
            &mut right_nested_left_heap,
            Term::small_int(2),
            left_nested_tail,
        )
        .unwrap();
        let right_nested = write_cons(
            &mut right_nested_right_heap,
            Term::small_int(2),
            right_nested_tail,
        )
        .unwrap();
        let mut left_root_tail_heap = [0_u64; 2];
        let mut right_root_tail_heap = [0_u64; 2];
        let mut left_root_heap = [0_u64; 2];
        let mut right_root_heap = [0_u64; 2];
        let left_root_tail = write_cons(&mut left_root_tail_heap, left_nested, Term::NIL).unwrap();
        let right_root_tail =
            write_cons(&mut right_root_tail_heap, right_nested, Term::NIL).unwrap();
        let left_list =
            write_cons(&mut left_root_heap, Term::small_int(1), left_root_tail).unwrap();
        let right_list =
            write_cons(&mut right_root_heap, Term::small_int(1), right_root_tail).unwrap();

        assert!(left_list < right_list);
    }

    #[test]
    fn proper_lists_compare_head_then_tail_iteratively() {
        let mut left_tail_heap = [0_u64; 2];
        let mut right_tail_heap = [0_u64; 2];
        let mut left_head_heap = [0_u64; 2];
        let mut right_head_heap = [0_u64; 2];
        let left_tail = write_cons(&mut left_tail_heap, Term::small_int(2), Term::NIL).unwrap();
        let right_tail = write_cons(&mut right_tail_heap, Term::small_int(3), Term::NIL).unwrap();
        let left = write_cons(&mut left_head_heap, Term::small_int(1), left_tail).unwrap();
        let right = write_cons(&mut right_head_heap, Term::small_int(1), right_tail).unwrap();

        assert!(left < right);
    }

    #[test]
    fn map_comparison_uses_sorted_key_order_then_values() {
        let mut left_heap = [0_u64; 6];
        let mut right_heap = [0_u64; 6];
        let mut different_value_heap = [0_u64; 6];
        let left = write_map(
            &mut left_heap,
            &[Term::small_int(2), Term::small_int(1)],
            &[Term::atom(Atom::ERROR), Term::atom(Atom::OK)],
        )
        .unwrap();
        let right = write_map(
            &mut right_heap,
            &[Term::small_int(1), Term::small_int(2)],
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR)],
        )
        .unwrap();
        let different_value = write_map(
            &mut different_value_heap,
            &[Term::small_int(1), Term::small_int(2)],
            &[Term::atom(Atom::ERROR), Term::atom(Atom::ERROR)],
        )
        .unwrap();

        assert_eq!(left, right);
        assert!(right < different_value);
    }

    #[test]
    fn exact_equality_covers_boxed_numeric_reference_fun_map_and_binary_terms() {
        let mut float_a_heap = [0_u64; 2];
        let mut float_b_heap = [0_u64; 2];
        let mut bigint_a_heap = [0_u64; 4];
        let mut bigint_b_heap = [0_u64; 4];
        let mut ref_a_heap = [0_u64; 2];
        let mut ref_b_heap = [0_u64; 2];
        let mut closure_a_heap = [0_u64; 6];
        let mut closure_b_heap = [0_u64; 6];
        let mut bin_a_heap = [0_u64; 3];
        let mut bin_b_heap = [0_u64; 3];

        let float_a = write_float(&mut float_a_heap, 2.5).unwrap();
        let float_b = write_float(&mut float_b_heap, 2.5).unwrap();
        let bigint_a = write_bigint(&mut bigint_a_heap, false, &[9]).unwrap();
        let bigint_b = write_bigint(&mut bigint_b_heap, false, &[9]).unwrap();
        let ref_a = write_reference(&mut ref_a_heap, 42).unwrap();
        let ref_b = write_reference(&mut ref_b_heap, 42).unwrap();
        let closure_a =
            write_closure(&mut closure_a_heap, Atom::OK, 1, 1, &[Term::small_int(1)]).unwrap();
        let closure_b =
            write_closure(&mut closure_b_heap, Atom::OK, 1, 1, &[Term::small_int(1)]).unwrap();
        let bin_a = write_binary(&mut bin_a_heap, b"ab").unwrap();
        let bin_b = write_binary(&mut bin_b_heap, b"ab").unwrap();

        assert_eq!(float_a, float_b);
        assert_eq!(bigint_a, bigint_b);
        assert_eq!(ref_a, ref_b);
        assert_eq!(closure_a, closure_b);
        assert_eq!(bin_a, bin_b);
    }

    #[test]
    fn edge_cases_cover_empty_tuple_empty_list_and_atom_nil() {
        let mut empty_tuple_heap = [0_u64; 1];
        let empty_tuple = write_tuple(&mut empty_tuple_heap, &[]).unwrap();

        assert_eq!(empty_tuple, empty_tuple);
        assert_eq!(Term::NIL, Term::NIL);
        assert_ne!(Term::NIL, Term::atom(Atom::NIL));
        assert!(Term::atom(Atom::NIL) < Term::NIL);
    }

    #[test]
    fn comparing_long_lists_does_not_stack_overflow() {
        const LEN: usize = 10_000;
        let mut left_heap = vec![[0_u64; 2]; LEN];
        let mut right_heap = vec![[0_u64; 2]; LEN];
        let mut left = Term::NIL;
        let mut right = Term::NIL;

        for index in (0..LEN).rev() {
            left = write_cons(&mut left_heap[index], Term::small_int(index as i64), left).unwrap();
            right =
                write_cons(&mut right_heap[index], Term::small_int(index as i64), right).unwrap();
        }

        assert_eq!(left, right);
        assert_eq!(cmp(left, right), Ordering::Equal);
    }
}
