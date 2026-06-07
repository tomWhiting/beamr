//! Full heap compaction.
//!
//! When old generation pressure requires it, copies all live young and old data
//! to fresh old space, defragmenting as a side effect. Both source generations
//! are reclaimed after every root and internal reference is updated.

use std::collections::VecDeque;

use crate::{
    gc::{
        ForwardingMap, GcError, GcStats, MAJOR_SHRINK_THRESHOLD, finish_stats, new_stats,
        object_size, release_all_refcounted_resources_in_compacted_sources,
        retain_refcounted_resource_arc, rewrite_copied_object, term_from_ptr_like,
    },
    process::{Process, heap::Heap},
    term::Term,
};

pub(crate) fn collect(process: &mut Process) -> Result<GcStats, GcError> {
    let mut stats = new_stats(process);
    let mut forwarding = ForwardingMap::new();
    let mut work_queue = VecDeque::new();
    let source_used = process.heap().total_used();
    let fresh_capacity = process
        .heap()
        .compacted_old_capacity_after_major(source_used, MAJOR_SHRINK_THRESHOLD);
    let mut fresh = process.heap().fresh_old_region(fresh_capacity);

    let mut roots = process.roots();
    for root in &mut roots {
        *root = copy_any_term(
            process,
            &mut fresh,
            *root,
            &mut forwarding,
            &mut work_queue,
            &mut stats,
        )?;
    }
    process.replace_roots(&roots);

    while let Some(term) = work_queue.pop_front() {
        rewrite_copied_object(term, &mut work_queue, |field, queue| {
            copy_any_term(
                process,
                &mut fresh,
                field,
                &mut forwarding,
                queue,
                &mut stats,
            )
        })?;
    }

    release_all_refcounted_resources_in_compacted_sources(process, |addr| {
        forwarding.contains_key(&addr)
    });
    process.heap_mut().replace_old(fresh);
    process.heap_mut().reset_young();
    finish_stats(&mut stats, process);
    Ok(stats)
}

fn copy_any_term(
    process: &mut Process,
    fresh: &mut crate::process::heap::HeapRegion,
    term: Term,
    forwarding: &mut ForwardingMap,
    work_queue: &mut VecDeque<Term>,
    stats: &mut GcStats,
) -> Result<Term, GcError> {
    let Some(src) = term.heap_ptr() else {
        return Ok(term);
    };
    if !process.heap().contains(src) {
        return Ok(term);
    }

    if let Some(forwarded) = forwarding.get(&src.addr()).copied() {
        return Ok(forwarded);
    }

    let Some(words) = object_size(term)? else {
        return Ok(term);
    };
    let copied_words = process.heap().copy_words_from_ptr(src, words);
    let dst = Heap::alloc_in_region(fresh, words)?;
    Heap::write_words(dst, &copied_words);
    let copied = term_from_ptr_like(term, dst.cast_const());
    if term.is_boxed() {
        retain_refcounted_resource_arc(dst.cast_const());
    }
    forwarding.insert(src.addr(), copied);
    work_queue.push_back(copied);
    stats.record_copy(words);
    Ok(copied)
}

#[cfg(test)]
mod gc_major_tests {
    use crate::{
        gc::{collect_major, collect_minor, tests::*},
        process::Process,
        term::Term,
    };

    #[test]
    fn nested_tuple_10_levels_deep_survives_major_gc() {
        let mut process = Process::new(1, 64);
        let mut term = Term::small_int(0);
        for level in 0..10 {
            term = alloc_tuple(&mut process, &[Term::small_int(level), term]);
        }
        process.set_x_reg(0, term);
        let expected = snapshot(term);

        collect_major(&mut process).expect("major GC succeeds");

        assert_eq!(snapshot(process.x_reg(0)), expected);
        assert_eq!(process.heap().young_used(), 0);
    }

    #[test]
    fn major_gc_reclaims_both_regions_and_tracks_live_utilization() {
        let mut process = Process::new(1, 64);
        let reachable = alloc_tuple(&mut process, &[Term::small_int(1)]);
        process.set_x_reg(0, reachable);
        collect_minor(&mut process).expect("promote reachable to old");
        let _unreachable_young = alloc_tuple(&mut process, &[Term::small_int(2)]);
        let _unreachable_old_words = process.heap_mut().alloc_old(4).expect("old has space");

        collect_major(&mut process).expect("major GC succeeds");

        assert_eq!(process.heap().young_used(), 0);
        assert_eq!(process.heap().old_used(), 2);
        assert_eq!(
            snapshot(process.x_reg(0)),
            Snapshot::Tuple(vec![Snapshot::Int(1)])
        );
    }

    #[test]
    fn major_gc_shrinks_underutilized_old_without_below_initial() {
        let mut process = Process::new(1, 233);
        process.heap_mut().grow_empty_old_to_for_test(987);
        let reachable = alloc_tuple(&mut process, &[Term::small_int(1)]);
        process.set_x_reg(0, reachable);

        collect_major(&mut process).expect("major GC succeeds");

        assert_eq!(process.heap().old_capacity(), 233);
    }
}
