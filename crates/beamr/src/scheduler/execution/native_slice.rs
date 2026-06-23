//! Native-process time-slice execution.
//!
//! `run_native_slice` is the native counterpart of `core::execute_slice`. It
//! is reached from the single dispatch branch in `run_process` when
//! `process.is_native()` is true, and returns the SAME `SliceOutcome` the
//! bytecode path returns, so the shared Requeue / Wait / Exited handling in
//! `run_process` applies verbatim (`NativeOutcome::Continue -> Requeue`,
//! `Wait -> Wait`, `Stop -> Exited`).
//!
//! The one native-specific obligation is the exit-tombstone check: it runs
//! FIRST, before the handler is ever invoked, so a supervision kill issued
//! while the process would otherwise run is honoured (the handler is not
//! called and the process terminates).

use std::sync::Arc;

use crate::ets::copy::OwnedTerm;
use crate::native::native_process::{NativeContext, NativeOutcome};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{ExitReason, Process, ProcessStatus};
use crate::scheduler::{SharedState, supervision_integration};
use crate::term::Term;

use super::core::SliceOutcome;

/// Run one native slice for `process` and map its outcome to a `SliceOutcome`.
pub(in crate::scheduler) fn run_native_slice(
    shared: &Arc<SharedState>,
    process: &mut Process,
) -> SliceOutcome {
    let pid = process.pid();

    // R3: exit-tombstone check FIRST — before building services or invoking
    // the handler — so a supervision kill is honoured and `handle` never runs
    // for a process already marked dead. This is the SAME tombstone path used
    // at the post-slice check in `run_process`.
    if let Some(reason) = shared.exit_tombstones.get(&pid) {
        return SliceOutcome::Exited(reason, OwnedTerm::immediate(Term::NIL));
    }

    // Reuse the bytecode status graph: New / Yielded / Waiting -> Running.
    if transition_to_running(process).is_err() {
        return SliceOutcome::Exited(ExitReason::Error, OwnedTerm::immediate(Term::NIL));
    }

    // Take the handler out so `NativeContext` can borrow the rest of the
    // `Process`. It is restored before this function returns (except on Stop,
    // where the process is terminated and dropped anyway). If the body ever
    // lacks a handler between slices (it should not), rebuild it from the
    // retained factory rather than dropping the process to a dead no-op — the
    // same factory NATIVE-002 uses for supervised restart.
    let mut handler = match process.native_body_mut() {
        Some(body) => match body.handler.take() {
            Some(handler) => handler,
            None => (body.factory)(),
        },
        None => {
            // Unreachable while `is_native()` gates this call.
            return SliceOutcome::Exited(ExitReason::Normal, OwnedTerm::immediate(Term::NIL));
        }
    };

    // The native services bundle always populates `local_send` and
    // `spawn_facility`; if that ever changes, restore the handler and exit
    // rather than panicking.
    let services = supervision_integration::build_native_services(shared, process.namespace_id());
    let (Some(local_send), Some(spawn)) = (services.local_send, services.spawn_facility) else {
        if let Some(body) = process.native_body_mut() {
            body.handler = Some(handler);
        }
        return SliceOutcome::Exited(ExitReason::Error, OwnedTerm::immediate(Term::NIL));
    };

    let replay_driver = shared.replay_driver.clone();
    let timers = Some(shared.timers.clone());
    let mut context = NativeContext::new(process, local_send, spawn, replay_driver, timers);
    let outcome = handler.handle(&mut context);
    let replay_error = context.take_replay_error();
    drop(context);

    // Restore the handler for the next slice. Harmless on the Stop path.
    if let Some(body) = process.native_body_mut() {
        body.handler = Some(handler);
    }

    // A send that failed replay validation is a determinism violation: exit
    // the process deterministically, exactly like the bytecode send path.
    if let Some(error) = replay_error {
        shared.exit_errors.insert(pid, error);
        let result = crate::scheduler::exit_capture::capture_term(process.x_reg(0));
        process.terminate(ExitReason::Error);
        return SliceOutcome::Exited(ExitReason::Error, result);
    }

    match outcome {
        NativeOutcome::Continue => {
            let _transition = process.transition_to(ProcessStatus::Yielded);
            SliceOutcome::Requeue(take_process(process))
        }
        NativeOutcome::Wait => {
            // R4: route to the EXISTING `SliceOutcome::Wait` arm. No separate
            // park path, no native-specific waiting-set registration — the
            // 3-phase park-gap (store -> register -> recheck) in `run_process`
            // applies because a native process is a `Process`.
            let _transition = process.transition_to(ProcessStatus::Waiting);
            SliceOutcome::Wait(take_process(process))
        }
        NativeOutcome::Stop(reason) => {
            let result = crate::scheduler::exit_capture::capture_term(process.x_reg(0));
            process.terminate(reason);
            SliceOutcome::Exited(reason, result)
        }
    }
}

/// Move a schedulable native process to Running, mirroring the bytecode
/// slice-start transition. A native process is only ever New, Yielded, or
/// Waiting between slices (it never suspends).
fn transition_to_running(process: &mut Process) -> Result<(), ()> {
    match process.status() {
        ProcessStatus::Running => Ok(()),
        ProcessStatus::New | ProcessStatus::Yielded | ProcessStatus::Waiting => process
            .transition_to(ProcessStatus::Running)
            .map_err(|_| ()),
        _ => Err(()),
    }
}

