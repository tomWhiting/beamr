//! Link facility trait — how BIFs manage bidirectional process links.
//!
//! The link facility is an abstraction that allows link management BIFs
//! (link/1, unlink/1) to establish and remove bidirectional links without
//! direct access to the scheduler or process table. The scheduler provides
//! an implementation that uses its internal process management; tests can
//! provide mock implementations.

use std::fmt;

/// Error returned when a link operation fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LinkError {
    /// The target process does not exist.
    NoProc,
    /// The caller process identity is unknown.
    NoCaller,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProc => f.write_str("target process does not exist"),
            Self::NoCaller => f.write_str("caller process identity unknown"),
        }
    }
}

impl std::error::Error for LinkError {}

/// Trait for managing bidirectional process links from BIFs.
///
/// Implementations are provided by the scheduler (or test mocks) and injected
/// into [`super::ProcessContext`] before BIF execution.
pub trait LinkFacility: Send + Sync {
    /// Establish a bidirectional link between the caller and target.
    ///
    /// If a link already exists between the two PIDs, this is a no-op.
    /// If caller and target are the same PID, this is a no-op.
    /// Returns `Err(LinkError::NoProc)` when the target does not exist.
    fn link(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError>;

    /// Remove a bidirectional link between the caller and target.
    ///
    /// If no link exists, this is a no-op returning `Ok(())`.
    fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError>;

    /// Set the `trap_exit` flag for `caller_pid`, returning the previous value.
    ///
    /// Returns `Err(LinkError::NoCaller)` when the caller PID is not known.
    fn set_trap_exit(&self, caller_pid: u64, value: bool) -> Result<bool, LinkError>;
}

/// Record of a link operation, used by test mocks to verify BIF behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkRecord {
    /// A link was established.
    Link {
        /// PID of the process requesting the link.
        caller_pid: u64,
        /// PID of the process being linked to.
        target_pid: u64,
    },
    /// A link was removed.
    Unlink {
        /// PID of the process requesting the unlink.
        caller_pid: u64,
        /// PID of the process being unlinked from.
        target_pid: u64,
    },
}
