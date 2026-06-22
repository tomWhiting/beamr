//! Supervision integration for the scheduler — exit signal propagation
//! through links and DOWN message delivery through monitors.
//!
//! Extracted from `mod.rs` to keep per-file line counts within the project
//! constraint (500 lines).

use std::{collections::VecDeque, sync::Arc};

use crate::atom::Atom;
use crate::distribution::control::{
    ControlDelivery, ControlRegistry, DistributionSendError, DistributionSendFacility,
    encode_send_frame,
};
use crate::distribution::remote_link::{DistributionControlFacility, RemoteLinkError};
use crate::ets::{EtsError, EtsTable, EtsTableId, EtsTableMetadata};
use crate::io::{CompletionRing, IoOp};
use crate::namespace::NamespaceId;
use crate::native::CapabilitySet;
use crate::native::ets_bifs::EtsFacility;
use crate::native::io_message::IoMessageFacility;
use crate::native::links::{LinkError, LinkFacility};
use crate::native::spawn::{
    SpawnError, SpawnFacility, SpawnMonitorResult, SpawnOptions, SpawnOptionsResult,
};
use crate::native::supervision::{MonitorResult, SupervisionError, SupervisionFacility};
use crate::native::{FileIoCompletion, FileIoContinuation, FileIoFacility};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{ExitReason, Priority, Process, ProcessStatus, RemotePid};
use crate::scheduler::process_slot::PendingExitSource;
use crate::supervision::link;
use crate::supervision::monitor;
use crate::term::Term;
use crate::term::boxed;
use crate::term::pid_ref::PidRef;

use super::execution::{cleanup_exited_process, wake_process};
use super::spawning::SpawnRequest;
use super::{ProcessSlot, ScheduledProcess, SharedState, lock_or_recover, namespace_registry};

/// Propagate exit signals through links and deliver DOWN messages through
/// monitors when a process exits. Uses a worklist pattern to handle cascade
/// deaths iteratively rather than recursively.
pub(super) fn propagate_exit(shared: &SharedState, pid: u64, reason: ExitReason) {
    // Collect linked PIDs from the exiting process.
    let linked_pids = take_links_from(shared, pid);
    let remote_links = take_remote_links_from(shared, pid);
    let terminal_reason = link::terminal_reason(reason);

    // Deliver DOWN messages to all monitors of this process.
    deliver_down_messages(shared, pid, reason);

    // Mark dead in link_set for future link_pid() calls.
    {
        let mut ls = lock_or_recover(&shared.link_set);
        ls.process_exited_tombstone(pid, terminal_reason);
    }

    // Send remote EXIT controls for cross-node links.
    for remote in remote_links {
        send_remote_exit(shared, pid, remote, terminal_reason);
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

pub(super) fn register_distribution_control_handler(shared: &Arc<SharedState>) {
    let shared_for_handler = Arc::clone(shared);
    shared
        .distribution_connections
        .register_control_frame_handler(move |control, payload| {
            let facility = SchedulerDistributionSendFacility {
                shared: Arc::clone(&shared_for_handler),
            };
            let _ = crate::distribution::control::handle_frame(
                control,
                payload,
                &shared_for_handler.atom_table,
                &facility,
                Some(&facility),
            );
        });
}

pub(super) fn deliver_ets_transfer(
    shared: &SharedState,
    recipient_pid: u64,
    table_id: EtsTableId,
    from_pid: u64,
    data: Term,
    atom_table: &crate::atom::AtomTable,
) -> bool {
    let Some(entry) = shared.process_bodies.get(&recipient_pid) else {
        return false;
    };
    let transfer_atom = atom_table.intern("ETS-TRANSFER");
    let mut slot = lock_or_recover(&entry);
    let delivered = match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            let Some(message) =
                build_ets_transfer_message(process, transfer_atom, table_id, from_pid, data)
            else {
                return false;
            };
            process.mailbox_mut().push_owned(message);
            true
        }
        ProcessSlot::Executing(metadata) => {
            let Ok(data) = crate::ets::copy_term_to_ets(data) else {
                return false;
            };
            metadata.pending_ets_transfer_messages.push(
                super::process_slot::PendingEtsTransferMessage {
                    table_id,
                    from_pid,
                    data,
                },
            );
            true
        }
        ProcessSlot::Absent => false,
    };
    drop(slot);
    if delivered {
        wake_process(shared, recipient_pid);
    }
    delivered
}

pub(super) fn build_ets_transfer_message(
    process: &mut Process,
    transfer_atom: Atom,
    table_id: EtsTableId,
    from_pid: u64,
    data: Term,
) -> Option<Term> {
    let table = Term::try_small_int(i64::try_from(table_id).ok()?)?;
    let from = Term::try_pid(from_pid)?;
    let data = crate::ets::copy_term_to_heap(data, process.heap_mut()).ok()?;
    let words = process.heap_mut().alloc_slice(5).ok()?;
    boxed::write_tuple(words, &[Term::atom(transfer_atom), table, from, data])
}

/// Real `GroupLeaderFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerGroupLeaderFacility {
    pub(super) shared: Arc<SharedState>,
}

impl crate::native::GroupLeaderFacility for SchedulerGroupLeaderFacility {
    fn set_group_leader(
        &self,
        pid: u64,
        leader: Term,
    ) -> Result<(), crate::native::group_leader::GroupLeaderError> {
        let Some(entry) = self.shared.process_bodies.get(&pid) else {
            return Err(crate::native::group_leader::GroupLeaderError::NoProc);
        };
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(ScheduledProcess(process)) => {
                process.set_group_leader(leader);
                Ok(())
            }
            ProcessSlot::Executing(metadata) => {
                metadata.group_leader = leader;
                Ok(())
            }
            ProcessSlot::Absent => Err(crate::native::group_leader::GroupLeaderError::NoProc),
        }
    }

    fn group_leader(
        &self,
        pid: u64,
    ) -> Result<Term, crate::native::group_leader::GroupLeaderError> {
        let Some(entry) = self.shared.process_bodies.get(&pid) else {
            return Err(crate::native::group_leader::GroupLeaderError::NoProc);
        };
        let slot = lock_or_recover(&entry);
        match &*slot {
            ProcessSlot::Present(ScheduledProcess(process)) => Ok(process.group_leader()),
            ProcessSlot::Executing(metadata) => Ok(metadata.group_leader),
            ProcessSlot::Absent => Err(crate::native::group_leader::GroupLeaderError::NoProc),
        }
    }
}

