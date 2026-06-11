//! Private process time-slice execution helpers.

use crate::atom::Atom;
use crate::ets::copy::OwnedTerm;
use crate::gc::release_all_refcounted_resources;
use crate::hook::HookDecision;
use crate::interpreter::{self, ExecutionResult};
use crate::io::resource::close_owned_resource_at;
use crate::native::{ExceptionClass, NativeEntry, ProcessContext};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{
    CodePosition, ExitReason, Process, ProcessStatus, SuspensionKind, SuspensionRecord,
};
use crate::scheduler::dirty::{DirtyJob, DirtyResult, DirtySchedulerKind, oneshot};
use crate::scheduler::suspension::{self, SuspensionResultPayload};
use crate::term::{Term, boxed::BoxedTag};
use std::sync::Arc;

use crate::replay::RecordedSchedule;
#[cfg(test)]
use crate::scheduler::ParkGap;
use crate::scheduler::{
    DEFAULT_REDUCTION_BUDGET, ProcessMetadata, ProcessSlot, RunQueue, ScheduledProcess,
    SharedState, lock_or_recover, namespace_registry, supervision_integration, timer_integration,
};

/// Runs the test-only park-gap hook, if installed, at an interleaving point
/// inside `run_process`'s park sequences.
#[cfg(test)]
fn invoke_park_gap_hook(shared: &SharedState, gap: ParkGap, pid: u64) {
    let hook = lock_or_recover(&shared.park_gap_hook);
    if let Some(hook) = hook.as_ref() {
        hook(shared, gap, pid);
    }
}

pub(in crate::scheduler) enum SliceOutcome {
    Requeue(Process),
    Wait(Process),
    Suspended(Process),
    /// The result is captured as an owning copy before `Process::terminate`
    /// frees the heap it pointed into.
    Exited(ExitReason, OwnedTerm),
}

pub(super) fn run_process(shared: &Arc<SharedState>, queue: &RunQueue, pid: u64, my_index: usize) {
    if shared.process_table.get(pid).is_none() {
        return;
    }
    let Some(mut process) = take_runnable_process(shared, pid) else {
        return;
    };
    let outcome = if shared.replay_mode {
        let Some(recorded_schedule) = take_replay_schedule(shared, pid, my_index) else {
            store_runnable_process(shared, process);
            cleanup_exited_process(shared, pid, ExitReason::Error);
            return;
        };
        execute_slice_with_recorded_schedule(shared, &mut process, recorded_schedule)
    } else {
        execute_slice(shared, &mut process)
    };
    if let Some(reason) = tombstone_reason(shared, pid) {
        store_runnable_process(shared, process);
        cleanup_exited_process(shared, pid, reason);
        return;
    }
    match outcome {
        SliceOutcome::Requeue(process) => {
            let priority = process.priority();
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            queue.push_with_priority(pid, priority);
        }
        SliceOutcome::Wait(mut process) => {
            timer_integration::register_receive_timer(shared, &mut process);
            let priority = process.priority();
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            // Park ordering against a concurrent deliver→wake (the sender
            // pushes the message under the slot lock, then calls
            // `wake_process`, which is a no-op unless the pid is registered
            // in `waiting`). Register BEFORE the final mailbox recheck so
            // every interleaving resolves to a scheduled process:
            //
            // 1. Delivery completes before the registration below: the wake
            //    no-ops, but the message is already visible (pushed into the
            //    mailbox, or merged from pending metadata by the store-back
            //    above), so the recheck sees it and self-wakes.
            // 2. Delivery lands between registration and the recheck: the
            //    wake moves the pid from `waiting` to `woken`; the recheck
            //    also sees the message, but its `waiting` removal finds
            //    nothing, so the process is scheduled exactly once (by the
            //    woken drain).
            // 3. Delivery lands after the recheck: the wake finds the pid in
            //    `waiting` and schedules it.
            //
            // Rechecking before registering (the previous order) lost
            // interleaving 1: a delivery in that gap woke nobody and the
            // process parked forever.
            // Gap hook: must stay between the store-back above and the
            // wait-set registration below — move it with them.
            #[cfg(test)]
            invoke_park_gap_hook(shared, ParkGap::WaitStored, pid);
            {
                let mut ws = lock_or_recover(&shared.wait_set);
                ws.waiting.insert(pid, my_index);
            }
            // Gap hook: must stay between the registration above and the
            // recheck below — move it with them.
            #[cfg(test)]
            invoke_park_gap_hook(shared, ParkGap::WaitRegistered, pid);
            // The recheck must notice EVERY wake source that can land before
            // the registration above: a delivered message, a receive timer
            // that fired while the slot was Executing or in the
            // store-to-register gap, and — for a host-await suspension — a
            // completion published while the slot was Executing
            // (expire_timers/wake_process found nothing in `waiting`, so
            // only this recheck can schedule the process; the event is
            // consumed at the start of the next slice). A stale mark costs
            // one benign spurious wake. A gated host-await park is treated
            // exactly like wake_process treats it: a queued message alone
            // must NOT self-wake it, or the await native would re-execute
            // and re-submit its host call. Message-wakeable parks (plain
            // receives, select, marker awaits) additionally self-wake on a
            // completion published while the slot was Executing.
            let gated = shared
                .suspensions
                .get(&pid)
                .is_some_and(|mirror| !mirror.wake_on_message);
            let wake_worthy = if gated {
                shared.has_consumable_suspension_event(pid)
            } else {
                process_has_queued_messages(shared, pid)
                    || timer_integration::has_pending_expired_timer(shared, pid)
                    || shared.has_consumable_suspension_event(pid)
            };
            if wake_worthy {
                let self_woke = {
                    let mut ws = lock_or_recover(&shared.wait_set);
                    ws.waiting.remove(&pid).is_some()
                };
                if self_woke {
                    queue.push_with_priority(pid, priority);
                }
            }
        }
        SliceOutcome::Suspended(process) => {
            // Suspended means a dirty native call is in flight or the hook
            // suspended the process (host-side request_suspend parks through
            // Waiting/Wait instead). Mailbox arrivals while parked here are
            // normal and must NOT resume the process — only the suspension's
            // own event may: the dirty completion published under the
            // suspension's call id, or a matching embedder resume. That
            // event arrives via resume_suspended or, when it landed before
            // this registration, the check below.
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            // Gap hook: must stay between the store-back above and the
            // wait-set registration below — move it with them.
            #[cfg(test)]
            invoke_park_gap_hook(shared, ParkGap::SuspendStored, pid);
            {
                let mut ws = lock_or_recover(&shared.wait_set);
                ws.waiting.insert(pid, my_index);
            }
            // The completion bridge publishes the dirty result, then calls
            // `resume_suspended` (flip status Suspended→Yielded, move the
            // pid from `waiting` to `woken`). Interleavings against the
            // store-back and registration above:
            //
            // 1. Bridge resume before the store-back: the slot is still
            //    Executing, so the status flip is refused and nothing is
            //    resumed; the resume below (status Suspended, pid
            //    registered) succeeds.
            // 2. Bridge resume between store-back and registration: the
            //    status flips to Yielded but the `waiting` removal finds
            //    nothing; the resume below then refuses because the status
            //    is no longer Suspended. The result is already published,
            //    so the only missing step is the unpark — performed by the
            //    fallback below.
            // 3. Bridge resume after registration: it fully succeeds; the
            //    resume below refuses (status already Yielded) and the
            //    fallback's `waiting` removal finds the pid already moved
            //    to `woken` — a no-op.
            //
            // The fallback re-verifies the pending event under the wait-set
            // lock: if a woken slice already consumed it (and possibly
            // parked again under a NEW suspension with a fresh call id),
            // the unpark must not fire. The event check is identity-keyed,
            // so a completion for the OLD suspension can never unpark the
            // NEW one — and even if an interleaving slips a stray unpark
            // through, the slice-start gate re-parks without executing.
            if shared.has_consumable_suspension_event(pid)
                && !timer_integration::resume_suspended(shared, pid)
            {
                let mut ws = lock_or_recover(&shared.wait_set);
                if shared.has_consumable_suspension_event(pid)
                    && let Some(index) = ws.waiting.remove(&pid)
                {
                    ws.woken.push((pid, index));
                    shared.wake_condvar.notify_all();
                }
            }
        }
        SliceOutcome::Exited(reason, result) => {
            shared.exit_results.insert(pid, result);
            store_runnable_process(shared, process);
            cleanup_exited_process(shared, pid, reason);
        }
    }
}

