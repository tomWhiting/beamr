use super::*;
use crate::{
    atom::{Atom, AtomTable},
    term::{
        binary::write_binary,
        boxed::{
            write_bigint, write_closure, write_cons, write_float, write_map, write_reference,
            write_tuple,
        },
    },
};

fn common_atoms() -> AtomTable {
    AtomTable::with_common_atoms()
}

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
    let right = write_tuple(&mut right_heap, &[Term::small_int(1), Term::small_int(2)]).unwrap();
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
    let mut closure_heap = [0_u64; 7];
    let mut tuple_heap = [0_u64; 1];
    let mut map_heap = [0_u64; 2];
    let mut list_heap = [0_u64; 2];
    let mut binary_heap = [0_u64; 3];

    let terms = [
        Term::small_int(1),
        Term::atom(Atom::OK),
        write_reference(&mut ref_heap, 1).unwrap(),
        write_closure(&mut closure_heap, Atom::OK, 0, 0, 1, 0, &[]).unwrap(),
        // Port rank is reserved in the comparator but no term encoding exists yet.
        Term::pid(1),
        write_tuple(&mut tuple_heap, &[]).unwrap(),
        write_map(&mut map_heap, &[], &[]).unwrap(),
        Term::NIL,
        write_cons(&mut list_heap, Term::small_int(1), Term::NIL).unwrap(),
        write_binary(&mut binary_heap, b"a").unwrap(),
    ];

    let atom_table = common_atoms();
    for window in terms.windows(2) {
        assert_eq!(cmp(window[0], window[1], &atom_table), Ordering::Less);
    }
    assert_eq!(
        cmp(Term::small_int(1), Term::atom(Atom::OK), &atom_table),
        Ordering::Less
    );
    assert_eq!(
        cmp(Term::atom(Atom::OK), Term::pid(1), &atom_table),
        Ordering::Less
    );
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

    let atom_table = common_atoms();
    assert_eq!(
        cmp(Term::small_int(1), Term::small_int(2), &atom_table),
        Ordering::Less
    );
    assert_ne!(
        cmp(Term::small_int(2), Term::small_int(1), &atom_table),
        Ordering::Less
    );
    assert_eq!(cmp(tuple_a, tuple_b, &atom_table), Ordering::Less);
    assert_eq!(cmp(tuple_one, tuple_two, &atom_table), Ordering::Less);
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
    let outer_left = write_tuple(&mut outer_left_heap, &[inner_left, Term::small_int(3)]).unwrap();
    let outer_right =
        write_tuple(&mut outer_right_heap, &[inner_right, Term::small_int(3)]).unwrap();
    let outer_diff = write_tuple(&mut outer_diff_heap, &[inner_diff, Term::small_int(3)]).unwrap();

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
    let right_root_tail = write_cons(&mut right_root_tail_heap, right_nested, Term::NIL).unwrap();
    let left_list = write_cons(&mut left_root_heap, Term::small_int(1), left_root_tail).unwrap();
    let right_list = write_cons(&mut right_root_heap, Term::small_int(1), right_root_tail).unwrap();

    let atom_table = common_atoms();
    assert_eq!(cmp(left_list, right_list, &atom_table), Ordering::Less);
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

    let atom_table = common_atoms();
    assert_eq!(cmp(left, right, &atom_table), Ordering::Less);
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

    let atom_table = common_atoms();
    assert_eq!(left, right);
    assert_eq!(cmp(right, different_value, &atom_table), Ordering::Less);
}

