//! Timer wheel and hook integration for the scheduler loop.
//!
//! Extracted from `mod.rs` to keep per-file line counts within the project
//! constraint (500 lines).

use std::time::Duration;

use crate::atom::Atom;
use crate::hook::{HookDecision, HookEvent};
use crate::process::{Process, ProcessStatus};
use crate::term::Term;
use crate::timer::TimerRef;

use super::{ScheduledProcess, SharedState, lock_or_recover};

/// Invoke the reduction-boundary hook if one is registered.
pub(super) fn invoke_hook(
    shared: &SharedState,
    process: &Process,
    reductions: u32,
) -> HookDecision {
    if !shared.hook.is_registered() {
        return HookDecision::Continue;
    }
    let (module, function, arity) = process.current_mfa().unwrap_or((Atom::NIL, Atom::NIL, 0));
    shared.hook.invoke(HookEvent {
        pid: process.pid(),
        module,
        function,
        arity,
        reductions_consumed: reductions,
    })
}

/// If the process has a pending `receive_timeout` and no timer reference yet,
/// schedule a timer in the shared wheel.
pub(super) fn register_receive_timer(shared: &SharedState, process: &mut Process) {
    let timeout = match process.receive_timeout() {
        Some(timeout) => timeout,
        None => return,
    };
    if process.receive_timer_ref().is_some() {
        return;
    }
    let delay = Duration::from_millis(timeout.milliseconds);
    let pid = process.pid();
    let timer_ref = lock_or_recover(&shared.timers).schedule(delay, pid, Term::NIL);
    process.set_receive_timer_ref(Some(timer_ref.id()));
}

/// Cancel a process's receive timer when a message arrives first.
pub(super) fn cancel_receive_timer(shared: &SharedState, pid: u64) {
    let timer_id = read_process_field(shared, pid, |p| p.receive_timer_ref());
    if let Some(id) = timer_id.flatten() {
        let _remaining = lock_or_recover(&shared.timers).cancel(TimerRef::from_id(id));
    }
}

/// Process expired timers: update the process code position to the timeout
/// label and move the process from the wait set to the woken list.
pub(super) fn tick_timers(shared: &SharedState) {
    let expired = lock_or_recover(&shared.timers).tick();
    for timer in expired {
        let pid = timer.target_pid;
        mutate_process(shared, pid, |process| {
            process.set_receive_timer_ref(None);
            if let Some(position) = process.receive_timeout().map(|t| t.timeout_position) {
                process.set_code_position(Some(position));
            }
        });
        let mut wait_set = lock_or_recover(&shared.wait_set);
        if let Some(index) = wait_set.waiting.remove(&pid) {
            wait_set.woken.push((pid, index));
            shared.wake_condvar.notify_all();
        }
    }
}

/// Resume a suspended process: transition it from Suspended to Yielded so the
/// scheduler picks it up, and move it from the wait set to the woken list.
/// Returns true if the process was found and resumed.
pub(super) fn resume_suspended(shared: &SharedState, pid: u64) -> bool {
    let transitioned = mutate_process_result(shared, pid, |process| {
        if process.status() == ProcessStatus::Suspended {
            process.transition_to(ProcessStatus::Yielded).is_ok()
        } else {
            false
        }
    });
    if transitioned != Some(true) {
        return false;
    }
    let mut wait_set = lock_or_recover(&shared.wait_set);
    if let Some(index) = wait_set.waiting.remove(&pid) {
        wait_set.woken.push((pid, index));
        shared.wake_condvar.notify_all();
        true
    } else {
        false
    }
}

fn read_process_field<T>(
    shared: &SharedState,
    pid: u64,
    f: impl FnOnce(&Process) -> T,
) -> Option<T> {
    let entry = shared.process_bodies.get(&pid)?;
    let slot = lock_or_recover(&entry);
    slot.as_ref().map(|ScheduledProcess(p)| f(p))
}

fn mutate_process(shared: &SharedState, pid: u64, f: impl FnOnce(&mut Process)) {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(process)) = slot.as_mut() {
            f(process);
        }
    }
}

fn mutate_process_result<T>(
    shared: &SharedState,
    pid: u64,
    f: impl FnOnce(&mut Process) -> T,
) -> Option<T> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    slot.as_mut().map(|ScheduledProcess(process)| f(process))
}
