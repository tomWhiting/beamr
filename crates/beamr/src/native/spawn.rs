//! Spawn facility trait — how BIFs request process creation.
//!
//! The spawn facility is an abstraction that allows process creation BIFs
//! (spawn/3, spawn_link/3) to request new processes without direct access
//! to the scheduler. The scheduler provides an implementation that uses its
//! internal spawn machinery; tests can provide mock implementations.

use std::fmt;

use crate::atom::Atom;
use crate::process::Priority;
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
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError>;

    /// Request creation of a new process and atomically establish a monitor
    /// from `caller_pid` to the child before the child can execute.
    fn spawn_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError>;

    /// Spawn a process from a lambda (FunT entry) by module and lambda index.
    ///
    /// The scheduler looks up the module's lambda table to find the entry
    /// label and starts the child process there. Used by `erlang:spawn/1`
    /// and `erlang:spawn_link/1` which receive a closure term.
    fn spawn_lambda(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError>;

    /// Spawn a process from a lambda and atomically establish a monitor from
    /// `caller_pid` to the child before the child can execute.
    fn spawn_lambda_monitor(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError>;

    /// Request creation of a new process with spawn options applied atomically
    /// before the child can execute.
    fn spawn_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError>;

    /// Spawn a process from a lambda with spawn options applied atomically
    /// before the child can execute.
    fn spawn_lambda_with_options(
        &self,
        caller_pid: u64,
        module: Atom,
        lambda_index: u32,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError>;
}

/// Options accepted by `erlang:spawn_opt/2,4`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SpawnOptions {
    pub link: bool,
    pub monitor: bool,
    pub priority: Option<Priority>,
    pub min_heap_size: Option<usize>,
}

/// Successful spawn result that may include a monitor reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpawnOptionsResult {
    pub pid: u64,
    pub reference: Option<u64>,
}

/// Successful atomic spawn-monitor result.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpawnMonitorResult {
    /// PID assigned to the child process.
    pub pid: u64,
    /// Monitor reference owned by the caller.
    pub reference: u64,
}

/// Record of a spawn request, used by test mocks to verify BIF behavior.
#[derive(Clone, Debug)]
pub struct SpawnRecord {
    /// Calling process PID requesting the spawn.
    pub caller_pid: u64,
    /// Module atom for the entry point.
    pub module: Atom,
    /// Function atom for the entry point.
    pub function: Atom,
    /// Arguments to pass to the new process.
    pub args: Vec<Term>,
    /// Parent PID to link to, if spawn_link was used.
    pub link_to: Option<u64>,
}
