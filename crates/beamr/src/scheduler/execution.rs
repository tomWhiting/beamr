//! Scheduler execution loop, wake/resume, and process lifecycle handling.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_deque::Stealer;
use crossbeam_queue::SegQueue;

use crate::error::ExecError;
use crate::process::ExitReason;
use crate::term::Term;

use super::{
    RunQueue, Scheduler, SharedState, SpawnRequest, lock_or_recover,
    spawning::materialize_spawn_request, steal, timer_integration,
};

impl Scheduler {
    /// Return a callback suitable for mailbox senders to wake `pid`.
    pub fn wake_notifier(&self, pid: u64) -> impl Fn() + Send + Sync + 'static {
        let shared = Arc::clone(&self.shared);
        move || wake_process(&shared, pid)
    }

    /// Wake a process that is in the Waiting state after message arrival.
    pub fn wake_process(&self, pid: u64) {
        wake_process(&self.shared, pid);
    }

    /// Resume a suspended process, returning true if the process was found in
    /// the wait set and re-enqueued.
    pub fn resume_process(&self, pid: u64) -> bool {
        timer_integration::resume_suspended(&self.shared, pid)
    }

    /// Shut down all worker threads after their current time slice.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_condvar.notify_all();
        let mut threads = lock_or_recover(&self.threads);
        for handle in threads.drain(..) {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Block until the given process exits, returning its exit reason and
    /// the value in x(0) at the time of exit.
    pub fn run_until_exit(&self, pid: u64) -> (ExitReason, Term) {
        loop {
            if let Some(entry) = self.shared.exit_tombstones.get(&pid) {
                let reason = *entry;
                let result = self
                    .shared
                    .exit_results
                    .remove(&pid)
                    .map(|(_, term)| term)
                    .unwrap_or(Term::NIL);
                return (reason, result);
            }
            let guard = lock_or_recover(&self.shared.wait_set);
            let timeout = std::time::Duration::from_millis(10);
            let _ = self.shared.wake_condvar.wait_timeout(guard, timeout);
        }
    }

    /// Retrieve the execution error that caused a process to exit, if any.
    pub fn take_exit_error(&self, pid: u64) -> Option<ExecError> {
        self.shared.exit_errors.remove(&pid).map(|(_, e)| e)
    }

    /// Retrieve the BEAM exception that caused a process to exit, if any.
    pub fn take_exit_exception(&self, pid: u64) -> Option<crate::process::Exception> {
        self.shared.exit_exceptions.remove(&pid).map(|(_, e)| e)
    }

    /// Wake a suspended process with a result term.
    pub fn wake_with_result(&self, pid: u64, result: Term) {
        self.shared.async_results.insert(pid, result);
        wake_process(&self.shared, pid);
    }

    /// Terminate a process externally, writing an exit tombstone so that
    /// `run_until_exit` returns with the given reason.
    pub fn terminate_process(&self, pid: u64, reason: ExitReason) {
        if self.shared.exit_tombstones.contains_key(&pid) {
            return;
        }
        cleanup_exited_process(&self.shared, pid, reason);
    }
}

pub(in crate::scheduler) fn scheduler_loop(
    shared: &Arc<SharedState>,
    queue: &RunQueue,
    my_index: usize,
    stealers: &[Stealer<u64>],
    inject: &SegQueue<SpawnRequest>,
) {
    let mut last_victim = my_index;
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        drain_injected(shared, queue, inject);
        if my_index == 0 {
            timer_integration::tick_timers(shared);
        }
        drain_woken(shared, queue, my_index);
        let pid = match queue.pop() {
            Some(pid) => pid,
            None => {
                let (result, next_victim) =
                    steal::try_steal(queue, my_index, stealers, last_victim);
                last_victim = next_victim;
                match result {
                    steal::StealResult::Stolen { .. } => match queue.pop() {
                        Some(pid) => pid,
                        None => {
                            park_thread(shared);
                            continue;
                        }
                    },
                    steal::StealResult::Empty => {
                        park_thread(shared);
                        continue;
                    }
                }
            }
        };
        run_process(shared, queue, pid, my_index);
    }
}

fn drain_injected(shared: &SharedState, queue: &RunQueue, inject: &SegQueue<SpawnRequest>) {
    while let Some(request) = inject.pop() {
        let pid = materialize_spawn_request(shared, request);
        queue.push(pid);
    }
}

mod core;
pub(in crate::scheduler) use core::cleanup_exited_process;
use core::run_process;
#[cfg(test)]
pub(in crate::scheduler) use core::{
    SliceOutcome, cleanup_if_tombstoned_after_store, execute_slice, store_runnable_process,
    take_runnable_process,
};
pub(in crate::scheduler) fn wake_process(shared: &SharedState, pid: u64) {
    timer_integration::cancel_receive_timer(shared, pid);
    let mut wait_set = lock_or_recover(&shared.wait_set);
    if let Some(scheduler_index) = wait_set.waiting.remove(&pid) {
        wait_set.woken.push((pid, scheduler_index));
        shared.wake_condvar.notify_all();
    }
}

fn drain_woken(shared: &SharedState, queue: &RunQueue, my_index: usize) {
    let woken = {
        let mut wait_set = lock_or_recover(&shared.wait_set);
        let mut mine = Vec::new();
        wait_set.woken.retain(|(pid, sched_idx)| {
            if *sched_idx == my_index {
                mine.push(*pid);
                false
            } else {
                true
            }
        });
        mine
    };
    for pid in woken {
        if shared.process_table.get(pid).is_some() {
            queue.push(pid);
        }
    }
}

fn park_thread(shared: &SharedState) {
    #[cfg(test)]
    shared.idle_parks.fetch_add(1, Ordering::Relaxed);
    if shared.shutdown.load(Ordering::Acquire) {
        return;
    }
    let guard = lock_or_recover(&shared.wait_set);
    let timeout = std::time::Duration::from_millis(5);
    match shared.wake_condvar.wait_timeout(guard, timeout) {
        Ok(_) => {}
        Err(error) => {
            let _recovered = error.into_inner();
        }
    }
}
