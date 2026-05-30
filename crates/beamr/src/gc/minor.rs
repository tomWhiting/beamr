//! Nursery collection — young generation to old generation copy.
//!
//! Walks the root set (stack, registers, mailbox), copies reachable
//! young-generation terms to the old generation, and updates all pointers. The
//! nursery is then reclaimed wholesale. Existing old objects are not moved.

use std::collections::VecDeque;

use crate::{
    gc::{
        ForwardingMap, GcError, GcStats, finish_stats, new_stats, object_size,
        rewrite_copied_object, term_from_ptr_like,
    },
    process::Process,
    term::Term,
};

pub(crate) fn collect(process: &mut Process) -> Result<GcStats, GcError> {
    let mut stats = new_stats(process);
    let mut forwarding = ForwardingMap::new();
    let mut work_queue = VecDeque::new();

    let mut roots = process.roots();
    for root in &mut roots {
        *root = copy_young_term(process, *root, &mut forwarding, &mut work_queue, &mut stats)?;
    }
    process.replace_roots(&roots);

    while let Some(term) = work_queue.pop_front() {
        rewrite_copied_object(term, &mut work_queue, |field, queue| {
            copy_young_term(process, field, &mut forwarding, queue, &mut stats)
        })?;
    }

    process.heap_mut().reset_young();
    finish_stats(&mut stats, process);
    Ok(stats)
}

fn copy_young_term(
    process: &mut Process,
    term: Term,
    forwarding: &mut ForwardingMap,
    work_queue: &mut VecDeque<Term>,
    stats: &mut GcStats,
) -> Result<Term, GcError> {
    let Some(src) = term.heap_ptr() else {
        return Ok(term);
    };
    if !process.heap().young_contains(src) {
        return Ok(term);
    }

    if let Some(forwarded) = forwarding.get(&src.addr()).copied() {
        return Ok(forwarded);
    }

    let Some(words) = object_size(term)? else {
        return Ok(term);
    };
    if process.heap().old_available() < words {
        return Err(GcError::HeapFull(crate::process::heap::HeapFull::new(
            words,
            process.heap().old_available(),
        )));
    }
    let copied_words = process.heap().copy_words_from_ptr(src, words);
    let dst = process.heap_mut().alloc_old(words)?;
    crate::process::heap::Heap::write_words(dst, &copied_words);
    let copied = term_from_ptr_like(term, dst.cast_const());
    forwarding.insert(src.addr(), copied);
    work_queue.push_back(copied);
    stats.record_copy(words);
    Ok(copied)
}

#[cfg(test)]
mod gc_minor_tests {
    use crate::{
        gc::{collect_minor, tests::*},
        process::Process,
        term::Term,
    };

    #[test]
    fn cons_chain_of_1000_elements_survives_minor_gc() {
        let mut process = Process::new(1, 377);
        let mut list = Term::NIL;
        for value in (0..1000).rev() {
            list = alloc_cons(&mut process, Term::small_int(value), list);
        }
        process.set_x_reg(0, list);
        let expected = snapshot(list);

        collect_minor(&mut process).expect("minor GC succeeds");

        assert_eq!(snapshot(process.x_reg(0)), expected);
        assert_eq!(process.heap().young_used(), 0);
        assert_no_term_pointer_into_young(&process, process.x_reg(0));
    }

    #[test]
    fn tuple_containing_nested_tuple_is_updated_after_minor_gc() {
        let mut process = Process::new(1, 64);
        let inner = alloc_tuple(&mut process, &[Term::small_int(1)]);
        let outer = alloc_tuple(&mut process, &[inner, Term::small_int(2)]);
        process.set_x_reg(0, outer);
        let expected = snapshot(outer);

        collect_minor(&mut process).expect("minor GC succeeds");

        assert_eq!(snapshot(process.x_reg(0)), expected);
        assert_no_term_pointer_into_young(&process, process.x_reg(0));
    }
}