fn take_replay_schedule(
    shared: &SharedState,
    pid: u64,
    scheduler_index: usize,
) -> Option<RecordedSchedule> {
    let replay_driver = shared.replay_driver.as_ref()?;
    let mut guard = match replay_driver.lock() {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    };
    let recorded = match guard.next_schedule(scheduler_index) {
        Ok(recorded) => recorded,
        Err(error) => {
            shared.exit_errors.insert(pid, error.into());
            return None;
        }
    };
    if recorded.pid == pid {
        Some(recorded)
    } else {
        shared.exit_errors.insert(
            pid,
            crate::error::ExecError::ReplayMismatch(format!(
                "schedule pid mismatch: expected pid {}, recorded pid {}",
                pid, recorded.pid
            )),
        );
        None
    }
}

fn validate_replay_schedule_reductions(
    shared: &SharedState,
    recorded_schedule: Option<RecordedSchedule>,
    reductions: u32,
) -> Result<(), ()> {
    let Some(recorded_schedule) = recorded_schedule else {
        return Ok(());
    };
    let Some(replay_driver) = shared.replay_driver.as_ref() else {
        return Ok(());
    };
    let guard = match replay_driver.lock() {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    };
    guard
        .validate_schedule_reductions(recorded_schedule, reductions)
        .map_err(|error| {
            shared
                .exit_errors
                .insert(recorded_schedule.pid, error.into());
        })
}

pub(in crate::scheduler) fn take_runnable_process(
    shared: &SharedState,
    pid: u64,
) -> Option<Process> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    match std::mem::take(&mut *slot) {
        ProcessSlot::Present(scheduled) => {
            let process = scheduled.0;
            let metadata = ProcessMetadata {
                namespace_id: process.namespace_id(),
                capabilities: process.capabilities().clone(),
                links: process.links().to_vec(),
                remote_links: process.remote_links().to_vec(),
                monitors: process.monitors().to_vec(),
                trap_exit: process.trap_exit(),
                priority: process.priority(),
                current_mfa: process.current_mfa(),
                heap_size: process.heap().total_used(),
                binary_heap_size: process.virtual_binary_heap(),
                message_queue_len: process.mailbox().message_count(),
                group_leader: process.group_leader(),
                logical_clock: process.logical_clock(),
                pending_exit_messages: Vec::new(),
                pending_down_messages: Vec::new(),
                pending_io_messages: Vec::new(),
                pending_distribution_payloads: Vec::new(),
                pending_ets_transfer_messages: Vec::new(),
                pending_udp_messages: Vec::new(),
                pending_tcp_messages: Vec::new(),
            };
            *slot = ProcessSlot::Executing(metadata);
            Some(process)
        }
        other => {
            *slot = other;
            None
        }
    }
}

