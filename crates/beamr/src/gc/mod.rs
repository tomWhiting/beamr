//! Garbage collection — each process cleans its own room.
//!
//! Per-process generational copying GC. Young generation (nursery) is collected
//! frequently; old generation is compacted rarely. Collection takes only
//! `&mut Process`, never a registry/table/scheduler lock, so collecting one
//! process cannot pause or mutate another process.
pub mod major;
pub mod minor;

use std::collections::{HashMap, VecDeque};
use std::fmt;

use crate::process::{Process, heap::HeapFull};
use crate::term::{
    Term,
    boxed::{BoxedHeader, BoxedTag},
};

/// Major-GC shrink threshold after full compaction.
pub const MAJOR_SHRINK_THRESHOLD: f64 = 0.25;

/// Result returned by GC entry points.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GcStats {
    /// Number of live objects copied during this collection.
    pub copied_objects: usize,
    /// Number of machine words copied during this collection.
    pub copied_words: usize,
    /// Young words used when the collection started.
    pub young_before: usize,
    /// Old words used when the collection started.
    pub old_before: usize,
    /// Young words used when the collection completed.
    pub young_after: usize,
    /// Old words used when the collection completed.
    pub old_after: usize,
}

impl GcStats {
    fn new(process: &Process) -> Self {
        Self {
            copied_objects: 0,
            copied_words: 0,
            young_before: process.heap().young_used(),
            old_before: process.heap().old_used(),
            young_after: process.heap().young_used(),
            old_after: process.heap().old_used(),
        }
    }

    fn finish(&mut self, process: &Process) {
        self.young_after = process.heap().young_used();
        self.old_after = process.heap().old_used();
    }

    pub(crate) fn record_copy(&mut self, words: usize) {
        self.copied_objects += 1;
        self.copied_words += words;
    }
}

/// GC/allocation error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GcError {
    /// Allocation still could not be satisfied after permitted GC/growth.
    HeapFull(HeapFull),
    /// Object header did not match any known boxed layout.
    InvalidObjectHeader(u64),
}

impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeapFull(error) => write!(f, "{error}"),
            Self::InvalidObjectHeader(header) => {
                write!(f, "invalid boxed object header word {header:#x}")
            }
        }
    }
}

impl std::error::Error for GcError {}

impl From<HeapFull> for GcError {
    fn from(error: HeapFull) -> Self {
        Self::HeapFull(error)
    }
}

pub(crate) type ForwardingMap = HashMap<usize, Term>;

/// Collect only the target process's nursery into old space.
pub fn collect_minor(process: &mut Process) -> Result<GcStats, GcError> {
    collect_minor_with_live(process, 256)
}

/// Collect only the target process's nursery using a live X-register prefix.
pub fn collect_minor_with_live(process: &mut Process, live_x: usize) -> Result<GcStats, GcError> {
    minor::collect(process, live_x)
}

/// Fully compact the target process heap into fresh old space.
pub fn collect_major(process: &mut Process) -> Result<GcStats, GcError> {
    major::collect(process)
}

/// Allocate in the process nursery, running per-process GC on HeapFull.
///
/// The policy is: try nursery allocation, minor collect and retry, grow the
/// nursery as needed, and run a full compaction only when promotion pressure
/// during minor GC requires old-space compaction. The function does not touch
/// any process except `process`.
pub fn alloc(process: &mut Process, words: usize) -> Result<*mut u64, GcError> {
    match process.heap_mut().alloc(words) {
        Ok(ptr) => return Ok(ptr),
        Err(_heap_full) => {}
    }

    ensure_space(process, words, 256)?;

    process.heap_mut().alloc(words).map_err(GcError::from)
}

/// Ensure `words` nursery words are available, collecting and growing if needed.
pub fn ensure_space(process: &mut Process, words: usize, live_x: usize) -> Result<(), GcError> {
    if process.heap().available() >= words {
        return Ok(());
    }

    match collect_minor_with_live(process, live_x) {
        Ok(_stats) => {}
        Err(GcError::HeapFull(_)) => {
            collect_major(process)?;
        }
        Err(error) => return Err(error),
    }

    if process.heap().available() >= words {
        return Ok(());
    }

    while process.heap().available() < words {
        process.heap_mut().grow_to_next_capacity_with_max()?;
    }
    Ok(())
}

