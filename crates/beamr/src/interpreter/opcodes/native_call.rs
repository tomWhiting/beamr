//! Native BIF call execution shared by resolved-import calls and
//! export-fun dispatch — capability audit, replay, dirty scheduling,
//! mailbox snapshots, and trampolines.

use std::sync::Arc;

use crate::atom::Atom;
use crate::capability::{
    CapabilityAuditEvent, CapabilityOperation, StderrViolationHandler, ViolationHandler,
};
use crate::error::ExecError;
use crate::gc::ensure_space;
use crate::interpreter::InstructionOutcome;
use crate::module::Module;
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::write_tuple;

use super::core::{
    ExtCallContext, charge_reduction, exception_class_atom, gc_error_to_exec, heap_slice, return_,
};
use super::trampoline;

/// How a native call hands control back when it completes (returns a value,
/// raises, or trampolines) — suspension parks never consume this: a parked
/// call either re-executes its instruction or applies a published result
/// under the suspension record's own continuation.
#[derive(Clone, Copy, Debug)]
pub enum NativeCallReturn {
    /// Body-position call: fall through to the next instruction.
    Advance,
    /// Tail-position call: return to the caller. `pop_frame` is true for a
    /// `call_ext_last` site whose y-frame pop was deferred to completion
    /// (so a suspension's wake re-execution cannot double-pop the stack).
    TailReturn { pop_frame: bool },
}

impl NativeCallReturn {
    /// Pop the deferred `call_ext_last` y-frame, if this call carries one.
    /// Must run on every COMPLETION path (value, exception, trampoline,
    /// replay) and never on a suspension park.
    fn pop_deferred_frame(self, process: &mut Process) -> Result<(), ExecError> {
        if let Self::TailReturn { pop_frame: true } = self {
            let _frame = process.stack_mut().pop_frame().map_err(ExecError::from)?;
        }
        Ok(())
    }
}