#[test]
fn exact_equality_covers_boxed_numeric_reference_fun_map_and_binary_terms() {
    let mut float_a_heap = [0_u64; 2];
    let mut float_b_heap = [0_u64; 2];
    let mut bigint_a_heap = [0_u64; 4];
    let mut bigint_b_heap = [0_u64; 4];
    let mut ref_a_heap = [0_u64; 2];
    let mut ref_b_heap = [0_u64; 2];
    let mut closure_a_heap = [0_u64; 8];
    let mut closure_b_heap = [0_u64; 8];
    let mut bin_a_heap = [0_u64; 3];
    let mut bin_b_heap = [0_u64; 3];

    let float_a = write_float(&mut float_a_heap, 2.5).unwrap();
    let float_b = write_float(&mut float_b_heap, 2.5).unwrap();
    let bigint_a = write_bigint(&mut bigint_a_heap, false, &[9]).unwrap();
    let bigint_b = write_bigint(&mut bigint_b_heap, false, &[9]).unwrap();
    let ref_a = write_reference(&mut ref_a_heap, 42).unwrap();
    let ref_b = write_reference(&mut ref_b_heap, 42).unwrap();
    let closure_a = write_closure(
        &mut closure_a_heap,
        Atom::OK,
        1,
        1,
        1,
        0,
        &[Term::small_int(1)],
    )
    .unwrap();
    let closure_b = write_closure(
        &mut closure_b_heap,
        Atom::OK,
        1,
        1,
        1,
        0,
        &[Term::small_int(1)],
    )
    .unwrap();
    let bin_a = write_binary(&mut bin_a_heap, b"ab").unwrap();
    let bin_b = write_binary(&mut bin_b_heap, b"ab").unwrap();

    assert_eq!(float_a, float_b);
    assert_eq!(bigint_a, bigint_b);
    assert_eq!(ref_a, ref_b);
    assert_eq!(closure_a, closure_b);
    assert_eq!(bin_a, bin_b);
}

#[test]
fn closure_comparison_includes_generation_and_unique_id_metadata() {
    let mut closure_a_heap = [0_u64; 7];
    let mut closure_b_heap = [0_u64; 7];
    let mut closure_c_heap = [0_u64; 7];
    let closure_a = write_closure(&mut closure_a_heap, Atom::OK, 1, 0, 1, 10, &[]).unwrap();
    let closure_b = write_closure(&mut closure_b_heap, Atom::OK, 1, 0, 2, 10, &[]).unwrap();
    let closure_c = write_closure(&mut closure_c_heap, Atom::OK, 1, 0, 1, 11, &[]).unwrap();

    let atom_table = common_atoms();
    assert_ne!(cmp(closure_a, closure_b, &atom_table), Ordering::Equal);
    assert_ne!(cmp(closure_a, closure_c, &atom_table), Ordering::Equal);
}

#[test]
fn numeric_ordering_compares_bigints_by_value() {
    let mut positive_small_heap = [0_u64; 4];
    let mut positive_large_heap = [0_u64; 5];
    let mut negative_small_heap = [0_u64; 4];
    let mut negative_large_heap = [0_u64; 5];
    let positive_small = write_bigint(&mut positive_small_heap, false, &[9]).unwrap();
    let positive_large = write_bigint(&mut positive_large_heap, false, &[0, 1]).unwrap();
    let negative_small = write_bigint(&mut negative_small_heap, true, &[9]).unwrap();
    let negative_large = write_bigint(&mut negative_large_heap, true, &[0, 1]).unwrap();

    let atom_table = common_atoms();
    assert_eq!(
        cmp(negative_large, negative_small, &atom_table),
        Ordering::Less
    );
    assert_eq!(
        cmp(negative_small, Term::small_int(0), &atom_table),
        Ordering::Less
    );
    assert_eq!(
        cmp(Term::small_int(9), positive_small, &atom_table),
        Ordering::Equal
    );
    assert_eq!(
        cmp(positive_small, Term::small_int(10), &atom_table),
        Ordering::Less
    );
    assert_eq!(
        cmp(positive_small, positive_large, &atom_table),
        Ordering::Less
    );
    assert_eq!(
        cmp(negative_small, positive_small, &atom_table),
        Ordering::Less
    );
}

#[test]
fn edge_cases_cover_empty_tuple_empty_list_and_atom_nil() {
    let mut empty_tuple_heap = [0_u64; 1];
    let empty_tuple = write_tuple(&mut empty_tuple_heap, &[]).unwrap();

    assert_eq!(empty_tuple, empty_tuple);
    assert_eq!(Term::NIL, Term::NIL);
    assert_ne!(Term::NIL, Term::atom(Atom::NIL));
    let atom_table = common_atoms();
    assert_eq!(
        cmp(Term::atom(Atom::NIL), Term::NIL, &atom_table),
        Ordering::Less
    );
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
        right = write_cons(&mut right_heap[index], Term::small_int(index as i64), right).unwrap();
    }

    let atom_table = common_atoms();
    assert_eq!(left, right);
    assert_eq!(cmp(left, right, &atom_table), Ordering::Equal);
}