/// Take the link set from an exiting process. The process body may already
/// have been removed, absent, or executing, so handle each slot explicitly.
pub(super) fn take_links_from(shared: &SharedState, pid: u64) -> Vec<u64> {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(ScheduledProcess(process)) => {
                return process.take_links();
            }
            ProcessSlot::Executing(metadata) => return metadata.links.clone(),
            ProcessSlot::Absent => {}
        }
    }
    Vec::new()
}

pub(super) fn take_remote_links_from(shared: &SharedState, pid: u64) -> Vec<RemotePid> {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(ScheduledProcess(process)) => {
                return process.take_remote_links();
            }
            ProcessSlot::Executing(metadata) => {
                return std::mem::take(&mut metadata.remote_links);
            }
            ProcessSlot::Absent => {}
        }
    }
    Vec::new()
}

pub(super) fn establish_remote_link(
    shared: &SharedState,
    local_pid: u64,
    remote: RemotePid,
) -> bool {
    let Some(entry) = shared.process_bodies.get(&local_pid) else {
        return false;
    };
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => process.add_remote_link(remote),
        ProcessSlot::Executing(metadata) => {
            metadata.add_remote_link(remote);
            true
        }
        ProcessSlot::Absent => false,
    }
}

pub(super) fn remove_remote_link(shared: &SharedState, local_pid: u64, remote: RemotePid) -> bool {
    let Some(entry) = shared.process_bodies.get(&local_pid) else {
        return false;
    };
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => process.remove_remote_link(remote),
        ProcessSlot::Executing(metadata) => {
            metadata.remove_remote_link(remote);
            true
        }
        ProcessSlot::Absent => false,
    }
}

#[allow(dead_code)] // Called by distribution connection layer and tests
pub(crate) fn process_remote_exit_signal(
    shared: &SharedState,
    source_pid: RemotePid,
    target_pid: u64,
    reason: ExitReason,
) {
    let Some(entry) = shared.process_bodies.get(&target_pid) else {
        return;
    };
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(target)) => {
            if matches!(target.status(), ProcessStatus::Exited(_)) {
                return;
            }
            target.remove_remote_link(source_pid);
            let should_die =
                reason == ExitReason::Kill || (reason != ExitReason::Normal && !target.trap_exit());
            if should_die {
                let propagated_reason = link::terminal_reason(reason);
                target.terminate(propagated_reason);
                drop(slot);
                drop(entry);
                cleanup_exited_process(shared, target_pid, propagated_reason);
            } else if target.trap_exit() {
                link::enqueue_remote_exit_message_pub(target, source_pid, reason);
                drop(slot);
                drop(entry);
                wake_process(shared, target_pid);
            }
        }
        ProcessSlot::Executing(metadata) => {
            metadata.remove_remote_link(source_pid);
            if metadata.trap_exit {
                metadata
                    .pending_exit_messages
                    .push((PendingExitSource::Remote(source_pid), reason));
                drop(slot);
                drop(entry);
                wake_process(shared, target_pid);
            } else if reason != ExitReason::Normal {
                shared
                    .exit_tombstones
                    .insert(target_pid, link::terminal_reason(reason));
            }
        }
        ProcessSlot::Absent => {}
    }
}

#[allow(dead_code)] // Called by distribution connection layer and tests
pub(crate) fn connection_down(shared: &SharedState, node: Atom) {
    let affected: Vec<(u64, RemotePid)> = shared
        .process_bodies
        .iter()
        .flat_map(|entry| {
            let pid = *entry.key();
            let slot = lock_or_recover(entry.value());
            match &*slot {
                ProcessSlot::Present(ScheduledProcess(process)) => process
                    .remote_links()
                    .iter()
                    .copied()
                    .filter(|remote| remote.node == node)
                    .map(|remote| (pid, remote))
                    .collect::<Vec<_>>(),
                ProcessSlot::Executing(metadata) => metadata
                    .remote_links
                    .iter()
                    .copied()
                    .filter(|remote| remote.node == node)
                    .map(|remote| (pid, remote))
                    .collect::<Vec<_>>(),
                ProcessSlot::Absent => Vec::new(),
            }
        })
        .collect();
    for (local_pid, remote_pid) in affected {
        process_remote_exit_signal(shared, remote_pid, local_pid, ExitReason::NoConnection);
    }
}

