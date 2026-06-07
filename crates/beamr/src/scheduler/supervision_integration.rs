//! Supervision integration for the scheduler — exit signal propagation
//! through links and DOWN message delivery through monitors.
//!
//! Extracted from `mod.rs` to keep per-file line counts within the project
//! constraint (500 lines).

use std::{collections::VecDeque, sync::Arc};

use crate::atom::Atom;
use crate::namespace::NamespaceId;
use crate::native::links::{LinkError, LinkFacility};
use crate::native::spawn::{SpawnError, SpawnFacility};
use crate::native::supervision::{MonitorResult, SupervisionError, SupervisionFacility};
use crate::process::{ExitReason, ProcessStatus};
use crate::supervision::link;
use crate::supervision::monitor;
use crate::term::Term;

use super::{
    ScheduledProcess, SharedState, cleanup_exited_process, lock_or_recover, namespace_registry,
    wake_process,
};

/// Propagate exit signals through links and deliver DOWN messages through
/// monitors when a process exits. Uses a worklist pattern to handle cascade
/// deaths iteratively rather than recursively.
pub(super) fn propagate_exit(shared: &SharedState, pid: u64, reason: ExitReason) {
    // Collect linked PIDs from the exiting process.
    let linked_pids = take_links_from(shared, pid);
    let terminal_reason = link::terminal_reason(reason);

    // Deliver DOWN messages to all monitors of this process.
    deliver_down_messages(shared, pid, reason);

    // Mark dead in link_set for future link_pid() calls.
    {
        let mut ls = lock_or_recover(&shared.link_set);
        ls.process_exited_tombstone(pid, terminal_reason);
    }

    // Process link cascade with worklist pattern.
    // The signal sent through links is always `terminal_reason`: Kill becomes
    // Killed, matching BEAM semantics where only a direct exit signal is
    // untrappable — propagation through links always uses the terminal reason.
    let mut worklist: VecDeque<(u64, u64, ExitReason)> = linked_pids
        .into_iter()
        .map(|linked_pid| (pid, linked_pid, terminal_reason))
        .collect();
    while let Some((source_pid, target_pid, signal_reason)) = worklist.pop_front() {
        let cascade = process_exit_signal(shared, source_pid, target_pid, signal_reason);
        worklist.extend(cascade);
    }
}

/// Take the link set from an exiting process. The process body may already
/// have been removed (or may not exist in process_bodies), so handle None.
fn take_links_from(shared: &SharedState, pid: u64) -> Vec<u64> {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(process)) = slot.as_mut() {
            return process.take_links();
        }
    }
    Vec::new()
}

/// Deliver a single exit signal to a linked process. Returns any cascade
/// entries (source_pid, linked_pid, reason) for processes that must also die.
fn process_exit_signal(
    shared: &SharedState,
    source_pid: u64,
    target_pid: u64,
    reason: ExitReason,
) -> Vec<(u64, u64, ExitReason)> {
    let Some(entry) = shared.process_bodies.get(&target_pid) else {
        return Vec::new();
    };
    let mut slot = lock_or_recover(&entry);
    let Some(ScheduledProcess(target)) = slot.as_mut() else {
        // Process body is taken by a scheduler thread for execution.
        // Record a tombstone so the scheduler discards the process
        // after the current slice and propagates exit to its links.
        if reason == ExitReason::Kill || reason != ExitReason::Normal {
            let propagated_reason = link::terminal_reason(reason);
            shared.exit_tombstones.insert(target_pid, propagated_reason);
        }
        return Vec::new();
    };

    // Already exited? Nothing to do.
    if matches!(target.status(), ProcessStatus::Exited(_)) {
        return Vec::new();
    }

    // Remove the reverse link.
    target.remove_link(source_pid);

    let should_die =
        reason == ExitReason::Kill || (reason != ExitReason::Normal && !target.trap_exit());

    if should_die {
        // Kill signal bypasses trap_exit and propagates as 'killed'.
        let propagated_reason = link::terminal_reason(reason);
        if reason == ExitReason::Kill {
            target.set_trap_exit(false);
        }

        // Collect this process's links for cascade before terminating.
        let cascade_links: Vec<u64> = target
            .take_links()
            .into_iter()
            .filter(|linked_pid| *linked_pid != source_pid)
            .collect();

        target.terminate(propagated_reason);

        // Record tombstone.
        shared.exit_tombstones.insert(target_pid, propagated_reason);
        {
            let mut ls = lock_or_recover(&shared.link_set);
            ls.process_exited_tombstone(target_pid, propagated_reason);
        }

        // Deliver DOWN messages for monitors on the cascaded process.
        // Must drop the slot lock first to avoid deadlock.
        drop(slot);
        drop(entry);
        deliver_down_messages(shared, target_pid, propagated_reason);

        // Remove from process table and wait set.
        let _removed = shared.process_table.remove(target_pid);
        {
            let mut wait_set = lock_or_recover(&shared.wait_set);
            wait_set.waiting.remove(&target_pid);
            wait_set.woken.retain(|(wp, _)| *wp != target_pid);
        }

        cascade_links
            .into_iter()
            .map(|linked_pid| (target_pid, linked_pid, propagated_reason))
            .collect()
    } else if target.trap_exit() {
        // Process traps exits: deliver {EXIT, SourcePid, Reason} as message.
        link::enqueue_exit_message_pub(target, source_pid, reason);

        // Wake the process if it was waiting for a message.
        let target_pid_copy = target_pid;
        drop(slot);
        drop(entry);
        wake_process(shared, target_pid_copy);

        Vec::new()
    } else {
        // Normal exit to non-trapping process: no action needed.
        Vec::new()
    }
}