fn ordered_atom_terms(table: &AtomTable) -> (Term, Term, Term) {
    (
        Term::atom(table.lookup("apple").unwrap()),
        Term::atom(table.lookup("mango").unwrap()),
        Term::atom(table.lookup("zebra").unwrap()),
    )
}

fn intern_atoms(table: &AtomTable, names: &[&str]) {
    for name in names {
        table.intern(name);
    }
}

fn assert_alphabetical_atom_order(table: &AtomTable) {
    let (apple, mango, zebra) = ordered_atom_terms(table);

    assert_eq!(cmp(apple, mango, table), Ordering::Less);
    assert_eq!(cmp(mango, zebra, table), Ordering::Less);
    assert_eq!(cmp(apple, zebra, table), Ordering::Less);
    assert_eq!(cmp(mango, apple, table), Ordering::Greater);
    assert_eq!(cmp(zebra, mango, table), Ordering::Greater);
    assert_eq!(cmp(zebra, apple, table), Ordering::Greater);
    assert_eq!(cmp(apple, apple, table), Ordering::Equal);
}

#[test]
fn atom_ordering_uses_names_when_interned_reverse_alphabetically() {
    let table = AtomTable::new();
    intern_atoms(&table, &["zebra", "mango", "apple"]);

    assert_alphabetical_atom_order(&table);
}

#[test]
fn atom_ordering_uses_names_when_interned_alphabetically() {
    let table = AtomTable::new();
    intern_atoms(&table, &["apple", "mango", "zebra"]);

    assert_alphabetical_atom_order(&table);
}

#[test]
fn sorting_atom_terms_is_stable_across_intern_sequences() {
    let reverse_table = AtomTable::new();
    intern_atoms(&reverse_table, &["zebra", "mango", "apple"]);
    let (apple_reverse, mango_reverse, zebra_reverse) = ordered_atom_terms(&reverse_table);
    let mut reverse_terms = vec![zebra_reverse, apple_reverse, mango_reverse];
    reverse_terms.sort_by(|left, right| cmp(*left, *right, &reverse_table));

    let forward_table = AtomTable::new();
    intern_atoms(&forward_table, &["apple", "mango", "zebra"]);
    let (apple_forward, mango_forward, zebra_forward) = ordered_atom_terms(&forward_table);
    let mut forward_terms = vec![zebra_forward, apple_forward, mango_forward];
    forward_terms.sort_by(|left, right| cmp(*left, *right, &forward_table));

    let reverse_names: Vec<_> = reverse_terms
        .iter()
        .filter_map(|term| term.as_atom().and_then(|atom| reverse_table.resolve(atom)))
        .collect();
    let forward_names: Vec<_> = forward_terms
        .iter()
        .filter_map(|term| term.as_atom().and_then(|atom| forward_table.resolve(atom)))
        .collect();

    assert_eq!(reverse_names, ["apple", "mango", "zebra"]);
    assert_eq!(forward_names, ["apple", "mango", "zebra"]);
}

#[test]
fn map_with_atom_keys_iterates_in_name_order() {
    let table = AtomTable::new();
    intern_atoms(&table, &["zebra", "apple"]);
    let apple = Term::atom(table.lookup("apple").unwrap());
    let zebra = Term::atom(table.lookup("zebra").unwrap());
    let mut entries = [(zebra, Term::small_int(1)), (apple, Term::small_int(2))];
    entries.sort_by(|(left, _), (right, _)| cmp(*left, *right, &table));

    let keys: Vec<_> = entries.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = entries.iter().map(|(_, value)| *value).collect();
    let mut heap = [0_u64; 6];
    let map_term = write_map(&mut heap, &keys, &values).unwrap();
    let map = Map::new(map_term).unwrap();

    assert_eq!(map.key(0).and_then(Term::as_atom), apple.as_atom());
    assert_eq!(map.key(1).and_then(Term::as_atom), zebra.as_atom());
}
