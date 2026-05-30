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
            let _ = enqueue_down_message(watcher, reference, target_pid, reason);
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
                    let _ = enqueue_down_message(watcher, reference, target_pid, reason);
                }
                if let Some(index) = process_index_by_pid(processes, target_pid) {
                    processes[index].remove_monitor(reference);
                }
            }
        }
    }

    fn allocate_reference(&mut self) -> Reference {
        let reference = self.next_reference;
        self.next_reference = self.next_reference.saturating_add(1);
        reference
    }
}

fn enqueue_down_message(
    watcher: &mut Process,
    reference: Reference,
    target_pid: u64,
    reason: ExitReason,
) -> Result<(), ()> {
    let reference_term = allocate_reference_term(watcher, reference)?;
    let elements = [
        Term::atom(Atom::DOWN),
        reference_term,
        Term::atom(Atom::PROCESS),
        Term::pid(target_pid),
        reason.as_term(),
    ];
    let words = watcher
        .heap_mut()
        .alloc(1 + elements.len())
        .map_err(|_| ())?;
    // SAFETY: `alloc` returned a live region with exactly the requested number
    // of words in the watcher heap, used only to initialize this tuple.
    let words = unsafe { std::slice::from_raw_parts_mut(words, 1 + elements.len()) };
    let message = boxed::write_tuple(words, &elements).ok_or(())?;
    watcher.mailbox_mut().push_owned(message);
    Ok(())
}

fn allocate_reference_term(watcher: &mut Process, reference: Reference) -> Result<Term, ()> {
    let words = watcher.heap_mut().alloc(2).map_err(|_| ())?;
    // SAFETY: `alloc` returned a two-word region in the watcher heap for the
    // boxed reference header and payload.
    let words = unsafe { std::slice::from_raw_parts_mut(words, 2) };
    boxed::write_reference(words, reference).ok_or(())
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
}