pub(crate) fn new_stats(process: &Process) -> GcStats {
    GcStats::new(process)
}

pub(crate) fn finish_stats(stats: &mut GcStats, process: &Process) {
    stats.finish(process);
}

pub(crate) fn object_size(term: Term) -> Result<Option<usize>, GcError> {
    if term.is_list() {
        return Ok(Some(2));
    }

    if !term.is_boxed() {
        return Ok(None);
    }

    let Some(ptr) = term.heap_ptr() else {
        return Ok(None);
    };
    // SAFETY: boxed terms are constructed only from heap word pointers. GC calls
    // this before reclaiming source storage, while object headers are live.
    let header = unsafe { *ptr };
    let Some(_tag) = BoxedHeader::tag(header) else {
        return Err(GcError::InvalidObjectHeader(header));
    };
    Ok(Some(1 + BoxedHeader::size(header)))
}

pub(crate) fn term_from_ptr_like(original: Term, ptr: *const u64) -> Term {
    if original.is_list() {
        Term::list_ptr(ptr)
    } else {
        Term::boxed_ptr(ptr)
    }
}

pub(crate) fn rewrite_copied_object(
    term: Term,
    work_queue: &mut VecDeque<Term>,
    mut copy_term: impl FnMut(Term, &mut VecDeque<Term>) -> Result<Term, GcError>,
) -> Result<(), GcError> {
    let Some(ptr) = term.heap_ptr() else {
        return Ok(());
    };

    if term.is_list() {
        rewrite_word(ptr, 0, work_queue, &mut copy_term)?;
        rewrite_word(ptr, 1, work_queue, &mut copy_term)?;
        return Ok(());
    }

    let header = read_raw_word(ptr, 0);
    let Some(tag) = BoxedHeader::tag(header) else {
        return Err(GcError::InvalidObjectHeader(header));
    };

    match tag {
        BoxedTag::Tuple => {
            for offset in 1..=BoxedHeader::size(header) {
                rewrite_word(ptr, offset, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::Closure => {
            let num_free = read_raw_word(ptr, 4) as usize;
            for index in 0..num_free {
                rewrite_word(ptr, 5 + index, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::Map => {
            let len = read_raw_word(ptr, 1) as usize;
            for offset in 2..(2 + len * 2) {
                rewrite_word(ptr, offset, work_queue, &mut copy_term)?;
            }
        }
        BoxedTag::MatchContext => rewrite_word(ptr, 3, work_queue, &mut copy_term)?,
        BoxedTag::Float
        | BoxedTag::BigInt
        | BoxedTag::Reference
        | BoxedTag::Binary
        | BoxedTag::BinaryBuilder => {}
    }

    Ok(())
}

fn rewrite_word(
    ptr: *const u64,
    offset: usize,
    work_queue: &mut VecDeque<Term>,
    copy_term: &mut impl FnMut(Term, &mut VecDeque<Term>) -> Result<Term, GcError>,
) -> Result<(), GcError> {
    let field = Term::from_raw(read_raw_word(ptr, offset));
    let rewritten = copy_term(field, work_queue)?;
    if rewritten.raw() != field.raw() {
        write_raw_word(ptr, offset, rewritten.raw());
    }
    Ok(())
}

fn read_raw_word(ptr: *const u64, offset: usize) -> u64 {
    // SAFETY: caller provides a live copied object pointer and an offset within
    // the object's layout.
    unsafe { *ptr.add(offset) }
}

fn write_raw_word(ptr: *const u64, offset: usize, value: u64) {
    // SAFETY: copied objects live in this process's mutable heap during GC; no
    // aliases are used to read/write the same slot concurrently.
    unsafe { *(ptr as *mut u64).add(offset) = value }
}

#[cfg(test)]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{
        atom::Atom,
        term::boxed::{Cons, Tuple, write_cons, write_tuple},
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
                }
            }
        }
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
            .push_frame(Atom::OK, 0, 3)
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
            process.stack_mut().push_frame(Atom::OK, 0, 2).expect("frame fits");
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
}
