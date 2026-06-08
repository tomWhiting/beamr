//! Process-side asynchronous I/O submission facility.

use std::fmt;
use std::sync::Arc;

use super::bridge::{PendingIoRegistry, ResultMode};
use super::{CompletionRing, IoOp};

/// Errors returned while submitting an asynchronous I/O request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IoError {
    /// The facility was asked to submit without a known caller pid.
    MissingPid,
    /// No asynchronous I/O facility is configured for the current runtime.
    Unavailable,
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPid => f.write_str("I/O submission requires a caller pid"),
            Self::Unavailable => f.write_str("asynchronous I/O is not configured"),
        }
    }
}

impl std::error::Error for IoError {}

/// Facility exposed to process-native code for asynchronous I/O submission.
pub trait IoFacility: Send + Sync {
    /// Submit an operation for this facility's bound process and suspend it.
    fn submit_and_suspend(&self, op: IoOp, mode: ResultMode) -> Result<(), IoError>;

    /// Submit an operation for an explicit process id and suspend it.
    fn submit_and_suspend_for_pid(
        &self,
        pid: u64,
        op: IoOp,
        mode: ResultMode,
    ) -> Result<(), IoError>;
}

/// Completion-ring-backed implementation of [`IoFacility`].
#[derive(Clone)]
pub struct CompletionRingIoFacility {
    ring: Arc<dyn CompletionRing>,
    registry: Arc<PendingIoRegistry>,
    pid: Option<u64>,
}

impl CompletionRingIoFacility {
    /// Create an unbound facility. Callers should use `submit_and_suspend_for_pid`.
    #[must_use]
    pub fn new(ring: Arc<dyn CompletionRing>, registry: Arc<PendingIoRegistry>) -> Self {
        Self {
            ring,
            registry,
            pid: None,
        }
    }

    /// Create a facility bound to one caller pid.
    #[must_use]
    pub fn bound(
        ring: Arc<dyn CompletionRing>,
        registry: Arc<PendingIoRegistry>,
        pid: u64,
    ) -> Self {
        Self {
            ring,
            registry,
            pid: Some(pid),
        }
    }

    fn submit_for_pid(&self, pid: u64, op: IoOp, mode: ResultMode) {
        let op_id = self.ring.submit(op);
        self.registry.register(op_id, pid, mode);
    }
}

impl IoFacility for CompletionRingIoFacility {
    fn submit_and_suspend(&self, op: IoOp, mode: ResultMode) -> Result<(), IoError> {
        let Some(pid) = self.pid else {
            return Err(IoError::MissingPid);
        };
        self.submit_for_pid(pid, op, mode);
        Ok(())
    }

    fn submit_and_suspend_for_pid(
        &self,
        pid: u64,
        op: IoOp,
        mode: ResultMode,
    ) -> Result<(), IoError> {
        self.submit_for_pid(pid, op, mode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{IoCompletion, IoResult};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[derive(Default)]
    struct MockRing {
        next: AtomicU64,
        submitted: Mutex<Vec<IoOp>>,
    }

    impl CompletionRing for MockRing {
        fn submit(&self, op: IoOp) -> u64 {
            self.submitted
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(op);
            self.next.fetch_add(1, Ordering::Relaxed) + 1
        }

        fn poll_completions(&self, _timeout: Duration) -> Vec<IoCompletion> {
            Vec::new()
        }

        fn pending_count(&self) -> usize {
            0
        }

        fn shutdown(&self) {}
    }

    #[test]
    fn submit_for_pid_registers_pending_operation() {
        let ring = Arc::new(MockRing::default());
        let registry = Arc::new(PendingIoRegistry::default());
        let facility = CompletionRingIoFacility::new(ring, Arc::clone(&registry));

        assert_eq!(
            facility.submit_and_suspend_for_pid(55, IoOp::Nop, ResultMode::XRegister),
            Ok(())
        );

        assert_eq!(
            registry.take(1),
            Some(super::super::bridge::PendingIo {
                pid: 55,
                result_mode: ResultMode::XRegister,
            })
        );
    }

    #[test]
    fn unbound_submit_reports_missing_pid() {
        let ring = Arc::new(MockRing::default());
        let registry = Arc::new(PendingIoRegistry::default());
        let facility = CompletionRingIoFacility::new(ring, registry);

        assert_eq!(
            facility.submit_and_suspend(IoOp::Nop, ResultMode::Discard),
            Err(IoError::MissingPid)
        );
    }

    #[test]
    fn bound_submit_uses_bound_pid() {
        let ring = Arc::new(MockRing::default());
        let registry = Arc::new(PendingIoRegistry::default());
        let facility = CompletionRingIoFacility::bound(ring, Arc::clone(&registry), 99);

        assert_eq!(
            facility.submit_and_suspend(IoOp::Nop, ResultMode::Message),
            Ok(())
        );

        assert_eq!(
            registry.take(1),
            Some(super::super::bridge::PendingIo {
                pid: 99,
                result_mode: ResultMode::Message,
            })
        );
    }

    fn _keep_io_result_import_live(_: IoResult) {}
}
