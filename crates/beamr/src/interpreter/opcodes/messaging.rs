//! Message passing, receive, and exception opcode handlers.

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::{
    CodePosition, Exception, ExceptionHandler, ExitReason, Process, ProcessStatus, ReceiveTimeout,
    Register,
};
use crate::term::Term;
use crate::term::boxed::write_tuple;

/// Send x(1) to the process identified by x(0) when the caller supplies a receiver.
pub fn send(
    process: &mut Process,
    receiver: Option<&mut Process>,
) -> Result<InstructionOutcome, ExecError> {
    let target = process.x_reg(0).as_pid().ok_or(ExecError::Badarg)?;
    let message = process.x_reg(1);
    if let Some(receiver) = receiver.filter(|receiver| receiver.pid() == target) {
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
    let milliseconds = timeout_milliseconds(process, timeout)?;
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

pub fn try_(
    process: &mut Process,
    module: &Module,
    destination: &Operand,
    label: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let destination = register(destination)?;
    let catch_position = label_position(module, label)?;
    process.push_exception_handler(ExceptionHandler {
        catch_position,
        destination,
    });
    Ok(InstructionOutcome::Continue)
}

pub fn try_end(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    let _ = register(source)?;
    let _ = process.pop_exception_handler();
    process.set_current_exception(None);
    Ok(InstructionOutcome::Continue)
}

pub fn try_case(process: &mut Process, source: &Operand) -> Result<InstructionOutcome, ExecError> {
    core::write_term(process, source, Term::NIL)?;
    if let Some(exception) = process.current_exception() {
        process.set_x_reg(0, exception.class);
        process.set_x_reg(1, exception.reason);
        process.set_x_reg(2, exception.stacktrace);
    }
    Ok(InstructionOutcome::Continue)
}

pub fn try_case_end(
    process: &mut Process,
    source: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, source)?;

    let reason = two_tuple(process, Term::atom(Atom::BADMATCH), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn raise(
    process: &mut Process,
    stacktrace: &Operand,
    reason: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let stacktrace = core::read_term(process, stacktrace)?;
    let reason = core::read_term(process, reason)?;

    let class = process
        .current_exception()
        .map_or(Term::atom(Atom::ERROR), |exception| exception.class);
    raise_exception(
        process,
        Exception {
            class,
            reason,
            stacktrace,
        },
    )
}

pub fn badmatch(process: &mut Process, value: &Operand) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, value)?;

    let reason = two_tuple(process, Term::atom(Atom::BADMATCH), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn case_end(process: &mut Process, value: &Operand) -> Result<InstructionOutcome, ExecError> {
    let value = core::read_term(process, value)?;

    let reason = two_tuple(process, Term::atom(Atom::CASE_CLAUSE), value)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn if_end(process: &mut Process) -> Result<InstructionOutcome, ExecError> {
    let reason = two_tuple(process, Term::atom(Atom::IF_CLAUSE), Term::NIL)?;
    raise_exception(process, Exception::error(reason, Term::NIL))
}

pub fn raise_exception(
    process: &mut Process,
    exception: Exception,
) -> Result<InstructionOutcome, ExecError> {
    if let Some(handler) = process.pop_exception_handler() {
        process.set_current_exception(Some(exception));
        write_register(process, handler.destination, exception.reason)?;
        Ok(InstructionOutcome::Jump(handler.catch_position))
    } else {
        process.set_current_exception(Some(exception));
        Ok(InstructionOutcome::Exit(ExitReason::Error))
    }
}

impl Exception {
    fn error(reason: Term, stacktrace: Term) -> Self {
        Self {
            class: Term::atom(Atom::ERROR),
            reason,
            stacktrace,
        }
    }
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

fn timeout_milliseconds(process: &Process, operand: &Operand) -> Result<u64, ExecError> {
    match operand {
        Operand::Unsigned(value) => Ok(*value),
        Operand::Integer(value) => u64::try_from(*value).map_err(|_| ExecError::Badarg),
        _ => core::read_term(process, operand)?
            .as_small_int()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(ExecError::Badarg),
    }
}

fn register(operand: &Operand) -> Result<Register, ExecError> {
    match operand {
        Operand::X(index) => u16::try_from(*index)
            .map(Register::X)
            .map_err(|_| ExecError::InvalidOperand("X register")),
        Operand::Y(index) => u16::try_from(*index)
            .map(Register::Y)
            .map_err(|_| ExecError::InvalidOperand("Y register")),
        Operand::TypedRegister { register, .. } => self::register(register),
        _ => Err(ExecError::InvalidOperand("register")),
    }
}


fn write_register(
    process: &mut Process,
    destination: Register,
    value: Term,
) -> Result<(), ExecError> {
    match destination {
        Register::X(index) => {
            process.set_x_reg(index, value);
            Ok(())
        }
        Register::Y(index) => process
            .stack_mut()
            .set_y_reg(index, value)
            .map_err(ExecError::from),
    }
}

fn two_tuple(process: &mut Process, left: Term, right: Term) -> Result<Term, ExecError> {
    let ptr = process.heap_mut().alloc(3).map_err(ExecError::from)?;
    let words = core::heap_slice(ptr, 3);
    write_tuple(words, &[left, right]).ok_or(ExecError::Badarg)
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
    use crate::interpreter::{ExecutionResult, run};
    use crate::loader::Instruction;
    use crate::term::boxed::Tuple;
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
            exports: HashMap::new(),
            label_index,
            code,
            literals: Vec::new(),
            resolved_imports: Vec::new(),
            lambdas: Vec::new(),
            string_table: Vec::new(),
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
            send(&mut sender, Some(&mut receiver)),
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

        assert_eq!(send(&mut sender, None), Ok(InstructionOutcome::Continue));
        assert_eq!(sender.x_reg(0), Term::atom(Atom::OK));
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
            send(&mut sender, Some(&mut receiver)),
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

    #[test]
    fn try_badmatch_captures_class_reason_and_stacktrace() {
        let code = module(vec![Instruction::Label { label: 20 }]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)),
            Ok(InstructionOutcome::Continue)
        );
        assert_eq!(
            badmatch(&mut process, &Operand::Integer(42)),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
        assert_eq!(
            try_case(&mut process, &Operand::X(0)),
            Ok(InstructionOutcome::Continue)
        );

        assert_eq!(process.x_reg(0), Term::atom(Atom::ERROR));
        let reason = Tuple::new(process.x_reg(1)).expect("badmatch reason tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::BADMATCH)));
        assert_eq!(reason.get(1), Some(Term::small_int(42)));
        assert_eq!(process.x_reg(2), Term::NIL);
    }

    #[test]
    fn nested_try_uses_inner_handler_first_and_raise_preserves_stacktrace() {
        let code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::Label { label: 20 },
        ]);
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(10), &Operand::Label(10)).expect("outer try");
        try_(&mut process, &code, &Operand::X(20), &Operand::Label(20)).expect("inner try");

        assert_eq!(
            raise(
                &mut process,
                &Operand::Integer(777),
                &Operand::Atom(Some(Atom::BADARG)),
            ),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 1,
            }))
        );
        try_case(&mut process, &Operand::X(0)).expect("expose exception");

        assert_eq!(process.exception_handler_count(), 1);
        assert_eq!(process.x_reg(0), Term::atom(Atom::ERROR));
        assert_eq!(process.x_reg(1), Term::atom(Atom::BADARG));
        assert_eq!(process.x_reg(2), Term::small_int(777));
    }

    #[test]
    fn case_if_and_try_case_end_build_catchable_error_reasons() {
        let code = module(vec![Instruction::Label { label: 20 }]);
        let mut process = Process::new(1, 64);
        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        assert_eq!(
            case_end(&mut process, &Operand::Atom(Some(Atom::OK))),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
        try_case(&mut process, &Operand::X(0)).expect("expose");
        let reason = Tuple::new(process.x_reg(1)).expect("case_clause tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::CASE_CLAUSE)));
        assert_eq!(reason.get(1), Some(Term::atom(Atom::OK)));

        try_(&mut process, &code, &Operand::X(0), &Operand::Label(20)).expect("try");
        assert_eq!(
            if_end(&mut process),
            Ok(InstructionOutcome::Jump(CodePosition {
                module: Atom::OK,
                instruction_pointer: 0,
            }))
        );
        try_case(&mut process, &Operand::X(0)).expect("expose");
        let reason = Tuple::new(process.x_reg(1)).expect("if_clause tuple");
        assert_eq!(reason.get(0), Some(Term::atom(Atom::IF_CLAUSE)));
        assert_eq!(reason.get(1), Some(Term::NIL));
    }
}
