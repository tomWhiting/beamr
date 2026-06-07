use std::collections::HashMap;
use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::{
    atom::Atom,
    constant_pool::materialise_literals,
    loader::{Instruction, Literal},
    module::Module,
    term::{
        boxed::{Closure, Cons, ProcBin, Tuple, write_closure, write_cons, write_tuple},
        shared_binary::{SharedBinary, write_proc_bin},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Snapshot {
    Int(i64),
    Atom(Atom),
    Nil,
    Tuple(Vec<Snapshot>),
    List(Vec<Snapshot>),
    Other,
}

pub(crate) fn module_pin(name: Atom) -> Arc<Module> {
    Arc::new(Module {
        name,
        generation: 0,
        exports: HashMap::new(),
        label_index: HashMap::from([(1, 0)]),
        code: vec![Instruction::Label { label: 1 }],
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
        line_info: Vec::new(),
    })
}

pub(crate) fn alloc_tuple(process: &mut Process, elements: &[Term]) -> Term {
    let ptr = alloc(process, 1 + elements.len()).expect("tuple allocation via GC should fit");
    // SAFETY: GC allocation returned `1 + elements.len()` writable words.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, 1 + elements.len()) };
    write_tuple(words, elements).expect("tuple writer should fit allocated words")
}

pub(crate) fn alloc_cons(process: &mut Process, head: Term, tail: Term) -> Term {
    let ptr = alloc(process, 2).expect("cons allocation via GC should fit");
    // SAFETY: GC allocation returned two writable words.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, 2) };
    write_cons(words, head, tail).expect("cons writer should fit allocated words")
}

pub(crate) fn alloc_closure(
    process: &mut Process,
    generation: u64,
    unique_id: u64,
    free_vars: &[Term],
) -> Term {
    let words_count = 7 + free_vars.len();
    let ptr = alloc(process, words_count).expect("closure allocation via GC should fit");
    // SAFETY: GC allocation returned `words_count` writable words.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, words_count) };
    write_closure(words, Atom::OK, 0, 0, generation, unique_id, free_vars)
        .expect("closure writer should fit allocated words")
}

pub(crate) fn alloc_proc_bin(process: &mut Process, shared: &SharedBinary) -> Term {
    let ptr = alloc(process, 3).expect("proc bin allocation via GC should fit");
    // SAFETY: GC allocation returned three writable words.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, 3) };
    write_proc_bin(words, shared).expect("proc bin writer should fit allocated words")
}

pub(crate) fn snapshot(term: Term) -> Snapshot {
    if let Some(value) = term.as_small_int() {
        return Snapshot::Int(value);
    }
    if let Some(atom) = term.as_atom() {
        return Snapshot::Atom(atom);
    }
    if term.is_nil() {
        return Snapshot::Nil;
    }
    if let Some(tuple) = Tuple::new(term) {
        return Snapshot::Tuple(
            (0..tuple.arity())
                .filter_map(|i| tuple.get(i))
                .map(snapshot)
                .collect(),
        );
    }
    if term.is_list() {
        let mut values = Vec::new();
        let mut tail = term;
        while tail.is_list() {
            let Some(cons) = Cons::new(tail) else {
                return Snapshot::Other;
            };
            values.push(snapshot(cons.head()));
            tail = cons.tail();
        }
        if tail.is_nil() {
            return Snapshot::List(values);
        }
    }
    Snapshot::Other
}

pub(crate) fn assert_no_reachable_pointer_into_young(process: &Process) {
    for term in process.x_regs() {
        assert_no_term_pointer_into_young(process, *term);
    }
    for term in process.mailbox().scan_iter() {
        assert_no_term_pointer_into_young(process, *term);
    }
}

pub(crate) fn assert_no_term_pointer_into_young(process: &Process, term: Term) {
    let mut stack = vec![term];
    while let Some(current) = stack.pop() {
        if let Some(ptr) = current.heap_ptr() {
            assert!(!process.heap().young_contains(ptr));
            if current.is_list() {
                let cons = Cons::new(current).expect("valid cons");
                stack.push(cons.head());
                stack.push(cons.tail());
            } else if let Some(tuple) = Tuple::new(current) {
                for index in 0..tuple.arity() {
                    if let Some(element) = tuple.get(index) {
                        stack.push(element);
                    }
                }
            } else if let Some(closure) = Closure::new(current) {
                for index in 0..closure.num_free() {
                    if let Some(free_var) = closure.free_var(index) {
                        stack.push(free_var);
                    }
                }
            }
        }
    }
}

#[test]
fn constant_pool_terms_remain_external_roots_across_gc() {
    let literals = vec![Literal::Tuple(vec![
        Literal::Integer(1),
        Literal::Integer(2),
    ])];
    let pool = materialise_literals(&literals, None).expect("constant pool");
    let literal = pool.get(0).expect("literal root");
    let literal_ptr = literal.heap_ptr();
    let mut process = Process::new(1, 32);
    process.set_x_reg(0, literal);

    collect_minor(&mut process).expect("minor GC succeeds");
    assert_eq!(process.x_reg(0).raw(), literal.raw());
    assert_eq!(process.x_reg(0).heap_ptr(), literal_ptr);

    collect_major(&mut process).expect("major GC succeeds");
    assert_eq!(process.x_reg(0).raw(), literal.raw());
    assert_eq!(process.x_reg(0).heap_ptr(), literal_ptr);
    assert_eq!(
        snapshot(process.x_reg(0)),
        Snapshot::Tuple(vec![Snapshot::Int(1), Snapshot::Int(2)])
    );
}

#[test]
fn constant_pool_terms_nested_in_process_heap_are_not_copied() {
    let literals = vec![Literal::List(
        vec![Literal::Integer(7)],
        Box::new(Literal::Nil),
    )];
    let pool = materialise_literals(&literals, None).expect("constant pool");
    let literal = pool.get(0).expect("literal root");
    let literal_ptr = literal.heap_ptr();
    let mut process = Process::new(1, 32);
    let tuple = alloc_tuple(&mut process, &[literal]);
    process.set_x_reg(0, tuple);

    collect_minor(&mut process).expect("minor GC succeeds");
    let tuple = Tuple::new(process.x_reg(0)).expect("tuple after minor");
    assert_eq!(tuple.get(0).expect("literal field").raw(), literal.raw());
    assert_eq!(tuple.get(0).expect("literal field").heap_ptr(), literal_ptr);

    collect_major(&mut process).expect("major GC succeeds");
    let tuple = Tuple::new(process.x_reg(0)).expect("tuple after major");
    assert_eq!(tuple.get(0).expect("literal field").raw(), literal.raw());
    assert_eq!(tuple.get(0).expect("literal field").heap_ptr(), literal_ptr);
}

#[test]
fn gc_process_isolation_does_not_touch_other_process() {
    let mut process_a = Process::new(1, 8);
    let mut process_b = Process::new(2, 8);
    let b_term = alloc_tuple(&mut process_b, &[Term::small_int(99)]);
    process_b.set_x_reg(0, b_term);
    let b_young_used = process_b.heap().young_used();
    let b_old_used = process_b.heap().old_used();
    let b_root = process_b.x_reg(0);

    let a_term = alloc_tuple(&mut process_a, &[Term::small_int(1)]);
    process_a.set_x_reg(0, a_term);
    collect_minor(&mut process_a).expect("minor GC succeeds");

    assert_eq!(process_b.heap().young_used(), b_young_used);
    assert_eq!(process_b.heap().old_used(), b_old_used);
    assert_eq!(process_b.x_reg(0).raw(), b_root.raw());
    assert_eq!(
        snapshot(process_b.x_reg(0)),
        Snapshot::Tuple(vec![Snapshot::Int(99)])
    );
}

#[test]
fn gc_triggered_allocation_reclaims_empty_nursery_without_growth() {
    let mut process = Process::new(1, 233);
    let _ptr = process.heap_mut().alloc(233).expect("fill nursery");

    let ptr = alloc(&mut process, 1).expect("GC allocation should collect then allocate");

    assert!(process.heap().young_contains(ptr));
    assert_eq!(process.heap().capacity(), 233);
    assert_eq!(process.heap().young_used(), 1);
}

#[test]
fn ensure_space_grows_with_fibonacci_policy_when_needed() {
    let mut process = Process::new(1, 233);

    ensure_space(&mut process, 300, 0).expect("growth below max succeeds");

    assert_eq!(process.heap().capacity(), 377);
    assert!(process.heap().available() >= 300);
}

#[test]
fn ensure_space_reports_heap_full_when_growth_exceeds_max() {
    let mut process = Process::new(1, 8);
    process.heap_mut().set_max_capacity(8);

    let error = ensure_space(&mut process, 9, 0).expect_err("growth above max fails");

    assert!(matches!(error, GcError::HeapFull(_)));
    assert_eq!(process.heap().capacity(), 8);
}

#[test]
fn mixed_x_y_and_mailbox_roots_survive_minor_gc() {
    let mut process = Process::new(1, 32);
    let x_term = alloc_tuple(&mut process, &[Term::small_int(5)]);
    let y_term = alloc_tuple(&mut process, &[Term::small_int(6)]);
    let mail_term = alloc_tuple(&mut process, &[Term::small_int(7)]);
    process.set_x_reg(5, x_term);
    process
        .stack_mut()
        .push_frame(Atom::OK, 0, module_pin(Atom::OK), 3)
        .expect("frame fits");
    process.stack_mut().set_y_reg(2, y_term).expect("Y2 exists");
    process.mailbox_mut().push_owned_for_test(mail_term);
    let expected = [snapshot(x_term), snapshot(y_term), snapshot(mail_term)];

    collect_minor(&mut process).expect("minor GC succeeds");

    assert_eq!(snapshot(process.x_reg(5)), expected[0]);
    assert_eq!(
        snapshot(process.stack().y_reg(2).expect("Y2 exists")),
        expected[1]
    );
    assert_eq!(
        snapshot(process.mailbox().front_for_test().expect("mailbox root")),
        expected[2]
    );
    assert_no_reachable_pointer_into_young(&process);
}

#[test]
fn closure_metadata_and_free_vars_survive_minor_gc() {
    let mut process = Process::new(1, 32);
    let captured = alloc_tuple(&mut process, &[Term::small_int(42)]);
    let closure = alloc_closure(&mut process, 7, 0x1234_5678, &[captured]);
    process.set_x_reg(0, closure);

    collect_minor(&mut process).expect("minor GC succeeds");

    let closure = Closure::new(process.x_reg(0)).expect("closure root");
    assert_eq!(closure.module(), Some(Atom::OK));
    assert_eq!(closure.function_index(), 0);
    assert_eq!(closure.arity(), 0);
    assert_eq!(closure.num_free(), 1);
    assert_eq!(closure.generation(), 7);
    assert_eq!(closure.unique_id(), 0x1234_5678);
    let free_var = closure.free_var(0).expect("captured term");
    assert_eq!(snapshot(free_var), Snapshot::Tuple(vec![Snapshot::Int(42)]));
    assert_no_term_pointer_into_young(&process, process.x_reg(0));
}

#[test]
fn proc_bin_survives_minor_gc_as_leaf_with_shared_bytes() {
    let shared = SharedBinary::new(vec![0xCD; 100 * 1024]);
    let mut process = Process::new(1, 32);
    let proc_bin = alloc_proc_bin(&mut process, &shared);
    process.set_x_reg(0, proc_bin);
    assert_eq!(shared.ref_count(), 2);

    collect_minor(&mut process).expect("minor GC succeeds");

    // B-072's GC stub retains the copied ProcBin's Arc reference; the source
    // nursery reference is intentionally leaked until B-075 adds ProcBin sweep.
    assert_eq!(shared.ref_count(), 3);
    let proc_bin = ProcBin::new(process.x_reg(0)).expect("proc bin root");
    assert_eq!(proc_bin.len(), 100 * 1024);
    assert_eq!(proc_bin.as_bytes(), shared.as_bytes());
    assert_no_term_pointer_into_young(&process, process.x_reg(0));
}

#[test]
fn exception_roots_are_rewritten_after_minor_gc() {
    let mut process = Process::new(1, 32);
    let reason = alloc_tuple(&mut process, &[Term::small_int(8)]);
    let stacktrace = alloc_tuple(&mut process, &[Term::small_int(9)]);
    let expected_reason = snapshot(reason);
    let expected_stacktrace = snapshot(stacktrace);
    process.set_current_exception(Some(crate::process::Exception {
        class: Term::atom(Atom::ERROR),
        reason,
        stacktrace,
    }));

    collect_minor(&mut process).expect("minor GC succeeds");

    let exception = process
        .current_exception()
        .expect("exception should remain installed");
    assert_eq!(snapshot(exception.reason), expected_reason);
    assert_eq!(snapshot(exception.stacktrace), expected_stacktrace);
    assert_no_term_pointer_into_young(&process, exception.reason);
    assert_no_term_pointer_into_young(&process, exception.stacktrace);
}

#[test]
fn dictionary_heap_terms_survive_minor_gc() {
    let mut process = Process::new(1, 32);
    let key = alloc_tuple(&mut process, &[Term::small_int(10)]);
    let value = alloc_tuple(&mut process, &[Term::small_int(11)]);
    let expected_key = snapshot(key);
    let expected_value = snapshot(value);

    process.dict_put(key, value);

    collect_minor(&mut process).expect("minor GC succeeds");

    let entries = process.dict_get_all();
    assert_eq!(entries.len(), 1);
    let (key, value) = entries[0];
    assert_eq!(snapshot(key), expected_key);
    assert_eq!(snapshot(value), expected_value);
    assert_no_term_pointer_into_young(&process, key);
    assert_no_term_pointer_into_young(&process, value);
}

#[test]
fn dictionary_immediate_terms_survive_minor_gc_unchanged() {
    let mut process = Process::new(1, 32);
    let key = Term::atom(Atom::OK);
    let value = Term::small_int(12);

    process.dict_put(key, value);
    let _garbage = alloc_tuple(&mut process, &[Term::small_int(13)]);

    collect_minor(&mut process).expect("minor GC succeeds");

    assert_eq!(process.dict_get_all(), &[(key, value)]);
    assert_eq!(process.dict_get(key), value);
}

#[test]
fn unreachable_young_terms_are_reclaimed() {
    let mut process = Process::new(1, 32);
    let reachable = alloc_tuple(&mut process, &[Term::small_int(1)]);
    let _unreachable = alloc_tuple(&mut process, &[Term::small_int(2)]);
    process.set_x_reg(0, reachable);

    collect_minor(&mut process).expect("minor GC succeeds");

    assert_eq!(process.heap().young_used(), 0);
    assert_eq!(process.heap().old_used(), 2);
    assert_eq!(
        snapshot(process.x_reg(0)),
        Snapshot::Tuple(vec![Snapshot::Int(1)])
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn gc_property_random_acyclic_terms_survive(seed in 0u64..10_000) {
        let mut process = Process::new(1, 377);
        let mut terms = vec![Term::small_int(seed as i64), Term::atom(Atom::OK), Term::NIL];
        for index in 0..24usize {
            let left = terms[(seed as usize + index) % terms.len()];
            let right = terms[(seed as usize + index * 7 + 1) % terms.len()];
            let next = if index % 3 == 0 {
                alloc_cons(&mut process, left, right)
            } else {
                alloc_tuple(&mut process, &[left, right, Term::small_int(index as i64)])
            };
            terms.push(next);
        }
        process.set_x_reg(0, terms[terms.len() - 1]);
        process
            .stack_mut()
            .push_frame(Atom::OK, 0, module_pin(Atom::OK), 2)
            .expect("frame fits");
        process.stack_mut().set_y_reg(0, terms[terms.len() / 2]).expect("Y0 exists");
        process.mailbox_mut().push_owned_for_test(terms[terms.len() / 3]);
        let expected_x = snapshot(process.x_reg(0));
        let expected_y = snapshot(process.stack().y_reg(0).expect("Y0 exists"));
        let expected_mail = snapshot(process.mailbox().front_for_test().expect("mailbox root"));

        if seed % 2 == 0 {
            collect_minor(&mut process).expect("minor GC succeeds");
        } else {
            collect_major(&mut process).expect("major GC succeeds");
        }

        prop_assert_eq!(snapshot(process.x_reg(0)), expected_x);
        prop_assert_eq!(snapshot(process.stack().y_reg(0).expect("Y0 exists")), expected_y);
        prop_assert_eq!(snapshot(process.mailbox().front_for_test().expect("mailbox root")), expected_mail);
    }
}
