//! Call-identity-gated suspension state shared between scheduler threads.
//!
//! Every result-gated suspension (host await, dirty call, hook suspend) is
//! identified by a per-process monotonically increasing call id, recorded on
//! the process at suspend time and mirrored here so side threads (completion
//! bridges, embedder wake calls) can publish completions *keyed by identity*
//! instead of by pid alone. The owning scheduler thread consumes a published
//! completion at the start of the process's next slice only when its call id
//! matches the process's current suspension record; stale completions are
//! dropped instead of being applied blind at the wrong park position.

use std::sync::Arc;

use crate::process::SuspensionKind;
use crate::scheduler::dirty::DirtyResult;
use crate::term::Term;

use super::SharedState;

/// Wildcard call id used by `Scheduler::resume_process` when the embedder
/// resumes before the hook suspension's call id is observable. Consumed by
/// the next hook suspension only — never by a dirty call or host await.
pub(super) const RESUME_ANY_HOOK: u64 = 0;

/// Side-thread-visible mirror of a process's current result-gated
/// suspension. Written exclusively by the thread that owns the process
/// (during its slice or at park), read by completion publishers and the
/// wake gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SuspensionMirror {
    pub(super) call_id: u64,
    pub(super) kind: SuspensionKind,
    /// True for message-wakeable host awaits (`request_suspend`): the wake
    /// gate lets any message wake them, and the slice-start gate re-executes
    /// the (re-entrant) native instead of re-parking.
    pub(super) wake_on_message: bool,
}

/// One completion published for suspension `(pid, call_id)`.
#[derive(Debug)]
pub(super) struct PendingSuspensionResult {
    pub(super) call_id: u64,
    pub(super) payload: SuspensionResultPayload,
}

/// Payload of a published suspension completion.
#[derive(Debug)]
pub(super) enum SuspensionResultPayload {
    /// Host-await result applied into x0 (the await call's return value).
    Host(Term),
    /// Dirty native completion (result/exception, plus follow-up requests).
    Dirty(DirtyResult),
}

impl SharedState {
    /// Mirror `pid`'s current suspension for side-thread publishers.
    pub(super) fn register_suspension_mirror(
        &self,
        pid: u64,
        call_id: u64,
        kind: SuspensionKind,
        wake_on_message: bool,
    ) {
        self.suspensions.insert(
            pid,
            SuspensionMirror {
                call_id,
                kind,
                wake_on_message,
            },
        );
    }

    /// Publish a completion for the exact suspension `(pid, call_id)`.
    ///
    /// Returns false (dropping the payload) when the process's current
    /// suspension is not `call_id` — the completion is stale, racing a
    /// timeout re-entry or an abandoned request — or when the process has
    /// exited. The post-insert liveness double-check removes an entry that
    /// raced `cleanup_exited_process`, so no dead-pid result can strand.
    pub(super) fn publish_suspension_result(
        &self,
        pid: u64,
        call_id: u64,
        payload: SuspensionResultPayload,
    ) -> bool {
        let matches = self
            .suspensions
            .get(&pid)
            .is_some_and(|mirror| mirror.call_id == call_id);
        if !matches {
            return false;
        }
        self.suspension_results
            .insert(pid, PendingSuspensionResult { call_id, payload });
        if self.process_table.get(pid).is_none() {
            let _orphan = self.suspension_results.remove(&pid);
            return false;
        }
        true
    }

    /// Publish a completion for `pid`'s *current* suspension of `kind`,
    /// resolving the call id at publish time.
    ///
    /// This is the pid-keyed embedder seam (`Scheduler::wake_with_result`):
    /// exact whenever at most one completion is outstanding per await. The
    /// id-keyed [`SharedState::publish_suspension_result`] is race-free even
    /// across timeout re-entries.
    pub(super) fn publish_suspension_result_current(
        &self,
        pid: u64,
        kind: SuspensionKind,
        payload: SuspensionResultPayload,
    ) -> bool {
        let Some(call_id) = self
            .suspensions
            .get(&pid)
            .filter(|mirror| mirror.kind == kind)
            .map(|mirror| mirror.call_id)
        else {
            return false;
        };
        self.publish_suspension_result(pid, call_id, payload)
    }

    /// True when `pid` has a result-gated suspension mirror and an event
    /// that its owning thread would consume at the next slice start: the
    /// matching completion, a file-I/O completion or fired receive timer
    /// (host awaits), or a matching/wildcard embedder resume (hook
    /// suspends).
    ///
    /// Used by the wake gate and the park-time rechecks. A process *without*
    /// a mirror is plain-receive parked and is always wakeable.
    pub(super) fn has_consumable_suspension_event(&self, pid: u64) -> bool {
        let Some(mirror) = self.suspensions.get(&pid).map(|mirror| *mirror) else {
            return false;
        };
        if self
            .suspension_results
            .get(&pid)
            .is_some_and(|result| result.call_id == mirror.call_id)
        {
            return true;
        }
        match mirror.kind {
            SuspensionKind::HostAwait => {
                self.file_io_results.contains_key(&pid)
                    || self.expired_receive_timers.contains_key(&pid)
            }
            SuspensionKind::DirtyCall => false,
            SuspensionKind::Hook => self
                .pending_resumes
                .get(&pid)
                .is_some_and(|resume| *resume == RESUME_ANY_HOOK || *resume == mirror.call_id),
        }
    }

    /// True when `pid` is parked under a result-gated suspension that plain
    /// message arrivals must not wake (no consumable event pending).
    /// Message-wakeable suspensions (select, marker awaits) never block.
    pub(super) fn suspension_blocks_wake(&self, pid: u64) -> bool {
        let gated = self
            .suspensions
            .get(&pid)
            .is_some_and(|mirror| !mirror.wake_on_message);
        gated && !self.has_consumable_suspension_event(pid)
    }

    /// Purge every per-pid suspension structure on process exit.
    pub(super) fn purge_suspension_state(&self, pid: u64) {
        let _mirror = self.suspensions.remove(&pid);
        let _result = self.suspension_results.remove(&pid);
        let _resume = self.pending_resumes.remove(&pid);
        let _file_io = self.file_io_results.remove(&pid);
    }
}

/// Scheduler-backed [`crate::native::SuspensionRegistrar`]: `request_suspend`
/// publishes the host-await call id before the native returns, so a host
/// completion racing the suspend always finds the mirror.
pub(super) struct SchedulerSuspensionRegistrar {
    pub(super) shared: Arc<SharedState>,
}

impl crate::native::SuspensionRegistrar for SchedulerSuspensionRegistrar {
    fn register_host_await(&self, pid: u64, call_id: u64, wake_on_message: bool) {
        self.shared.register_suspension_mirror(
            pid,
            call_id,
            SuspensionKind::HostAwait,
            wake_on_message,
        );
    }

    fn cancel_host_await(&self, pid: u64, call_id: u64) {
        self.shared
            .suspensions
            .remove_if(&pid, |_, mirror| mirror.call_id == call_id);
    }
}
