//! GC-pressure regression tests for native rooting.
//!
//! These tests force collections in the middle of BIF allocation sequences
//! with boxed (heap-pointer) terms live, which corrupted results before the
//! rooted-scope mechanism existed: x-registers above the BIF arity were not
//! GC roots, and accumulated `Vec<Term>` state was never traced.

use crate::atom::AtomTable;
use crate::native::{NativeContinuation, ProcessContext};
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Cons, Float, Tuple};

use super::lists_bifs::list_from_vec;
use super::lists_hof_bifs::ListsHofState;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(std::sync::Arc::new(AtomTable::with_common_atoms())));
    context.attach_process(process, 0);
    context
}

fn alloc_floats(ctx: &mut ProcessContext<'_>, count: usize) -> Vec<Term> {
    (0..count)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            ctx.alloc_float(index as f64 + 0.5).expect("float alloc")
        })
        .collect()
}

#[test]
fn list_from_vec_preserves_boxed_floats_under_gc_pressure() {
    // Small heap so the reserve inside list_from_vec must collect.
    let mut process = Process::new(1, 96);
    let mut ctx = context(&mut process);

    let floats = alloc_floats(&mut ctx, 24);
    let list = list_from_vec(&floats, &mut ctx).expect("list_from_vec");

    let mut current = list;
    let mut seen = 0usize;
    while !current.is_nil() {
        let cons = Cons::new(current).expect("cons cell");
        let float = Float::new(cons.head()).expect("element must still be a float");
        #[allow(clippy::cast_precision_loss)]
        let expected = seen as f64 + 0.5;
        assert!((float.value() - expected).abs() < f64::EPSILON);
        seen += 1;
        current = cons.tail();
    }
    assert_eq!(seen, 24);
}

#[test]
fn list_from_vec_handles_more_elements_than_x_registers() {
    // The previous implementation spilled elements into x-registers and
    // panicked past index 1023; the rooted scope has no such bound.
    let mut process = Process::new(1, 4096);
    let mut ctx = context(&mut process);

    let elements: Vec<Term> = (0..1500).map(Term::small_int).collect();
    let list = list_from_vec(&elements, &mut ctx).expect("large list");

    let mut current = list;
    let mut seen = 0i64;
    while !current.is_nil() {
        let cons = Cons::new(current).expect("cons cell");
        assert_eq!(cons.head().as_small_int(), Some(seen));
        seen += 1;
        current = cons.tail();
    }
    assert_eq!(seen, 1500);
}

#[test]
fn alloc_tuple_roots_boxed_arguments_across_gc() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);

    let a = ctx.alloc_float(1.25).expect("float");
    let b = ctx.alloc_float(2.5).expect("float");
    // Force pressure: this tuple alloc must reserve and may collect, moving
    // a and b. The allocator roots them internally.
    let tuple = ctx.alloc_tuple(&[a, b]).expect("tuple");

    let tuple = Tuple::new(tuple).expect("tuple term");
    let a = Float::new(tuple.get(0).expect("a")).expect("a is float");
    let b = Float::new(tuple.get(1).expect("b")).expect("b is float");
    assert!((a.value() - 1.25).abs() < f64::EPSILON);
    assert!((b.value() - 2.5).abs() < f64::EPSILON);
}

#[test]
fn rooted_push_accumulation_survives_gc() {
    let mut process = Process::new(1, 96);
    let mut ctx = context(&mut process);

    let result = ctx.with_rooted(&[], |ctx, roots| {
        for index in 0..16 {
            #[allow(clippy::cast_precision_loss)]
            let float = ctx.alloc_float(index as f64)?;
            ctx.rooted_push(roots, float)?;
        }
        (0..ctx.rooted_len(roots))
            .map(|index| ctx.rooted(roots, index))
            .collect::<Result<Vec<_>, _>>()
    });

    let values = result.expect("rooted accumulation");
    for (index, term) in values.iter().enumerate() {
        let float = Float::new(*term).expect("accumulated float survives");
        #[allow(clippy::cast_precision_loss)]
        let expected = index as f64;
        assert!((float.value() - expected).abs() < f64::EPSILON);
    }
}

#[test]
fn native_continuation_terms_are_gc_roots() {
    let mut process = Process::new(1, 96);

    // Build continuation state holding boxed floats, as a lists:map
    // trampoline does between closure calls.
    let (fun, remaining, results) = {
        let mut ctx = context(&mut process);
        let fun = ctx.alloc_float(9.75).expect("stand-in fun term");
        let remaining = alloc_floats(&mut ctx, 4);
        let results = alloc_floats(&mut ctx, 3);
        (fun, remaining, results)
    };
    process.set_native_continuation(Some(NativeContinuation::Lists(ListsHofState::Map {
        fun,
        remaining: remaining.clone(),
        results: results.clone(),
    })));

    // Force a full collection cycle with no live x registers.
    crate::gc::ensure_space(&mut process, 64, 0).expect("gc");

    let continuation = process
        .take_native_continuation()
        .expect("continuation survives");
    let NativeContinuation::Lists(ListsHofState::Map {
        fun,
        remaining,
        results,
    }) = continuation
    else {
        panic!("continuation shape preserved");
    };
    let fun = Float::new(fun).expect("fun term forwarded");
    assert!((fun.value() - 9.75).abs() < f64::EPSILON);
    assert_eq!(remaining.len(), 4);
    assert_eq!(results.len(), 3);
    for term in remaining.iter().chain(results.iter()) {
        let _ = Float::new(*term).expect("continuation element forwarded");
    }
}
