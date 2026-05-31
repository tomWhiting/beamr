use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::native::stdlib_stubs::lists_bifs::{
    bif_lists_append_1, bif_lists_append_2, bif_lists_join, bif_lists_reverse_2, bif_lists_seq,
};
use crate::native::stdlib_stubs::maps_bifs::{
    bif_maps_find, bif_maps_keys, bif_maps_put, bif_maps_to_list, bif_maps_values, bif_maps_with,
    bif_maps_without,
};
use crate::term::Term;
use crate::term::boxed::{Cons, Map, Tuple, write_map};

fn context() -> ProcessContext {
    ProcessContext::new()
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn list_from_slice(ctx: &mut ProcessContext, elements: &[Term]) -> Term {
    let mut tail = Term::NIL;
    for element in elements.iter().rev() {
        tail = ctx
            .alloc_cons(*element, tail)
            .expect("test cons allocation");
    }
    tail
}

fn list_to_vec(term: Term) -> Vec<Term> {
    let mut elements = Vec::new();
    let mut current = term;
    while !current.is_nil() {
        let cons = Cons::new(current).expect("proper list");
        elements.push(cons.head());
        current = cons.tail();
    }
    elements
}

fn map_from_pairs(pairs: &[(Term, Term)]) -> Term {
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|(left, _), (right, _)| left.cmp(right));
    let keys: Vec<_> = sorted.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = sorted.iter().map(|(_, value)| *value).collect();
    let heap: &mut [u64] = Box::leak(vec![0_u64; 2 + pairs.len() * 2].into_boxed_slice());
    write_map(heap, &keys, &values).expect("test map allocation")
}

fn assert_map_entries(term: Term, expected: &[(Term, Term)]) {
    let map = Map::new(term).expect("map result");
    assert_eq!(map.len(), expected.len());
    for (key, value) in expected {
        assert_eq!(map.get(*key), Some(*value));
    }
}

#[test]
fn maps_put_inserts_and_replaces_entries() {
    let mut ctx = context();
    let empty = map_from_pairs(&[]);
    let inserted = bif_maps_put(&[Term::atom(Atom::OK), Term::small_int(1), empty], &mut ctx)
        .expect("put insert");
    assert_map_entries(inserted, &[(Term::atom(Atom::OK), Term::small_int(1))]);

    let replaced = bif_maps_put(
        &[Term::atom(Atom::OK), Term::small_int(2), inserted],
        &mut ctx,
    )
    .expect("put replace");
    assert_map_entries(replaced, &[(Term::atom(Atom::OK), Term::small_int(2))]);
}

#[test]
fn maps_find_returns_ok_tuple_or_error_atom() {
    let mut ctx = context();
    let map = map_from_pairs(&[(Term::atom(Atom::OK), Term::small_int(7))]);
    let found = bif_maps_find(&[Term::atom(Atom::OK), map], &mut ctx).expect("find hit");
    let tuple = Tuple::new(found).expect("{ok, value}");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(Term::small_int(7)));
    assert_eq!(
        bif_maps_find(&[Term::atom(Atom::ERROR), map], &mut ctx),
        Ok(Term::atom(Atom::ERROR))
    );
}