pub(in crate::scheduler) fn store_runnable_process(shared: &SharedState, mut process: Process) {
    let pid = process.pid();
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Executing(metadata) = &mut *slot {
            process.set_group_leader(metadata.group_leader);
            process.set_logical_clock(metadata.logical_clock);
            process.set_capabilities(metadata.capabilities.clone());
            for linked_pid in &metadata.links {
                process.add_link(*linked_pid);
            }
            for remote_link in &metadata.remote_links {
                process.add_remote_link(*remote_link);
            }
            for monitor in process.monitors().to_vec() {
                if !metadata
                    .monitors
                    .iter()
                    .any(|metadata_monitor| metadata_monitor.reference() == monitor.reference())
                {
                    process.remove_monitor(monitor.reference());
                }
            }
            for monitor in &metadata.monitors {
                if !process
                    .monitors()
                    .iter()
                    .any(|process_monitor| process_monitor.reference() == monitor.reference())
                {
                    process.add_monitor(*monitor);
                }
            }
            for (source, reason) in metadata.pending_exit_messages.drain(..) {
                match source {
                    crate::scheduler::process_slot::PendingExitSource::Local(source_pid) => {
                        crate::supervision::link::enqueue_exit_message_pub(
                            &mut process,
                            source_pid,
                            reason,
                        );
                    }
                    crate::scheduler::process_slot::PendingExitSource::Remote(remote_pid) => {
                        crate::supervision::link::enqueue_remote_exit_message_pub(
                            &mut process,
                            remote_pid,
                            reason,
                        );
                    }
                }
            }
            for (reference, target_pid, reason) in metadata.pending_down_messages.drain(..) {
                crate::supervision::monitor::enqueue_down_message_pub(
                    &mut process,
                    reference,
                    target_pid,
                    reason,
                );
            }
            for message in metadata.pending_io_messages.drain(..) {
                process.mailbox_mut().push_owned(message);
            }
            for payload in metadata.pending_distribution_payloads.drain(..) {
                let mut context = crate::native::ProcessContext::new();
                context.attach_process(&mut process, 0);
                let Ok(message) =
                    crate::etf::decode::decode_term(&payload, &mut context, &shared.atom_table)
                else {
                    continue;
                };
                process.mailbox_mut().push_owned(message);
            }
            let transfer_atom = shared.atom_table.intern("ETS-TRANSFER");
            for message in metadata.pending_ets_transfer_messages.drain(..) {
                let Some(message) =
                    crate::scheduler::supervision_integration::build_ets_transfer_message(
                        &mut process,
                        transfer_atom,
                        message.table_id,
                        message.from_pid,
                        message.data.root(),
                    )
                else {
                    continue;
                };
                process.mailbox_mut().push_owned(message);
            }
            for udp_msg in metadata.pending_udp_messages.drain(..) {
                if let Some(message) = super::build_udp_active_message_for_process(
                    &shared.atom_table,
                    &mut process,
                    &udp_msg.fd,
                    &udp_msg.bytes,
                    udp_msg.addr,
                ) {
                    process.mailbox_mut().push_owned(message);
                }
            }
            for tcp_msg in metadata.pending_tcp_messages.drain(..) {
                if let Some(message) = super::build_tcp_active_message_for_process(
                    &shared.atom_table,
                    &mut process,
                    &tcp_msg.fd,
                    &tcp_msg.bytes,
                ) {
                    process.mailbox_mut().push_owned(message);
                }
            }
        }
        *slot = ProcessSlot::Present(ScheduledProcess(process));
    } else {
        shared.process_bodies.insert(
            pid,
            std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
        );
    }
}

pub(in crate::scheduler) fn cleanup_if_tombstoned_after_store(
    shared: &SharedState,
    pid: u64,
) -> bool {
    if let Some(reason) = tombstone_reason(shared, pid) {
        cleanup_exited_process(shared, pid, reason);
        true
    } else {
        false
    }
}

fn tombstone_reason(shared: &SharedState, pid: u64) -> Option<ExitReason> {
    shared.exit_tombstones.get(&pid).map(|reason| *reason)
}

fn process_has_queued_messages(shared: &SharedState, pid: u64) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let slot = lock_or_recover(&entry);
    match &*slot {
        ProcessSlot::Present(ScheduledProcess(process)) => !process.mailbox().is_empty(),
        ProcessSlot::Executing(_) | ProcessSlot::Absent => false,
    }
}

pub(in crate::scheduler) fn execute_slice(
    shared: &Arc<SharedState>,
    process: &mut Process,
) -> SliceOutcome {
    execute_slice_with_budget(shared, process, None)
}

pub(in crate::scheduler) fn execute_slice_with_recorded_schedule(
    shared: &Arc<SharedState>,
    process: &mut Process,
    recorded_schedule: RecordedSchedule,
) -> SliceOutcome {
    execute_slice_with_budget(shared, process, Some(recorded_schedule))
}