fn send_remote_exit(shared: &SharedState, caller_pid: u64, target: RemotePid, reason: ExitReason) {
    shared
        .control_router
        .send_exit(shared.local_node.name, caller_pid, target, reason);
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
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(target)) => {
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

                // Record tombstone and remove resources owned by the terminated process.
                shared.exit_tombstones.insert(target_pid, propagated_reason);
                let _deleted_tables = shared.transfer_or_delete_tables_owned_by(target_pid);
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
                // Purge per-pid suspension/timer state exactly like
                // cleanup_exited_process: a killed suspended process must
                // not strand its mirror or a published completion.
                let _stale_marks = shared.expired_receive_timers.remove(&target_pid);
                shared.purge_suspension_state(target_pid);

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
        ProcessSlot::Executing(metadata) => {
            metadata.remove_link(source_pid);
            let should_die =
                reason == ExitReason::Kill || (reason != ExitReason::Normal && !metadata.trap_exit);

            if should_die {
                let propagated_reason = link::terminal_reason(reason);
                if reason == ExitReason::Kill {
                    metadata.trap_exit = false;
                }
                let cascade_links: Vec<u64> = metadata
                    .links
                    .iter()
                    .copied()
                    .filter(|linked_pid| *linked_pid != source_pid)
                    .collect();
                shared.exit_tombstones.insert(target_pid, propagated_reason);
                let _deleted_tables = shared.transfer_or_delete_tables_owned_by(target_pid);
                {
                    let mut ls = lock_or_recover(&shared.link_set);
                    ls.process_exited_tombstone(target_pid, propagated_reason);
                }
                drop(slot);
                drop(entry);
                deliver_down_messages(shared, target_pid, propagated_reason);

                cascade_links
                    .into_iter()
                    .map(|linked_pid| (target_pid, linked_pid, propagated_reason))
                    .collect()
            } else if metadata.trap_exit {
                // Process traps exits: queue {EXIT, SourcePid, Reason} for
                // delivery when the slice completes. This mirrors the Present
                // arm's `else if target.trap_exit()` and MUST include NORMAL
                // exits (OTP delivers {'EXIT', Pid, normal} for a normal
                // linked exit to a trapping process). `should_die` has already
                // peeled off Kill and abnormal-non-trapping cases, so reaching
                // here means a trapping target for any non-kill reason.
                metadata
                    .pending_exit_messages
                    .push((PendingExitSource::Local(source_pid), reason));
                drop(slot);
                drop(entry);
                wake_process(shared, target_pid);
                Vec::new()
            } else {
                Vec::new()
            }
        }
        ProcessSlot::Absent => Vec::new(),
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
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(watcher)) => {
            if matches!(watcher.status(), ProcessStatus::Exited(_)) {
                return false;
            }

            watcher.remove_monitor(reference);
            monitor::enqueue_down_message_pub(watcher, reference, target_pid, reason);
            true
        }
        ProcessSlot::Executing(metadata) => {
            metadata.remove_monitor(reference);
            metadata
                .pending_down_messages
                .push((reference, target_pid, reason));
            true
        }
        ProcessSlot::Absent => false,
    }
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
    let group_leader: Arc<dyn crate::native::GroupLeaderFacility> =
        Arc::new(SchedulerGroupLeaderFacility {
            shared: Arc::clone(shared),
        });
    let supervision: Arc<dyn crate::native::supervision::SupervisionFacility> =
        Arc::new(SchedulerSupervisionFacility {
            shared: Arc::clone(shared),
        });
    let process_info: Arc<dyn crate::native::ProcessInfoFacility> =
        Arc::new(SchedulerProcessInfoFacility {
            shared: Arc::clone(shared),
        });
    let code_management: Arc<dyn crate::native::CodeManagementFacility> =
        Arc::new(super::module_management::SchedulerCodeManagementFacility {
            shared: Arc::clone(shared),
        });
    let system_info: Arc<dyn crate::native::SystemInfoFacility> =
        Arc::new(SchedulerSystemInfoFacility {
            shared: Arc::clone(shared),
        });
    let ets_facility: Arc<dyn crate::native::EtsFacility> = Arc::new(SchedulerEtsFacility {
        shared: Arc::clone(shared),
    });
    let pg_facility: Arc<dyn crate::distribution::pg::PgFacility> =
        Arc::clone(&shared.pg_registry) as _;
    let file_io_facility: Arc<dyn FileIoFacility> = Arc::new(SchedulerFileIoFacility {
        shared: Arc::clone(shared),
    });
    let distribution_send: Arc<dyn DistributionSendFacility> =
        Arc::new(SchedulerDistributionSendFacility {
            shared: Arc::clone(shared),
        });
    let local_send: Arc<dyn crate::native::local_send::LocalSendFacility> =
        Arc::new(SchedulerLocalSendFacility {
            shared: Arc::clone(shared),
        });
    crate::interpreter::NativeServices {
        atom_table: Some(Arc::clone(&shared.atom_table)),
        local_node: Some(shared.local_node),
        net_kernel: Some(Arc::clone(&shared.net_kernel)),
        distribution_send: Some(distribution_send),
        local_send: Some(local_send),
        ets_facility: Some(ets_facility),
        pg_facility: Some(pg_facility),
        timers: Some(Arc::clone(&shared.timers)),
        spawn_facility: Some(spawn),
        link_facility: Some(link),
        distribution_control_facility: Some(Arc::new(SchedulerDistributionControlFacility {
            shared: Arc::clone(shared),
        })),
        group_leader_facility: Some(group_leader),
        supervision_facility: Some(supervision),
        process_info_facility: Some(process_info),
        io_sink: Some(Arc::clone(&lock_or_recover(&shared.output_sink))),
        code_management_facility: Some(code_management),
        system_info_facility: Some(system_info),
        io_facility: if shared.replay_mode {
            None
        } else {
            shared.io_facility.clone()
        },
        io_message_facility: Some(Arc::new(SchedulerIoMessageFacility {
            shared: Arc::clone(shared),
        })),
        file_io_facility: (!shared.replay_mode).then_some(file_io_facility),
        tcp_io_facility: (!shared.replay_mode).then(|| {
            Arc::new(SchedulerTcpIoFacility {
                shared: Arc::clone(shared),
            }) as Arc<dyn crate::native::TcpIoFacility>
        }),
        jit_cache: Some(Arc::clone(&shared.jit_cache)),
        replay_driver: shared.replay_driver.clone(),
        bif_registry: Some(Arc::clone(&shared.bif_registry)),
        nif_private_data: shared.nif_private_data.clone(),
        suspension_registrar: Some(Arc::new(
            crate::scheduler::suspension::SchedulerSuspensionRegistrar {
                shared: Arc::clone(shared),
            },
        )),
        ..crate::interpreter::NativeServices::default()
    }
}

// ── Facility implementations ────────────────────────────────────────────────

struct SchedulerDistributionSendFacility {
    shared: Arc<SharedState>,
}

impl DistributionSendFacility for SchedulerDistributionSendFacility {
    fn send_remote(&self, target: Term, message: Term) -> Result<(), DistributionSendError> {
        let pid = PidRef::new(target).ok_or(DistributionSendError::Encode)?;
        let node = pid.node().ok_or(DistributionSendError::Encode)?;
        let node_name = self
            .shared
            .atom_table
            .resolve(node)
            .ok_or(DistributionSendError::NoConnection)?
            .to_owned();
        let frame = encode_send_frame(
            Term::atom(Atom::OK),
            target,
            message,
            &self.shared.atom_table,
        )
        .map_err(|_| DistributionSendError::Encode)?;
        block_on_distribution_send(
            &self.shared.distribution_connections,
            node,
            &node_name,
            &frame,
        )
    }
}

fn block_on_distribution_send(
    manager: &crate::distribution::connection::ConnectionManager,
    node: Atom,
    node_name: &str,
    frame: &[u8],
) -> Result<(), DistributionSendError> {
    let manager = manager.clone();
    let node_name = node_name.to_owned();
    let frame = frame.to_vec();
    let future = async move {
        let connection = match manager.get_connection(node) {
            Some(connection) => connection,
            None => manager
                .connect(&node_name)
                .await
                .map_err(|_| DistributionSendError::NoConnection)?,
        };
        connection
            .write_raw(&frame)
            .await
            .map_err(|_| DistributionSendError::NoConnection)
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|_| DistributionSendError::NoConnection)?
                    .block_on(future)
            })
            .join()
            .map_err(|_| DistributionSendError::NoConnection)?
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| DistributionSendError::NoConnection)?
            .block_on(future)
    }
}

