//! Trampoline, suspend, and mailbox snapshot helpers for BIF re-entry.
//!
//! When a BIF (like `select`) needs the interpreter to invoke a BEAM closure,
//! it stores a trampoline request in ProcessContext. These helpers handle
//! setting up the closure call, suspending the process, and managing
//! mailbox snapshots for select operations.

use std::sync::Arc;

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::module::Module;
use crate::native::NativeContinuation;
use crate::native::select::MailboxSnapshot;
use crate::native::stdlib_stubs::lists_bifs::resume_lists_map;
use crate::native::stdlib_stubs::maps_bifs::{ContinuationStep, resume_maps_continuation};
use crate::process::{CodePosition, Process, ProcessStatus, ReceiveTimeout};
use crate::term::Term;
use crate::term::boxed::Closure;

use super::core::{charge_reduction, label_ip};

/// Build a mailbox snapshot for the select facility.
///
/// Drains arrived messages into the scan list and snapshots all messages
/// from the save pointer forward.
pub fn build_mailbox_snapshot(process: &mut Process) -> Option<Arc<MailboxSnapshot>> {
    process.mailbox_mut().drain_arrival();
    let messages: Vec<Term> = process.mailbox().scan_iter().copied().collect();
    if messages.is_empty() {
        return None;
    }
    Some(Arc::new(MailboxSnapshot::new(messages)))
}

/// Apply a mailbox removal recorded by the select facility.
pub fn apply_mailbox_removal(process: &mut Process, snapshot: &MailboxSnapshot) {
    if let Some(index) = snapshot.removed_index() {
        // The snapshot was built from the scan list starting at position 0.
        // We need to set the save pointer to the target index, then remove.
        process.mailbox_mut().reset_save_pointer();
        for _ in 0..index {
            process.mailbox_mut().advance_save_pointer();
        }
        let _ = process.mailbox_mut().remove_current_message();
    }
}

/// Handle a trampoline request by setting up a closure call.
///
/// The closure's return value will end up in x(0), which is what the
/// caller of the BIF (e.g., `select`) expects.
pub fn handle_trampoline(
    process: &mut Process,
    module: &Module,
    registry: Option<&crate::module::ModuleRegistry>,
    trampoline: crate::native::TrampolineRequest,
) -> Result<InstructionOutcome, ExecError> {
    let closure = Closure::new(trampoline.fun).ok_or(ExecError::Badfun {
        term: trampoline.fun,
    })?;
    let arity = closure.arity();

    // Load arguments into x registers.
    for (index, arg) in trampoline.args.iter().enumerate() {
        let register = u8::try_from(index)
            .map_err(|_| ExecError::InvalidOperand("trampoline argument register"))?;
        process.set_x_reg(register, *arg);
    }

    // Load free variables after the arguments.
    let free_count = closure.num_free();
    for index in 0..free_count {
        let value = closure.free_var(index).ok_or(ExecError::InvalidOperand(
            "trampoline closure free variable",
        ))?;
        let register = u8::try_from(usize::from(arity) + index)
            .map_err(|_| ExecError::InvalidOperand("trampoline X register"))?;
        process.set_x_reg(register, value);
    }

    // Look up the lambda entry point.
    let function_index = usize::try_from(closure.function_index())
        .map_err(|_| ExecError::InvalidOperand("trampoline closure function index"))?;
    let target_module_atom = closure.module().unwrap_or(module.name);
    let target_module = registry.and_then(|registry| registry.lookup(target_module_atom));
    let target_module = target_module.as_deref().unwrap_or(module);
    let lambda = target_module
        .lambdas
        .get(function_index)
        .ok_or(ExecError::InvalidOperand(
            "trampoline closure function index",
        ))?;

    // Push a return frame so the closure returns to the BIF's caller.
    let return_ip = process
        .code_position()
        .map_or(0, |pos| pos.instruction_pointer);
    if trampoline.continuation.is_none() || !process.has_native_continuation() {
        process
            .stack_mut()
            .push_frame(module.name, return_ip, 0)
            .map_err(ExecError::from)?;
    }
    process.set_native_continuation(trampoline.continuation);

    let target = CodePosition {
        module: target_module_atom,
        instruction_pointer: label_ip(target_module, lambda.label)?,
    };

    charge_reduction(process)?;
    Ok(if process.reductions_exhausted() {
        process.set_code_position(Some(target));
        InstructionOutcome::Yield
    } else {
        InstructionOutcome::Jump(target)
    })
}

