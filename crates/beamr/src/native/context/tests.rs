use super::*;
use crate::atom::Atom;
use crate::process::Process;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Float, Map, Tuple};

fn heap_context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

fn assert_on_heap(heap: &crate::process::heap::Heap, term: Term) {
    let ptr = term.heap_ptr().expect("boxed/list term has heap pointer");
    assert!(heap.contains(ptr));
}

#[test]
fn allocation_helpers_write_valid_terms_on_process_heap() {
    let mut process = Process::new(1, 32);
    let tuple = {
        let mut context = heap_context(&mut process);
        let float = context.alloc_float(1.5).expect("float allocation");
        let binary = context.alloc_binary(b"beamr").expect("binary allocation");
        let list = context
            .alloc_list(&[Term::small_int(1), Term::small_int(2)])
            .expect("list allocation");
        let map = context
            .alloc_map(&[Term::atom(Atom::OK)], &[binary])
            .expect("map allocation");
        let bigint = context
            .alloc_bigint(false, &[u64::MAX])
            .expect("bigint allocation");
        let tuple = context
            .alloc_tuple(&[float, binary, list, map, bigint])
            .expect("tuple allocation");

        for term in [float, binary, list, map, bigint, tuple] {
            assert_on_heap(context.process_heap().expect("process heap"), term);
        }

        assert_eq!(Float::new(float).expect("float accessor").value(), 1.5);
        assert_eq!(
            Binary::new(binary).expect("binary accessor").as_bytes(),
            b"beamr"
        );
        let cons = Cons::new(list).expect("list accessor");
        assert_eq!(cons.head(), Term::small_int(1));
        assert_eq!(
            Map::new(map)
                .expect("map accessor")
                .get(Term::atom(Atom::OK)),
            Some(binary)
        );
        assert_eq!(Tuple::new(tuple).expect("tuple accessor").arity(), 5);
        tuple
    };
    assert_on_heap(process.heap(), tuple);
}

#[test]
fn detached_context_allocations_are_owned_until_taken() {
    let mut context = ProcessContext::new();
    let tuple = context
        .alloc_tuple(&[Term::atom(Atom::OK)])
        .expect("detached tuple allocation");
    assert_eq!(Tuple::new(tuple).expect("tuple accessor").arity(), 1);

    let owned = context
        .take_detached_result(tuple)
        .expect("detached allocation ownership");
    assert_eq!(owned.allocation_count(), 1);
    assert!(context.take_detached_result(Term::NIL).is_none());
}

#[test]
fn exception_class_defaults_sets_and_resets_to_error() {
    let mut context = ProcessContext::new();
    assert_eq!(context.take_exception_class(), ExceptionClass::Error);

    context.set_exception_class(ExceptionClass::Throw);
    assert_eq!(context.take_exception_class(), ExceptionClass::Throw);
    assert_eq!(context.take_exception_class(), ExceptionClass::Error);
}
