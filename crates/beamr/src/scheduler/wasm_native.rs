//! Cooperative single-threaded execution of native (`NativeHandler`) processes
//! on the [`WasmScheduler`] (WR-0 spike).
//!
//! The threaded scheduler drives native processes through
//! [`execution::native_slice::run_native_slice`](super::execution), which reaches
//! into the threaded [`SharedState`](super::SharedState) for its facilities,
//! timer wheel, replay driver, and exit tombstones. None of that exists on the
//! cooperative `WasmScheduler`, so this module provides single-threaded
//! equivalents of the two facilities a trivial native slice actually needs —
//! [`SpawnFacility`] and [`LocalSendFacility`] — plus the slice driver and a
//! native-aware turn.
//!
//! ## The single-threaded effect-buffer pattern
//!
//! A [`NativeContext`] holds its facilities as `Arc<dyn …>` and the facility
//! traits are `Send + Sync` (kept deliberately — see the design's Decision D3).
//! The scheduler owns the running [`Process`] for the duration of a slice, so a
//! facility cannot also borrow `&mut WasmScheduler`. Instead each facility
//! records its requested effect (a deferred send or a deferred spawn) into a
//! shared [`DeferredEffects`] buffer; the scheduler drains and applies the
//! buffer against its own state *after* the handler returns. The buffer is an
//! `Arc<Mutex<…>>` purely to satisfy the `Send + Sync` facility bounds — the
//! lock is uncontended because everything runs on one thread.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::ets::{OwnedTerm, copy_term_to_ets};
use crate::native::native_process::{
    NativeBody, NativeContext, NativeHandlerFactory, NativeOutcome,
};
use crate::native::spawn::{
    SpawnError, SpawnFacility, SpawnMonitorResult, SpawnOptions, SpawnOptionsResult,
};
use crate::native::{CapabilitySet, LocalSendError, LocalSendFacility, LocalSendRequest};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{ExitReason, Priority, Process, ProcessStatus};
use crate::supervision::link;
use crate::term::Term;

use super::{WasmAsyncCompletion, WasmRunSummary, WasmScheduler};

/// A native spawn requested by a handler during a slice, applied afterwards.
struct DeferredSpawn {
    pid: u64,
    factory: NativeHandlerFactory,
    link_to: Option<u64>,
}

/// A local send requested by a handler during a slice, applied afterwards.
///
/// The message is captured into self-owned storage immediately at request time
/// (while the sender heap is still alive) so it can outlive the slice and be
/// copied into the receiver heap at drain time.
struct DeferredSend {
    target_pid: u64,
    message: OwnedTerm,
}

/// Effects a native slice asked for, collected for the scheduler to apply once
/// the handler has returned and released its borrow of the running process.
#[derive(Default)]
struct DeferredEffects {
    spawns: Vec<DeferredSpawn>,
    sends: Vec<DeferredSend>,
}

/// Shared, single-threaded effect buffer handed to both facilities.
///
/// `Arc<Mutex<…>>` only to satisfy the `Send + Sync` facility bounds; never
/// contended (one thread).
type SharedEffects = Arc<Mutex<DeferredEffects>>;

/// Cooperative [`SpawnFacility`]: pre-allocates a pid and records the spawn.
struct CooperativeSpawn {
    effects: SharedEffects,
    next_pid: Arc<Mutex<u64>>,
}