/// Deliver DOWN messages to all watchers of `target_pid`.
fn deliver_down_messages(shared: &SharedState, target_pid: u64, reason: ExitReason) {
    // Collect monitor info under monitor_set lock, then release.
    let watcher_info: Vec<(u64, u64)> = {
        let mut ms = lock_or_recover(&shared.monitor_set);
        ms.collect_watchers_and_remove(target_pid, reason)
    };

    for (watcher_pid, reference) in watcher_info {
        let delivered = deliver_single_down(shared, watcher_pid, reference, target_pid, reason);
        if delivered {
            wake_process(shared, watcher_pid);
        }
    }
}

/// Deliver a single DOWN message to a watcher process. Returns true if
/// the message was successfully enqueued.
fn deliver_single_down(
    shared: &SharedState,
    watcher_pid: u64,
    reference: u64,
    target_pid: u64,
    reason: ExitReason,
) -> bool {
    let Some(entry) = shared.process_bodies.get(&watcher_pid) else {
        return false;
    };
    let mut slot = lock_or_recover(&entry);
    let Some(ScheduledProcess(watcher)) = slot.as_mut() else {
        return false;
    };

    if matches!(watcher.status(), ProcessStatus::Exited(_)) {
        return false;
    }

    watcher.remove_monitor(reference);
    monitor::enqueue_down_message_pub(watcher, reference, target_pid, reason);
    true
}

/// Build the `NativeServices` bundle for a scheduler time slice.
pub(super) fn build_native_services(
    shared: &Arc<SharedState>,
    namespace_id: NamespaceId,
) -> crate::interpreter::NativeServices {
    let spawn: Arc<dyn SpawnFacility> = Arc::new(SchedulerSpawnFacility {
        shared: Arc::clone(shared),
        namespace_id,
    });
    let link: Arc<dyn crate::native::links::LinkFacility> = Arc::new(SchedulerLinkFacility {
        shared: Arc::clone(shared),
    });
    let supervision: Arc<dyn crate::native::supervision::SupervisionFacility> =
        Arc::new(SchedulerSupervisionFacility {
            shared: Arc::clone(shared),
        });
    let code_management: Arc<dyn crate::native::CodeManagementFacility> =
        Arc::new(super::SchedulerCodeManagementFacility {
            shared: Arc::clone(shared),
        });
    crate::interpreter::NativeServices {
        atom_table: Some(Arc::clone(&shared.atom_table)),
        timers: Some(Arc::clone(&shared.timers)),
        spawn_facility: Some(spawn),
        link_facility: Some(link),
        supervision_facility: Some(supervision),
        io_sink: Some(Arc::clone(&lock_or_recover(&shared.output_sink))),
        code_management_facility: Some(code_management),
    }
}

// ── Facility implementations ────────────────────────────────────────────────

/// Real `SpawnFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerSpawnFacility {
    pub(super) shared: Arc<SharedState>,
    pub(super) namespace_id: NamespaceId,
}

impl SpawnFacility for SchedulerSpawnFacility {
    fn spawn(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let registry = namespace_registry(&self.shared, namespace_id)
            .unwrap_or_else(|| Arc::clone(&self.shared.module_registry));
        let arity = u8::try_from(args.len()).map_err(|_| SpawnError::UnresolvedMfa)?;
        let entry = registry
            .lookup_mfa(module, function, arity)
            .map_err(|_| SpawnError::UnresolvedMfa)?;
        let ip = entry
            .module
            .label_ip(entry.label)
            .map_err(|_| SpawnError::UnresolvedMfa)?;

        let child_pid = self
            .shared
            .next_pid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);
        self.shared.process_namespaces.insert(child_pid, namespace_id);

