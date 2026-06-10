use super::*;
use crate::{
    process::{Monitor, ProcessStatus},
    term::boxed::{Reference as RefTerm, Tuple},
};

fn running(pid: u64) -> Process {
    let mut process = Process::new(pid, 64);
    process
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process starts: {error}"));
    process
}

fn down_tuple(process: &mut Process) -> Tuple {
    process.mailbox_mut().drain_arrival();
    Tuple::new(
        process
            .mailbox()
            .front_for_test()
            .unwrap_or_else(|| panic!("down message exists")),
    )
    .unwrap_or_else(|| panic!("down message is tuple"))
}

#[test]
fn monitor_returns_unique_references_and_down_message_without_killing_watcher() {
    let mut monitors = MonitorSet::new();
    let mut watcher = running(1);
    let mut target = running(2);
    let first = monitors.monitor(&mut watcher, &mut target);
    let second = monitors.monitor(&mut watcher, &mut target);

    assert_ne!(first, second);
    monitors.process_exited(
        target.pid(),
        ExitReason::Error,
        &mut [&mut watcher, &mut target],
    );

    assert_eq!(watcher.status(), ProcessStatus::Running);
    let tuple = down_tuple(&mut watcher);
    assert_eq!(tuple.arity(), 5);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::DOWN)));
    assert_eq!(
        tuple.get(1).and_then(RefTerm::new).map(RefTerm::id),
        Some(first)
    );
    assert_eq!(tuple.get(2), Some(Term::atom(Atom::PROCESS)));
    assert_eq!(tuple.get(3).and_then(Term::as_pid), Some(2));
    assert_eq!(tuple.get(4), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn collect_watchers_preserves_monitor_insertion_order() {
    let mut monitors = MonitorSet::new();
    let mut first_watcher = running(1);
    let mut second_watcher = running(2);
    let mut target = running(3);
    let first = monitors.monitor(&mut first_watcher, &mut target);
    let second = monitors.monitor(&mut second_watcher, &mut target);

    let watchers = monitors.collect_watchers_and_remove(target.pid(), ExitReason::Error);

    assert_eq!(watchers, vec![(1, first), (2, second)]);
}

#[test]
fn demonitor_preserves_remaining_monitor_order() {
    let mut monitors = MonitorSet::new();
    let mut first_watcher = running(1);
    let mut removed_watcher = running(2);
    let mut third_watcher = running(3);
    let mut target = running(4);
    let first = monitors.monitor(&mut first_watcher, &mut target);
    let removed = monitors.monitor(&mut removed_watcher, &mut target);
    let third = monitors.monitor(&mut third_watcher, &mut target);

    assert_eq!(
        monitors.demonitor(
            removed,
            &mut [
                &mut first_watcher,
                &mut removed_watcher,
                &mut third_watcher,
                &mut target
            ],
        ),
        Some(Monitor::new(removed, 2, 4))
    );
    let watchers = monitors.collect_watchers_and_remove(target.pid(), ExitReason::Error);

    assert_eq!(watchers, vec![(1, first), (3, third)]);
}

#[test]
fn demonitor_prevents_down_delivery() {
    let mut monitors = MonitorSet::new();
    let mut watcher = running(1);
    let mut target = running(2);
    let reference = monitors.monitor(&mut watcher, &mut target);

    assert_eq!(
        monitors.demonitor(reference, &mut [&mut watcher, &mut target]),
        Some(Monitor::new(reference, 1, 2))
    );
    monitors.process_exited(
        target.pid(),
        ExitReason::Error,
        &mut [&mut watcher, &mut target],
    );

    assert_eq!(watcher.status(), ProcessStatus::Running);
    assert!(watcher.mailbox().is_empty());
}

#[test]
fn monitor_dead_process_delivers_immediate_down() {
    let mut monitors = MonitorSet::new();
    let mut watcher = running(1);
    monitors.record_dead(2, ExitReason::Normal);

    let reference = monitors.monitor_pid(&mut watcher, 2);

    let tuple = down_tuple(&mut watcher);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::DOWN)));
    assert_eq!(
        tuple.get(1).and_then(RefTerm::new).map(RefTerm::id),
        Some(reference)
    );
    assert_eq!(tuple.get(3).and_then(Term::as_pid), Some(2));
    assert_eq!(tuple.get(4), Some(Term::atom(Atom::NORMAL)));
}