/// Move the process out of `*process`, leaving a throwaway placeholder, so it
/// can be carried by an owning `SliceOutcome` (the mirror of `core`'s helper).
fn take_process(process: &mut Process) -> Process {
    std::mem::replace(process, Process::new(u64::MAX, DEFAULT_HEAP_SIZE))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{SliceOutcome, run_native_slice};
    use crate::atom::Atom;
    use crate::module::ModuleRegistry;
    use crate::namespace::NamespaceId;
    use crate::native::native_process::{NativeBody, NativeContext, NativeHandler, NativeOutcome};
    use crate::process::heap::DEFAULT_HEAP_SIZE;
    use crate::process::{ExitReason, Process};
    use crate::replay::{RecordedDeliveryKind, RecordedMessageDelivery, ReplayEvent, ReplayLog};
    use crate::scheduler::{Scheduler, SchedulerConfig, supervision_integration};
    use crate::term::Term;
    use std::sync::Arc;

    struct FlagHandler {
        invoked: Arc<AtomicBool>,
    }

    impl NativeHandler for FlagHandler {
        fn handle(&mut self, _ctx: &mut NativeContext<'_>) -> NativeOutcome {
            self.invoked.store(true, Ordering::SeqCst);
            NativeOutcome::Wait
        }
    }

    fn single_thread_scheduler() -> Scheduler {
        let config = SchedulerConfig {
            thread_count: Some(1),
            ..Default::default()
        };
        Scheduler::new(config, Arc::new(ModuleRegistry::new())).expect("scheduler starts")
    }

    #[test]
    fn tombstone_is_checked_before_handler_runs() {
        // R3: a kill pending for the pid must terminate the process with the
        // tombstone reason and the handler must NEVER be invoked.
        let scheduler = single_thread_scheduler();
        let invoked = Arc::new(AtomicBool::new(false));
        let invoked_for_handler = Arc::clone(&invoked);
        let pid = 4242;
        let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
        process.set_native_body(NativeBody::new(Box::new(move || {
            Box::new(FlagHandler {
                invoked: Arc::clone(&invoked_for_handler),
            })
        })));
        scheduler
            .shared
            .exit_tombstones
            .insert(pid, ExitReason::Kill);

        let outcome = run_native_slice(&scheduler.shared, &mut process);
        assert!(matches!(outcome, SliceOutcome::Exited(ExitReason::Kill, _)));
        assert!(
            !invoked.load(Ordering::SeqCst),
            "handler must not run for a tombstoned pid"
        );
        scheduler.shutdown();
    }

    #[test]
    fn native_send_validates_recorded_delivery_under_replay() {
        // R6: a native send routes through LocalSendFacility and is validated
        // against the recorded delivery in replay_mode.
        let message = Term::atom(Atom::OK);
        let log = ReplayLog::new(vec![ReplayEvent::MessageDelivery(
            RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(2),
                receiver_pid: 1,
                sender_clock: 1,
                receiver_clock: 2,
                message,
            },
        )]);
        let scheduler =
            Scheduler::new_replay(SchedulerConfig::default(), log).expect("replay scheduler");
        let receiver_pid = scheduler.spawn_test_process(false);
        assert_eq!(receiver_pid, 1, "first spawned pid is 1");

        let services =
            supervision_integration::build_native_services(&scheduler.shared, NamespaceId::DEFAULT);
        let local_send = services.local_send.clone().expect("local send facility");
        let spawn = services.spawn_facility.clone().expect("spawn facility");
        let replay_driver = scheduler.shared.replay_driver.clone();

        let mut sender = Process::new(2, DEFAULT_HEAP_SIZE);
        {
            let mut ctx = NativeContext::new(&mut sender, local_send, spawn, replay_driver, None);
            ctx.send(receiver_pid, message);
            assert!(
                ctx.take_replay_error().is_none(),
                "a matching recorded delivery must not error"
            );
        }
        assert_eq!(sender.logical_clock(), 1, "sender clock ticked once");
        assert_eq!(scheduler.has_message(receiver_pid, message), Some(true));
        assert!(
            scheduler
                .shared
                .replay_driver
                .as_ref()
                .expect("driver")
                .lock()
                .expect("driver lock")
                .is_complete(),
            "the recorded delivery was consumed"
        );
        scheduler.shutdown();
    }

    #[test]
    fn native_send_rolls_back_on_replay_mismatch() {
        // R6: a recorded clock mismatch is rejected, the sender clock is rolled
        // back, and no message is delivered.
        let message = Term::atom(Atom::OK);
        let log = ReplayLog::new(vec![ReplayEvent::MessageDelivery(
            RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(2),
                receiver_pid: 1,
                sender_clock: 99,
                receiver_clock: 2,
                message,
            },
        )]);
        let scheduler =
            Scheduler::new_replay(SchedulerConfig::default(), log).expect("replay scheduler");
        let receiver_pid = scheduler.spawn_test_process(false);

        let services =
            supervision_integration::build_native_services(&scheduler.shared, NamespaceId::DEFAULT);
        let local_send = services.local_send.clone().expect("local send facility");
        let spawn = services.spawn_facility.clone().expect("spawn facility");
        let replay_driver = scheduler.shared.replay_driver.clone();

        let mut sender = Process::new(2, DEFAULT_HEAP_SIZE);
        {
            let mut ctx = NativeContext::new(&mut sender, local_send, spawn, replay_driver, None);
            ctx.send(receiver_pid, message);
            assert!(
                ctx.take_replay_error().is_some(),
                "a clock mismatch must surface a replay error"
            );
        }
        assert_eq!(sender.logical_clock(), 0, "sender clock rolled back");
        assert_eq!(
            scheduler.has_message(receiver_pid, message),
            Some(false),
            "no message is delivered on a replay mismatch"
        );
        scheduler.shutdown();
    }
}
