//! Test-support helpers for scheduler integration tests.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::module::Module;
use crate::namespace::NamespaceId;
use crate::process::Process;
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::term::Term;

use super::{ProcessSlot, ScheduledProcess, Scheduler, lock_or_recover};

impl Scheduler {
    /// Enqueue an immediate atom message into a live process mailbox.
    #[must_use]
    pub fn enqueue_atom_message(&self, target_pid: u64, atom: Atom) -> bool {
        let delivered = self
            .with_process(target_pid, |process| {
                process.mailbox_mut().push_owned(Term::atom(atom));
                true
            })
            .unwrap_or(false);
        if delivered {
            self.wake_process(target_pid);
        }
        delivered
    }

    fn with_process<T>(&self, pid: u64, f: impl FnOnce(&mut Process) -> T) -> Option<T> {
        let entry = self.shared.process_bodies.get(&pid)?;
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(scheduled) => Some(f(&mut scheduled.0)),
            ProcessSlot::Executing(_) | ProcessSlot::Absent => None,
        }
    }

    /// Spawn an inert process without module code for host-side policy tests.
    pub fn spawn_test_process(&self, trap_exit: bool) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
        process.set_trap_exit(trap_exit);
        self.shared.process_bodies.insert(
            pid,
            Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
        );
        pid
    }

    /// Spawn an inert process pinned to a module in a namespace for policy tests.
    pub fn spawn_test_process_in(&self, namespace: NamespaceId, module: Arc<Module>) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
        process.set_namespace_id(namespace);
        process.set_current_module(module);
        self.shared.process_bodies.insert(
            pid,
            Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
        );
        pid
    }

    /// Spawn an inert process linked to a live parent for host-side policy tests.
    pub fn spawn_linked_test_process(
        &self,
        parent_pid: u64,
    ) -> Result<u64, crate::native::links::LinkError> {
        let Some(parent_entry) = self.shared.process_bodies.get(&parent_pid) else {
            return Err(crate::native::links::LinkError::NoProc);
        };
        let mut parent_slot = lock_or_recover(&parent_entry);
        let ProcessSlot::Present(ScheduledProcess(parent)) = &mut *parent_slot else {
            return Err(crate::native::links::LinkError::NoProc);
        };
        let child_pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);
        let mut child = Process::new(child_pid, DEFAULT_HEAP_SIZE);
        child.add_link(parent_pid);
        parent.add_link(child_pid);
        self.shared.process_bodies.insert(
            child_pid,
            Mutex::new(ProcessSlot::Present(ScheduledProcess(child))),
        );
        Ok(child_pid)
    }

    /// Return true when a term is queued in a live process mailbox.
    #[must_use]
    pub fn has_message(&self, target_pid: u64, expected: Term) -> Option<bool> {
        self.with_process(target_pid, |process| {
            process.mailbox_mut().drain_arrival();
            process
                .mailbox()
                .scan_iter()
                .any(|message| *message == expected)
        })
    }

    /// Return true when a trapped EXIT message from `source_pid` is queued.
    #[must_use]
    pub fn has_trapped_exit_message(&self, target_pid: u64, source_pid: u64) -> Option<bool> {
        self.with_process(target_pid, |process| {
            process.mailbox_mut().drain_arrival();
            process.mailbox().scan_iter().any(|message| {
                let Some(tuple) = crate::term::boxed::Tuple::new(*message) else {
                    return false;
                };
                tuple.arity() == 3
                    && tuple.get(0) == Some(Term::atom(Atom::EXIT))
                    && tuple.get(1) == Some(Term::pid(source_pid))
            })
        })
    }
}