impl SpawnFacility for CooperativeSpawn {
    fn spawn(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _function: Atom,
        _args: Vec<Term>,
        _link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        // WR-0 spike: only native spawning is exercised cooperatively.
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_native(
        &self,
        _caller_pid: u64,
        factory: NativeHandlerFactory,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        let pid = {
            let mut guard = lock(&self.next_pid);
            let pid = *guard;
            *guard = guard.saturating_add(1);
            pid
        };
        lock(&self.effects).spawns.push(DeferredSpawn {
            pid,
            factory,
            link_to,
        });
        Ok(pid)
    }

    fn spawn_monitor(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _function: Atom,
        _args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        _link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda_monitor(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_with_options(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _function: Atom,
        _args: Vec<Term>,
        _options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }

    fn spawn_lambda_with_options(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        _options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        Err(SpawnError::UnresolvedMfa)
    }
}

/// Cooperative [`LocalSendFacility`]: captures the message and records the send.
struct CooperativeLocalSend {
    effects: SharedEffects,
}

impl LocalSendFacility for CooperativeLocalSend {
    fn send_local(&self, request: LocalSendRequest<'_>) -> Result<(), LocalSendError> {
        // Capture the sender-heap term into self-owned storage now, while the
        // sender body is alive; a term that cannot be captured is dropped
        // silently, matching the dead-target drop semantics of the trait.
        if let Ok(message) = copy_term_to_ets(request.message) {
            lock(&self.effects).sends.push(DeferredSend {
                target_pid: request.target_pid,
                message,
            });
        }
        Ok(())
    }
}

/// Outcome of one cooperative native slice, mapped from [`NativeOutcome`].
enum NativeSliceResult {
    Continue,
    Wait,
    Stop(ExitReason),
}

impl WasmScheduler {
    /// Spawn a root native process from `factory` and make it runnable.
    ///
    /// Mirrors [`WasmScheduler::spawn_in`](super::WasmScheduler) for the native
    /// case: it allocates a pid, attaches the [`NativeBody`], and pushes the
    /// process onto the ready queue. Returns the new pid.
    pub fn spawn_native_root(&mut self, factory: NativeHandlerFactory) -> u64 {
        let pid = self.alloc_pid();
        let mut process = Process::with_capabilities(pid, DEFAULT_HEAP_SIZE, CapabilitySet::all());
        process.set_group_leader(Term::pid(pid));
        process.set_priority(Priority::Normal);
        process.set_native_body(NativeBody::new(factory));
        self.ready.push(pid, process.priority());
        self.processes.insert(pid, process);
        pid
    }

    /// Deliver an async-NIF completion to a parked NATIVE process as a mailbox
    /// message and wake it (WR-7).
    ///
    /// Called by [`WasmScheduler::complete_async`](super::WasmScheduler) when the
    /// target is native. The completion payload is wrapped on the target's own
    /// heap as `{ok, Value}` (fulfilment) or `{error, Reason}` (rejection) — the
    /// same shape a Gleam/Erlang caller expects from an async result — pushed
    /// through the standard owner-side mailbox path, and the process woken. The
    /// resumed handler reads it with
    /// [`NativeContext::recv`](crate::native::native_process::NativeContext::recv).
    /// Returns whether the process was woken (false if it was not parked, e.g. a
    /// duplicate completion); the message is still enqueued so it is never lost.
    pub(super) fn deliver_native_async_completion(
        &mut self,
        pid: u64,
        completion: WasmAsyncCompletion,
    ) -> bool {
        let (tag, payload) = match completion {
            WasmAsyncCompletion::Ok(term) => (Atom::OK, term),
            WasmAsyncCompletion::Error(term) => (Atom::ERROR, term),
        };
        let was_waiting = self.waiting.contains(&pid);
        let Some(process) = self.processes.get_mut(&pid) else {
            return false;
        };
        // Build `{tag, Value}` on the target heap, then deliver it. A heap-full
        // target drops the completion (BEAM delivery is best-effort) but is still
        // woken explicitly so it can make progress rather than hang.
        match copy_payload_into_tuple(process, tag, payload) {
            Some(envelope) => {
                process.mailbox_mut().push_owned(envelope);
                // Cancels any receive timer and wakes the process if it was parked,
                // exactly as a normal local send / timer delivery does.
                self.after_successful_enqueue(pid);
            }
            None => {
                let _woken = self.wake(pid);
            }
        }
        was_waiting
    }

    /// Run one cooperative turn that understands native processes.
    ///
    /// Native processes run their [`NativeHandler`] for one slice via the
    /// cooperative facilities; any other process is left for the bytecode turn.
    /// Returns the pids that exited during this turn.
    pub fn run_native_until_idle(&mut self) -> Vec<u64> {
        let mut exited = Vec::new();
        let budget = self.ready_len();

        for _ in 0..budget {
            let Some(pid) = self.ready.pop() else {
                break;
            };
            if self.waiting.contains(&pid) {
                continue;
            }
            let Some(mut process) = self.processes.remove(&pid) else {
                continue;
            };
            if !process.is_native() {
                // Not a native process: requeue and leave it for the bytecode
                // turn. (Mixed scheduling is WR-3, out of WR-0 scope.)
                let priority = process.priority();
                self.processes.insert(pid, process);
                self.ready.push(pid, priority);
                continue;
            }

            match self.run_one_native_slice(&mut process) {
                NativeSliceResult::Continue => {
                    let priority = process.priority();
                    let _transition = process.transition_to(ProcessStatus::Yielded);
                    self.processes.insert(pid, process);
                    self.ready.push(pid, priority);
                }
                NativeSliceResult::Wait => {
                    let _transition = process.transition_to(ProcessStatus::Waiting);
                    self.processes.insert(pid, process);
                    self.waiting.insert(pid);
                }
                NativeSliceResult::Stop(reason) => {
                    let result = capture_exit_result(&process);
                    // WR-5: propagate the exit to linked processes BEFORE
                    // terminating (which clears the link set), then drop the
                    // process and record its exit.
                    self.propagate_native_exit(&mut process, reason);
                    process.terminate(reason);
                    self.record_native_exit(pid, reason, result);
                    exited.push(pid);
                }
            }
        }

        exited
    }

    /// Dispatch one native slice for a process already popped from the ready
    /// queue inside [`WasmScheduler::run_until_idle`](super::WasmScheduler), and
    /// fold its outcome into the turn's [`WasmRunSummary`].
    ///
    /// This is the WR-3 native branch: it lets native (`NativeHandler`) and
    /// bytecode processes share a single host pump. `process` has already been
    /// removed from `self.processes` and transitioned to `Running` by the caller;
    /// on `Continue`/`Wait` it is re-inserted, and on `Stop` it is terminated and
    /// its exit recorded the same way the standalone native turn records it.
    pub(super) fn dispatch_native_in_turn(
        &mut self,
        pid: u64,
        priority: Priority,
        mut process: Process,
        summary: &mut WasmRunSummary,
        yielded_next_tick: &mut Vec<(u64, Priority)>,
    ) {
        summary.executed += 1;
        match self.run_one_native_slice(&mut process) {
            NativeSliceResult::Continue => {
                let _transition = process.transition_to(ProcessStatus::Yielded);
                self.processes.insert(pid, process);
                yielded_next_tick.push((pid, priority));
                summary.yielded.push(pid);
            }
            NativeSliceResult::Wait => {
                let _transition = process.transition_to(ProcessStatus::Waiting);
                self.processes.insert(pid, process);
                self.waiting.insert(pid);
                summary.waiting.push(pid);
            }
            NativeSliceResult::Stop(reason) => {
                let result = capture_exit_result(&process);
                // WR-5: propagate to linked processes before termination clears
                // the link set, then drop and record the exit.
                self.propagate_native_exit(&mut process, reason);
                process.terminate(reason);
                self.record_native_exit(pid, reason, result);
                summary.exited.push(pid);
            }
        }
    }

    /// Execute exactly one native slice for an already-removed `process`,
    /// applying any sends/spawns it requested before returning.
    fn run_one_native_slice(&mut self, process: &mut Process) -> NativeSliceResult {
        if transition_to_running(process).is_err() {
            return NativeSliceResult::Stop(ExitReason::Error);
        }

        let mut handler = match process.native_body_mut() {
            Some(body) => body.handler.take().unwrap_or_else(|| (body.factory)()),
            None => return NativeSliceResult::Stop(ExitReason::Normal),
        };

        let effects: SharedEffects = Arc::new(Mutex::new(DeferredEffects::default()));
        let local_send: Arc<dyn LocalSendFacility> = Arc::new(CooperativeLocalSend {
            effects: Arc::clone(&effects),
        });
        let spawn: Arc<dyn SpawnFacility> = Arc::new(CooperativeSpawn {
            effects: Arc::clone(&effects),
            next_pid: Arc::clone(&self.shared_next_pid),
        });

        // WR-4: hand the slice the scheduler's shared native timer wheel so
        // `NativeContext::send_after`/`schedule` build real `Deliver` timers
        // (instead of the `None`/inert wheel of the WR-0 spike). The replay
        // driver stays `None`: replay is `threads`-gated and not part of the
        // cooperative wasm runtime (design §8 open question 3).
        let timers = Arc::clone(&self.native_timers);
        // WR-7: hand the slice the scheduler's installed async-NIF host bridge so
        // a `NativeHandler` can `start_async` host work (fetch/OPFS/JS) through the
        // SAME `WasmAsyncNifFacility` the bytecode async-NIF path uses; `None` when
        // no host facility is installed (the handler's `start_async` then errors).
        let async_facility = self.wasm_async_nif_facility.clone();
        let outcome = {
            let mut context = NativeContext::new(process, local_send, spawn, None, Some(timers));
            context.set_wasm_async_nif_facility(async_facility);
            handler.handle(&mut context)
        };

        if let Some(body) = process.native_body_mut() {
            body.handler = Some(handler);
        }

        self.apply_deferred_effects(process, &effects);

        match outcome {
            NativeOutcome::Continue => NativeSliceResult::Continue,
            NativeOutcome::Wait => NativeSliceResult::Wait,
            NativeOutcome::Stop(reason) => NativeSliceResult::Stop(reason),
        }
    }

    /// Drain the slice's effect buffer: materialize spawned children and
    /// deliver queued local sends against this scheduler's own state.
    ///
    /// `running` is the process whose handler produced these effects. It has
    /// been removed from `self.processes` for the slice's duration, so the
    /// parent side of any `link_to` link must be added to it here directly
    /// rather than via the process map (a deferred spawn's `link_to` is always
    /// the caller, i.e. the running process — that is the only pid a handler can
    /// pass through `spawn_native`).
    fn apply_deferred_effects(&mut self, running: &mut Process, effects: &SharedEffects) {
        let drained = {
            let mut guard = lock(effects);
            std::mem::take(&mut *guard)
        };
        let running_pid = running.pid();
        for spawn in drained.spawns {
            if spawn.link_to == Some(running_pid) {
                let _linked = running.add_link(spawn.pid);
            }
            self.materialize_native_child(spawn);
        }
        for send in drained.sends {
            // A missing target is a silent drop (BEAM semantics); ignore errors.
            let _delivered = self.send_owned(send.target_pid, &send.message);
        }
    }

    /// Build the `Process` for a deferred native spawn and make it runnable.
    ///
    /// When `link_to` is set, the child→parent half of the link is recorded on
    /// the child here. The parent→child half is added in
    /// [`WasmScheduler::apply_deferred_effects`] when the parent is the running
    /// process (the only possible `link_to` target for a handler-initiated
    /// spawn); a `link_to` naming any other resident process is linked here too,
    /// mirroring the threaded `spawn_native` bidirectional link establishment.
    fn materialize_native_child(&mut self, spawn: DeferredSpawn) {
        let DeferredSpawn {
            pid,
            factory,
            link_to,
        } = spawn;
        let mut process = Process::with_capabilities(pid, DEFAULT_HEAP_SIZE, CapabilitySet::all());
        process.set_group_leader(Term::pid(pid));
        process.set_priority(Priority::Normal);
        process.set_native_body(NativeBody::new(factory));
        if let Some(parent_pid) = link_to {
            let _child_linked = process.add_link(parent_pid);
            if let Some(parent) = self.processes.get_mut(&parent_pid) {
                let _parent_linked = parent.add_link(pid);
            }
        }
        self.ready.push(pid, process.priority());
        self.processes.insert(pid, process);
    }

    /// WR-5: propagate `exiting`'s exit to its linked processes using BEAM link
    /// semantics, cascading transitively through chains of non-trapping links —
    /// the cooperative analogue of the threaded
    /// [`LinkRegistry::process_exited`](crate::supervision::link::LinkRegistry::process_exited)
    /// worklist.
    ///
    /// Seeded with `exiting`'s links, the worklist processes one `(source,
    /// target, reason)` edge at a time. For each target (skipping any already
    /// gone from the process table, so cycles and shared targets are visited at
    /// most once):
    /// - the reverse link is severed first, so the dead source is never
    ///   re-signalled;
    /// - a process that should die (`should_die_from_signal`: an untrappable
    ///   `Kill`, or any abnormal reason while not trapping) is removed from the
    ///   process table and every ready/waiting set, its exit recorded with the
    ///   terminal reason, and ITS OWN links enqueued so the cascade continues
    ///   from it. Removing it (rather than terminating in place) is essential:
    ///   the turn loops skip a popped pid whose entry is gone from the table, so
    ///   a stale ready entry cannot re-enter a dead process's handler;
    /// - a trapping survivor receives an `{'EXIT', source, reason}` message (the
    ///   same builder the threaded path uses) carrying the ORIGINAL reason, and
    ///   is woken if parked so a supervisor can decide to restart.
    fn propagate_native_exit(&mut self, exiting: &mut Process, reason: ExitReason) {
        let mut cascade: VecDeque<(u64, u64, ExitReason)> = exiting
            .take_links()
            .into_iter()
            .map(|linked_pid| (exiting.pid(), linked_pid, reason))
            .collect();

        while let Some((source_pid, linked_pid, signal_reason)) = cascade.pop_front() {
            let Some(target) = self.processes.get_mut(&linked_pid) else {
                continue;
            };
            // Sever the reverse link first so the dead source is never re-signalled.
            let _unlinked = target.remove_link(source_pid);

            if link::should_die_from_signal(target, signal_reason) {
                let terminal = link::terminal_reason(signal_reason);
                // Take the dying target's own links to continue the cascade, then
                // remove it from scheduling entirely.
                let onward = target.take_links();
                target.terminate(terminal);
                cascade.extend(onward.into_iter().map(|next| (linked_pid, next, terminal)));
                self.processes.remove(&linked_pid);
                self.waiting.remove(&linked_pid);
                self.record_native_exit(linked_pid, terminal, undefined_result());
            } else if target.trap_exit() {
                link::enqueue_exit_message_pub(target, source_pid, signal_reason);
                // Wake the (possibly parked) supervisor so it runs and sees the
                // `{'EXIT', …}` message; a no-op if it was already runnable.
                let _woken = self.wake(linked_pid);
            }
        }
    }
}

/// Exit result for a process killed by link propagation (it produced no x(0)).
fn undefined_result() -> OwnedTerm {
    OwnedTerm::immediate(Term::atom(Atom::UNDEFINED))
}

/// Build `{tag, Value}` on `process`'s heap from an owned async-completion
/// `payload`, copying the payload into the heap first (WR-7 native delivery).
/// Returns `None` when the heap cannot fit the copied payload or the tuple,
/// matching the best-effort drop semantics of mailbox delivery.
fn copy_payload_into_tuple(process: &mut Process, tag: Atom, payload: OwnedTerm) -> Option<Term> {
    let value = payload.copy_to_heap(process.heap_mut()).ok()?;
    let elements = [Term::atom(tag), value];
    // A tuple of arity N needs N + 1 heap words (1 header + N elements), exactly
    // as `NativeContext::alloc_tuple` sizes its allocation.
    let words = 1usize.checked_add(elements.len())?;
    let slice = process.heap_mut().alloc_slice(words).ok()?;
    crate::term::boxed::write_tuple(slice, &elements)
}

/// Move a schedulable native process to `Running`, mirroring the threaded
/// slice-start transition. A native process is only ever New, Yielded, or
/// Waiting between slices.
fn transition_to_running(process: &mut Process) -> Result<(), ()> {
    match process.status() {
        ProcessStatus::Running => Ok(()),
        ProcessStatus::New | ProcessStatus::Yielded | ProcessStatus::Waiting => process
            .transition_to(ProcessStatus::Running)
            .map_err(|_| ()),
        _ => Err(()),
    }
}

/// Capture the exiting process's x(0) into self-owned storage, falling back to
/// `undefined` for terms that cannot leave their heap.
fn capture_exit_result(process: &Process) -> OwnedTerm {
    copy_term_to_ets(process.x_reg(0))
        .unwrap_or_else(|_| OwnedTerm::immediate(Term::atom(Atom::UNDEFINED)))
}

/// Lock a single-threaded mutex, recovering from poisoning (which cannot occur
/// without a panic across the uncontended lock, but must be handled to keep the
/// path panic-free).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[path = "wasm_native_tests.rs"]
mod tests;
