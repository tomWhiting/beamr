//! Local message delivery facility.
//!
//! The Send opcode resolves a target PID but, for a cross-process local send,
//! does not hold the receiver process body — that body lives in the scheduler's
//! `process_bodies` map. [`LocalSendFacility`] is the scheduler-implemented
//! bridge that locks the target slot and delivers the message using the exact
//! lock-slot / Present-Executing-Absent / push-before-wake template the I/O
//! delivery paths already use.
//!
//! The facility mirrors
//! [`DistributionSendFacility`](crate::distribution::control::DistributionSendFacility):
//! a trait named in the interpreter and implemented in the scheduler crate.

use std::sync::{Arc, Mutex};

use crate::replay::ReplayDriver;
use crate::term::Term;

/// Error returned by [`LocalSendFacility::send_local`].
///
/// A dead/absent target is NOT an error — it is a silent drop, matching BEAM
/// semantics. The only failure surface is a replay-determinism mismatch, which
/// the caller maps to [`ExecError::ReplayMismatch`](crate::error::ExecError).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalSendError {
    /// The recorded delivery clocks did not match the live observation.
    ReplayMismatch(String),
}

/// Synchronous request to deliver one message to a local target process.
///
/// The `message` term references the *sender's* heap. The call is synchronous,
/// performed inside the sender's slice while the sender body is alive, so the
/// facility may read/encode the term during the call. The facility MUST NOT
/// store the raw term past the call: the `Present` branch copies it into the
/// receiver heap immediately; the `Executing` branch ETF-encodes it to bytes
/// immediately.
pub struct LocalSendRequest<'a> {
    /// PID of the local target process.
    pub target_pid: u64,
    /// PID of the sending process (recorded for replay determinism).
    pub sender_pid: u64,
    /// The message term, referencing the sender's heap.
    pub message: Term,
    /// The sender's logical clock value, already ticked by the caller.
    pub sender_clock: u64,
    /// Replay driver used to validate recorded delivery determinism.
    pub replay_driver: Option<&'a Arc<Mutex<ReplayDriver>>>,
}

/// Delivers a local message term to a live target process body held by the
/// scheduler.
///
/// Returns `Ok(())` whether or not the target exists (a dead/absent pid is a
/// silent drop, matching BEAM semantics); `Err` only for a genuine
/// replay-determinism mismatch.
pub trait LocalSendFacility: Send + Sync {
    /// Deliver `request.message` to `request.target_pid`.
    fn send_local(&self, request: LocalSendRequest<'_>) -> Result<(), LocalSendError>;
}