/// Allocate a 2-element tuple directly on the process nursery (no GC), so it
/// is a live young-generation object the next heap growth must preserve.
fn alloc_young_tuple(process: &mut Process, elements: &[Term]) -> Term {
    let ptr = process
        .heap_mut()
        .alloc(1 + elements.len())
        .unwrap_or_else(|_| panic!("young tuple fits"));
    // SAFETY: `alloc` returned `1 + elements.len()` writable young-heap words.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, 1 + elements.len()) };
    boxed::write_tuple(words, elements).unwrap_or_else(|| panic!("tuple writes"))
}

/// PR-6: delivering a DOWN message to a watcher whose nursery is near-full
/// must not abandon live young-generation terms. Before the fix the path
/// called `grow_to_next_capacity()` directly, replacing the nursery with a
/// fresh empty region without copying live data or rewriting roots, leaving
/// the tuple in X0 dangling. Routing through `gc::ensure_space` preserves it.
#[test]
fn down_on_near_full_heap_preserves_live_young_terms() {
    let mut monitors = MonitorSet::new();
    let mut watcher = Process::new(1, 16);
    watcher
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process starts: {error}"));
    monitors.record_dead(2, ExitReason::Error);

    let live = alloc_young_tuple(&mut watcher, &[Term::small_int(7), Term::small_int(8)]);
    watcher.set_x_reg(0, live);

    // Fill the remaining nursery so the 7-word DOWN message cannot fit
    // without growth, forcing the heap-growth path on delivery.
    while watcher.heap().available() >= 7 {
        let _ = watcher.heap_mut().alloc(1);
    }
    assert!(watcher.heap().available() < 7);

    let reference = monitors.monitor_pid(&mut watcher, 2);

    let recovered = Tuple::new(watcher.x_reg(0)).unwrap_or_else(|| panic!("X0 is still a tuple"));
    assert_eq!(recovered.arity(), 2);
    assert_eq!(recovered.get(0), Some(Term::small_int(7)));
    assert_eq!(recovered.get(1), Some(Term::small_int(8)));

    assert_eq!(watcher.status(), ProcessStatus::Running);
    let tuple = down_tuple(&mut watcher);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::DOWN)));
    assert_eq!(
        tuple.get(1).and_then(RefTerm::new).map(RefTerm::id),
        Some(reference)
    );
}

/// PR-7: the DOWN path must not panic when the watcher heap is exhausted and
/// cannot grow. Before the fix four `.expect()` calls turned an
/// unsatisfiable allocation into a VM-aborting panic; now the path returns an
/// error which `enqueue_down_message_pub` converts into terminating the
/// watcher with `Error` instead of panicking.
/// Build a process whose young capacity (4 words) is smaller than a 7-word
/// DOWN message and whose growth is forbidden, so the message can never be
/// satisfied even after minor GC promotes live data into old space — the
/// young region is too small to hold the message and `max_capacity` blocks
/// growth. This drives the formerly-`.expect()` allocations onto the error
/// path.
fn exhausted_watcher(pid: u64) -> Process {
    let mut watcher = Process::new(pid, 4);
    watcher
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process starts: {error}"));
    // Forbid any nursery growth: a 7-word message cannot fit a 4-word young
    // region, and `ensure_space` only grows young up to `max_capacity`.
    watcher.heap_mut().set_max_capacity(4);
    watcher
}

#[test]
fn down_delivery_without_growth_room_terminates_instead_of_panicking() {
    // Must not panic; the public delivery helper converts the error into
    // terminating the watcher with `Error`.
    let mut watcher = exhausted_watcher(1);
    enqueue_down_message_pub(&mut watcher, 0, 2, ExitReason::Error);
    assert_eq!(watcher.status(), ProcessStatus::Exited(ExitReason::Error));

    // The raw fallible path reports the failure rather than panicking.
    let mut victim = exhausted_watcher(3);
    assert!(enqueue_down_message(&mut victim, 0, 2, ExitReason::Error).is_err());
}

#[test]
fn down_delivery_grows_mailbox_heap_instead_of_dropping_signal() {
    let mut monitors = MonitorSet::new();
    let mut watcher = Process::new(1, 1);
    watcher
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process starts: {error}"));
    monitors.record_dead(2, ExitReason::Error);

    let reference = monitors.monitor_pid(&mut watcher, 2);

    assert_eq!(watcher.status(), ProcessStatus::Running);
    assert!(watcher.heap().capacity() >= 7);
    let tuple = down_tuple(&mut watcher);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::DOWN)));
    assert_eq!(
        tuple.get(1).and_then(RefTerm::new).map(RefTerm::id),
        Some(reference)
    );
    assert_eq!(tuple.get(4), Some(Term::atom(Atom::ERROR)));
}