impl ControlDelivery for SchedulerDistributionSendFacility {
    fn deliver_payload(&self, target_pid: u64, payload_etf: &[u8]) -> bool {
        let Some(entry) = self.shared.process_bodies.get(&target_pid) else {
            return false;
        };
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(process) => {
                let mut context = crate::native::ProcessContext::new();
                context.attach_process(&mut process.0, 0);
                let Ok(message) = crate::etf::decode::decode_term(
                    payload_etf,
                    &mut context,
                    &self.shared.atom_table,
                ) else {
                    return false;
                };
                process.0.mailbox_mut().push_owned(message);
            }
            ProcessSlot::Executing(metadata) => {
                metadata
                    .pending_distribution_payloads
                    .push(payload_etf.to_vec());
            }
            ProcessSlot::Absent => return false,
        }
        drop(slot);
        drop(entry);
        wake_process(&self.shared, target_pid);
        true
    }
}

impl ControlRegistry for SchedulerDistributionSendFacility {
    fn whereis(&self, name: Atom) -> Option<u64> {
        self.shared.process_registry.get(&name).map(|entry| *entry)
    }
}

/// Scheduler-side implementation of [`LocalSendFacility`]: delivers a local
/// message to a target process body held in `process_bodies`, mirroring the
/// I/O delivery template (lock slot → Present/Executing/Absent → push before
/// wake) and the distribution-payload deferral for the Executing case.
struct SchedulerLocalSendFacility {
    shared: Arc<SharedState>,
}

impl crate::native::local_send::LocalSendFacility for SchedulerLocalSendFacility {
    fn send_local(
        &self,
        request: crate::native::local_send::LocalSendRequest<'_>,
    ) -> Result<(), crate::native::local_send::LocalSendError> {
        // Self-send from the BYTECODE path is handled in-hand by
        // `messaging::send` against the sender's own body and never reaches the
        // facility. A NATIVE self-send (NATIVE-001 R7), by contrast, is routed
        // here deliberately: the sender's slot is `Executing` during its slice,
        // so the message lands in `pending_local_messages` and is merged into
        // the mailbox at store-back (visible on the next slice) — exactly the
        // existing Executing-slot deferral, with no native-specific code path.
        let Some(entry) = self.shared.process_bodies.get(&request.target_pid) else {
            // Absent/dead pid: silent drop, matching BEAM semantics.
            return Ok(());
        };
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(process) => {
                // The Present branch is the ONLY place replay can occur for a
                // cross-process send: on a single replay thread no other pid can
                // be Executing, so the receiver is always Present (or Absent).
                let previous_receiver_clock = process.0.logical_clock();
                let receiver_clock = process.0.observe_message_clock(request.sender_clock);
                if let Some(driver) = request.replay_driver {
                    let mut guard = match driver.lock() {
                        Ok(guard) => guard,
                        Err(error) => error.into_inner(),
                    };
                    let recorded = match guard.next_message_delivery(
                        crate::replay::RecordedDeliveryKind::Message,
                        Some(request.sender_pid),
                        request.target_pid,
                        request.message,
                    ) {
                        Ok(recorded) => recorded,
                        Err(error) => {
                            process.0.set_logical_clock(previous_receiver_clock);
                            return Err(crate::native::local_send::LocalSendError::ReplayMismatch(
                                error.to_string(),
                            ));
                        }
                    };
                    if recorded.sender_clock != request.sender_clock
                        || recorded.receiver_clock != receiver_clock
                    {
                        process.0.set_logical_clock(previous_receiver_clock);
                        return Err(crate::native::local_send::LocalSendError::ReplayMismatch(
                            format!(
                                "message delivery clock mismatch: expected sender/receiver clocks ({}, {}), recorded ({}, {})",
                                request.sender_clock,
                                receiver_clock,
                                recorded.sender_clock,
                                recorded.receiver_clock
                            ),
                        ));
                    }
                }
                #[cfg(feature = "telemetry")]
                {
                    if process
                        .0
                        .mailbox()
                        .sender()
                        .send_traced(
                            request.sender_pid,
                            request.target_pid,
                            request.message,
                            process.0.heap_mut(),
                        )
                        .is_err()
                    {
                        // BEAM `!` cannot fail, so we still return Ok and drop the
                        // message — but a copy-into-mailbox failure (e.g. the
                        // receiver's young heap is full; beamr's bump allocator
                        // returns HeapFull rather than GCing) should never be
                        // silent. Surface it via telemetry. NOT a debug_assert:
                        // HeapFull is a legitimate runtime condition, not an
                        // invariant violation, so it must not crash debug builds.
                        crate::telemetry::metrics::record_message_dropped("mailbox_present");
                        return Ok(());
                    }
                }
                #[cfg(not(feature = "telemetry"))]
                {
                    if process
                        .0
                        .mailbox()
                        .sender()
                        .send(request.message, process.0.heap_mut())
                        .is_err()
                    {
                        // See the telemetry branch above: keep `!` infallible and
                        // drop on copy failure (HeapFull). No telemetry symbol in a
                        // non-telemetry build; no debug_assert because HeapFull is a
                        // legitimate runtime condition. Matches the I/O delivery
                        // template's silent-drop posture.
                        return Ok(());
                    }
                }
                // The scheduler's `wake_process` (below) requeues a parked
                // receiver and the slice machinery flips its status on resume —
                // exactly as the I/O delivery template does. The facility must
                // NOT force the status transition itself (doing so leaves a
                // parked process in an inconsistent Running state).
            }
            ProcessSlot::Executing(metadata) => {
                // Live-mode-only path: the receiver is mid-slice on another
                // thread, so we cannot touch its heap. ETF-encode the message
                // here so it survives the heap crossing, then decode it onto the
                // receiver heap at store-back (see scheduler/execution/core.rs).
                //
                // Clock note: delivering to an Executing receiver does NOT advance
                // the receiver's Lamport clock here. That is intentional and
                // consistent with the I/O and distribution deferred-delivery paths
                // (pending_io_messages / pending_distribution_payloads), which also
                // defer the receiver-side clock work. It is replay-safe because
                // replay is single-threaded: a receiver is never Executing during
                // replay, so this arm is only ever taken in live mode and never
                // contributes to the recorded delivery ordering.
                let payload =
                    match crate::etf::encode::encode_term(request.message, &self.shared.atom_table)
                    {
                        Ok(payload) => payload,
                        Err(_error) => {
                            // BEAM `!` cannot fail, so we keep returning Ok and drop
                            // the message rather than erroring the send. Surface the
                            // drop via telemetry so a codec gap is never an invisible
                            // message loss and is countable in prod.
                            //
                            // KNOWN LIMITATION (follow-up): `encode_term` rejects
                            // free-variable closures (`num_free != 0`), so a closure
                            // capturing variables sent to an *Executing* receiver is
                            // dropped here, even though the Present/in-hand path
                            // (`copy_term`) would deliver it faithfully. This is a
                            // pre-existing ETF asymmetry, not introduced by this fix.
                            // It is NOT a debug_assert: a free-var-closure send is
                            // legitimate user code, not an invariant violation, so it
                            // must not crash debug builds. Closing the gap requires
                            // symmetric closure ETF (or a heap-fragment copy for the
                            // Executing case) — tracked separately. References, the
                            // common actor case, DO round-trip (see etf/decode.rs).
                            #[cfg(feature = "telemetry")]
                            crate::telemetry::metrics::record_message_dropped("etf_encode");
                            return Ok(());
                        }
                    };
                metadata.pending_local_messages.push(payload);
            }
            ProcessSlot::Absent => return Ok(()),
        }
        drop(slot);
        drop(entry);
        wake_process(&self.shared, request.target_pid);
        Ok(())
    }
}

