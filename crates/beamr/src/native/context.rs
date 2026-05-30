//! Minimal process-facing context exposed to native code.
//!
//! Native functions deliberately receive this allocation subset instead of the
//! full process so they cannot inspect scheduler, mailbox, or process internals.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::term::Term;
use crate::timer::{TimerRef, TimerWheel};

use super::spawn::SpawnFacility;

/// Minimal process-facing context exposed to native code.
///
/// Native functions deliberately receive this allocation subset instead of the
/// full process so they cannot inspect scheduler, mailbox, or process internals.
pub struct ProcessContext {
    pid: Option<u64>,
    timers: Option<Arc<Mutex<TimerWheel>>>,
    spawn_facility: Option<Arc<dyn SpawnFacility>>,
}

impl fmt::Debug for ProcessContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessContext")
            .field("pid", &self.pid)
            .field("timers", &self.timers)
            .field(
                "spawn_facility",
                &self.spawn_facility.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessContext {
    /// Creates an empty process context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pid: None,
            timers: None,
            spawn_facility: None,
        }
    }

    /// Creates a context with timer services for asynchronous timer BIFs.
    #[must_use]
    pub fn with_timer_services(pid: u64, timers: Arc<Mutex<TimerWheel>>) -> Self {
        Self {
            pid: Some(pid),
            timers: Some(timers),
            spawn_facility: None,
        }
    }

    /// Return the calling process id when provided by the runtime.
    #[must_use]
    pub fn pid(&self) -> Option<u64> {
        self.pid
    }

    /// Set the calling process id.
    pub fn set_pid(&mut self, pid: Option<u64>) {
        self.pid = pid;
    }

    /// Return the spawn facility, if one has been configured.
    #[must_use]
    pub fn spawn_facility(&self) -> Option<&dyn SpawnFacility> {
        self.spawn_facility.as_deref()
    }

    /// Set the spawn facility for process creation BIFs.
    pub fn set_spawn_facility(&mut self, facility: Option<Arc<dyn SpawnFacility>>) {
        self.spawn_facility = facility;
    }

    /// Schedule a timer via the runtime timer wheel.
    pub fn schedule_timer(
        &mut self,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> Option<TimerRef> {
        let timers = self.timers.as_ref()?;
        Some(
            timers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .schedule(delay, target_pid, message),
        )
    }

    /// Reserve a timer reference and schedule with a message derived from it.
    pub fn schedule_timer_with_reference<F>(
        &mut self,
        delay: Duration,
        target_pid: u64,
        message: F,
    ) -> Option<TimerRef>
    where
        F: FnOnce(TimerRef) -> Term,
    {
        let timers = self.timers.as_ref()?;
        let mut timers = timers.lock().unwrap_or_else(|error| error.into_inner());
        let reference = timers.reserve_reference();
        timers.schedule_reserved(reference, delay, target_pid, message(reference))
    }

    /// Cancel a timer via the runtime timer wheel.
    pub fn cancel_timer(&mut self, reference: TimerRef) -> Option<Duration> {
        let timers = self.timers.as_ref()?;
        timers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel(reference)
    }

    /// Allocates a term on the calling process heap.
    ///
    /// Gate 1 only has immediate terms, so this currently returns the term
    /// unchanged. Boxed values can later route through the process heap without
    /// changing the native calling convention.
    pub const fn allocate_term(&mut self, term: Term) -> Term {
        term
    }
}
