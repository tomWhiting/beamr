//! Message passing and receive opcode handlers.

use crate::distribution::control::{DistributionSendError, DistributionSendFacility};
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::core;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::native::local_send::{LocalSendError, LocalSendFacility, LocalSendRequest};
use std::sync::{Arc, Mutex};

use crate::process::{CodePosition, Process, ProcessStatus, ReceiveTimeout};
use crate::replay::{RecordedDeliveryKind, ReplayDriver};
use crate::term::Term;
use crate::term::pid_ref::PidRef;

/// Send x(1) to the process identified by x(0) when the caller supplies a receiver.
pub fn send(
    process: &mut Process,
    receiver: Option<&mut Process>,
    distribution: Option<&dyn DistributionSendFacility>,
    replay_driver: Option<&Arc<Mutex<ReplayDriver>>>,
    local_send: Option<&dyn LocalSendFacility>,
) -> Result<InstructionOutcome, ExecError> {
    let target_term = process.x_reg(0);
    let target = PidRef::new(target_term).ok_or(ExecError::Badarg)?;
    let message = process.x_reg(1);
    if !target.is_local() {
        let facility = distribution.ok_or(ExecError::NoConnection)?;
        facility
            .send_remote(target_term, message)
            .map_err(distribution_send_error)?;
        #[cfg(feature = "telemetry")]
        crate::telemetry::metrics::record_message_sent();
        process.set_x_reg(0, message);
        return Ok(InstructionOutcome::Continue);
    }
    let target_pid = target.pid_number();
    if let Some(receiver) = receiver.filter(|receiver| receiver.pid() == target_pid) {
        // In-hand delivery path: the scheduler/unit test supplied the target
        // body by reference. Unchanged from the pre-facility behaviour.
        //
        // NOTE: this branch is vestigial for production. Live cross-process local
        // delivery now goes through `LocalSendFacility`; the scheduler passes
        // `receiver = None`, so this path is exercised only by unit tests that hand
        // a target `Process` directly to `dispatch_with_receiver`.
        let previous_sender_clock = process.logical_clock();
        let previous_receiver_clock = receiver.logical_clock();
        let sender_clock = process.tick_logical_clock();
        let receiver_clock = receiver.observe_message_clock(sender_clock);
        if let Some(driver) = replay_driver {
            let mut guard = match driver.lock() {
                Ok(guard) => guard,
                Err(error) => error.into_inner(),
            };
            let recorded = match guard.next_message_delivery(
                RecordedDeliveryKind::Message,
                Some(process.pid()),
                target_pid,
                message,
            ) {
                Ok(recorded) => recorded,
                Err(error) => {
                    process.set_logical_clock(previous_sender_clock);
                    receiver.set_logical_clock(previous_receiver_clock);
                    return Err(error.into());
                }
            };
            if recorded.sender_clock != sender_clock || recorded.receiver_clock != receiver_clock {
                process.set_logical_clock(previous_sender_clock);
                receiver.set_logical_clock(previous_receiver_clock);
                return Err(ExecError::ReplayMismatch(format!(
                    "message delivery clock mismatch: expected sender/receiver clocks ({}, {}), recorded ({}, {})",
                    sender_clock, receiver_clock, recorded.sender_clock, recorded.receiver_clock
                )));
            }
        }
        #[cfg(feature = "telemetry")]
        receiver
            .mailbox()
            .sender()
            .send_traced(process.pid(), target_pid, message, receiver.heap_mut())
            .map_err(send_error)?;
        #[cfg(not(feature = "telemetry"))]
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
    } else if target_pid == process.pid() {
        // Self-send: the sender's own body is taken out of the slot during its
        // slice (it is Executing), so it must be delivered to the in-hand
        // process directly rather than via the facility/slot. Both clocks are
        // this process's clock.
        send_to_self(process, message, replay_driver)?;
    } else if let Some(facility) = local_send {
        // Cross-process local send: route through the scheduler-implemented
        // facility, which performs the slot-locked delivery, the Present-case
        // clock/replay work, and the wake.
        let previous_sender_clock = process.logical_clock();
        let sender_clock = process.tick_logical_clock();
        let sender_pid = process.pid();
        if let Err(LocalSendError::ReplayMismatch(detail)) = facility.send_local(LocalSendRequest {
            target_pid,
            sender_pid,
            message,
            sender_clock,
            replay_driver,
        }) {
            process.set_logical_clock(previous_sender_clock);
            return Err(ExecError::ReplayMismatch(detail));
        }
        #[cfg(feature = "telemetry")]
        crate::telemetry::metrics::record_message_sent();
    }
    // No `local_send` facility (e.g. bare `run()` with no scheduler) falls
    // through here, preserving the pre-facility silent-set-x0 behaviour.
    process.set_x_reg(0, message);
    Ok(InstructionOutcome::Continue)
}