struct SchedulerIoMessageFacility {
    shared: Arc<SharedState>,
}

impl IoMessageFacility for SchedulerIoMessageFacility {
    fn send_message(&self, sender_pid: u64, target_pid: u64, message: Term) -> bool {
        let _ = sender_pid;
        let Some(entry) = self.shared.process_bodies.get(&target_pid) else {
            return false;
        };
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(process) => {
                process.0.mailbox_mut().push_owned(message);
            }
            ProcessSlot::Executing(metadata) => {
                metadata.pending_io_messages.push(message);
            }
            ProcessSlot::Absent => return false,
        }
        drop(slot);
        drop(entry);
        wake_process(&self.shared, target_pid);
        true
    }
}

struct SchedulerFileIoFacility {
    shared: Arc<SharedState>,
}

struct SchedulerEtsFacility {
    shared: Arc<SharedState>,
}

impl EtsFacility for SchedulerEtsFacility {
    fn create_table(&self, metadata: EtsTableMetadata) -> Result<EtsTableId, EtsError> {
        self.shared.ets_registry.try_create_table(metadata)
    }

    fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.shared.ets_registry.lookup_table(id)
    }

    fn lookup_named_table(&self, name: Atom) -> Option<Arc<dyn EtsTable>> {
        self.shared.ets_registry.lookup_named_table(name)
    }

    fn lookup_table_by_name(&self, name: Atom) -> Option<EtsTableId> {
        self.shared.ets_registry.lookup_table_by_name(name)
    }

    fn delete_table(&self, id: EtsTableId) -> bool {
        self.shared.ets_registry.delete_table(id)
    }

    fn give_away_table(
        &self,
        table_id: EtsTableId,
        new_owner: u64,
        from_pid: u64,
        gift_data: Term,
        atom_table: &crate::atom::AtomTable,
    ) -> Result<(), EtsError> {
        if !deliver_ets_transfer(
            &self.shared,
            new_owner,
            table_id,
            from_pid,
            gift_data,
            atom_table,
        ) {
            return Err(EtsError::Badarg);
        }
        if self
            .shared
            .ets_registry
            .transfer_table_owner(table_id, new_owner)
        {
            Ok(())
        } else {
            Err(EtsError::Badarg)
        }
    }
}

impl FileIoFacility for SchedulerFileIoFacility {
    fn submit_file_io(&self, pid: u64, op: IoOp, continuation: FileIoContinuation) -> u64 {
        let op_id = self.shared.file_io_ring.submit(op);
        self.track_submitted_file_io(pid, op_id, continuation);
        op_id
    }

    fn track_submitted_file_io(&self, pid: u64, op_id: u64, continuation: FileIoContinuation) {
        if let Some((_, completion)) = self.shared.file_io_orphans.remove(&op_id) {
            self.shared.file_io_results.insert(
                pid,
                FileIoCompletion {
                    op_id,
                    continuation,
                    completion,
                },
            );
            super::execution::wake_process(&self.shared, pid);
        } else {
            self.shared
                .file_io_pending
                .insert(op_id, (pid, continuation));
        }
    }

    fn take_file_io_completion(&self, pid: u64) -> Option<FileIoCompletion> {
        self.shared
            .file_io_results
            .remove(&pid)
            .map(|(_, result)| result)
    }

    fn cancel_pending_file_io_for_pid(&self, pid: u64) {
        let op_ids: Vec<u64> = self
            .shared
            .file_io_pending
            .iter()
            .filter_map(|entry| (entry.value().0 == pid).then_some(*entry.key()))
            .collect();
        for op_id in op_ids {
            if self.shared.file_io_pending.remove(&op_id).is_some() {
                self.shared.file_io_canceled.insert(op_id);
            }
        }
        self.shared.file_io_results.remove(&pid);
    }

    fn ring(&self) -> &dyn CompletionRing {
        self.shared.file_io_ring.as_ref()
    }
}

struct SchedulerTcpIoFacility {
    shared: Arc<SharedState>,
}

impl crate::native::TcpIoFacility for SchedulerTcpIoFacility {
    fn submit_active_tcp_read(
        &self,
        socket: Arc<crate::io::resource::FdInner>,
        buf_len: usize,
    ) -> Option<u64> {
        let op_id = self.shared.file_io_ring.submit(IoOp::Read {
            fd: socket.fd(),
            buf_len,
            offset: u64::MAX,
        });
        self.shared.file_io_pending.insert(
            op_id,
            (
                socket.controlling_process(),
                crate::native::FileIoContinuation::TcpActiveRecv { fd: socket },
            ),
        );
        Some(op_id)
    }
}