#[test]
fn maps_keys_values_and_to_list_project_sorted_entries() {
    let mut ctx = context();
    let map = map_from_pairs(&[
        (Term::atom(Atom::ERROR), Term::small_int(2)),
        (Term::atom(Atom::OK), Term::small_int(1)),
    ]);

    assert_eq!(
        list_to_vec(bif_maps_keys(&[map], &mut ctx).expect("keys")),
        vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)]
    );
    assert_eq!(
        list_to_vec(bif_maps_values(&[map], &mut ctx).expect("values")),
        vec![Term::small_int(1), Term::small_int(2)]
    );

    let pairs = list_to_vec(bif_maps_to_list(&[map], &mut ctx).expect("to_list"));
    let first = Tuple::new(pairs[0]).expect("first pair");
    let second = Tuple::new(pairs[1]).expect("second pair");
    assert_eq!(first.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(first.get(1), Some(Term::small_int(1)));
    assert_eq!(second.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(second.get(1), Some(Term::small_int(2)));
}

#[test]
fn maps_with_and_without_select_entries_and_validate_keys_list() {
    let mut ctx = context();
    let map = map_from_pairs(&[
        (Term::atom(Atom::OK), Term::small_int(1)),
        (Term::atom(Atom::ERROR), Term::small_int(2)),
    ]);
    let keys = list_from_slice(&mut ctx, &[Term::atom(Atom::ERROR)]);

    let only = bif_maps_with(&[keys, map], &mut ctx).expect("with");
    assert_map_entries(only, &[(Term::atom(Atom::ERROR), Term::small_int(2))]);
    let without = bif_maps_without(&[keys, map], &mut ctx).expect("without");
    assert_map_entries(without, &[(Term::atom(Atom::OK), Term::small_int(1))]);
    assert_eq!(
        bif_maps_with(&[Term::small_int(1), map], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn lists_append_flattens_list_of_lists_and_rejects_improper_parts() {
    let mut ctx = context();
    let left = list_from_slice(&mut ctx, &[Term::small_int(1), Term::small_int(2)]);
    let right = list_from_slice(&mut ctx, &[Term::small_int(3), Term::small_int(4)]);
    let lists = list_from_slice(&mut ctx, &[left, right]);

    let result = bif_lists_append_1(&[lists], &mut ctx).expect("append/1");
    assert_eq!(
        list_to_vec(result),
        vec![
            Term::small_int(1),
            Term::small_int(2),
            Term::small_int(3),
            Term::small_int(4)
        ]
    );
    assert_eq!(bif_lists_append_1(&[left], &mut ctx), Err(badarg()));
}

#[test]
fn lists_append_two_preserves_right_tail_and_validates_left_list() {
    let mut ctx = context();
    let left = list_from_slice(&mut ctx, &[Term::small_int(1), Term::small_int(2)]);
    let right = list_from_slice(&mut ctx, &[Term::small_int(3), Term::small_int(4)]);

    let result = bif_lists_append_2(&[left, right], &mut ctx).expect("append/2");
    assert_eq!(
        list_to_vec(result),
        vec![
            Term::small_int(1),
            Term::small_int(2),
            Term::small_int(3),
            Term::small_int(4)
        ]
    );
    assert_eq!(
        bif_lists_append_2(&[Term::small_int(1), right], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn lists_join_inserts_separator_and_handles_empty_list() {
    let mut ctx = context();
    let list = list_from_slice(
        &mut ctx,
        &[Term::small_int(1), Term::small_int(2), Term::small_int(3)],
    );

    let result = bif_lists_join(&[Term::atom(Atom::OK), list], &mut ctx).expect("join");
    assert_eq!(
        list_to_vec(result),
        vec![
            Term::small_int(1),
            Term::atom(Atom::OK),
            Term::small_int(2),
            Term::atom(Atom::OK),
            Term::small_int(3)
        ]
    );
    assert_eq!(
        bif_lists_join(&[Term::atom(Atom::OK), Term::NIL], &mut ctx),
        Ok(Term::NIL)
    );
}

#[test]
fn lists_reverse_two_prepends_reversed_list_to_tail() {
    let mut ctx = context();
    let list = list_from_slice(
        &mut ctx,
        &[Term::small_int(1), Term::small_int(2), Term::small_int(3)],
    );
    let tail = list_from_slice(&mut ctx, &[Term::small_int(4), Term::small_int(5)]);

    let result = bif_lists_reverse_2(&[list, tail], &mut ctx).expect("reverse/2");
    assert_eq!(
        list_to_vec(result),
        vec![
            Term::small_int(3),
            Term::small_int(2),
            Term::small_int(1),
            Term::small_int(4),
            Term::small_int(5)
        ]
    );
    assert_eq!(
        bif_lists_reverse_2(&[Term::small_int(1), tail], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn lists_seq_builds_inclusive_range_and_empty_descending_range() {
    let mut ctx = context();
    let result =
        bif_lists_seq(&[Term::small_int(1), Term::small_int(5)], &mut ctx).expect("seq ascending");
    assert_eq!(
        list_to_vec(result),
        vec![
            Term::small_int(1),
            Term::small_int(2),
            Term::small_int(3),
            Term::small_int(4),
            Term::small_int(5)
        ]
    );
    assert_eq!(
        bif_lists_seq(&[Term::small_int(5), Term::small_int(1)], &mut ctx),
        Ok(Term::NIL)
    );
}
