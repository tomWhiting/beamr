//! Spawn facility trait — how BIFs request process creation.
//!
//! The spawn facility is an abstraction that allows process creation BIFs
//! (spawn/3, spawn_link/3) to request new processes without direct access
//! to the scheduler. The scheduler provides an implementation that uses its
//! internal spawn machinery; tests can provide mock implementations.

use std::fmt;

use crate::atom::Atom;
use crate::term::Term;

/// Error returned when a spawn request fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpawnError {
    /// The requested module/function/arity could not be resolved.
    UnresolvedMfa,
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedMfa => f.write_str("unresolved module/function/arity for spawn"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// Trait for requesting process creation from BIFs.
///
/// Implementations are provided by the scheduler (or test mocks) and injected
/// into [`super::ProcessContext`] before BIF execution.
pub trait SpawnFacility: Send + Sync {
    /// Request creation of a new process at the given Module:Function entry
    /// point with the supplied arguments.
    ///
    /// If `link_to` is `Some(parent_pid)`, a bidirectional link between the
    /// parent and child is established atomically before the child starts.
    ///
    /// Returns the new process PID on success, or an error when the
    /// module/function cannot be resolved or process creation fails.
    fn spawn(
        &self,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError>;
}

/// Record of a spawn request, used by test mocks to verify BIF behavior.
#[derive(Clone, Debug)]
pub struct SpawnRecord {
    /// Module atom for the entry point.
    pub module: Atom,
    /// Function atom for the entry point.
    pub function: Atom,
    /// Arguments to pass to the new process.
    pub args: Vec<Term>,
    /// Parent PID to link to, if spawn_link was used.
    pub link_to: Option<u64>,
}