/// Real `ProcessInfoFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerProcessInfoFacility {
    pub(super) shared: Arc<SharedState>,
}

impl crate::native::ProcessInfoFacility for SchedulerProcessInfoFacility {
    fn process_info(
        &self,
        pid: u64,
        item: crate::native::ProcessInfoItem,
    ) -> Option<crate::native::ProcessInfoValue> {
        self.shared.process_info(pid, item)
    }
}

/// Real `SpawnFacility` backed by the scheduler's shared state.
pub(super) struct SchedulerSpawnFacility {
    pub(super) shared: Arc<SharedState>,
    pub(super) namespace_id: NamespaceId,
}

pub(super) struct SchedulerSystemInfoFacility {
    pub(super) shared: Arc<SharedState>,
}

impl crate::native::SystemInfoFacility for SchedulerSystemInfoFacility {
    fn scheduler_count(&self) -> usize {
        self.shared.scheduler_count()
    }

    fn process_count(&self) -> usize {
        self.shared.process_count()
    }

    fn atom_count(&self) -> usize {
        self.shared.atom_count()
    }

    fn atom_limit(&self) -> usize {
        self.shared.atom_table.limit()
    }

    fn memory_summary(&self) -> crate::native::system_info_bifs::MemorySummary {
        self.shared.memory_summary()
    }
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
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = self.caller_capabilities(caller_pid);
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

        let mut child = super::spawning::build_process(super::spawning::SpawnRequest {
            pid: child_pid,
            module: entry.module.name,
            module_version: Arc::clone(&entry.module),
            instruction_pointer: ip,
            args,
            namespace_id,
            group_leader,
            capabilities,
            priority: Priority::Normal,
            heap_size: DEFAULT_HEAP_SIZE,
            parent_pid: caller_pid,
            function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        });

        if let Some(parent_pid) = link_to {
            let child_linked = child.add_link(parent_pid);
            let parent_linked = add_link_to_slot(&self.shared, parent_pid, child_pid);
            if child_linked && parent_linked {
                #[cfg(feature = "telemetry")]
                crate::telemetry::lifecycle::record_process_linked(parent_pid, child_pid);
            }
        }

        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(child))),
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

        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_spawned(
            &self.shared.atom_table,
            child_pid,
            caller_pid,
            entry.module.name,
            function,
            arity,
        );

        Ok(child_pid)
    }

    fn spawn_native(
        &self,
        caller_pid: u64,
        factory: crate::native::native_process::NativeHandlerFactory,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        // Mirror `spawn`, but build a native `Process` (no bytecode setup, no
        // instruction pointer) carrying the handler the factory produces.
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = self.caller_capabilities(caller_pid);

        let child_pid = self
            .shared
            .next_pid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);

        let mut child = Process::with_capabilities(child_pid, DEFAULT_HEAP_SIZE, capabilities);
        child.set_group_leader(group_leader);
        child.set_namespace_id(namespace_id);
        child.set_priority(Priority::Normal);
        child.set_native_body(crate::native::native_process::NativeBody::new(factory));

        if let Some(parent_pid) = link_to {
            let child_linked = child.add_link(parent_pid);
            let parent_linked = add_link_to_slot(&self.shared, parent_pid, child_pid);
            if child_linked && parent_linked {
                #[cfg(feature = "telemetry")]
                crate::telemetry::lifecycle::record_process_linked(parent_pid, child_pid);
            }
        }

        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(child))),
        );

        self.shared
            .spawn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut ws = lock_or_recover(&self.shared.wait_set);
            ws.woken.push((child_pid, 0));
        }
        self.shared.wake_condvar.notify_all();

        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_spawned(
            &self.shared.atom_table,
            child_pid,
            caller_pid,
            Atom::NIL,
            Atom::NIL,
            0,
        );

        Ok(child_pid)
    }

    fn spawn_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        self.spawn_mfa_with_monitor(caller_pid, module, function, args)
    }

    fn spawn_lambda(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = self.caller_capabilities(caller_pid);
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

        let mut child = super::spawning::build_process(super::spawning::SpawnRequest {
            pid: child_pid,
            module: loaded.name,
            module_version: Arc::clone(&loaded),
            instruction_pointer: ip,
            args: Vec::new(),
            namespace_id,
            group_leader,
            capabilities,
            priority: Priority::Normal,
            heap_size: DEFAULT_HEAP_SIZE,
            parent_pid: caller_pid,
            function: Atom::NIL,
            arity: 0,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        });

        if let Some(parent_pid) = link_to {
            let child_linked = child.add_link(parent_pid);
            let parent_linked = add_link_to_slot(&self.shared, parent_pid, child_pid);
            if child_linked && parent_linked {
                #[cfg(feature = "telemetry")]
                crate::telemetry::lifecycle::record_process_linked(parent_pid, child_pid);
            }
        }

        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(child))),
        );

        self.shared
            .spawn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut ws = lock_or_recover(&self.shared.wait_set);
            ws.woken.push((child_pid, 0));
        }
        self.shared.wake_condvar.notify_all();

        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_spawned(
            &self.shared.atom_table,
            child_pid,
            caller_pid,
            loaded.name,
            Atom::NIL,
            0,
        );

        Ok(child_pid)
    }

    fn spawn_lambda_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        self.spawn_lambda_with_monitor(caller_pid, module, lambda_index)
    }

    fn spawn_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        self.spawn_mfa_with_options(caller_pid, module, function, args, options)
    }

    fn spawn_lambda_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        self.spawn_lambda_with_options_impl(caller_pid, module, lambda_index, options)
    }
}

impl SchedulerSpawnFacility {
    fn spawn_mfa_with_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = self.caller_capabilities(caller_pid);
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

        let child = super::spawning::build_process(super::spawning::SpawnRequest {
            pid: child_pid,
            module: entry.module.name,
            module_version: Arc::clone(&entry.module),
            instruction_pointer: ip,
            args,
            namespace_id,
            group_leader,
            capabilities,
            priority: Priority::Normal,
            heap_size: DEFAULT_HEAP_SIZE,
            parent_pid: caller_pid,
            function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        });

