//! Unidirectional monitor management.
//!
//! A monitor lets one process watch another without sharing fate.
//! When the monitored process exits, the monitoring process receives
//! a DOWN message with the exit reason. The monitored process is
//! unaware of the monitor. Monitors are identified by unique
//! references for cancellation.

use std::collections::HashMap;

use crate::{
    atom::Atom,
    process::{ExitReason, Monitor, Process},
    term::{Term, boxed},
};

/// Unique monitor reference id.
pub type Reference = u64;

/// Registry for unidirectional process monitors.
#[derive(Debug, Default)]
pub struct MonitorSet {
    next_reference: Reference,
    monitors: HashMap<Reference, Monitor>,
    by_target: HashMap<u64, Vec<Reference>>,
    dead: HashMap<u64, ExitReason>,
}

impl MonitorSet {
    /// Create an empty monitor registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a dead target reason for immediate DOWN delivery to future
    /// monitors of an already-exited process.
    pub fn record_dead(&mut self, pid: u64, reason: ExitReason) {
        self.dead.insert(pid, reason);
    }

    /// Register `watcher` as observing live `target`.
    pub fn monitor(&mut self, watcher: &mut Process, target: &mut Process) -> Reference {
        let reference = self.allocate_reference();
        let monitor = Monitor::new(reference, watcher.pid(), target.pid());
        watcher.add_monitor(monitor);
        target.add_monitor(monitor);
        self.monitors.insert(reference, monitor);
        self.by_target
            .entry(target.pid())
            .or_default()
            .push(reference);
        reference
    }

    /// Register `watcher` as observing a PID that may already be dead.
    pub fn monitor_pid(&mut self, watcher: &mut Process, target_pid: u64) -> Reference {
        let reference = self.allocate_reference();
        if let Some(reason) = self.dead.get(&target_pid).copied() {
            if enqueue_down_message(watcher, reference, target_pid, reason).is_err() {
                watcher.terminate(ExitReason::Error);
            }
            return reference;
        }

        let monitor = Monitor::new(reference, watcher.pid(), target_pid);
        watcher.add_monitor(monitor);
        self.monitors.insert(reference, monitor);
        self.by_target
            .entry(target_pid)
            .or_default()
            .push(reference);
        reference
    }

    /// Remove a monitor before target death notification.
    pub fn demonitor(
        &mut self,
        reference: Reference,
        processes: &mut [&mut Process],
    ) -> Option<Monitor> {
        let monitor = self.monitors.remove(&reference)?;
        if let Some(references) = self.by_target.get_mut(&monitor.target()) {
            references.retain(|seen| *seen != reference);
            if references.is_empty() {
                self.by_target.remove(&monitor.target());
            }
        }
        for process in processes.iter_mut() {
            process.remove_monitor(reference);
        }
        Some(monitor)
    }

    /// Notify watchers that `target_pid` exited.
    pub fn process_exited(
        &mut self,
        target_pid: u64,
        reason: ExitReason,
        processes: &mut [&mut Process],
    ) {
        self.record_dead(target_pid, reason);
        let references = self.by_target.remove(&target_pid).unwrap_or_default();
        for reference in references {
            if let Some(monitor) = self.monitors.remove(&reference) {
                if let Some(index) = process_index_by_pid(processes, monitor.watcher()) {
                    let watcher = &mut processes[index];
                    watcher.remove_monitor(reference);
                    if enqueue_down_message(watcher, reference, target_pid, reason).is_err() {
                        watcher.terminate(ExitReason::Error);
                    }
                }
                if let Some(index) = process_index_by_pid(processes, target_pid) {
                    processes[index].remove_monitor(reference);
                }
            }
        }
    }

    /// Collect all watcher (watcher_pid, reference) pairs for a target that has
    /// exited, and remove the monitor entries. Used by the scheduler's
    /// supervision integration which delivers DOWN messages through
    /// process_bodies directly.
    pub fn collect_watchers_and_remove(
        &mut self,
        target_pid: u64,
        reason: ExitReason,
    ) -> Vec<(u64, Reference)> {
        self.record_dead(target_pid, reason);
        let references = self.by_target.remove(&target_pid).unwrap_or_default();
        let mut watchers = Vec::with_capacity(references.len());
        for reference in references {
            if let Some(monitor) = self.monitors.remove(&reference) {
                watchers.push((monitor.watcher(), reference));
            }
        }
        watchers
    }