/// Resume a native higher-order BIF continuation after a closure returns in x(0).
pub fn handle_native_continuation(
    process: &mut Process,
    module: &Module,
    registry: Option<&crate::module::ModuleRegistry>,
) -> Result<InstructionOutcome, ExecError> {
    let continuation = process
        .take_native_continuation()
        .ok_or(ExecError::InvalidOperand("native continuation"))?;
    let closure_result = process.x_reg(0);
    let step = match continuation {
        NativeContinuation::Maps(state) => resume_maps_continuation(state, closure_result),
        NativeContinuation::ListsMap(state) => resume_lists_map(state, closure_result),
    }
    .map_err(|_| ExecError::Badarg)?;

    match step {
        ContinuationStep::Done(result) => {
            process.set_x_reg(0, result);
            Ok(InstructionOutcome::Continue)
        }
        ContinuationStep::Call {
            fun,
            args,
            continuation,
        } => {
            process.set_native_continuation(Some(continuation.clone()));
            handle_trampoline(
                process,
                module,
                registry,
                crate::native::TrampolineRequest {
                    fun,
                    args,
                    continuation: Some(continuation),
                },
            )
        }
    }
}

/// Handle a suspend request by transitioning the process to Waiting state.
pub fn handle_suspend(
    process: &mut Process,
    _module: &Module,
    suspend: crate::native::SuspendRequest,
) -> Result<InstructionOutcome, ExecError> {
    // Store the current position as the resume point. When the process is
    // woken (new message arrives), it will re-execute the select BIF call.
    let resume = process
        .code_position()
        .ok_or(ExecError::InvalidOperand("suspend code position"))?;

    if let Some(timeout_ms) = suspend.timeout_ms {
        // Set up a receive timeout so the scheduler can expire it.
        process.set_receive_timeout(Some(ReceiveTimeout {
            timeout_position: resume,
            milliseconds: timeout_ms,
        }));
    }

    // Transition to Waiting state.
    if process.status() == ProcessStatus::New {
        process
            .transition_to(ProcessStatus::Running)
            .map_err(|_| ExecError::Badarg)?;
    }
    process
        .transition_to(ProcessStatus::Waiting)
        .map_err(|_| ExecError::Badarg)?;

    charge_reduction(process)?;
    Ok(InstructionOutcome::Waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::select::SelectFacility;
    use crate::process::Process;

    #[test]
    fn build_mailbox_snapshot_empty_returns_none() {
        let mut process = Process::new(1, 32);
        assert!(build_mailbox_snapshot(&mut process).is_none());
    }

    #[test]
    fn build_mailbox_snapshot_captures_messages() {
        let mut process = Process::new(1, 32);
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(1));
        process
            .mailbox_mut()
            .push_owned_for_test(Term::small_int(2));

        let snapshot = build_mailbox_snapshot(&mut process).expect("should have messages");
        assert_eq!(snapshot.message_count(), 2);
        assert_eq!(snapshot.peek_message(0), Some(Term::small_int(1)));
        assert_eq!(snapshot.peek_message(1), Some(Term::small_int(2)));
    }

    #[test]
    fn apply_mailbox_removal_removes_correct_message() {
        let mut process = Process::new(1, 32);
        for value in [1, 2, 3] {
            process
                .mailbox_mut()
                .push_owned_for_test(Term::small_int(value));
        }

        let snapshot = MailboxSnapshot::new(vec![
            Term::small_int(1),
            Term::small_int(2),
            Term::small_int(3),
        ]);
        snapshot.remove_message(1); // Remove message at index 1 (value=2)

        apply_mailbox_removal(&mut process, &snapshot);

        // After removal, mailbox should have 2 messages: 1 and 3.
        assert_eq!(process.mailbox().message_count(), 2);
    }

    #[test]
    fn handle_suspend_transitions_to_waiting() {
        use crate::atom::Atom;
        use crate::loader::Instruction;
        use crate::module::Module;
        use std::collections::HashMap;

        let module = Module {
            name: Atom::OK,
            exports: HashMap::new(),
            code: vec![Instruction::Return],
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            line_info: Vec::new(),
        };
        let mut process = Process::new(1, 32);
        process.set_code_position(Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        }));
        process
            .transition_to(ProcessStatus::Running)
            .expect("start running");

        let suspend = crate::native::SuspendRequest {
            timeout_ms: Some(5000),
        };
        let result = handle_suspend(&mut process, &module, suspend).expect("suspend ok");
        assert_eq!(result, InstructionOutcome::Waiting);
        assert_eq!(process.status(), ProcessStatus::Waiting);

        let timeout = process.receive_timeout().expect("timeout set");
        assert_eq!(timeout.milliseconds, 5000);
    }
}