        let result = self.register_monitor_insert_and_wake(caller_pid, child_pid, child);
        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_spawned(
            &self.shared.atom_table,
            child_pid,
            caller_pid,
            entry.module.name,
            function,
            arity,
        );
        Ok(result)
    }

    fn spawn_mfa_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = options
            .capabilities
            .clone()
            .unwrap_or_else(|| self.caller_capabilities(caller_pid));
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

        let request = SpawnRequest {
            pid: self.next_child_pid(),
            module: entry.module.name,
            module_version: Arc::clone(&entry.module),
            instruction_pointer: ip,
            args,
            namespace_id,
            group_leader,
            capabilities,
            priority: options.priority.unwrap_or(Priority::Normal),
            heap_size: options.min_heap_size.unwrap_or(DEFAULT_HEAP_SIZE),
            parent_pid: caller_pid,
            function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        };
        Ok(self.insert_options_child(caller_pid, request, options))
    }

    fn spawn_lambda_with_options_impl(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
        let capabilities = options
            .capabilities
            .clone()
            .unwrap_or_else(|| self.caller_capabilities(caller_pid));
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

        let request = SpawnRequest {
            pid: self.next_child_pid(),
            module: loaded.name,
            module_version: Arc::clone(&loaded),
            instruction_pointer: ip,
            args: Vec::new(),
            namespace_id,
            group_leader,
            capabilities,
            priority: options.priority.unwrap_or(Priority::Normal),
            heap_size: options.min_heap_size.unwrap_or(DEFAULT_HEAP_SIZE),
            parent_pid: caller_pid,
            function: Atom::NIL,
            arity: 0,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        };
        Ok(self.insert_options_child(caller_pid, request, options))
    }

    fn next_child_pid(&self) -> u64 {
        let child_pid = self
            .shared
            .next_pid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);
        child_pid
    }

    fn insert_options_child(
        &self,
        caller_pid: u64,
        request: SpawnRequest,
        options: SpawnOptions,
    ) -> SpawnOptionsResult {
        let child_pid = request.pid;
        #[cfg(feature = "telemetry")]
        let parent_pid = request.parent_pid;
        #[cfg(feature = "telemetry")]
        let module = request.module;
        #[cfg(feature = "telemetry")]
        let function = request.function;
        #[cfg(feature = "telemetry")]
        let arity = request.arity;
        let mut child = super::spawning::build_process(request);
        if options.link {
            let child_linked = child.add_link(caller_pid);
            let caller_linked = add_link_to_slot(&self.shared, caller_pid, child_pid);
            if child_linked && caller_linked {
                #[cfg(feature = "telemetry")]
                crate::telemetry::lifecycle::record_process_linked(caller_pid, child_pid);
            }
        }
        if options.monitor {
            let result = self.register_monitor_insert_and_wake(caller_pid, child_pid, child);
            #[cfg(feature = "telemetry")]
            crate::telemetry::lifecycle::record_process_spawned(
                &self.shared.atom_table,
                child_pid,
                parent_pid,
                module,
                function,
                arity,
            );
            SpawnOptionsResult {
                pid: result.pid,
                reference: Some(result.reference),
            }
        } else {
            self.insert_and_wake(child_pid, child);
            #[cfg(feature = "telemetry")]
            crate::telemetry::lifecycle::record_process_spawned(
                &self.shared.atom_table,
                child_pid,
                parent_pid,
                module,
                function,
                arity,
            );
            SpawnOptionsResult {
                pid: child_pid,
                reference: None,
            }
        }
    }

    fn spawn_lambda_with_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        let namespace_id = self.caller_namespace(caller_pid);
        let group_leader = self.caller_group_leader(caller_pid);
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

        let child = super::spawning::build_process(super::spawning::SpawnRequest {
            pid: child_pid,
            module: loaded.name,
            module_version: Arc::clone(&loaded),
            instruction_pointer: ip,
            args: Vec::new(),
            namespace_id,
            group_leader,
            capabilities: self.caller_capabilities(caller_pid),
            priority: Priority::Normal,
            heap_size: DEFAULT_HEAP_SIZE,
            parent_pid: caller_pid,
            function: Atom::NIL,
            arity: 0,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        });

        let result = self.register_monitor_insert_and_wake(caller_pid, child_pid, child);
        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_spawned(
            &self.shared.atom_table,
            child_pid,
            caller_pid,
            loaded.name,
            Atom::NIL,
            0,
        );
        Ok(result)
    }

    fn register_monitor_insert_and_wake(
        &self,
        caller_pid: u64,
        child_pid: u64,
        mut child: crate::process::Process,
    ) -> SpawnMonitorResult {
        let reference = {
            let mut ms = lock_or_recover(&self.shared.monitor_set);
            let reference = ms.allocate_reference_pub();
            let mon = crate::process::Monitor::new(reference, caller_pid, child_pid);
            ms.register_monitor(reference, mon, child_pid);
            child.add_monitor(mon);
            drop(ms);
            add_monitor_to_slot(&self.shared, caller_pid, mon);
            reference
        };

        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_monitored(caller_pid, child_pid, reference);

        self.insert_and_wake(child_pid, child);

        SpawnMonitorResult {
            pid: child_pid,
            reference,
        }
    }

    fn insert_and_wake(&self, child_pid: u64, child: Process) {
        self.shared.process_bodies.insert(
            child_pid,
            std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(child))),
        );
        self.shared
            .spawn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut ws = lock_or_recover(&self.shared.wait_set);
            ws.woken.push((child_pid, 0));
        }
        self.shared.wake_condvar.notify_all();
    }

    fn caller_namespace(&self, caller_pid: u64) -> NamespaceId {
        if let Some(parent_entry) = self.shared.process_bodies.get(&caller_pid) {
            let parent_slot = lock_or_recover(&parent_entry);
            match &*parent_slot {
                ProcessSlot::Present(ScheduledProcess(parent)) => return parent.namespace_id(),
                ProcessSlot::Executing(metadata) => return metadata.namespace_id,
                ProcessSlot::Absent => {}
            }
        }
        self.namespace_id
    }

    fn caller_group_leader(&self, caller_pid: u64) -> Term {
        if let Some(parent_entry) = self.shared.process_bodies.get(&caller_pid) {
            let parent_slot = lock_or_recover(&parent_entry);
            match &*parent_slot {
                ProcessSlot::Present(ScheduledProcess(parent)) => return parent.group_leader(),
                ProcessSlot::Executing(metadata) => return metadata.group_leader,
                ProcessSlot::Absent => {}
            }
        }
        match Term::try_pid(caller_pid) {
            Some(pid_term) => pid_term,
            None => Term::NIL,
        }
    }

    fn caller_capabilities(&self, caller_pid: u64) -> CapabilitySet {
        if let Some(parent_entry) = self.shared.process_bodies.get(&caller_pid) {
            let parent_slot = lock_or_recover(&parent_entry);
            match &*parent_slot {
                ProcessSlot::Present(ScheduledProcess(parent)) => {
                    return parent.capabilities().clone();
                }
                ProcessSlot::Executing(metadata) => return metadata.capabilities.clone(),
                ProcessSlot::Absent => {}
            }
        }
        CapabilitySet::all()
    }
}