fn execute_slice_with_budget(
    shared: &Arc<SharedState>,
    process: &mut Process,
    recorded_schedule: Option<RecordedSchedule>,
) -> SliceOutcome {
    if !matches!(
        process.status(),
        ProcessStatus::New
            | ProcessStatus::Yielded
            | ProcessStatus::Waiting
            | ProcessStatus::Suspended
    ) {
        return SliceOutcome::Exited(
            exit_reason_from_status(process.status()),
            crate::scheduler::exit_capture::capture_term(process.x_reg(0)),
        );
    }
    if transition_to_running(process).is_err() {
        return SliceOutcome::Exited(
            exit_reason_from_status(process.status()),
            crate::scheduler::exit_capture::capture_term(process.x_reg(0)),
        );
    }
    #[cfg(feature = "telemetry")]
    let span = start_slice_span(shared, process);
    // Suspension gate: a result-gated suspension (host await, dirty call,
    // hook suspend) is consumed here, on the owning thread, only by its
    // matching event — the completion published under the suspension's call
    // id, a file-I/O completion, the receive timeout, or a matching embedder
    // resume. Stale completions are dropped; with nothing consumable the
    // process re-parks untouched, so a stray wake can never re-execute the
    // parked call instruction (and double-submit its side effect).
    match consume_suspension_event(shared, process) {
        SuspensionGate::Run => {}
        SuspensionGate::Repark(kind) => {
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                0,
                crate::telemetry::spans::SliceSpanOutcome::Waiting,
            );
            return repark_suspended(process, kind);
        }
        SuspensionGate::Exit(reason) => {
            let outcome = exit_process(shared, process, reason);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                0,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            return outcome;
        }
        SuspensionGate::Error(error) => {
            let pid = process.pid();
            shared.exit_errors.insert(pid, error);
            let outcome = exit_process(shared, process, ExitReason::Error);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                0,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            return outcome;
        }
    }
    let reduction_budget = recorded_schedule.map_or(DEFAULT_REDUCTION_BUDGET, |recorded| {
        recorded.reduction_budget
    });
    process.reset_reductions(reduction_budget);
    let module_atom = match process.code_position() {
        Some(position) => position.module,
        None => {
            let outcome = exit_process(shared, process, ExitReason::Normal);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                0,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            return outcome;
        }
    };
    let registry = namespace_registry(shared, process.namespace_id())
        .unwrap_or_else(|| Arc::clone(&shared.module_registry));
    let module = if let Some(current) = process.current_module()
        && current.name == module_atom
        && current
            .code
            .get(
                process
                    .code_position()
                    .map_or(usize::MAX, |pos| pos.instruction_pointer),
            )
            .is_some()
    {
        Arc::clone(current)
    } else {
        let Some(module) = registry.lookup(module_atom) else {
            let outcome = exit_process(shared, process, ExitReason::Error);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                0,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            return outcome;
        };
        process.set_current_module(Arc::clone(&module));
        module
    };
    let services = supervision_integration::build_native_services(shared, process.namespace_id());
    let result = interpreter::run_with_native_services(process, &module, &registry, &services);
    let reductions = reduction_budget.saturating_sub(process.reduction_counter());
    if validate_replay_schedule_reductions(shared, recorded_schedule, reductions).is_err() {
        let outcome = exit_process(shared, process, ExitReason::Error);
        #[cfg(feature = "telemetry")]
        finish_slice_span(
            shared,
            process,
            span,
            reductions,
            crate::telemetry::spans::SliceSpanOutcome::Exited,
        );
        return outcome;
    }
    if matches!(
        result,
        Ok(ExecutionResult::Yielded) | Ok(ExecutionResult::Waiting)
    ) && timer_integration::invoke_hook(shared, process, reductions) == HookDecision::Suspend
        // A hook suspend must NOT target an await-parked slice: a Waiting
        // slice that parked through request_suspend/request_await_suspend
        // already carries a result-gated suspension record, and installing
        // the Hook record over it would invalidate the await's call id —
        // published completions would be dropped as stale and the eventual
        // embedder resume would re-execute the parked await native,
        // double-submitting its host side effect. The hook still observes
        // the slice; its Suspend decision is ignored and the process parks
        // under the await it requested.
        && process.suspension().is_none()
    {
        // Record the hook suspension's identity before parking so an
        // embedder resume_process targets exactly this suspension and a
        // resume racing the park gap is consumed by the slice-start gate.
        let call_id = process.allocate_suspension_call_id();
        process.set_suspension(Some(SuspensionRecord {
            call_id,
            kind: SuspensionKind::Hook,
            position: process.code_position(),
            wake_on_message: false,
        }));
        shared.register_suspension_mirror(process.pid(), call_id, SuspensionKind::Hook, false);
        let _t = process.transition_to(ProcessStatus::Suspended);
        #[cfg(feature = "telemetry")]
        finish_slice_span(
            shared,
            process,
            span,
            reductions,
            crate::telemetry::spans::SliceSpanOutcome::Waiting,
        );
        return SliceOutcome::Suspended(take_process(process));
    }
    match result {
        Ok(ExecutionResult::Yielded) => {
            let _t = process.transition_to(ProcessStatus::Yielded);
            process.reset_reductions(reduction_budget);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Yielded,
            );
            SliceOutcome::Requeue(take_process(process))
        }
        Ok(ExecutionResult::Waiting) => {
            let _t = process.transition_to(ProcessStatus::Waiting);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Waiting,
            );
            SliceOutcome::Wait(take_process(process))
        }
        Ok(ExecutionResult::DirtyCall {
            entry,
            args,
            module,
            function,
            arity,
            kind,
        }) => {
            // Record the dirty call's identity before submission so the
            // completion bridge publishes its result under this exact call
            // id and the wake gate holds the process parked meanwhile.
            let call_id = process.allocate_suspension_call_id();
            process.set_suspension(Some(SuspensionRecord {
                call_id,
                kind: SuspensionKind::DirtyCall,
                position: process.code_position(),
                wake_on_message: false,
            }));
            shared.register_suspension_mirror(
                process.pid(),
                call_id,
                SuspensionKind::DirtyCall,
                false,
            );
            if let Err(error) = submit_dirty_call(
                shared,
                process,
                call_id,
                entry,
                args,
                (module, function, arity),
                kind,
            ) {
                let _withdrawn = process.take_suspension();
                shared.suspensions.remove(&process.pid());
                shared.exit_errors.insert(process.pid(), error.into());
                let outcome = exit_process(shared, process, ExitReason::Error);
                #[cfg(feature = "telemetry")]
                finish_slice_span(
                    shared,
                    process,
                    span,
                    reductions,
                    crate::telemetry::spans::SliceSpanOutcome::Exited,
                );
                return outcome;
            }
            let _t = process.transition_to(ProcessStatus::Suspended);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Waiting,
            );
            SliceOutcome::Suspended(take_process(process))
        }
        Ok(ExecutionResult::Exited(reason)) => {
            let outcome = exit_process(shared, process, reason);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            outcome
        }
        Err(error) => {
            let pid = process.pid();
            shared.exit_errors.insert(pid, error);
            let outcome = exit_process(shared, process, ExitReason::Error);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                shared,
                process,
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            outcome
        }
    }
}

