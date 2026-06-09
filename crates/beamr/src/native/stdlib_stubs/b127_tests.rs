use crate::atom::{Atom, AtomTable};
use crate::native::stdlib_stubs::lists_bifs::{
    bif_lists_flatten, bif_lists_keydelete, bif_lists_keyfind, bif_lists_keysort,
    bif_lists_keystore, bif_lists_last, bif_lists_member, bif_lists_nth, bif_lists_sort,
    bif_lists_unzip, bif_lists_zip,
};
use crate::native::stdlib_stubs::lists_hof_bifs::{
    bif_lists_filter, bif_lists_foreach, resume_lists_continuation,
};
use crate::native::stdlib_stubs::maps_bifs::{
    ContinuationStep, bif_maps_map, resume_maps_continuation,
};
use crate::native::{NativeContinuation, ProcessContext};
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Cons, Map, Tuple, write_closure};

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

fn closure(process: &mut Process, unique_id: u64) -> Term {
    let heap = process.heap_mut().alloc_slice(7).expect("closure heap");
    write_closure(heap, Atom::OK, 0, 1, 1, unique_id, &[]).expect("closure")
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

#[test]
fn list_query_bifs_reject_invalid_positions_and_empty_last() {
    let mut process = Process::new(1, 512);
    let mut ctx = context(&mut process);
    let list = list_from_slice(&mut ctx, &[Term::small_int(1)]);
    let short_tuple = ctx.alloc_tuple(&[Term::small_int(1)]).expect("tuple");
    let tuple_list = list_from_slice(&mut ctx, &[short_tuple]);

    assert_eq!(
        bif_lists_nth(&[Term::small_int(0), list], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
    assert_eq!(
        bif_lists_nth(&[Term::small_int(2), list], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
    assert_eq!(
        bif_lists_keyfind(
            &[Term::small_int(1), Term::small_int(2), tuple_list],
            &mut ctx
        ),
        Err(Term::atom(Atom::BADARG))
    );
    assert_eq!(
        bif_lists_last(&[Term::NIL], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
}

#[test]
fn transform_bifs_reject_improper_shapes() {
    let mut process = Process::new(1, 512);
    let mut ctx = context(&mut process);
    let one = list_from_slice(&mut ctx, &[Term::small_int(1)]);
    let two = list_from_slice(&mut ctx, &[Term::small_int(1), Term::small_int(2)]);
    let bad_pair = ctx
        .alloc_tuple(&[Term::small_int(1), Term::small_int(2), Term::small_int(3)])
        .expect("bad pair");
    let bad_pair_list = list_from_slice(&mut ctx, &[bad_pair]);

    assert_eq!(
        bif_lists_zip(&[one, two], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
    assert_eq!(
        bif_lists_flatten(&[Term::small_int(1)], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
    assert_eq!(
        bif_lists_unzip(&[bad_pair_list], &mut ctx),
        Err(Term::atom(Atom::BADARG))
    );
}

#[test]
fn filter_uses_continuation_trampoline() {
    let mut process = Process::new(1, 1024);
    let fun = closure(&mut process, 0x127);
    let mut ctx = context(&mut process);
    let list = list_from_slice(
        &mut ctx,
        &[Term::small_int(1), Term::small_int(2), Term::small_int(3)],
    );

    let placeholder = bif_lists_filter(&[fun, list], &mut ctx).expect("filter starts");
    assert_eq!(placeholder, Term::NIL);
    let request = ctx.take_trampoline().expect("filter trampoline");
    assert_eq!(request.fun, fun);
    assert_eq!(request.args, vec![Term::small_int(1)]);
    let Some(NativeContinuation::Lists(state)) = request.continuation else {
        panic!("expected lists continuation");
    };

    let next =
        resume_lists_continuation(state, Term::atom(Atom::TRUE), &mut ctx).expect("filter resumes");
    let ContinuationStep::Call {
        args, continuation, ..
    } = next
    else {
        panic!("expected next filter call");
    };
    assert_eq!(args, vec![Term::small_int(2)]);

    let next = resume_lists_continuation(
        match continuation {
            NativeContinuation::Lists(state) => state,
            _ => panic!("expected lists continuation"),
        },
        Term::atom(Atom::FALSE),
        &mut ctx,
    )
    .expect("filter resumes again");
    let ContinuationStep::Call { continuation, .. } = next else {
        panic!("expected final filter call");
    };
    let done = resume_lists_continuation(
        match continuation {
            NativeContinuation::Lists(state) => state,
            _ => panic!("expected lists continuation"),
        },
        Term::atom(Atom::TRUE),
        &mut ctx,
    )
    .expect("filter completes");
    let ContinuationStep::Done(result) = done else {
        panic!("expected filter done");
    };
    assert_eq!(
        list_to_vec(result),
        vec![Term::small_int(1), Term::small_int(3)]
    );
}

#[test]
fn maps_map_uses_continuation_trampoline_and_preserves_original() {
    let mut process = Process::new(1, 1024);
    let fun = closure(&mut process, 0x169);
    let mut ctx = context(&mut process);
    let keys = [Term::atom(Atom::OK), Term::atom(Atom::ERROR)];
    let values = [Term::small_int(1), Term::small_int(2)];
    let map_term = ctx.alloc_map(&keys, &values).expect("map");

    let placeholder = bif_maps_map(&[fun, map_term], &mut ctx).expect("map starts");
    assert_eq!(placeholder, Term::NIL);
    let request = ctx.take_trampoline().expect("map trampoline");
    assert_eq!(request.fun, fun);
    assert_eq!(request.args, vec![keys[0], values[0]]);
    let Some(NativeContinuation::Maps(state)) = request.continuation else {
        panic!("expected maps continuation");
    };

    let next = resume_maps_continuation(state, Term::small_int(2), &mut ctx).expect("map resumes");
    let ContinuationStep::Call {
        args, continuation, ..
    } = next
    else {
        panic!("expected next map call");
    };
    assert_eq!(args, vec![keys[1], values[1]]);

    let done = resume_maps_continuation(
        match continuation {
            NativeContinuation::Maps(state) => state,
            _ => panic!("expected maps continuation"),
        },
        Term::small_int(3),
        &mut ctx,
    )
    .expect("map completes");
    let ContinuationStep::Done(result) = done else {
        panic!("expected map done");
    };
    let mapped = Map::new(result).expect("mapped result");
    assert_eq!(mapped.get(keys[0]), Some(Term::small_int(2)));
    assert_eq!(mapped.get(keys[1]), Some(Term::small_int(3)));
    let original = Map::new(map_term).expect("original map");
    assert_eq!(original.get(keys[0]), Some(values[0]));
    assert_eq!(original.get(keys[1]), Some(values[1]));
}

#[test]
fn foreach_discards_results_and_finishes_with_ok() {
    let mut process = Process::new(1, 1024);
    let fun = closure(&mut process, 0x128);
    let mut ctx = context(&mut process);
    let list = list_from_slice(&mut ctx, &[Term::small_int(1), Term::small_int(2)]);

    assert_eq!(
        bif_lists_foreach(&[fun, list], &mut ctx).expect("foreach starts"),
        Term::NIL
    );
    let request = ctx.take_trampoline().expect("foreach trampoline");
    assert_eq!(request.args, vec![Term::small_int(1)]);
    let Some(NativeContinuation::Lists(state)) = request.continuation else {
        panic!("expected lists continuation");
    };
    let next =
        resume_lists_continuation(state, Term::small_int(99), &mut ctx).expect("foreach resumes");
    let ContinuationStep::Call {
        args, continuation, ..
    } = next
    else {
        panic!("expected second foreach call");
    };
    assert_eq!(args, vec![Term::small_int(2)]);
    let done = resume_lists_continuation(
        match continuation {
            NativeContinuation::Lists(state) => state,
            _ => panic!("expected lists continuation"),
        },
        Term::small_int(100),
        &mut ctx,
    )
    .expect("foreach completes");
    let ContinuationStep::Done(result) = done else {
        panic!("expected foreach done");
    };
    assert_eq!(result, Term::atom(Atom::OK));
}
