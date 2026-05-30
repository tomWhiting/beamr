//! Process table and PID assignment.
//!
//! The table maps unique, monotonically assigned PIDs to process ownership
//! handles. It is safe for scheduler threads to allocate and look up handles;
//! the process value itself remains owned by one scheduler at a time and is not
//! `Send` or `Sync`.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::process::Process;
use crate::process::heap::DEFAULT_HEAP_SIZE;

/// Ownership handle for a process table entry.
///
/// This brief stores metadata sufficient for concurrent PID lookup without
/// making [`Process`] itself thread-safe. Scheduler ownership transfer and run
/// queues are implemented by later scheduler briefs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ProcessHandle {
    pid: u64,
}

impl ProcessHandle {
    /// PID owned by this handle.
    #[must_use]
    pub const fn pid(self) -> u64 {
        self.pid
    }
}

/// Concurrent process table with monotonically increasing PIDs.
#[derive(Debug)]
pub struct ProcessTable {
    next_pid: AtomicU64,
    processes: DashMap<u64, ProcessHandle>,
}

impl ProcessTable {
    /// Create an empty process table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_pid: AtomicU64::new(0),
            processes: DashMap::new(),
        }
    }

    /// Spawn a fresh process entry, assigning the next PID.
    ///
    /// The process is constructed with the default 233-word heap to validate the
    /// lifecycle invariant for new process creation. The scheduler ownership
    /// model that stores/moves the non-`Send` process body is intentionally
    /// deferred; the concurrent table records a PID ownership handle.
    pub fn spawn(&self) -> u64 {
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        let process = Process::new(pid, DEFAULT_HEAP_SIZE);
        let handle = ProcessHandle { pid: process.pid() };
        self.processes.insert(pid, handle);
        pid
    }

    /// Get the ownership handle for `pid`, when the process is still live.
    #[must_use]
    pub fn get(&self, pid: u64) -> Option<ProcessHandle> {
        self.processes.get(&pid).map(|entry| *entry.value())
    }

    /// Remove `pid` from the process table on exit.
    pub fn remove(&self, pid: u64) -> Option<ProcessHandle> {
        self.processes.remove(&pid).map(|(_pid, handle)| handle)
    }

    /// Number of live process handles in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Returns true when the table contains no live process handles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessHandle, ProcessTable};

    #[test]
    fn new_table_is_empty() {
        let table = ProcessTable::new();

        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn spawn_assigns_sequential_pids_from_zero() {
        let table = ProcessTable::new();

        let first = table.spawn();
        let second = table.spawn();

        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert_ne!(first, second);
    }

    #[test]
    fn get_returns_handle_for_spawned_process() {
        let table = ProcessTable::new();

        let pid = table.spawn();

        assert_eq!(table.get(pid), Some(ProcessHandle { pid }));
    }

    #[test]
    fn get_returns_none_for_missing_process() {
        let table = ProcessTable::new();

        assert_eq!(table.get(99), None);
    }

    #[test]
    fn removed_pids_are_not_reused() {
        let table = ProcessTable::new();

        let first = table.spawn();
        assert_eq!(table.remove(first), Some(ProcessHandle { pid: first }));
        let second = table.spawn();

        assert_eq!(table.get(first), None);
        assert_eq!(second, 1);
        assert_eq!(table.get(second), Some(ProcessHandle { pid: second }));
    }
}