/// Move a schedulable process to Running. A Suspended process reaches the
/// owning thread without the resume flip only on a spurious wake; route it
/// through Yielded so the lifecycle graph holds (the slice-start gate then
/// re-parks it unless a consumable event is pending).
fn transition_to_running(process: &mut Process) -> Result<(), ()> {
    if process.status() == ProcessStatus::Suspended
        && process.transition_to(ProcessStatus::Yielded).is_err()
    {
        return Err(());
    }
    process
        .transition_to(ProcessStatus::Running)
        .map_err(|_| ())
}

/// Decision produced by the slice-start suspension gate.
enum SuspensionGate {
    /// No live suspension, or its event was consumed: run the interpreter.
    Run,
    /// The suspension has no consumable event: park again untouched.
    Repark(SuspensionKind),
    /// Applying a dirty exception unwound the process to an exit.
    Exit(ExitReason),
    /// Applying the completion failed.
    Error(crate::error::ExecError),
}

/// Consume `process`'s pending suspension event, if any. See the call site
/// in [`execute_slice_with_budget`] for the protocol contract.
fn consume_suspension_event(shared: &SharedState, process: &mut Process) -> SuspensionGate {
    let pid = process.pid();
    let Some(record) = process.suspension() else {
        // No live suspension: any published completion or mirror is the
        // orphan of an abandoned/superseded suspend request — drop it,
        // then apply a plain receive timer fire.
        let _stale_mirror = shared.suspensions.remove(&pid);
        let _orphan_result = shared.suspension_results.remove(&pid);
        let _jumped = timer_integration::apply_expired_receive_timer(shared, process);
        return SuspensionGate::Run;
    };
    if let Some((_, result)) = shared
        .suspension_results
        .remove_if(&pid, |_, result| result.call_id == record.call_id)
    {
        let _consumed = process.take_suspension();
        shared.suspensions.remove(&pid);
        if record.position != process.code_position() {
            // The identity matched but the park position moved: a protocol
            // violation. Refuse to mutate the instruction pointer blind —
            // that desync is exactly the "invalid operand for instruction
            // pointer" crash class this gate exists to prevent.
            debug_assert_eq!(
                record.position,
                process.code_position(),
                "suspension result applied at a different position than it was produced"
            );
            return SuspensionGate::Error(crate::error::ExecError::InvalidOperand(
                "suspension result position",
            ));
        }
        // The completion owns the timed-await lifecycle: clear the timeout
        // metadata so a raced timer fire is dropped as stale and a later
        // plain wait cannot arm a timer at this stale resume position.
        process.set_receive_timeout(None);
        process.set_receive_timer_ref(None);
        let _stale_marks = shared.expired_receive_timers.remove(&pid);
        return match result.payload {
            SuspensionResultPayload::Host(term) => {
                process.set_x_reg(0, term);
                advance_past_current_instruction(process);
                SuspensionGate::Run
            }
            SuspensionResultPayload::Dirty(mut dirty_result) => {
                // Follow-up requests from the dirty native (B-5b): a
                // re-suspend re-parks at the dirty call instruction under a
                // NEW host-await suspension; a trampoline sets up the
                // requested closure call. Only honored on the Ok path (the
                // exception wins, matching call_native_entry).
                if dirty_result.result.is_ok() {
                    if let Some(suspend) = dirty_result.suspend.take() {
                        return apply_dirty_suspend(shared, process, suspend);
                    }
                    if let Some(trampoline) = dirty_result.trampoline.take() {
                        return apply_dirty_trampoline(shared, process, trampoline);
                    }
                }
                match apply_dirty_result(process, *dirty_result) {
                    Ok(InstructionOutcomeAfterDirty::Continue) => SuspensionGate::Run,
                    Ok(InstructionOutcomeAfterDirty::Exit(reason)) => SuspensionGate::Exit(reason),
                    Err(error) => SuspensionGate::Error(error),
                }
            }
        };
    }
    // A published completion that did not match above is stale — produced
    // for an earlier suspension whose await already timed out — and must
    // never be applied. (remove_if keeps a *matching* completion that lands
    // concurrently: its wake re-schedules this process.)
    let _stale_result = shared
        .suspension_results
        .remove_if(&pid, |_, result| result.call_id != record.call_id);
    match record.kind {
        SuspensionKind::HostAwait => {
            if shared.file_io_results.contains_key(&pid) {
                // A ring completion resumes the await by re-executing the
                // native at the park position; the native consumes it via
                // take_file_io_completion. The suspension itself is done.
                let _consumed = process.take_suspension();
                shared.suspensions.remove(&pid);
                process.set_receive_timeout(None);
                process.set_receive_timer_ref(None);
                let _stale_marks = shared.expired_receive_timers.remove(&pid);
                SuspensionGate::Run
            } else if timer_integration::apply_expired_receive_timer(shared, process) {
                // Timed out: the native re-executes at the recorded timeout
                // position. receive_timeout stays set so the native observes
                // receive_timeout_expired and reports the timeout. This
                // suspension is superseded; its late completion becomes an
                // orphan and is dropped, never applied.
                let _consumed = process.take_suspension();
                shared.suspensions.remove(&pid);
                SuspensionGate::Run
            } else if record.wake_on_message {
                // Message-wakeable suspend (select, marker awaits): any
                // wake re-executes the re-entrant native, which scans the
                // mailbox and may suspend again under a NEW call id. The
                // current suspension ends here; a completion later
                // published for it is stale and will be dropped.
                let _consumed = process.take_suspension();
                shared.suspensions.remove(&pid);
                SuspensionGate::Run
            } else {
                SuspensionGate::Repark(SuspensionKind::HostAwait)
            }
        }
        SuspensionKind::DirtyCall => SuspensionGate::Repark(SuspensionKind::DirtyCall),
        SuspensionKind::Hook => {
            let matching_resume = shared.pending_resumes.remove_if(&pid, |_, resume| {
                *resume == suspension::RESUME_ANY_HOOK || *resume == record.call_id
            });
            if matching_resume.is_some() {
                let _consumed = process.take_suspension();
                shared.suspensions.remove(&pid);
                let _jumped = timer_integration::apply_expired_receive_timer(shared, process);
                SuspensionGate::Run
            } else {
                // A resume targeting an older hook suspension is stale.
                let _stale_resume = shared.pending_resumes.remove_if(&pid, |_, resume| {
                    *resume != suspension::RESUME_ANY_HOOK && *resume != record.call_id
                });
                SuspensionGate::Repark(SuspensionKind::Hook)
            }
        }
    }
}

