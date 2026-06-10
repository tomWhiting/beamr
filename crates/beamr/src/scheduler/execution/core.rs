//! Private process time-slice execution helpers.

use crate::atom::Atom;
use crate::gc::release_all_refcounted_resources;
use crate::hook::HookDecision;
use crate::interpreter::{self, ExecutionResult};
use crate::io::resource::close_owned_resource_at;
use crate::native::{ExceptionClass, NativeEntry, ProcessContext};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, ExitReason, Process, ProcessStatus};
use crate::scheduler::dirty::{DirtyJob, DirtyResult, DirtySchedulerKind, oneshot};
use crate::term::{Term, boxed::BoxedTag};
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
            if process_has_queued_messages(shared, pid) {
                timer_integration::cancel_receive_timer(shared, pid);
                queue.push_with_priority(pid, priority);
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
            {
                let mut ws = lock_or_recover(&shared.wait_set);
                ws.waiting.insert(pid, my_index);
            }
            if shared.dirty_results.contains_key(&pid) {
                let _resumed = timer_integration::resume_suspended(shared, pid);
            }
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
                remote_links: process.remote_links().to_vec(),
                monitors: process.monitors().to_vec(),
                trap_exit: process.trap_exit(),
                priority: process.priority(),
                current_mfa: process.current_mfa(),
                heap_size: process.heap().total_used(),
                binary_heap_size: process.virtual_binary_heap(),
                message_queue_len: process.mailbox().message_count(),
                group_leader: process.group_leader(),
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
    if !matches!(
        process.status(),
        ProcessStatus::New
            | ProcessStatus::Yielded
            | ProcessStatus::Waiting
            | ProcessStatus::Suspended
    ) {
        #[cfg(feature = "telemetry")]
        let span = start_slice_span(shared, process);
        #[cfg(feature = "telemetry")]
        finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    if process.transition_to(ProcessStatus::Running).is_err() {
        #[cfg(feature = "telemetry")]
        let span = start_slice_span(shared, process);
        #[cfg(feature = "telemetry")]
        finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    #[cfg(feature = "telemetry")]
    let span = start_slice_span(shared, process);
    if let Some((_, dirty_result)) = shared.dirty_results.remove(&process.pid()) {
        match apply_dirty_result(process, dirty_result) {
            Ok(InstructionOutcomeAfterDirty::Continue) => {}
            Ok(InstructionOutcomeAfterDirty::Exit(reason)) => {
                let outcome = exit_process(shared, process, reason);
                #[cfg(feature = "telemetry")]
                finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
                return outcome;
            }
            Err(error) => {
                let pid = process.pid();
                shared.exit_errors.insert(pid, error);
                let outcome = exit_process(shared, process, ExitReason::Error);
                #[cfg(feature = "telemetry")]
                finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
                return outcome;
            }
        }
    }
    if let Some((_, result_term)) = shared.async_results.remove(&process.pid()) {
        process.set_x_reg(0, result_term);
        advance_past_current_instruction(process);
    }
    process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
    let module_atom = match process.code_position() {
        Some(position) => position.module,
        None => {
            let outcome = exit_process(shared, process, ExitReason::Normal);
            #[cfg(feature = "telemetry")]
            finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
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
            finish_slice_span(span, 0, crate::telemetry::spans::SliceSpanOutcome::Exited);
            return outcome;
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
        let outcome = SliceOutcome::Suspended(take_process(process));
        #[cfg(feature = "telemetry")]
        finish_slice_span(
            span,
            reductions,
            crate::telemetry::spans::SliceSpanOutcome::Waiting,
        );
        return outcome;
    }
    match result {
        Ok(ExecutionResult::Yielded) => {
            let _t = process.transition_to(ProcessStatus::Yielded);
            process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
            let outcome = SliceOutcome::Requeue(take_process(process));
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Yielded,
            );
            outcome
        }
        Ok(ExecutionResult::Waiting) => {
            let _t = process.transition_to(ProcessStatus::Waiting);
            let outcome = SliceOutcome::Wait(take_process(process));
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Waiting,
            );
            outcome
        }
        Ok(ExecutionResult::DirtyCall { entry, args, kind }) => {
            if let Err(error) = submit_dirty_call(shared, process, entry, args, kind) {
                shared.exit_errors.insert(process.pid(), error.into());
                let outcome = exit_process(shared, process, ExitReason::Error);
                #[cfg(feature = "telemetry")]
                finish_slice_span(
                    span,
                    reductions,
                    crate::telemetry::spans::SliceSpanOutcome::Exited,
                );
                return outcome;
            }
            let _t = process.transition_to(ProcessStatus::Suspended);
            let outcome = SliceOutcome::Suspended(take_process(process));
            #[cfg(feature = "telemetry")]
            finish_slice_span(
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Waiting,
            );
            outcome
        }
        Ok(ExecutionResult::Exited(reason)) => {
            let outcome = exit_process(shared, process, reason);
            #[cfg(feature = "telemetry")]
            finish_slice_span(
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
                span,
                reductions,
                crate::telemetry::spans::SliceSpanOutcome::Exited,
            );
            outcome
        }
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
    span: crate::telemetry::spans::ExecutionSliceSpan,
    reductions_consumed: u32,
    outcome: crate::telemetry::spans::SliceSpanOutcome,
) {
    span.finish(reductions_consumed, outcome);
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

enum InstructionOutcomeAfterDirty {
    Continue,
    Exit(ExitReason),
}

fn apply_dirty_result(
    process: &mut Process,
    dirty_result: DirtyResult,
) -> Result<InstructionOutcomeAfterDirty, crate::error::ExecError> {
    match dirty_result.result {
        Ok(value) => {
            process.set_x_reg(0, value);
            advance_past_current_instruction(process);
            Ok(InstructionOutcomeAfterDirty::Continue)
        }
        Err(reason) => {
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
    entry: NativeEntry,
    args: Vec<Term>,
    kind: DirtySchedulerKind,
) -> Result<(), DirtySubmissionError> {
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
    if let Some(sink) = services.io_sink {
        context.set_io_sink(sink);
    }

    let (result_sender, result_receiver) = oneshot::channel();
    let job = DirtyJob {
        pid: process.pid(),
        function: entry.function,
        args,
        context,
        result_sender,
    };
    match kind {
        DirtySchedulerKind::Cpu => shared.dirty_cpu.submit(job),
        DirtySchedulerKind::Io => shared.dirty_io.submit(job),
    }
    .map_err(|_| DirtySubmissionError::PoolUnavailable)?;

    let shared_for_completion = Arc::clone(shared);
    let pid = process.pid();
    std::thread::Builder::new()
        .name(format!("dirty-complete-{pid}"))
        .spawn(move || match result_receiver.recv() {
            Ok(result) => {
                shared_for_completion.dirty_results.insert(pid, result);
                let _resumed = timer_integration::resume_suspended(&shared_for_completion, pid);
            }
            Err(_closed) => {
                shared_for_completion.dirty_results.insert(
                    pid,
                    DirtyResult {
                        result: Err(Term::atom(Atom::ERROR)),
                        exception_class: ExceptionClass::Error,
                        exception_stacktrace: Term::NIL,
                    },
                );
                let _resumed = timer_integration::resume_suspended(&shared_for_completion, pid);
            }
        })
        .map_err(|_| DirtySubmissionError::CompletionBridgeSpawn)?;
    Ok(())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DirtySubmissionError {
    PoolUnavailable,
    CompletionBridgeSpawn,
}

impl From<DirtySubmissionError> for crate::error::ExecError {
    fn from(_error: DirtySubmissionError) -> Self {
        Self::Badarg
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
    let _deleted_tables = shared.transfer_or_delete_tables_owned_by(pid);
    supervision_integration::propagate_exit(shared, pid, reason);
    close_owned_fd_resources_on_exit(shared, pid);
    let _removed = shared.process_table.remove(pid);
    let _removed_body = shared.process_bodies.remove(&pid);
    let mut wait_set = lock_or_recover(&shared.wait_set);
    wait_set.waiting.remove(&pid);
    wait_set.woken.retain(|(woken_pid, _)| *woken_pid != pid);
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
