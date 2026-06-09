//! Message passing and receive opcode handlers.

use crate::distribution::control::{DistributionSendError, DistributionSendFacility};
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::{CodePosition, Process, ProcessStatus, ReceiveTimeout};
use crate::term::pid_ref::PidRef;

/// Send x(1) to the process identified by x(0) when the caller supplies a receiver.
pub fn send(
    process: &mut Process,
    receiver: Option<&mut Process>,
    distribution: Option<&dyn DistributionSendFacility>,
) -> Result<InstructionOutcome, ExecError> {
    let target_term = process.x_reg(0);
    let target = PidRef::new(target_term).ok_or(ExecError::Badarg)?;
    let message = process.x_reg(1);
    if !target.is_local() {
        let facility = distribution.ok_or(ExecError::NoConnection)?;
        facility
            .send_remote(target_term, message)
            .map_err(distribution_send_error)?;
        process.set_x_reg(0, message);
        return Ok(InstructionOutcome::Continue);
    }
    let target_pid = target.pid_number();
    if let Some(receiver) = receiver.filter(|receiver| receiver.pid() == target_pid) {
        receiver
            .mailbox()
            .sender()
            .send(message, receiver.heap_mut())
            .map_err(send_error)?;
        if receiver.status() == ProcessStatus::Waiting {
            receiver
                .transition_to(ProcessStatus::Running)
                .map_err(|_| ExecError::Badarg)?;
        }
    }
    process.set_x_reg(0, message);
    Ok(InstructionOutcome::Continue)
}

fn distribution_send_error(error: DistributionSendError) -> ExecError {
    match error {
        DistributionSendError::NoConnection => ExecError::NoConnection,
        DistributionSendError::Encode => ExecError::Badarg,
    }
}

pub fn loop_rec(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
    destination: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    if let Some(message) = process.mailbox_mut().current_message() {
        core::write_term(process, destination, message)?;
        Ok(InstructionOutcome::Continue)
    } else {
        jump_to_label(module, fail)
    }
}

pub fn loop_rec_end(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    process.mailbox_mut().advance_save_pointer();
    jump_to_label(module, fail)
}

pub fn remove_message(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    let _ = process.mailbox_mut().remove_current_message();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
    Ok(InstructionOutcome::Continue)
}

pub fn wait(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let continuation = label_position(module, fail)?;
    process.set_code_position(Some(continuation));
    transition_to_waiting(process)?;
    Ok(InstructionOutcome::Waiting)
}

pub fn wait_timeout(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
    timeout: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let continuation = label_position(module, fail)?;
    let milliseconds = timeout_milliseconds(process, module, timeout)?;
    let timeout_position = continuation;
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: next_instruction_pointer(continuation.instruction_pointer)?,
    }));
    process.set_receive_timeout(Some(ReceiveTimeout {
        timeout_position,
        milliseconds,
    }));
    transition_to_waiting(process)?;
    Ok(InstructionOutcome::Waiting)
}

pub fn timeout(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    process.mailbox_mut().reset_save_pointer();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
    Ok(InstructionOutcome::Continue)
}

fn jump_to_label(module: &Module, label: &Operand) -> Result<InstructionOutcome, ExecError> {
    label_position(module, label).map(InstructionOutcome::Jump)
}

fn label_position(module: &Module, label: &Operand) -> Result<CodePosition, ExecError> {
    Ok(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, core::operand_label(label)?)?,
    })
}

fn next_instruction_pointer(instruction_pointer: usize) -> Result<usize, ExecError> {
    instruction_pointer
        .checked_add(1)
        .ok_or(ExecError::InvalidOperand("instruction pointer"))
}

fn transition_to_waiting(process: &mut Process) -> Result<(), ExecError> {
    if process.status() == ProcessStatus::New {
        process
            .transition_to(ProcessStatus::Running)
            .map_err(|_| ExecError::Badarg)?;
    }
    process
        .transition_to(ProcessStatus::Waiting)
        .map_err(|_| ExecError::Badarg)
}