/// Park a gated process again without running it: back to Waiting (host
/// awaits park through the Wait arm) or Suspended (dirty calls and hook
/// suspends park through the Suspended arm).
fn repark_suspended(process: &mut Process, kind: SuspensionKind) -> SliceOutcome {
    match kind {
        SuspensionKind::HostAwait => {
            let _t = process.transition_to(ProcessStatus::Waiting);
            SliceOutcome::Wait(take_process(process))
        }
        SuspensionKind::DirtyCall | SuspensionKind::Hook => {
            let _t = process.transition_to(ProcessStatus::Suspended);
            SliceOutcome::Suspended(take_process(process))
        }
    }
}

/// Re-park a process whose dirty native requested suspension: install a NEW
/// host-await suspension at the (unadvanced) dirty call instruction. The
/// completion arrives through the pid-resolved `Scheduler::wake_with_result`
/// (or `wake_with_result_for` once the embedder learns the id), the optional
/// timeout re-executes the dirty call, and — for a message-wakeable request —
/// any message re-executes the (re-entrant) dirty call.
fn apply_dirty_suspend(
    shared: &SharedState,
    process: &mut Process,
    suspend: crate::native::SuspendRequest,
) -> SuspensionGate {
    let Some(position) = process.code_position() else {
        return SuspensionGate::Error(crate::error::ExecError::InvalidOperand(
            "dirty suspend code position",
        ));
    };
    if let Some(timeout_ms) = suspend.timeout_ms {
        process.set_receive_timeout(Some(crate::process::ReceiveTimeout {
            timeout_position: position,
            milliseconds: timeout_ms,
        }));
    }
    // A detached dirty context cannot allocate from the process counter;
    // the id is allocated here, on the owning thread.
    let call_id = suspend
        .call_id
        .unwrap_or_else(|| process.allocate_suspension_call_id());
    process.set_suspension(Some(SuspensionRecord {
        call_id,
        kind: SuspensionKind::HostAwait,
        position: Some(position),
        wake_on_message: suspend.wake_on_message,
    }));
    shared.register_suspension_mirror(
        process.pid(),
        call_id,
        SuspensionKind::HostAwait,
        suspend.wake_on_message,
    );
    SuspensionGate::Repark(SuspensionKind::HostAwait)
}

/// Set up the closure call a dirty native requested: copy the owned fun and
/// arguments onto the resuming process heap and run the normal trampoline
/// path (which pushes the continuation under the suspension protocol's
/// position-gated readiness check).
fn apply_dirty_trampoline(
    shared: &SharedState,
    process: &mut Process,
    trampoline: crate::scheduler::dirty::OwnedDirtyTrampoline,
) -> SuspensionGate {
    let registry = namespace_registry(shared, process.namespace_id())
        .unwrap_or_else(|| Arc::clone(&shared.module_registry));
    let Some(position) = process.code_position() else {
        return SuspensionGate::Error(crate::error::ExecError::InvalidOperand(
            "dirty trampoline code position",
        ));
    };
    let module = if let Some(current) = process
        .current_module()
        .filter(|m| m.name == position.module)
    {
        Arc::clone(current)
    } else if let Some(module) = registry.lookup(position.module) {
        module
    } else {
        return SuspensionGate::Error(crate::error::ExecError::InvalidOperand(
            "dirty trampoline module",
        ));
    };
    let badarg = |_| crate::error::ExecError::Badarg;
    let fun = match trampoline
        .fun
        .copy_to_heap(process.heap_mut())
        .map_err(badarg)
    {
        Ok(fun) => fun,
        Err(error) => return SuspensionGate::Error(error),
    };
    let mut args = Vec::with_capacity(trampoline.args.len());
    for arg in &trampoline.args {
        match arg.copy_to_heap(process.heap_mut()).map_err(badarg) {
            Ok(arg) => args.push(arg),
            Err(error) => return SuspensionGate::Error(error),
        }
    }
    match crate::interpreter::opcodes::trampoline::handle_trampoline(
        process,
        &module,
        Some(&registry),
        crate::native::TrampolineRequest {
            fun,
            args,
            continuation: Some(trampoline.continuation),
        },
    ) {
        // Jump: the closure entry becomes the slice's starting position.
        Ok(crate::interpreter::InstructionOutcome::Jump(target)) => {
            process.set_code_position(Some(target));
            SuspensionGate::Run
        }
        // Yield: handle_trampoline already stored the target position.
        Ok(_) => SuspensionGate::Run,
        Err(error) => SuspensionGate::Error(error),
    }
}

#[cfg(feature = "telemetry")]
fn start_slice_span(
    shared: &SharedState,
    process: &Process,
) -> crate::telemetry::spans::ExecutionSliceSpan {
    crate::telemetry::spans::ExecutionSliceSpan::start(&shared.atom_table, process)
}

#[cfg(feature = "telemetry")]
fn finish_slice_span(
    shared: &SharedState,
    process: &Process,
    span: crate::telemetry::spans::ExecutionSliceSpan,
    reductions_consumed: u32,
    outcome: crate::telemetry::spans::SliceSpanOutcome,
) {
    shared.record_process_slice_metrics(process, reductions_consumed);
    span.finish(&shared.atom_table, process, reductions_consumed, outcome);
}

