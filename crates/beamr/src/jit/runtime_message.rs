//! Message-passing runtime helpers callable from JIT-generated code.

use crate::process::{JitStatus, Process, ProcessStatus, ReceiveTimeout};
use crate::term::Term;
use crate::term::pid_ref::PidRef;

use super::ir_exceptions::JitReturn;
use super::runtime::process_from_abi;

const RECEIVE_STATUS_MESSAGE: u8 = 0;
const RECEIVE_STATUS_EMPTY: u8 = 1;
const WAIT_STATUS_NEW_MESSAGE: u8 = 0;
const WAIT_STATUS_TIMEOUT: u8 = 1;
const WAIT_STATUS_WAITING: u8 = 2;

pub(crate) extern "C" fn jit_send_message(
    process: *mut Process,
    dest_pid: u64,
    message: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return message;
    };
    let message_term = Term::from_raw(message);
    if let Some(PidRef::Local(pid)) = PidRef::new(Term::from_raw(dest_pid))
        && pid == process.pid()
    {
        process.mailbox_mut().push_owned(message_term);
        if process.status() == ProcessStatus::Waiting {
            let _ = process.transition_to(ProcessStatus::Running);
        }
    }
    message
}

pub(crate) extern "C" fn jit_receive_peek(process: *mut Process) -> JitReturn {
    let Some(process) = process_from_abi(process) else {
        return receive_return(RECEIVE_STATUS_EMPTY, 0);
    };
    match process.mailbox_mut().current_message() {
        Some(message) => receive_return(RECEIVE_STATUS_MESSAGE, message.raw()),
        None => receive_return(RECEIVE_STATUS_EMPTY, 0),
    }
}

const fn receive_return(status: u8, value: u64) -> JitReturn {
    JitReturn {
        status,
        _padding: [0; 7],
        value,
    }
}

pub(crate) extern "C" fn jit_receive_next(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    process.mailbox_mut().advance_save_pointer();
}

pub(crate) extern "C" fn jit_receive_accept(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    let _ = process.mailbox_mut().remove_current_message();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
}

pub(crate) extern "C" fn jit_receive_wait(process: *mut Process) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return WAIT_STATUS_WAITING;
    };
    if process.mailbox_mut().current_message().is_some() {
        return WAIT_STATUS_NEW_MESSAGE;
    }
    transition_process_to_waiting(process);
    process.set_jit_status(Some(JitStatus::Yield));
    WAIT_STATUS_WAITING
}

pub(crate) extern "C" fn jit_receive_wait_timeout(process: *mut Process, timeout: u64) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return WAIT_STATUS_WAITING;
    };
    if process.mailbox_mut().current_message().is_some() {
        return WAIT_STATUS_NEW_MESSAGE;
    }
    let milliseconds = Term::from_raw(timeout)
        .as_small_int()
        .and_then(|value| u64::try_from(value).ok());
    if milliseconds == Some(0) {
        return WAIT_STATUS_TIMEOUT;
    }
    if let Some(milliseconds) = milliseconds
        && let Some(position) = process.code_position()
    {
        process.set_receive_timeout(Some(ReceiveTimeout {
            timeout_position: position,
            milliseconds,
        }));
        process.set_receive_timer_ref(None);
    }
    transition_process_to_waiting(process);
    process.set_jit_status(Some(JitStatus::Yield));
    WAIT_STATUS_WAITING
}

pub(crate) extern "C" fn jit_receive_timeout(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    process.mailbox_mut().reset_save_pointer();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
}

fn transition_process_to_waiting(process: &mut Process) {
    if process.status() == ProcessStatus::New {
        let _ = process.transition_to(ProcessStatus::Running);
    }
    let _ = process.transition_to(ProcessStatus::Waiting);
}
