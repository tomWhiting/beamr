use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::native::stdlib_stubs::lists_bifs::{
    bif_lists_flatten, bif_lists_keydelete, bif_lists_keyfind, bif_lists_keysort,
    bif_lists_keystore, bif_lists_last, bif_lists_member, bif_lists_nth, bif_lists_sort,
    bif_lists_unzip, bif_lists_zip,
};
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(std::sync::Arc::new(AtomTable::with_common_atoms())));
    context.attach_process(process, 0);
    context
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

fn tuple_to_vec(term: Term) -> Vec<Term> {
    let tuple = Tuple::new(term).expect("tuple");
    (0..tuple.arity())
        .map(|index| tuple.get(index).expect("tuple element"))
        .collect()
}

#[test]
fn list_query_bifs_return_expected_terms() {
    let mut process = Process::new(1, 512);
    let mut ctx = context(&mut process);
    let a = Term::atom(Atom::OK);
    let b = Term::atom(Atom::ERROR);
    let c = Term::atom(Atom::TRUE);
    let list = list_from_slice(&mut ctx, &[a, b, c]);

    assert_eq!(
        bif_lists_nth(&[Term::small_int(2), list], &mut ctx).expect("nth"),
        b
    );
    assert_eq!(
        bif_lists_member(&[b, list], &mut ctx).expect("member"),
        Term::atom(Atom::TRUE)
    );
    assert_eq!(bif_lists_last(&[list], &mut ctx).expect("last"), c);

    let tuple_a = ctx.alloc_tuple(&[a, Term::small_int(1)]).expect("tuple a");
    let tuple_b = ctx.alloc_tuple(&[b, Term::small_int(2)]).expect("tuple b");
    let tuples = list_from_slice(&mut ctx, &[tuple_a, tuple_b]);
    assert_eq!(
        bif_lists_keyfind(&[b, Term::small_int(1), tuples], &mut ctx).expect("keyfind hit"),
        tuple_b
    );
    assert_eq!(
        bif_lists_keyfind(&[c, Term::small_int(1), tuples], &mut ctx).expect("keyfind miss"),
        Term::atom(Atom::FALSE)
    );
}

#[test]
fn list_transform_bifs_build_expected_results() {
    let mut process = Process::new(1, 1024);
    let mut ctx = context(&mut process);
    let unsorted = list_from_slice(
        &mut ctx,
        &[Term::small_int(3), Term::small_int(1), Term::small_int(2)],
    );
    let sorted = bif_lists_sort(&[unsorted], &mut ctx).expect("sort");
    assert_eq!(
        list_to_vec(sorted),
        vec![Term::small_int(1), Term::small_int(2), Term::small_int(3)]
    );

    let nested_inner = list_from_slice(&mut ctx, &[Term::small_int(3)]);
    let nested_mid = list_from_slice(&mut ctx, &[Term::small_int(2), nested_inner]);
    let nested = list_from_slice(&mut ctx, &[Term::small_int(1), nested_mid]);
    let flattened = bif_lists_flatten(&[nested], &mut ctx).expect("flatten");
    assert_eq!(
        list_to_vec(flattened),
        vec![Term::small_int(1), Term::small_int(2), Term::small_int(3)]
    );

    let left = list_from_slice(&mut ctx, &[Term::atom(Atom::OK), Term::atom(Atom::ERROR)]);
    let right = list_from_slice(&mut ctx, &[Term::small_int(1), Term::small_int(2)]);
    let zipped = bif_lists_zip(&[left, right], &mut ctx).expect("zip");
    let pairs = list_to_vec(zipped);
    assert_eq!(
        tuple_to_vec(pairs[0]),
        vec![Term::atom(Atom::OK), Term::small_int(1)]
    );
    assert_eq!(
        tuple_to_vec(pairs[1]),
        vec![Term::atom(Atom::ERROR), Term::small_int(2)]
    );

    let unzipped = bif_lists_unzip(&[zipped], &mut ctx).expect("unzip");
    let parts = tuple_to_vec(unzipped);
    assert_eq!(
        list_to_vec(parts[0]),
        vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)]
    );
    assert_eq!(
        list_to_vec(parts[1]),
        vec![Term::small_int(1), Term::small_int(2)]
    );
}

#[test]
fn key_manipulation_bifs_preserve_order_and_key_semantics() {
    let mut process = Process::new(1, 1024);
    let mut ctx = context(&mut process);
    let a = Term::atom(Atom::OK);
    let b = Term::atom(Atom::ERROR);
    let tuple_a = ctx.alloc_tuple(&[a, Term::small_int(1)]).expect("tuple a");
    let tuple_b = ctx.alloc_tuple(&[b, Term::small_int(2)]).expect("tuple b");
    let tuples = list_from_slice(&mut ctx, &[tuple_a, tuple_b]);
    let new_a = ctx
        .alloc_tuple(&[a, Term::small_int(99)])
        .expect("new tuple a");

    let stored =
        bif_lists_keystore(&[a, Term::small_int(1), tuples, new_a], &mut ctx).expect("keystore");
    assert_eq!(list_to_vec(stored), vec![new_a, tuple_b]);

    let deleted =
        bif_lists_keydelete(&[a, Term::small_int(1), tuples], &mut ctx).expect("keydelete");
    assert_eq!(list_to_vec(deleted), vec![tuple_b]);

    let keyed = list_from_slice(&mut ctx, &[tuple_b, tuple_a]);
    let sorted = bif_lists_keysort(&[Term::small_int(1), keyed], &mut ctx).expect("keysort");
    assert_eq!(list_to_vec(sorted), vec![tuple_b, tuple_a]);
}