fn exit_process(shared: &SharedState, process: &mut Process, reason: ExitReason) -> SliceOutcome {
    let pid = process.pid();
    // Capture before `terminate` below replaces the heap the result points
    // into.
    let result = crate::scheduler::exit_capture::capture_term(process.x_reg(0));
    if let Some(exception) = process.current_exception() {
        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_crashed(&shared.atom_table, pid, exception);
        // Capture while the process heap is still alive; the exception terms
        // point into it and the heap is freed during cleanup. Raise-time raw
        // frames are resolved to names here so diagnostics keep a usable
        // stacktrace even when the exception term carries none.
        let frames = resolve_raise_frames(shared, process);
        shared.exit_exceptions.insert(
            pid,
            crate::scheduler::exit_capture::OwnedException::capture_with_frames(exception, frames),
        );
    } else if reason != ExitReason::Normal {
        #[cfg(feature = "telemetry")]
        crate::telemetry::lifecycle::record_process_crashed_reason(&shared.atom_table, pid, reason);
    }
    #[cfg(feature = "telemetry")]
    if let Some(trace_context) = process.trace_context() {
        trace_context.finish(exit_reason_label(reason));
    }
    process.terminate(reason);
    SliceOutcome::Exited(reason, result)
}

/// Resolve the raise-time raw stacktrace into owned, name-resolved frames.
fn resolve_raise_frames(
    shared: &SharedState,
    process: &Process,
) -> Vec<crate::scheduler::exit_capture::CapturedFrame> {
    process
        .raw_stacktrace()
        .iter()
        .map(|entry| {
            let (function, arity) = entry
                .mfa
                .map(|(_, function, arity)| (function, arity))
                .or_else(|| entry.module.function_at_ip(entry.ip))
                .unwrap_or((crate::atom::Atom::UNDEFINED, 0));
            crate::scheduler::exit_capture::CapturedFrame {
                module: shared
                    .atom_table
                    .resolve(entry.module.name)
                    .unwrap_or("#<unknown>")
                    .to_owned(),
                function: shared
                    .atom_table
                    .resolve(function)
                    .unwrap_or("#<unknown>")
                    .to_owned(),
                arity,
                line: entry.module.line_at_ip(entry.ip),
            }
        })
        .collect()
}

#[cfg(feature = "telemetry")]
const fn exit_reason_label(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::Normal => "normal",
        ExitReason::Kill => "kill",
        ExitReason::Killed => "killed",
        ExitReason::Error => "error",
        ExitReason::NoConnection => "noconnection",
    }
}

enum InstructionOutcomeAfterDirty {
    Continue,
    Exit(ExitReason),
}

fn apply_dirty_result(
    process: &mut Process,
    dirty_result: DirtyResult,
) -> Result<InstructionOutcomeAfterDirty, crate::error::ExecError> {
    let owned_result = dirty_result.owned_result;
    match dirty_result.result {
        Ok(value) => {
            let value = match owned_result.as_ref() {
                Some(owned) => owned
                    .copy_to_heap(process.heap_mut())
                    .map_err(|_| crate::error::ExecError::Badarg)?,
                None => value,
            };
            process.set_x_reg(0, value);
            advance_past_current_instruction(process);
            Ok(InstructionOutcomeAfterDirty::Continue)
        }
        Err(reason) => {
            let reason = match owned_result.as_ref() {
                Some(owned) => owned
                    .copy_to_heap(process.heap_mut())
                    .map_err(|_| crate::error::ExecError::Badarg)?,
                None => reason,
            };
            let exception = crate::process::Exception {
                class: Term::atom(exception_class_atom(dirty_result.exception_class)),
                reason,
                stacktrace: dirty_result.exception_stacktrace,
            };
            match crate::interpreter::opcodes::exceptions::raise_exception(process, exception)? {
                crate::interpreter::InstructionOutcome::Jump(target) => {
                    process.set_code_position(Some(target));
                    Ok(InstructionOutcomeAfterDirty::Continue)
                }
                crate::interpreter::InstructionOutcome::Exit(reason) => {
                    Ok(InstructionOutcomeAfterDirty::Exit(reason))
                }
                crate::interpreter::InstructionOutcome::Continue
                | crate::interpreter::InstructionOutcome::Yield
                | crate::interpreter::InstructionOutcome::Waiting
                | crate::interpreter::InstructionOutcome::NativeContinuation
                | crate::interpreter::InstructionOutcome::OnLoadComplete
                | crate::interpreter::InstructionOutcome::DirtyCall { .. } => {
                    Ok(InstructionOutcomeAfterDirty::Exit(ExitReason::Error))
                }
            }
        }
    }
}

fn advance_past_current_instruction(process: &mut Process) {
    if let Some(pos) = process.code_position() {
        process.set_code_position(Some(CodePosition {
            module: pos.module,
            instruction_pointer: pos.instruction_pointer.saturating_add(1),
        }));
    }
}

fn exception_class_atom(class: ExceptionClass) -> Atom {
    match class {
        ExceptionClass::Error => Atom::ERROR,
        ExceptionClass::Throw => Atom::THROW,
        ExceptionClass::Exit => Atom::EXIT_CLASS,
    }
}