fn timeout_milliseconds(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<u64, ExecError> {
    match operand {
        Operand::Unsigned(value) => Ok(*value),
        Operand::Integer(value) => u64::try_from(*value).map_err(|_| ExecError::Badarg),
        _ => core::read_term(process, module, operand)?
            .as_small_int()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(ExecError::Badarg),
    }
}

fn send_error(error: crate::mailbox::SendError) -> ExecError {
    match error {
        crate::mailbox::SendError::HeapFull(error) => ExecError::from(error),
        crate::mailbox::SendError::InvalidBoxedTerm => ExecError::Badarg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::interpreter::{ExecutionResult, run};
    use crate::loader::Instruction;
    use crate::module::ModuleOrigin;
    use crate::process::{ExitReason, Process};
    use crate::term::Term;
    use crate::term::boxed::write_external_pid;
    use std::collections::HashMap;

    fn module(code: Vec<Instruction>) -> Module {
        let label_index = code
            .iter()
            .enumerate()
            .filter_map(|(ip, instruction)| match instruction {
                Instruction::Label { label } => Some((*label, ip)),
                _ => None,
            })
            .collect();
        Module {
            name: Atom::OK,
            generation: 0,
            origin: ModuleOrigin::Preloaded,
            exports: HashMap::new(),
            label_index,
            code,
            literals: Vec::new(),
            constant_pool: Default::default(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
            function_table: Vec::new(),
            line_table: Vec::new(),
            line_info: Vec::new(),
        }
    }

    #[test]
    fn send_delivers_to_matching_pid_and_leaves_message_in_x0() {
        let mut sender = Process::new(0, 32);
        let mut receiver = Process::new(1, 32);
        let message = Term::atom(Atom::OK);
        sender.set_x_reg(0, Term::pid(1));
        sender.set_x_reg(1, message);

        assert_eq!(
            send(&mut sender, Some(&mut receiver), None),
            Ok(InstructionOutcome::Continue)
        );

        assert_eq!(sender.x_reg(0), message);
        assert_eq!(receiver.mailbox_mut().current_message(), Some(message));
    }

    #[test]
    fn send_to_missing_pid_is_silent_drop() {
        let mut sender = Process::new(0, 32);
        sender.set_x_reg(0, Term::pid(99));
        sender.set_x_reg(1, Term::atom(Atom::OK));

        assert_eq!(
            send(&mut sender, None, None),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(sender.x_reg(0), Term::atom(Atom::OK));
    }

    #[test]
    fn send_to_remote_pid_without_distribution_returns_noconnection() {
        let mut sender = Process::new(0, 32);
        let mut heap = [0_u64; 4];
        let remote = write_external_pid(&mut heap, Atom::OK, 99, 0).expect("external pid fits");
        sender.set_x_reg(0, remote);
        sender.set_x_reg(1, Term::atom(Atom::OK));

        assert_eq!(send(&mut sender, None, None), Err(ExecError::NoConnection));
    }

    #[test]
    fn dispatch_send_delivers_to_resolved_process_and_receiver_selectively_receives() {
        let send_code = module(vec![Instruction::Send]);
        let receive_code = module(vec![
            Instruction::LoopRec {
                fail: Operand::Label(10),
                destination: Operand::X(0),
            },
            Instruction::RemoveMessage,
            Instruction::Return,
            Instruction::Label { label: 10 },
            Instruction::Wait {
                fail: Operand::Label(10),
            },
        ]);
        let mut sender = Process::new(0, 32);
        let mut receiver = Process::new(1, 32);
        let message = Term::atom(Atom::OK);
        sender.set_x_reg(0, Term::pid(1));
        sender.set_x_reg(1, message);

        assert_eq!(
            crate::interpreter::opcodes::dispatch_with_receiver(
                &mut sender,
                &send_code,
                &Instruction::Send,
                1,
                Some(&mut receiver),
                None,
            ),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(sender.x_reg(0), message);

        assert_eq!(
            run(&mut receiver, &receive_code),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(receiver.x_reg(0), message);
        assert!(receiver.mailbox().is_empty());
    }

    #[test]
    fn selective_receive_scans_advances_and_removes_only_current_message() {
        let code = module(vec![Instruction::Label { label: 10 }]);
        let mut process = Process::new(1, 32);
        for value in [1, 2, 3] {
            process
                .mailbox_mut()
                .push_owned_for_test(Term::small_int(value));
        }

        assert_eq!(
            loop_rec(&mut process, &code, &Operand::Label(10), &Operand::X(0)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0), Term::small_int(1));
        assert_eq!(
            loop_rec_end(&mut process, &code, &Operand::Label(10)),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
        assert_eq!(
            loop_rec(&mut process, &code, &Operand::Label(10), &Operand::X(0)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(process.x_reg(0), Term::small_int(2));
        assert_eq!(
            remove_message(&mut process),
            Ok(InstructionOutcome::Continue)
        );

        assert_eq!(
            process.mailbox_mut().current_message(),
            Some(Term::small_int(1))
        );
        assert_eq!(process.mailbox().message_count(), 2);
    }

    #[test]
    fn loop_rec_jumps_to_fail_when_no_unscanned_message_exists() {
        let code = module(vec![Instruction::Label { label: 10 }]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            loop_rec(&mut process, &code, &Operand::Label(10), &Operand::X(0)),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
    }

    #[test]
    fn wait_and_wait_timeout_mark_process_waiting_and_record_timeout() {
        let code = module(vec![Instruction::Label { label: 10 }]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            wait_timeout(
                &mut process,
                &code,
                &Operand::Label(10),
                &Operand::Unsigned(100),
            ),
            Ok(InstructionOutcome::Waiting)
        );
        assert_eq!(process.status(), ProcessStatus::Waiting);
        assert_eq!(
            process
                .receive_timeout()
                .map(|timeout| timeout.milliseconds),
            Some(100)
        );

        process
            .transition_to(ProcessStatus::Running)
            .expect("waiting can resume");
        assert_eq!(timeout(&mut process), Ok(InstructionOutcome::Continue));
        assert_eq!(process.receive_timeout(), None);
    }

    #[test]
    fn run_wait_suspends_and_send_wakes_waiting_receiver() {
        let wait_code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Wait {
                fail: Operand::Label(10),
            },
        ]);
        let mut sender = Process::new(0, 32);
        let mut receiver = Process::new(1, 32);

        assert_eq!(run(&mut receiver, &wait_code), Ok(ExecutionResult::Waiting));
        assert_eq!(receiver.status(), ProcessStatus::Waiting);
        sender.set_x_reg(0, Term::pid(1));
        sender.set_x_reg(1, Term::atom(Atom::OK));

        assert_eq!(
            send(&mut sender, Some(&mut receiver), None),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(receiver.status(), ProcessStatus::Running);
        assert_eq!(
            receiver.mailbox_mut().current_message(),
            Some(Term::atom(Atom::OK))
        );
    }

    #[test]
    fn dispatch_wait_timeout_records_deadline_and_timeout_cleans_receive_state() {
        let timeout_code = module(vec![
            Instruction::WaitTimeout {
                fail: Operand::Label(10),
                timeout: Operand::Unsigned(100),
            },
            Instruction::Label { label: 10 },
            Instruction::Timeout,
            Instruction::Return,
            Instruction::Label { label: 20 },
            Instruction::Return,
        ]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            run(&mut process, &timeout_code),
            Ok(ExecutionResult::Waiting)
        );
        assert_eq!(process.status(), ProcessStatus::Waiting);
        assert_eq!(
            process.code_position(),
            Some(CodePosition {
                module: Atom::OK,
                instruction_pointer: 2,
            }),
            "a message wakeup resumes at the normal continuation after the wait timeout label"
        );
        assert_eq!(
            process.receive_timeout(),
            Some(ReceiveTimeout {
                timeout_position: CodePosition {
                    module: Atom::OK,
                    instruction_pointer: 1,
                },
                milliseconds: 100,
            })
        );

        process
            .transition_to(ProcessStatus::Running)
            .expect("timeout expiry requeues process");
        process.set_code_position(
            process
                .receive_timeout()
                .map(|timeout| timeout.timeout_position),
        );
        assert_eq!(
            run(&mut process, &timeout_code),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.receive_timeout(), None);

        let message_code = module(vec![
            Instruction::WaitTimeout {
                fail: Operand::Label(20),
                timeout: Operand::Unsigned(100),
            },
            Instruction::Return,
            Instruction::Label { label: 20 },
            Instruction::Timeout,
            Instruction::Return,
        ]);
        let mut process = Process::new(2, 32);
        assert_eq!(
            run(&mut process, &message_code),
            Ok(ExecutionResult::Waiting)
        );
        process
            .transition_to(ProcessStatus::Running)
            .expect("message arrival requeues process");
        assert_eq!(
            run(&mut process, &message_code),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
    }
}