        let mut child = super::build_process(super::SpawnRequest {
            pid: child_pid,
            module: entry.module.name,
            module_version: Arc::clone(&entry.module),
            instruction_pointer: ip,
            args,
            namespace_id,
        });

        if let Some(parent_pid) = link_to {
            child.add_link(parent_pid);
            if let Some(parent_entry) = self.shared.process_bodies.get(&parent_pid) {
                let mut parent_slot = lock_or_recover(&parent_entry);
                if let Some(ScheduledProcess(parent)) = parent_slot.as_mut() {
                    parent.add_link(child_pid);
                } else {
                    self.shared
                        .pending_links
                        .entry(parent_pid)
                        .or_default()
                        .push(child_pid);
                }
            }
        }

        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(Some(ScheduledProcess(child))),
        );

        // Enqueue to a scheduler thread's inject queue by notifying.
        self.shared
            .spawn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Put on process table directly (already done above), and wake a thread.
        // NOTE: The child is already in process_bodies; the scheduler loop will
        // find it when it picks up the PID. We need to put the PID in a run queue.
        // Since we don't have direct access to inject queues from here, we put
        // the pid into the wait set as woken so it gets picked up.
        {
            let mut ws = lock_or_recover(&self.shared.wait_set);
            ws.woken.push((child_pid, 0));
        }
        self.shared.wake_condvar.notify_all();

        Ok(child_pid)
    }

    fn spawn_lambda(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let registry = namespace_registry(&self.shared, namespace_id)
            .unwrap_or_else(|| Arc::clone(&self.shared.module_registry));
        let loaded = registry.lookup(module).ok_or(SpawnError::UnresolvedMfa)?;
        let lambda = loaded
            .lambdas
            .get(lambda_index as usize)
            .ok_or(SpawnError::UnresolvedMfa)?;
        let ip = loaded
            .label_ip(lambda.label)
            .map_err(|_| SpawnError::UnresolvedMfa)?;

        let child_pid = self
            .shared
            .next_pid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);
        self.shared.process_namespaces.insert(child_pid, namespace_id);

        let mut child = super::build_process(super::SpawnRequest {
            pid: child_pid,
            module: loaded.name,
            module_version: Arc::clone(&loaded),
            instruction_pointer: ip,
            args: Vec::new(),
            namespace_id,
        });

        if let Some(parent_pid) = link_to {
            child.add_link(parent_pid);
            if let Some(parent_entry) = self.shared.process_bodies.get(&parent_pid) {
                let mut parent_slot = lock_or_recover(&parent_entry);
                if let Some(ScheduledProcess(parent)) = parent_slot.as_mut() {
                    parent.add_link(child_pid);
                } else {
                    self.shared
                        .pending_links
                        .entry(parent_pid)
                        .or_default()
                        .push(child_pid);
                }
            }
        }

        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(Some(ScheduledProcess(child))),
        );

        self.shared
            .spawn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut ws = lock_or_recover(&self.shared.wait_set);
            ws.woken.push((child_pid, 0));
        }
        self.shared.wake_condvar.notify_all();

        Ok(child_pid)
    }
}

impl SchedulerSpawnFacility {
    fn caller_namespace(&self, caller_pid: u64) -> NamespaceId {
        if let Some(parent_entry) = self.shared.process_bodies.get(&caller_pid) {
            let parent_slot = lock_or_recover(&parent_entry);
            if let Some(ScheduledProcess(parent)) = parent_slot.as_ref() {
                return parent.namespace_id();
            }
        }
        self.namespace_id
    }
}

/// Real `LinkFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerLinkFacility {
    pub(super) shared: Arc<SharedState>,
}

impl LinkFacility for SchedulerLinkFacility {
    fn link(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        if caller_pid == target_pid {
            return Ok(());
        }

        // Check if target is already dead.
        if self.shared.exit_tombstones.contains_key(&target_pid) {
            return Err(LinkError::NoProc);
        }

        // Check target exists in process table.
        if self.shared.process_table.get(target_pid).is_none() {
            return Err(LinkError::NoProc);
        }

        // Add link to caller.
        if let Some(entry) = self.shared.process_bodies.get(&caller_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(caller)) = slot.as_mut() {
                caller.add_link(target_pid);
            } else {
                return Err(LinkError::NoCaller);
            }
        } else {
            return Err(LinkError::NoCaller);
        }

