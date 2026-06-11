//! Timer wheel and hook integration for the scheduler loop.
//!
//! Extracted from `mod.rs` to keep per-file line counts within the project
//! constraint (500 lines).

use std::time::Duration;

use crate::atom::Atom;
use crate::hook::{HookDecision, HookEvent};
use crate::process::{ExitReason, Process, ProcessStatus};
use crate::term::Term;

use super::{ProcessSlot, ScheduledProcess, SharedState, lock_or_recover};

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

/// Process a timer batch recorded in the replay log.
pub(super) fn tick_replay_timers(shared: &SharedState) {
    let Some(driver) = &shared.replay_driver else {
        return;
    };
    let recorded = match driver.lock() {
        Ok(mut guard) => guard.next_timer_expiry(),
        Err(error) => error.into_inner().next_timer_expiry(),
    };
    match recorded {
        Ok(recorded) => {
            let _discarded = lock_or_recover(&shared.timers).tick_at(recorded.now);
            expire_timers(shared, recorded.expired);
        }
        Err(error) => fail_replay_timer(shared, error),
    }
}

fn fail_replay_timer(shared: &SharedState, error: crate::replay::ReplayMismatch) {
    let exec_error = crate::error::ExecError::from(error);
    for entry in &shared.process_bodies {
        let pid = *entry.key();
        shared.exit_errors.insert(pid, exec_error.clone());
        shared.exit_tombstones.insert(pid, ExitReason::Error);
        let _removed = shared.process_table.remove(pid);
    }
    shared
        .shutdown
        .store(true, std::sync::atomic::Ordering::Release);
    shared.wake_condvar.notify_all();
}

/// Process expired timers: mark each fired timer for its target process and
/// wake the process if it is parked in the wait set.
pub(super) fn tick_timers(shared: &SharedState) {
    let expired = lock_or_recover(&shared.timers).tick();
    expire_timers(shared, expired);
}

/// Mark-and-wake for fired timers. The timeout-label jump is NOT applied
/// here: the firing thread may observe the slot as `Executing` (the process
/// is mid-slice or inside a park gap), where a position mutation would be
/// lost or land on a stale position. Instead the fired timer id is recorded
/// in `expired_receive_timers`; the owning scheduler thread consumes the mark
/// at the start of the process's next slice and applies the jump there. A
/// process parked in the wait set is woken so that next slice happens
/// promptly; a process whose park raced the expiry is caught by the Wait
/// arm's post-registration recheck in `run_process`.
///
/// Stale fires (the receive completed or was replaced before the wheel
/// fired) insert marks whose ids no longer match the process's armed
/// receive-timer ref; consumption drops them, at worst after one benign
/// spurious wake.
fn expire_timers(shared: &SharedState, expired: Vec<crate::timer::ExpiredTimer>) {
    for timer in expired {
        let pid = timer.target_pid;
        if shared.process_table.get(pid).is_none() {
            continue;
        }
        mark_fired_receive_timer(shared, pid, timer.reference.id());
    }
}

/// Insert a fired-timer mark for `pid` and wake the process if it is parked.
///
/// Exit cleanup (`cleanup_exited_process`) can win the race against the
/// liveness check in `expire_timers`: it purges the process table and the
/// pid's marks between that check and the insert below, after which the
/// freshly inserted mark would orphan permanently — pids are never reused,
/// so nothing would ever consume or clear it. The post-insert double-check
/// closes that window: if the pid has vanished from the table, the mark is
/// removed again. A concurrent inserter for the same dead pid runs the same
/// double-check, so no dead-pid mark survives.
pub(super) fn mark_fired_receive_timer(shared: &SharedState, pid: u64, timer_id: u64) {
    shared
        .expired_receive_timers
        .entry(pid)
        .or_default()
        .push(timer_id);
    if shared.process_table.get(pid).is_none() {
        let _orphaned_mark = shared.expired_receive_timers.remove(&pid);
        return;
    }
    let mut wait_set = lock_or_recover(&shared.wait_set);
    if let Some(index) = wait_set.waiting.remove(&pid) {
        wait_set.woken.push((pid, index));
        shared.wake_condvar.notify_all();
    }
}

/// True when a fired receive timer is marked for `pid` and not yet consumed.
/// Used by the Wait arm's post-registration recheck: a timer that fired
/// before the pid was registered in the wait set woke nobody, so the parking
/// thread must notice the mark itself.
pub(super) fn has_pending_expired_timer(shared: &SharedState, pid: u64) -> bool {
    shared.expired_receive_timers.contains_key(&pid)
}

/// Consume `pid`'s fired-timer marks. If one of them is the process's armed
/// receive timer, clear the ref and jump to the recorded timeout
/// continuation (the instruction after the parking `wait_timeout`, or the
/// native resume position for suspend-based timeouts). Ids that match
/// nothing are stale — their receive completed before the wheel fired — and
/// are dropped.
pub(super) fn apply_expired_receive_timer(shared: &SharedState, process: &mut Process) {
    let Some((_, fired)) = shared.expired_receive_timers.remove(&process.pid()) else {
        return;
    };
    let (Some(timeout), Some(armed)) = (process.receive_timeout(), process.receive_timer_ref())
    else {
        return;
    };
    if fired.contains(&armed) {
        process.set_receive_timer_ref(None);
        process.set_code_position(Some(timeout.timeout_position));
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

fn mutate_process_result<T>(
    shared: &SharedState,
    pid: u64,
    f: impl FnOnce(&mut Process) -> T,
) -> Option<T> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => Some(f(process)),
        ProcessSlot::Executing(_) | ProcessSlot::Absent => None,
    }
}
