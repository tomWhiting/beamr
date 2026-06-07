//! Group leader process metadata facility.
//!
//! The scheduler provides this facility so BIFs can update group-leader
//! metadata for processes other than the attached caller without exposing the
//! process table directly to native code.

use std::fmt;

use crate::term::Term;

/// Error returned when a group-leader operation fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GroupLeaderError {
    /// The target process does not exist.
    NoProc,
}

impl fmt::Display for GroupLeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProc => formatter.write_str("target process does not exist"),
        }
    }
}

impl std::error::Error for GroupLeaderError {}

/// Trait for reading and writing group-leader process metadata from BIFs.
pub trait GroupLeaderFacility: Send + Sync {
    /// Set `pid`'s group leader to `leader`.
    fn set_group_leader(&self, pid: u64, leader: Term) -> Result<(), GroupLeaderError>;

    /// Read `pid`'s current group leader.
    fn group_leader(&self, pid: u64) -> Result<Term, GroupLeaderError>;
}