/// Executes a native BIF `entry` for the `mfa` target with full service
/// support — capability audit, replay, dirty scheduling, mailbox snapshots,
/// and trampolines. Shared by resolved-import calls and export-fun dispatch.
pub(crate) fn call_native_entry(
    process: &mut Process,
    module: &Module,
    entry: crate::native::NativeEntry,
    mfa: (Atom, Atom, u8),
    call_return: NativeCallReturn,
    ctx: &ExtCallContext<'_>,
) -> Result<InstructionOutcome, ExecError> {
    let (target_module, target_function, target_arity) = mfa;
    let audit_event = CapabilityAuditEvent {
        pid: process.pid(),
        capability: entry.capability,
        operation: CapabilityOperation {
            module: target_module,
            function: target_function,
            arity: target_arity,
        },
        granted: process.capabilities().contains(entry.capability),
        process_capabilities: process.capabilities().clone(),
    };
    if let Some(svc) = ctx.services
        && let Some(sink) = &svc.capability_audit_sink
    {
        sink.record(audit_event.clone());
    }
    if !audit_event.granted {
        if let Some(handler) = ctx
            .services
            .and_then(|svc| svc.capability_violation_handler.as_ref())
        {
            handler.on_violation(audit_event);
        } else {
            StderrViolationHandler.on_violation(audit_event);
        }
        let result = capability_denied_result(process)?;
        return complete_native_value(process, result, call_return);
    }

    let mut args = Vec::with_capacity(usize::from(target_arity));
    for register in 0..target_arity {
        args.push(process.x_reg(register.into()));
    }
    if let Some(kind) = entry.dirty_kind {
        return Ok(InstructionOutcome::DirtyCall {
            entry,
            args,
            module: target_module,
            function: target_function,
            arity: target_arity,
            kind,
        });
    }
    if matches!(
        entry.capability,
        crate::native::Capability::ExternalIo | crate::native::Capability::Entropy
    ) && let Some(svc) = ctx.services
        && let Some(driver) = &svc.replay_driver
    {
        let mut driver = match driver.lock() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        };
        let recorded = driver
            .next_native_call(process.pid(), target_module, target_function, target_arity)
            .map_err(ExecError::from)?;
        return apply_replayed_native_result(process, recorded.outcome, call_return);
    }
    let mut context = match ctx.timers {
        Some(timers) => ProcessContext::with_timer_services(process.pid(), Arc::clone(timers)),
        None => {
            let mut pctx = ProcessContext::new();
            pctx.set_pid(Some(process.pid()));
            pctx
        }
    };
    if let Some(svc) = ctx.services {
        context.set_atom_table(svc.atom_table.clone());
        #[cfg(feature = "net")]
        context.set_local_node(svc.local_node);
        #[cfg(feature = "net")]
        context.set_net_kernel(svc.net_kernel.clone());
        #[cfg(feature = "net")]
        context.set_distribution_send_facility(svc.distribution_send.clone());
        context.set_spawn_facility(svc.spawn_facility.clone());
        context.set_link_facility(svc.link_facility.clone());
        #[cfg(feature = "net")]
        context.set_distribution_control_facility(svc.distribution_control_facility.clone());
        #[cfg(feature = "net")]
        context.set_global_name_facility(svc.global_name_facility.clone());
        context.set_group_leader_facility(svc.group_leader_facility.clone());
        context.set_supervision_facility(svc.supervision_facility.clone());
        context.set_process_info_facility(svc.process_info_facility.clone());
        context.set_code_management_facility(svc.code_management_facility.clone());
        context.set_system_info_facility(svc.system_info_facility.clone());
        context.set_ets_facility(svc.ets_facility.clone());
        #[cfg(feature = "net")]
        context.set_pg_facility(svc.pg_facility.clone());
        #[cfg(feature = "threads")]
        context.set_io_facility(svc.io_facility.clone());
        context.set_io_message_facility(svc.io_message_facility.clone());
        #[cfg(feature = "threads")]
        context.set_file_io_facility(svc.file_io_facility.clone());
        #[cfg(feature = "threads")]
        context.set_tcp_io_facility(svc.tcp_io_facility.clone());
        context.set_replay_driver(svc.replay_driver.clone());
        #[cfg(feature = "threads")]
        if let Some(sink) = &svc.io_sink {
            context.set_io_sink(Arc::clone(sink));
        }
        context.set_current_native(Some((target_module, target_function, target_arity)));
        context.set_wasm_async_nif_facility(svc.wasm_async_nif_facility.clone());
        context.set_nif_private_data(svc.nif_private_data.clone());
        context.set_suspension_registrar(svc.suspension_registrar.clone());
    }

    // Provide mailbox access for select BIFs before borrowing the process for heap allocation.
    let mut replay_select = None;
    let snapshot = if should_replay_select(ctx, target_module, target_function, target_arity) {
        let driver = ctx
            .services
            .and_then(|svc| svc.replay_driver.as_ref())
            .ok_or(ExecError::InvalidOperand("replay select driver"))?;
        let facility =
            crate::replay::ReplayDriver::select_facility(Arc::clone(driver), process.pid())
                .map_err(ExecError::from)?;
        let select_facility: Arc<dyn crate::native::SelectFacility> = facility.clone();
        context.set_select_facility(Some(select_facility));
        replay_select = Some(facility);
        None
    } else {
        let snapshot = trampoline::build_mailbox_snapshot(process);
        context.set_select_facility(
            snapshot
                .clone()
                .map(|s| s as Arc<dyn crate::native::SelectFacility>),
        );
        snapshot
    };
    context.attach_process(process, usize::from(target_arity));

    let call_result = (entry.function)(&args, &mut context);
    let shutdown_requested = context.take_shutdown_request();
    let suspend = context.take_suspend();
    let trampoline_req = context.take_trampoline();
    let exception_class = context.take_exception_class();
    let exception_stacktrace = context.take_exception_stacktrace();
    context.detach_process();
    let result = match call_result {
        Ok(value) => value,
        Err(reason) => {
            // The native raised after requesting suspension: withdraw the
            // published host-await registration so its call id cannot
            // strand a stale mirror (and silently swallow a completion).
            if let Some(request) = &suspend {
                context.cancel_suspend_request(request);
            }
            // The raise completes the tail call: settle the deferred
            // y-frame pop so the unwind sees the same stack an eager
            // `call_ext_last` pop would have left.
            call_return.pop_deferred_frame(process)?;
            let exception = crate::process::Exception {
                class: Term::atom(exception_class_atom(exception_class)),
                reason,
                stacktrace: exception_stacktrace,
            };
            return super::exceptions::raise_exception(process, exception);
        }
    };

    // Handle mailbox removal if the select facility recorded one.
    if let Some(facility) = replay_select {
        if let Some(index) = facility.removed_index() {
            trampoline::apply_mailbox_removal_at(process, Some(index));
        }
    } else if let Some(snapshot) = snapshot {
        trampoline::apply_mailbox_removal(process, &snapshot);
    }

    // Check for suspend request before trampoline (suspend takes priority
    // when no message matched). The park keeps the deferred y-frame: wake
    // re-execution re-runs this call instruction against the SAME stack,
    // and a published host result settles the frame via the suspension
    // record's continuation.
    if let Some(suspend) = suspend {
        return trampoline::handle_suspend(process, module, suspend, call_return);
    }

    // Check for trampoline request from the BIF. The trampoline completes
    // the native call: settle the deferred frame first.
    if let Some(trampoline_req) = trampoline_req {
        call_return.pop_deferred_frame(process)?;
        return trampoline::handle_trampoline(process, module, ctx.registry, trampoline_req);
    }

    process.set_x_reg(0, result);
    if shutdown_requested {
        return Ok(InstructionOutcome::Exit(crate::process::ExitReason::Normal));
    }
    charge_reduction(process)?;
    match call_return {
        NativeCallReturn::Advance => Ok(InstructionOutcome::Continue),
        NativeCallReturn::TailReturn { .. } => {
            call_return.pop_deferred_frame(process)?;
            return_(process)
        }
    }
}

