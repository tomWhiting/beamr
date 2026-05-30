//! Supervision facility trait — how BIFs manage monitors and exit signals.
//!
//! The supervision facility is an abstraction that allows process supervision
//! BIFs (monitor/2, demonitor/1, exit/2) to manage monitors and deliver exit
//! signals without direct access to the scheduler or process table. The
//! scheduler provides an implementation that uses its internal process
//! management; tests can provide mock implementations.

use crate::process::ExitReason;
use std::fmt;

/// Error returned when a supervision operation fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SupervisionError {
    /// The target process does not exist.
    NoProc,
    /// The caller process identity is unknown.
    NoCaller,
}

impl fmt::Display for SupervisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProc => f.write_str("target process does not exist"),
            Self::NoCaller => f.write_str("caller process identity unknown"),
        }
    }
}

impl std::error::Error for SupervisionError {}

/// Result of a successful monitor operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonitorResult {
    /// Unique reference identifying the monitor.
    pub reference: u64,
    /// Whether the target was already dead (DOWN was immediately enqueued).
    pub immediate_down: bool,
}

/// Trait for managing process monitors and exit signals from BIFs.
///
/// Implementations are provided by the scheduler (or test mocks) and injected
/// into [`super::ProcessContext`] before BIF execution.
pub trait SupervisionFacility: Send + Sync {
    /// Establish a unidirectional monitor from the caller to the target PID.
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError>;

    /// Remove a monitor identified by its reference.
    fn demonitor(&self, caller_pid: u64, reference: u64) -> Result<(), SupervisionError>;

    /// Send an exit signal from the caller to the target process.
    fn exit_signal(
        &self,
        caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), SupervisionError>;
}

/// Record of a supervision operation, used by test mocks to verify BIF behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupervisionRecord {
    /// A monitor was established.
    Monitor {
        caller_pid: u64,
        target_pid: u64,
    },
    /// A monitor was removed.
    Demonitor {
        caller_pid: u64,
        reference: u64,
    },
    /// An exit signal was sent.
    ExitSignal {
        caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    },
}
