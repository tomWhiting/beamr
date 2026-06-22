//! Native process model — a Rust handler that runs as a first-class,
//! scheduler-supervised beamr process (Shape B of `NATIVE-PROCESS-DESIGN.md`).
//!
//! A native process *is* a [`crate::process::Process`] that additionally
//! carries a [`NativeBody`] (a [`NativeHandler`] plus the factory that built
//! it). It keeps the heap, [`crate::mailbox::Mailbox`], logical clock, the
//! `ProcessMetadata` swap, the 3-phase park-gap protocol, and exit-tombstones
//! unchanged. The only genuinely new behaviour is *what executes during a
//! slice*: if the process is native, the scheduler runs the handler (see
//! `scheduler::execution::native_slice::run_native_slice`) instead of the
//! bytecode interpreter.
//!
//! Concurrency note: this model introduces NO new synchronisation primitive.
//! [`NativeContext`] borrows the running `Process` and the shared services for
//! the duration of one slice only; sends route through the existing
//! [`LocalSendFacility`], and spawns through the existing [`SpawnFacility`].

use std::sync::{Arc, Mutex};

use crate::error::ExecError;
use crate::native::local_send::{LocalSendError, LocalSendFacility, LocalSendRequest};
use crate::native::spawn::{SpawnError, SpawnFacility};
use crate::process::{ExitReason, Process};
use crate::replay::ReplayDriver;
use crate::term::Term;

/// Factory that reconstructs a handler instance.
///
/// Taken at `spawn_native` time and stored on the [`NativeBody`] so a
/// supervisor can rebuild a crashed native child without cloning a live
/// handler (NATIVE-002 restart). `Send + Sync` because it is held inside a
/// scheduler slot that crosses threads.
pub type NativeHandlerFactory = Box<dyn Fn() -> Box<dyn NativeHandler> + Send + Sync>;

/// What a native process does when the scheduler gives it a slice.
///
/// `handle` is called when the process is scheduled (it has mail, was woken,
/// or just spawned). The handler drains and processes messages via `ctx`,
/// optionally sends replies or spawns children, and returns a [`NativeOutcome`]
/// describing how the slice ends.
pub trait NativeHandler: Send + 'static {
    /// Run one native slice against `ctx`.
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome;
}

/// How a native slice ends. Mapped to the scheduler's `SliceOutcome` by
/// `run_native_slice` (`Continue -> Requeue`, `Wait -> Wait`,
/// `Stop -> Exited`).
pub enum NativeOutcome {
    /// Re-queue immediately (more work to do this turn).
    Continue,
    /// Nothing to do; park until a message arrives. Routes through the
    /// existing 3-phase park-gap path — NOT a separate park.
    Wait,
    /// Terminate this process with the given reason (drives
    /// `cleanup_exited_process` and, later, supervision).
    Stop(ExitReason),
}

/// The native handler plus its factory, carried by a `Process`.
///
/// The handler is held in an `Option` so `run_native_slice` can take it out
/// for the duration of a slice (letting the [`NativeContext`] borrow the rest
/// of the `Process`) and put it back afterwards. The factory is retained for
/// restart and never dropped silently.
pub(crate) struct NativeBody {
    pub(crate) handler: Option<Box<dyn NativeHandler>>,
    pub(crate) factory: NativeHandlerFactory,
}

impl NativeBody {
    /// Build a body by invoking `factory` once to produce the initial handler.
    pub(crate) fn new(factory: NativeHandlerFactory) -> Self {
        let handler = factory();
        Self {
            handler: Some(handler),
            factory,
        }
    }
}

impl std::fmt::Debug for NativeBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeBody")
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

/// The capability surface a handler is given for exactly one slice.
///
/// Borrows the running `Process` and the shared services. All sends route
/// through [`LocalSendFacility`] (so sender-clock ticking and replay
/// validation are reused verbatim); spawns route through [`SpawnFacility`].
pub struct NativeContext<'a> {
    process: &'a mut Process,
    local_send: Arc<dyn LocalSendFacility>,
    spawn: Arc<dyn SpawnFacility>,
    replay_driver: Option<Arc<Mutex<ReplayDriver>>>,
    replay_error: Option<ExecError>,
}

impl<'a> NativeContext<'a> {
    /// Build a context over the running `Process` and the slice services.
    pub(crate) fn new(
        process: &'a mut Process,
        local_send: Arc<dyn LocalSendFacility>,
        spawn: Arc<dyn SpawnFacility>,
        replay_driver: Option<Arc<Mutex<ReplayDriver>>>,
    ) -> Self {
        Self {
            process,
            local_send,
            spawn,
            replay_driver,
            replay_error: None,
        }
    }

    /// PID of the running native process.
    #[must_use]
    pub fn self_pid(&self) -> u64 {
        self.process.pid()
    }

    /// True when there is at least one queued message to drain this slice.
    #[must_use]
    pub fn has_messages(&self) -> bool {
        !self.process.mailbox().is_empty()
    }

