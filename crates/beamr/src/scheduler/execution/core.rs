//! Private process time-slice execution helpers.

use crate::hook::HookDecision;
use crate::interpreter::{self, ExecutionResult};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, ExitReason, Process, ProcessStatus};
use crate::term::Term;
use std::sync::Arc;

use crate::scheduler::{
    DEFAULT_REDUCTION_BUDGET, ProcessMetadata, ProcessSlot, RunQueue, ScheduledProcess,
    SharedState, lock_or_recover, namespace_registry, supervision_integration, timer_integration,
};

pub(in crate::scheduler) enum SliceOutcome {
    Requeue(Process),
    Wait(Process),
    Suspended(Process),
    Exited(ExitReason, Term),
}

pub(super) fn run_process(shared: &Arc<SharedState>, queue: &RunQueue, pid: u64, my_index: usize) {
    if shared.process_table.get(pid).is_none() {
        return;
    }
    let Some(mut process) = take_runnable_process(shared, pid) else {
        return;
    };
    let outcome = execute_slice(shared, &mut process);
    if let Some(reason) = tombstone_reason(shared, pid) {
        store_runnable_process(shared, process);
        cleanup_exited_process(shared, pid, reason);
        return;
    }
    match outcome {
        SliceOutcome::Requeue(process) => {
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            queue.push(pid);
        }
        SliceOutcome::Wait(mut process) => {
            timer_integration::register_receive_timer(shared, &mut process);
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            let mut ws = lock_or_recover(&shared.wait_set);
            ws.waiting.insert(pid, my_index);
        }
        SliceOutcome::Suspended(process) => {
            store_runnable_process(shared, process);
            if cleanup_if_tombstoned_after_store(shared, pid) {
                return;
            }
            let mut ws = lock_or_recover(&shared.wait_set);
            ws.waiting.insert(pid, my_index);
        }
        SliceOutcome::Exited(reason, result) => {
            shared.exit_results.insert(pid, result);
            store_runnable_process(shared, process);
            cleanup_exited_process(shared, pid, reason);
        }
    }
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
                links: process.links().to_vec(),
                monitors: process.monitors().to_vec(),
                trap_exit: process.trap_exit(),
                pending_exit_messages: Vec::new(),
                pending_down_messages: Vec::new(),
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
            for linked_pid in &metadata.links {
                process.add_link(*linked_pid);
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
            for (source_pid, reason) in metadata.pending_exit_messages.drain(..) {
                crate::supervision::link::enqueue_exit_message_pub(
                    &mut process,
                    source_pid,
                    reason,
                );
            }
            for (reference, target_pid, reason) in metadata.pending_down_messages.drain(..) {
                crate::supervision::monitor::enqueue_down_message_pub(
                    &mut process,
                    reference,
                    target_pid,
                    reason,
                );
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

pub(in crate::scheduler) fn execute_slice(
    shared: &Arc<SharedState>,
    process: &mut Process,
) -> SliceOutcome {
    if !matches!(
        process.status(),
        ProcessStatus::New
            | ProcessStatus::Yielded
            | ProcessStatus::Waiting
            | ProcessStatus::Suspended
    ) {
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    if process.transition_to(ProcessStatus::Running).is_err() {
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    if let Some((_, result_term)) = shared.async_results.remove(&process.pid()) {
        process.set_x_reg(0, result_term);
        if let Some(pos) = process.code_position() {
            process.set_code_position(Some(CodePosition {
                module: pos.module,
                instruction_pointer: pos.instruction_pointer.saturating_add(1),
            }));
        }
    }
    process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
    let module_atom = match process.code_position() {
        Some(position) => position.module,
        None => return exit_process(shared, process, ExitReason::Normal),
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
            return exit_process(shared, process, ExitReason::Error);
        };
        process.set_current_module(Arc::clone(&module));
        module
    };
    let services = supervision_integration::build_native_services(shared, process.namespace_id());
    let result = interpreter::run_with_native_services(process, &module, &registry, &services);
    let reductions = DEFAULT_REDUCTION_BUDGET.saturating_sub(process.reduction_counter());
    if matches!(
        result,
        Ok(ExecutionResult::Yielded) | Ok(ExecutionResult::Waiting)
    ) && timer_integration::invoke_hook(shared, process, reductions) == HookDecision::Suspend
    {
        let _t = process.transition_to(ProcessStatus::Suspended);
        return SliceOutcome::Suspended(take_process(process));
    }
    match result {
        Ok(ExecutionResult::Yielded) => {
            let _t = process.transition_to(ProcessStatus::Yielded);
            process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
            SliceOutcome::Requeue(take_process(process))
        }
        Ok(ExecutionResult::Waiting) => {
            let _t = process.transition_to(ProcessStatus::Waiting);
            SliceOutcome::Wait(take_process(process))
        }
        Ok(ExecutionResult::Exited(reason)) => exit_process(shared, process, reason),
        Err(error) => {
            let pid = process.pid();
            shared.exit_errors.insert(pid, error);
            exit_process(shared, process, ExitReason::Error)
        }
    }
}

fn exit_process(shared: &SharedState, process: &mut Process, reason: ExitReason) -> SliceOutcome {
    let pid = process.pid();
    let result = process.x_reg(0);
    if let Some(exception) = process.current_exception() {
        shared.exit_exceptions.insert(pid, exception);
    }
    process.terminate(reason);
    SliceOutcome::Exited(reason, result)
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
    supervision_integration::propagate_exit(shared, pid, reason);
    let _removed = shared.process_table.remove(pid);
    let _removed_body = shared.process_bodies.remove(&pid);
    let mut wait_set = lock_or_recover(&shared.wait_set);
    wait_set.waiting.remove(&pid);
    wait_set.woken.retain(|(woken_pid, _)| *woken_pid != pid);
}

fn take_process(process: &mut Process) -> Process {
    std::mem::replace(process, Process::new(u64::MAX, DEFAULT_HEAP_SIZE))
}