    /// Allocate a unique monitor reference. Used by the scheduler's supervision
    /// facility when creating monitors for already-dead targets.
    pub fn allocate_reference_pub(&mut self) -> Reference {
        self.allocate_reference()
    }

    /// Look up a monitor by reference without removing it.
    #[must_use]
    pub fn get_monitor(&self, reference: Reference) -> Option<crate::process::Monitor> {
        self.monitors.get(&reference).copied()
    }

    /// Register a pre-built monitor entry. Used by the scheduler's supervision
    /// facility when process bodies are managed via DashMap and cannot be passed
    /// as `&mut Process` simultaneously.
    pub fn register_monitor(
        &mut self,
        reference: Reference,
        monitor: crate::process::Monitor,
        target_pid: u64,
    ) {
        self.monitors.insert(reference, monitor);
        self.by_target
            .entry(target_pid)
            .or_default()
            .push(reference);
    }

    /// Remove a monitor entry by reference. Used by the scheduler's supervision
    /// facility for demonitor operations.
    pub fn remove_monitor(&mut self, reference: Reference) {
        if let Some(monitor) = self.monitors.remove(&reference)
            && let Some(references) = self.by_target.get_mut(&monitor.target())
        {
            references.retain(|r| *r != reference);
            if references.is_empty() {
                self.by_target.remove(&monitor.target());
            }
        }
    }

    fn allocate_reference(&mut self) -> Reference {
        let reference = self.next_reference;
        self.next_reference = self.next_reference.saturating_add(1);
        reference
    }
}

/// Enqueue a DOWN message on a watcher's mailbox. Public for scheduler
/// supervision integration which delivers DOWN messages through
/// process_bodies directly.
pub fn enqueue_down_message_pub(
    watcher: &mut Process,
    reference: Reference,
    target_pid: u64,
    reason: ExitReason,
) {
    if enqueue_down_message(watcher, reference, target_pid, reason).is_err() {
        watcher.terminate(ExitReason::Error);
    }
}

fn enqueue_down_message(
    watcher: &mut Process,
    reference: Reference,
    target_pid: u64,
    reason: ExitReason,
) -> Result<(), ()> {
    const DOWN_MESSAGE_WORDS: usize = 7;

    // Route heap growth through the GC: `ensure_space` runs minor/major
    // collection (moving live young-generation data into old space and
    // rewriting roots) BEFORE growing the nursery. Calling
    // `grow_to_next_capacity` directly here would swap in a fresh empty region
    // without copying live data, leaving registers, prior mailbox messages, and
    // stack slots dangling. See `gc::ensure_space` and `heap.rs` region invariant.
    crate::gc::ensure_space(watcher, DOWN_MESSAGE_WORDS, 256).map_err(|_| ())?;

    let reference_words = watcher.heap_mut().alloc(2).map_err(|_| ())?;
    // SAFETY: `alloc` returned a two-word region in the watcher heap for the
    // boxed reference header and payload.
    let reference_words = unsafe { std::slice::from_raw_parts_mut(reference_words, 2) };
    let reference_term = boxed::write_reference(reference_words, reference).ok_or(())?;
    let elements = [
        Term::atom(Atom::DOWN),
        reference_term,
        Term::atom(Atom::PROCESS),
        Term::pid(target_pid),
        reason.as_term(),
    ];
    let words = watcher.heap_mut().alloc(1 + elements.len()).map_err(|_| ())?;
    // SAFETY: `alloc` returned a live region with exactly the requested number
    // of words in the watcher heap, used only to initialize this tuple.
    let words = unsafe { std::slice::from_raw_parts_mut(words, 1 + elements.len()) };
    let message = boxed::write_tuple(words, &elements).ok_or(())?;
    watcher.mailbox_mut().push_owned(message);
    Ok(())
}

fn process_index_by_pid(processes: &[&mut Process], pid: u64) -> Option<usize> {
    processes.iter().position(|process| process.pid() == pid)
}

#[cfg(test)]
mod tests {
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

        let recovered =
            Tuple::new(watcher.x_reg(0)).unwrap_or_else(|| panic!("X0 is still a tuple"));
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
}