    /// True when this native process is trapping exits.
    #[must_use]
    pub fn trap_exit(&self) -> bool {
        self.process.trap_exit()
    }

    /// Enable or disable exit trapping for this native process — the native
    /// equivalent of `process_flag(trap_exit, true)`. Returns the previous
    /// value.
    ///
    /// When trapping is enabled, an exit signal from a linked process is
    /// converted into an `{'EXIT', source, reason}` message and delivered to
    /// this process's mailbox (drained at the slice boundary by the SAME
    /// shared store-back the bytecode path uses) instead of terminating it —
    /// so a native handler can supervise linked children. This flips the flag
    /// on the underlying `Process`, the single source of truth the pid-keyed
    /// `propagate_exit` path consults; it adds no native-specific trap state.
    pub fn set_trap_exit(&mut self, value: bool) -> bool {
        let previous = self.process.trap_exit();
        self.process.set_trap_exit(value);
        previous
    }

    /// Remove and return the next mailbox message in arrival order, or `None`
    /// when the mailbox is empty.
    ///
    /// Implemented over the existing `Mailbox` API (`current_message` then
    /// `remove_current_message`) — it adds no new mailbox method. The returned
    /// term references this process's own heap and is valid for the rest of
    /// the slice.
    pub fn recv(&mut self) -> Option<Term> {
        let message = self.process.mailbox_mut().current_message()?;
        let _removed = self.process.mailbox_mut().remove_current_message();
        Some(message)
    }

    /// Advance the selective-receive save pointer past the current message
    /// without removing it (the `Mailbox` skip primitive), for handlers that
    /// want to leave a message in place and scan the next one.
    pub fn skip_message(&mut self) {
        self.process.mailbox_mut().advance_save_pointer();
    }

    /// Send `message` to `target_pid`, routed through the existing
    /// [`LocalSendFacility`].
    ///
    /// Ticks this process's logical clock before delivery and passes the
    /// `sender_clock` through, exactly like `interpreter::opcodes::messaging`.
    /// On a replay mismatch the clock tick is rolled back and the error is
    /// recorded for `run_native_slice` to surface as an exit, so replay stays
    /// deterministic. A self-send lands in the process's own `Executing` slot
    /// and is merged into the mailbox at store-back (no special case here).
    pub fn send(&mut self, target_pid: u64, message: Term) {
        let previous_sender_clock = self.process.logical_clock();
        let sender_clock = self.process.tick_logical_clock();
        let sender_pid = self.process.pid();
        let request = LocalSendRequest {
            target_pid,
            sender_pid,
            message,
            sender_clock,
            replay_driver: self.replay_driver.as_ref(),
        };
        if let Err(LocalSendError::ReplayMismatch(detail)) = self.local_send.send_local(request) {
            self.process.set_logical_clock(previous_sender_clock);
            self.replay_error = Some(ExecError::ReplayMismatch(detail));
        }
    }

    /// Spawn a native child from `factory`, optionally linking it to this
    /// process, delegating to the same [`SpawnFacility`] the scheduler uses.
    pub fn spawn_native(
        &mut self,
        factory: NativeHandlerFactory,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.spawn
            .spawn_native(self.process.pid(), factory, link_to)
    }

    /// Take any replay-determinism error recorded by [`Self::send`] during the
    /// slice, so the caller can terminate the process deterministically.
    pub(crate) fn take_replay_error(&mut self) -> Option<ExecError> {
        self.replay_error.take()
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeBody, NativeContext, NativeHandler, NativeOutcome};
    use crate::process::Process;

    struct Noop;

    impl NativeHandler for Noop {
        fn handle(&mut self, _ctx: &mut NativeContext<'_>) -> NativeOutcome {
            NativeOutcome::Wait
        }
    }

    fn noop_body() -> NativeBody {
        NativeBody::new(Box::new(|| Box::new(Noop)))
    }

    #[test]
    fn process_with_native_body_reports_is_native() {
        let mut process = Process::new(7, 64);
        assert!(
            !process.is_native(),
            "a fresh bytecode process is not native"
        );
        process.set_native_body(noop_body());
        assert!(
            process.is_native(),
            "a process with a native body is native"
        );
        // R2: a native process carries no code position or x-registers.
        assert!(process.code_position().is_none());
        assert_eq!(process.x_reg(0), crate::term::Term::NIL);
    }

    #[test]
    fn structural_clone_is_non_native() {
        // R2 audit assertion: Process::clone drops the handler — the clone is a
        // non-native copy, never a dead no-op carrying a silently-lost handler.
        let mut process = Process::new(7, 64);
        process.set_native_body(noop_body());
        assert!(process.is_native());

        let clone = process.clone();
        assert!(
            !clone.is_native(),
            "a structural clone must be non-native (handler not cloned)"
        );
        assert!(process.is_native(), "the original retains its handler");
    }
}
