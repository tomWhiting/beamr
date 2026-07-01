//! Optional OpenTelemetry integration for Beamr runtime events.
//!
//! This module is compiled only with the `telemetry` feature so default builds
//! do not carry OpenTelemetry dependencies or call-site overhead.

pub mod lifecycle;
pub mod metrics;
pub mod spans;

pub use metrics::{
    record_workflow_finished, record_workflow_started, record_workflow_step_completed,
};
pub use spans::{
    ProcessTraceContext, TraceCarrier, extract_context, inject_context, inject_current_context,
};

#[must_use]
pub fn current_trace_context() -> TraceCarrier {
    inject_current_context()
}

#[cfg(test)]
pub(crate) mod test_lock {
    use std::sync::{Mutex, MutexGuard};

    /// Serializes tests that install process-global OpenTelemetry providers
    /// (tracer / meter / logger) or drive the process-wide metric instruments.
    /// Those tests share one global provider slot and the `INSTRUMENTS`
    /// `OnceLock`, so running them concurrently lets one test observe another's
    /// spans/metrics (or a stale provider). Hold this guard for the duration of
    /// any such test.
    static TELEMETRY_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the shared telemetry-test lock, recovering from poisoning so a
    /// panicking test does not cascade into unrelated failures.
    pub(crate) fn guard() -> MutexGuard<'static, ()> {
        TELEMETRY_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
