//! Unidirectional monitor management.
//!
//! A monitor lets one process watch another without sharing fate.
//! When the monitored process exits, the monitoring process receives
//! a DOWN message with the exit reason. The monitored process is
//! unaware of the monitor. Monitors are identified by unique
//! references for cancellation.

use std::collections::HashMap;

#[cfg(feature = "net")]
use crate::distribution::control_monitor::RemotePid;
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
    monitors: Vec<Monitor>,
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
        self.monitors.push(monitor);
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
        self.monitors.push(monitor);
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
        let monitor = self.remove_monitor_entry(reference)?;
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
            if let Some(monitor) = self.remove_monitor_entry(reference) {
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
            if let Some(monitor) = self.remove_monitor_entry(reference) {
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
        self.monitors
            .iter()
            .find(|monitor| monitor.reference() == reference)
            .copied()
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
        if self.get_monitor(reference).is_some() {
            self.remove_monitor(reference);
        }
        self.monitors.push(monitor);
        self.by_target
            .entry(target_pid)
            .or_default()
            .push(reference);
    }

    /// Remove a monitor entry by reference. Used by the scheduler's supervision
    /// facility for demonitor operations.
    pub fn remove_monitor(&mut self, reference: Reference) {
        if let Some(monitor) = self.remove_monitor_entry(reference)
            && let Some(references) = self.by_target.get_mut(&monitor.target())
        {
            references.retain(|r| *r != reference);
            if references.is_empty() {
                self.by_target.remove(&monitor.target());
            }
        }
    }

    fn remove_monitor_entry(&mut self, reference: Reference) -> Option<Monitor> {
        let index = self
            .monitors
            .iter()
            .position(|monitor| monitor.reference() == reference)?;
        Some(self.monitors.remove(index))
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

/// Enqueue a DOWN message whose PID element is an external PID.
#[cfg(feature = "net")]
pub fn enqueue_remote_down_message_pub(
    watcher: &mut Process,
    reference: Reference,
    target: RemotePid,
    reason: ExitReason,
) {
    if enqueue_remote_down_message(watcher, reference, target, reason).is_err() {
        watcher.terminate(ExitReason::Error);
    }
}

#[cfg(feature = "net")]
fn enqueue_remote_down_message(
    watcher: &mut Process,
    reference: Reference,
    target: RemotePid,
    reason: ExitReason,
) -> Result<(), ()> {
    const DOWN_MESSAGE_WORDS: usize = 11;

    crate::gc::ensure_space(watcher, DOWN_MESSAGE_WORDS, 256).map_err(|_| ())?;

    let reference_term = {
        let reference_words = watcher.heap_mut().alloc_slice(2).map_err(|_| ())?;
        boxed::write_reference(reference_words, reference).ok_or(())?
    };
    let target_term = {
        let pid_words = watcher.heap_mut().alloc_slice(4).map_err(|_| ())?;
        boxed::write_external_pid(pid_words, target.node, target.pid_number, target.serial)
            .ok_or(())?
    };
    let elements = [
        Term::atom(Atom::DOWN),
        reference_term,
        Term::atom(Atom::PROCESS),
        target_term,
        reason.as_term(),
    ];
    let words = watcher
        .heap_mut()
        .alloc_slice(1 + elements.len())
        .map_err(|_| ())?;
    let message = boxed::write_tuple(words, &elements).ok_or(())?;
    watcher.mailbox_mut().push_owned(message);
    Ok(())
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

fn process_index_by_pid(processes: &[&mut Process], pid: u64) -> Option<usize> {
    processes.iter().position(|process| process.pid() == pid)
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