        // Add link to target.
        if let Some(entry) = self.shared.process_bodies.get(&target_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(target)) = slot.as_mut() {
                target.add_link(caller_pid);
            }
        }

        Ok(())
    }

    fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        if caller_pid == target_pid {
            return Ok(());
        }

        if let Some(entry) = self.shared.process_bodies.get(&caller_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(caller)) = slot.as_mut() {
                caller.remove_link(target_pid);
            }
        }

        if let Some(entry) = self.shared.process_bodies.get(&target_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(target)) = slot.as_mut() {
                target.remove_link(caller_pid);
            }
        }

        Ok(())
    }

    fn set_trap_exit(&self, caller_pid: u64, value: bool) -> Result<bool, LinkError> {
        let Some(entry) = self.shared.process_bodies.get(&caller_pid) else {
            return Err(LinkError::NoCaller);
        };
        let mut slot = lock_or_recover(&entry);
        let Some(ScheduledProcess(process)) = slot.as_mut() else {
            return Err(LinkError::NoCaller);
        };
        let old = process.trap_exit();
        process.set_trap_exit(value);
        Ok(old)
    }
}

/// Real `SupervisionFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerSupervisionFacility {
    pub(super) shared: Arc<SharedState>,
}

impl SupervisionFacility for SchedulerSupervisionFacility {
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError> {
        let mut ms = lock_or_recover(&self.shared.monitor_set);

        // Check if target is already dead.
        if let Some(reason) = self.shared.exit_tombstones.get(&target_pid).map(|r| *r) {
            // Allocate reference from monitor set.
            let reference = ms.allocate_reference_pub();

            // Deliver immediate DOWN to caller.
            if let Some(entry) = self.shared.process_bodies.get(&caller_pid) {
                let mut slot = lock_or_recover(&entry);
                if let Some(ScheduledProcess(caller)) = slot.as_mut() {
                    monitor::enqueue_down_message_pub(caller, reference, target_pid, reason);
                }
            }

            return Ok(MonitorResult {
                reference,
                immediate_down: true,
            });
        }

        // Both processes must exist.
        if self.shared.process_table.get(target_pid).is_none() {
            return Err(SupervisionError::NoProc);
        }

        // Allocate reference and register monitor in monitor_set.
        let reference = ms.allocate_reference_pub();
        let mon = crate::process::Monitor::new(reference, caller_pid, target_pid);
        ms.register_monitor(reference, mon, target_pid);
        drop(ms);

        // Add monitor to caller process.
        if let Some(entry) = self.shared.process_bodies.get(&caller_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(p)) = slot.as_mut() {
                p.add_monitor(mon);
            }
        }

        // Add monitor to target process.
        if let Some(entry) = self.shared.process_bodies.get(&target_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(p)) = slot.as_mut() {
                p.add_monitor(mon);
            }
        }

        Ok(MonitorResult {
            reference,
            immediate_down: false,
        })
    }

    fn demonitor(&self, caller_pid: u64, reference: u64) -> Result<(), SupervisionError> {
        let mut ms = lock_or_recover(&self.shared.monitor_set);

        // Get the monitor info before removing.
        let monitor = ms.get_monitor(reference);
        if let Some(monitor) = monitor {
            // Remove from both processes.
            if let Some(entry) = self.shared.process_bodies.get(&caller_pid) {
                let mut slot = lock_or_recover(&entry);
                if let Some(ScheduledProcess(process)) = slot.as_mut() {
                    process.remove_monitor(reference);
                }
            }
            if let Some(entry) = self.shared.process_bodies.get(&monitor.target()) {
                let mut slot = lock_or_recover(&entry);
                if let Some(ScheduledProcess(process)) = slot.as_mut() {
                    process.remove_monitor(reference);
                }
            }
            ms.remove_monitor(reference);
        }

        Ok(())
    }

    fn exit_signal(
        &self,
        _caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), SupervisionError> {
        // Deliver exit signal to target process.
        if let Some(entry) = self.shared.process_bodies.get(&target_pid) {
            let mut slot = lock_or_recover(&entry);
            if let Some(ScheduledProcess(target)) = slot.as_mut() {
                if matches!(target.status(), ProcessStatus::Exited(_)) {
                    return Ok(());
                }

                let should_die = reason == ExitReason::Kill
                    || (reason != ExitReason::Normal && !target.trap_exit());

                if should_die {
                    let terminal = link::terminal_reason(reason);
                    target.terminate(terminal);
                    drop(slot);
                    drop(entry);
                    cleanup_exited_process(&self.shared, target_pid, terminal);
                } else if target.trap_exit() {
                    link::enqueue_exit_message_pub(target, _caller_pid, reason);
                    drop(slot);
                    drop(entry);
                    wake_process(&self.shared, target_pid);
                }
            }
        }
        Ok(())
    }
}