fn add_monitor_to_slot(shared: &SharedState, pid: u64, monitor: crate::process::Monitor) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            process.add_monitor(monitor);
            true
        }
        ProcessSlot::Executing(metadata) => {
            metadata.add_monitor(monitor);
            true
        }
        ProcessSlot::Absent => false,
    }
}

fn add_link_to_slot(shared: &SharedState, pid: u64, linked_pid: u64) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            process.add_link(linked_pid);
            true
        }
        ProcessSlot::Executing(metadata) => {
            metadata.add_link(linked_pid, pid);
            true
        }
        ProcessSlot::Absent => false,
    }
}

fn slot_has_link(shared: &SharedState, pid: u64, linked_pid: u64) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let slot = lock_or_recover(&entry);
    match &*slot {
        ProcessSlot::Present(ScheduledProcess(process)) => process.links().contains(&linked_pid),
        ProcessSlot::Executing(metadata) => metadata.links.contains(&linked_pid),
        ProcessSlot::Absent => false,
    }
}

fn remove_link_from_slot(shared: &SharedState, pid: u64, linked_pid: u64) {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        match &mut *slot {
            ProcessSlot::Present(ScheduledProcess(process)) => {
                process.remove_link(linked_pid);
            }
            ProcessSlot::Executing(metadata) => metadata.remove_link(linked_pid),
            ProcessSlot::Absent => {}
        }
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

        let already_linked = slot_has_link(&self.shared, caller_pid, target_pid);

        // Add link to caller.
        if !add_link_to_slot(&self.shared, caller_pid, target_pid) {
            return Err(LinkError::NoCaller);
        }

        // Add link to target.
        let target_linked = add_link_to_slot(&self.shared, target_pid, caller_pid);

        if !already_linked && target_linked {
            #[cfg(feature = "telemetry")]
            crate::telemetry::lifecycle::record_process_linked(caller_pid, target_pid);
        }

        Ok(())
    }

    fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
        if caller_pid == target_pid {
            return Ok(());
        }

        remove_link_from_slot(&self.shared, caller_pid, target_pid);

        remove_link_from_slot(&self.shared, target_pid, caller_pid);

        Ok(())
    }

    fn set_trap_exit(&self, caller_pid: u64, value: bool) -> Result<bool, LinkError> {
        let Some(entry) = self.shared.process_bodies.get(&caller_pid) else {
            return Err(LinkError::NoCaller);
        };
        let mut slot = lock_or_recover(&entry);
        let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
            return Err(LinkError::NoCaller);
        };
        let old = process.trap_exit();
        process.set_trap_exit(value);
        Ok(old)
    }
}

/// Real `DistributionControlFacility` backed by scheduler remote-link metadata.
pub(super) struct SchedulerDistributionControlFacility {
    pub(super) shared: Arc<SharedState>,
}

impl DistributionControlFacility for SchedulerDistributionControlFacility {
    fn link_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError> {
        if self.shared.process_table.get(caller_pid).is_none() {
            return Err(RemoteLinkError::BadTarget);
        }
        if !establish_remote_link(&self.shared, caller_pid, target) {
            return Err(RemoteLinkError::BadTarget);
        }
        self.shared
            .control_router
            .send_link(self.shared.local_node.name, caller_pid, target);
        Ok(())
    }

    fn unlink_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError> {
        remove_remote_link(&self.shared, caller_pid, target);
        self.shared
            .control_router
            .send_unlink(self.shared.local_node.name, caller_pid, target);
        Ok(())
    }

    fn exit_remote(
        &self,
        caller_pid: u64,
        target: RemotePid,
        reason: ExitReason,
    ) -> Result<(), RemoteLinkError> {
        self.shared.control_router.send_exit(
            self.shared.local_node.name,
            caller_pid,
            target,
            reason,
        );
        Ok(())
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
                if let ProcessSlot::Present(ScheduledProcess(caller)) = &mut *slot {
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
            if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
                p.add_monitor(mon);
            }
        }

        // Add monitor to target process.
        if let Some(entry) = self.shared.process_bodies.get(&target_pid) {
            let mut slot = lock_or_recover(&entry);
            if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
                p.add_monitor(mon);
            }
        }

        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_monitored(caller_pid, target_pid, reference);

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
                if let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot {
                    process.remove_monitor(reference);
                }
            }
            if let Some(entry) = self.shared.process_bodies.get(&monitor.target()) {
                let mut slot = lock_or_recover(&entry);
                if let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot {
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
            match &mut *slot {
                ProcessSlot::Present(ScheduledProcess(target)) => {
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
                ProcessSlot::Executing(metadata) => {
                    let should_die = reason == ExitReason::Kill
                        || (reason != ExitReason::Normal && !metadata.trap_exit);
                    if should_die {
                        let terminal = link::terminal_reason(reason);
                        shared_exit_tombstone(&self.shared, target_pid, terminal);
                    } else if reason != ExitReason::Normal && metadata.trap_exit {
                        metadata
                            .pending_exit_messages
                            .push((PendingExitSource::Local(_caller_pid), reason));
                        drop(slot);
                        drop(entry);
                        wake_process(&self.shared, target_pid);
                    }
                }
                ProcessSlot::Absent => {}
            }
        }
        Ok(())
    }
}

fn shared_exit_tombstone(shared: &SharedState, pid: u64, reason: ExitReason) {
    shared.exit_tombstones.insert(pid, reason);
    let _deleted_tables = shared.transfer_or_delete_tables_owned_by(pid);
    let mut ls = lock_or_recover(&shared.link_set);
    ls.process_exited_tombstone(pid, reason);
}