/// Deliver a message from a process to itself, performing the single-process
/// clock observation and replay check on the in-hand process.
fn send_to_self(
    process: &mut Process,
    message: Term,
    replay_driver: Option<&Arc<Mutex<ReplayDriver>>>,
) -> Result<(), ExecError> {
    let self_pid = process.pid();
    let previous_clock = process.logical_clock();
    let sender_clock = process.tick_logical_clock();
    let receiver_clock = process.observe_message_clock(sender_clock);
    if let Some(driver) = replay_driver {
        let mut guard = match driver.lock() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        };
        let recorded = match guard.next_message_delivery(
            RecordedDeliveryKind::Message,
            Some(self_pid),
            self_pid,
            message,
        ) {
            Ok(recorded) => recorded,
            Err(error) => {
                process.set_logical_clock(previous_clock);
                return Err(error.into());
            }
        };
        if recorded.sender_clock != sender_clock || recorded.receiver_clock != receiver_clock {
            process.set_logical_clock(previous_clock);
            return Err(ExecError::ReplayMismatch(format!(
                "message delivery clock mismatch: expected sender/receiver clocks ({}, {}), recorded ({}, {})",
                sender_clock, receiver_clock, recorded.sender_clock, recorded.receiver_clock
            )));
        }
    }
    #[cfg(feature = "telemetry")]
    process
        .mailbox()
        .sender()
        .send_traced(self_pid, self_pid, message, process.heap_mut())
        .map_err(send_error)?;
    #[cfg(not(feature = "telemetry"))]
    process
        .mailbox()
        .sender()
        .send(message, process.heap_mut())
        .map_err(send_error)?;
    Ok(())
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
    #[cfg(feature = "telemetry")]
    let removed = process.mailbox_mut().remove_current_message_with_trace();
    #[cfg(not(feature = "telemetry"))]
    let _ = process.mailbox_mut().remove_current_message();
    #[cfg(feature = "telemetry")]
    if let Some((_message, trace_context)) = removed {
        let wait_duration = process.take_receive_wait_duration();
        crate::telemetry::spans::record_message_receive(
            process.pid(),
            wait_duration,
            true,
            trace_context.as_ref(),
        );
    }
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
    // BEAM semantics: a message wakeup resumes at the fail label (the
    // receive loop, so the new message is scanned), while timer expiry falls
    // through to the instruction AFTER wait_timeout — the `timeout` opcode,
    // then the after-body. The code position still points at this
    // wait_timeout instruction while it executes, so the fall-through is the
    // next instruction pointer.
    let current = process
        .code_position()
        .ok_or(ExecError::InvalidOperand("wait_timeout code position"))?;
    let timeout_position = CodePosition {
        module: current.module,
        instruction_pointer: next_instruction_pointer(current.instruction_pointer)?,
    };
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
        .map_err(|_| ExecError::Badarg)?;
    #[cfg(feature = "telemetry")]
    process.mark_receive_wait_started();
    Ok(())
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
    use crate::replay::{RecordedMessageDelivery, ReplayEvent, ReplayLog};
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
            send(&mut sender, Some(&mut receiver), None, None, None),
            Ok(InstructionOutcome::Continue)
        );

        assert_eq!(sender.x_reg(0), message);
        assert_eq!(receiver.mailbox_mut().current_message(), Some(message));
    }

    #[test]
    fn replay_send_consumes_recorded_delivery_before_mailbox_visibility() {
        let mut sender = Process::new(0, 32);
        let mut receiver = Process::new(1, 32);
        let message = Term::atom(Atom::OK);
        sender.set_x_reg(0, Term::pid(1));
        sender.set_x_reg(1, message);
        let replay_driver = Arc::new(Mutex::new(ReplayDriver::new(ReplayLog::new(vec![
            ReplayEvent::MessageDelivery(RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(0),
                receiver_pid: 1,
                sender_clock: 1,
                receiver_clock: 2,
                message,
            }),
        ]))));

        assert_eq!(
            send(
                &mut sender,
                Some(&mut receiver),
                None,
                Some(&replay_driver),
                None
            ),
            Ok(InstructionOutcome::Continue)
        );

        assert_eq!(receiver.mailbox_mut().current_message(), Some(message));
        assert!(replay_driver.lock().expect("driver lock").is_complete());
    }

    #[test]
    fn replay_send_mismatch_does_not_enqueue_message() {
        let mut sender = Process::new(0, 32);
        let mut receiver = Process::new(1, 32);
        let message = Term::atom(Atom::OK);
        sender.set_x_reg(0, Term::pid(1));
        sender.set_x_reg(1, message);
        let replay_driver = Arc::new(Mutex::new(ReplayDriver::new(ReplayLog::new(vec![
            ReplayEvent::MessageDelivery(RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(99),
                receiver_pid: 1,
                sender_clock: 1,
                receiver_clock: 2,
                message,
            }),
        ]))));

        assert!(matches!(
            send(
                &mut sender,
                Some(&mut receiver),
                None,
                Some(&replay_driver),
                None
            ),
            Err(ExecError::ReplayMismatch(_))
        ));

        assert!(receiver.mailbox().is_empty());
        assert_eq!(sender.logical_clock(), 0);
        assert_eq!(receiver.logical_clock(), 0);
        assert_eq!(replay_driver.lock().expect("driver lock").cursor(), 0);
    }

    #[test]
    fn send_to_missing_pid_is_silent_drop() {
        let mut sender = Process::new(0, 32);
        sender.set_x_reg(0, Term::pid(99));
        sender.set_x_reg(1, Term::atom(Atom::OK));

        assert_eq!(
            send(&mut sender, None, None, None, None),
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

        assert_eq!(
            send(&mut sender, None, None, None, None),
            Err(ExecError::NoConnection)
        );
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
        // wait_timeout reads its own position to compute the timeout
        // fall-through; pretend it sits at ip 3.
        process.set_code_position(Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: 3,
        }));

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
            send(&mut sender, Some(&mut receiver), None, None, None),
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
        // The erlc receive-after shape: the wait_timeout fail label is the
        // receive loop, and the after-clause is the wait_timeout
        // fall-through (`timeout`, then the after-body).
        let receive_after_code = module(vec![
            Instruction::Label { label: 10 },
            Instruction::LoopRec {
                fail: Operand::Label(20),
                destination: Operand::X(0),
            },
            Instruction::RemoveMessage,
            Instruction::Return,
            Instruction::Label { label: 20 },
            Instruction::WaitTimeout {
                fail: Operand::Label(10),
                timeout: Operand::Unsigned(100),
            },
            Instruction::Timeout,
            Instruction::Return,
        ]);
        let mut process = Process::new(1, 32);

        assert_eq!(
            run(&mut process, &receive_after_code),
            Ok(ExecutionResult::Waiting)
        );
        assert_eq!(process.status(), ProcessStatus::Waiting);
        assert_eq!(
            process.code_position(),
            Some(CodePosition {
                module: Atom::OK,
                instruction_pointer: 1,
            }),
            "a message wakeup resumes at the receive loop (loop_rec)"
        );
        assert_eq!(
            process.receive_timeout(),
            Some(ReceiveTimeout {
                timeout_position: CodePosition {
                    module: Atom::OK,
                    instruction_pointer: 6,
                },
                milliseconds: 100,
            }),
            "timer expiry falls through to the timeout instruction after wait_timeout"
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
            run(&mut process, &receive_after_code),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.receive_timeout(), None);

        // A message wakeup rescans the loop and completes the receive.
        let mut process = Process::new(2, 32);
        assert_eq!(
            run(&mut process, &receive_after_code),
            Ok(ExecutionResult::Waiting)
        );
        process.mailbox_mut().push_owned(Term::small_int(5));
        process
            .transition_to(ProcessStatus::Running)
            .expect("message arrival requeues process");
        assert_eq!(
            run(&mut process, &receive_after_code),
            Ok(ExecutionResult::Exited(ExitReason::Normal))
        );
        assert_eq!(process.x_reg(0), Term::small_int(5));
        assert_eq!(process.receive_timeout(), None);
    }
}