fn capability_denied_result(process: &mut Process) -> Result<Term, ExecError> {
    let words = 3;
    ensure_space(process, words, 0).map_err(gc_error_to_exec)?;
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    write_tuple(
        heap,
        &[Term::atom(Atom::ERROR), Term::atom(Atom::CAPABILITY_DENIED)],
    )
    .ok_or(ExecError::Badarg)
}

fn complete_native_value(
    process: &mut Process,
    result: Term,
    call_return: NativeCallReturn,
) -> Result<InstructionOutcome, ExecError> {
    process.set_x_reg(0, result);
    charge_reduction(process)?;
    match call_return {
        NativeCallReturn::Advance => Ok(InstructionOutcome::Continue),
        NativeCallReturn::TailReturn { .. } => {
            call_return.pop_deferred_frame(process)?;
            return_(process)
        }
    }
}

fn should_replay_select(ctx: &ExtCallContext<'_>, module: Atom, function: Atom, arity: u8) -> bool {
    let Some(services) = ctx.services else {
        return false;
    };
    let Some(atom_table) = ctx.atom_table else {
        return false;
    };
    if services.replay_driver.is_none() || function != atom_table.intern("select") {
        return false;
    }
    let Some(module_name) = atom_table.resolve(module) else {
        return false;
    };
    matches!(
        (module_name, arity),
        ("gleam_erlang_ffi", 1 | 2) | ("erlang", 1 | 2)
    )
}

fn apply_replayed_native_result(
    process: &mut Process,
    outcome: crate::replay::NativeOutcome,
    call_return: NativeCallReturn,
) -> Result<InstructionOutcome, ExecError> {
    match outcome.result {
        Ok(value) => {
            process.set_x_reg(0, value);
            charge_reduction(process)?;
            match call_return {
                NativeCallReturn::Advance => Ok(InstructionOutcome::Continue),
                NativeCallReturn::TailReturn { .. } => {
                    call_return.pop_deferred_frame(process)?;
                    return_(process)
                }
            }
        }
        Err(reason) => {
            call_return.pop_deferred_frame(process)?;
            let exception = crate::process::Exception {
                class: Term::atom(exception_class_atom(outcome.exception_class)),
                reason,
                stacktrace: outcome.exception_stacktrace,
            };
            super::exceptions::raise_exception(process, exception)
        }
    }
}