fn submit_dirty_call(
    shared: &Arc<SharedState>,
    process: &Process,
    call_id: u64,
    entry: NativeEntry,
    args: Vec<Term>,
    mfa: (Atom, Atom, u8),
    kind: DirtySchedulerKind,
) -> Result<(), DirtySubmissionError> {
    if let Some(driver) = &shared.replay_driver {
        let (module, function, arity) = mfa;
        let recorded = match driver.lock() {
            Ok(mut guard) => guard.next_native_call(process.pid(), module, function, arity),
            Err(error) => {
                error
                    .into_inner()
                    .next_native_call(process.pid(), module, function, arity)
            }
        }
        .map_err(DirtySubmissionError::ReplayMismatch)?;
        let _published = shared.publish_suspension_result(
            process.pid(),
            call_id,
            SuspensionResultPayload::Dirty(Box::new(DirtyResult {
                result: recorded.outcome.result,
                exception_class: recorded.outcome.exception_class,
                exception_stacktrace: recorded.outcome.exception_stacktrace,
                owned_result: None,
                suspend: None,
                trampoline: None,
            })),
        );
        let _resumed = timer_integration::resume_suspended(shared, process.pid());
        return Ok(());
    }
    let mut context =
        ProcessContext::with_timer_services(process.pid(), Arc::clone(&shared.timers));
    let services = supervision_integration::build_native_services(shared, process.namespace_id());
    context.set_atom_table(services.atom_table);
    context.set_local_node(services.local_node);
    context.set_net_kernel(services.net_kernel);
    context.set_distribution_send_facility(services.distribution_send);
    context.set_spawn_facility(services.spawn_facility);
    context.set_remote_spawn_facility(services.remote_spawn_facility);
    context.set_link_facility(services.link_facility);
    context.set_group_leader_facility(services.group_leader_facility);
    context.set_supervision_facility(services.supervision_facility);
    context.set_process_info_facility(services.process_info_facility);
    context.set_code_management_facility(services.code_management_facility);
    context.set_system_info_facility(services.system_info_facility);
    context.set_replay_driver(services.replay_driver);
    context.set_nif_private_data(services.nif_private_data);
    context.set_suspension_registrar(services.suspension_registrar);
    if let Some(sink) = services.io_sink {
        context.set_io_sink(sink);
    }

    let (result_sender, result_receiver) = oneshot::channel();
    let pid = process.pid();
    let job = DirtyJob {
        pid,
        function: entry.function,
        args,
        context,
        result_sender,
    };
    // The caller registered the DirtyCall suspension mirror before this
    // submission: while no completion is published under its call id,
    // `wake_process` leaves the process parked so a mailbox arrival cannot
    // schedule a slice that re-executes the call instruction (and
    // double-submits the dirty call).
    if match kind {
        DirtySchedulerKind::Cpu => shared.dirty_cpu.submit(job),
        DirtySchedulerKind::Io => shared.dirty_io.submit(job),
    }
    .is_err()
    {
        return Err(DirtySubmissionError::PoolUnavailable);
    }

    let shared_for_completion = Arc::clone(shared);
    let bridge = std::thread::Builder::new()
        .name(format!("dirty-complete-{pid}"))
        .spawn(move || {
            let result = match result_receiver.recv() {
                Ok(result) => result,
                Err(_closed) => DirtyResult {
                    result: Err(Term::atom(Atom::ERROR)),
                    owned_result: None,
                    exception_class: ExceptionClass::Error,
                    exception_stacktrace: Term::NIL,
                    suspend: None,
                    trampoline: None,
                },
            };
            // Published under the submitting call's id: the owning thread
            // applies it only while this exact suspension is current, and
            // `publish_suspension_result`'s liveness double-check removes
            // the entry if the process exited concurrently.
            let _published = shared_for_completion.publish_suspension_result(
                pid,
                call_id,
                SuspensionResultPayload::Dirty(Box::new(result)),
            );
            let _resumed = timer_integration::resume_suspended(&shared_for_completion, pid);
        });
    if bridge.is_err() {
        return Err(DirtySubmissionError::CompletionBridgeSpawn);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DirtySubmissionError {
    PoolUnavailable,
    CompletionBridgeSpawn,
    ReplayMismatch(crate::replay::ReplayMismatch),
}

impl From<DirtySubmissionError> for crate::error::ExecError {
    fn from(error: DirtySubmissionError) -> Self {
        match error {
            DirtySubmissionError::PoolUnavailable => Self::Badarg,
            DirtySubmissionError::CompletionBridgeSpawn => Self::Badarg,
            DirtySubmissionError::ReplayMismatch(error) => Self::from(error),
        }
    }
}

fn exit_reason_from_status(status: ProcessStatus) -> ExitReason {
    match status {
        ProcessStatus::Exited(reason) => reason,
        _ => ExitReason::Error,
    }
}

pub(in crate::scheduler) fn cleanup_exited_process(
    shared: &SharedState,
    pid: u64,
    reason: ExitReason,
) {
    shared.exit_tombstones.insert(pid, reason);
    #[cfg(feature = "telemetry")]
    crate::telemetry::lifecycle::record_process_exited(&shared.atom_table, pid, reason);
    let _deleted_tables = shared.transfer_or_delete_tables_owned_by(pid);
    supervision_integration::propagate_exit(shared, pid, reason);
    close_owned_fd_resources_on_exit(shared, pid);
    let _removed = shared.process_table.remove(pid);
    let _removed_body = shared.process_bodies.remove(&pid);
    #[cfg(feature = "telemetry")]
    {
        shared.remove_process_metric_state(pid);
        shared.record_scheduler_executing(std::time::Duration::ZERO);
    }
    let mut wait_set = lock_or_recover(&shared.wait_set);
    wait_set.waiting.remove(&pid);
    wait_set.woken.retain(|(woken_pid, _)| *woken_pid != pid);
    drop(wait_set);
    let _stale_marks = shared.expired_receive_timers.remove(&pid);
    // Purge every (pid, *) suspension structure — mirrors, published
    // completions, sticky resumes, file-I/O completions — so a dead pid can
    // neither leak entries nor have a late completion misattributed.
    shared.purge_suspension_state(pid);
}

fn close_owned_fd_resources_on_exit(shared: &SharedState, pid: u64) {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return;
    };
    let mut slot = lock_or_recover(&entry);
    let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
        return;
    };

    process.heap().visit_boxed_objects(|ptr, tag, _words| {
        if tag == BoxedTag::FdResource {
            close_owned_resource_at(ptr, pid);
        }
    });
    release_all_refcounted_resources(process);
}

fn take_process(process: &mut Process) -> Process {
    std::mem::replace(process, Process::new(u64::MAX, DEFAULT_HEAP_SIZE))
}
