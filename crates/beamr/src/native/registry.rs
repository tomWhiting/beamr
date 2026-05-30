//! Registry facility trait -- how BIFs manage the process name registry.
//!
//! The registry facility is an abstraction that allows process registry BIFs
//! (register/2, unregister/1, whereis/1) to manage name-to-PID associations
//! without direct access to the scheduler or process table. The scheduler
//! provides an implementation that uses its internal state; tests can provide
//! mock implementations.

use crate::atom::Atom;
use std::fmt;

/// Error returned when a registry operation fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// The name is already registered to another process.
    AlreadyRegistered,
    /// The PID is already registered under another name.
    PidAlreadyRegistered,
    /// The name is not currently registered.
    NotRegistered,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRegistered => f.write_str("name is already registered"),
            Self::PidAlreadyRegistered => f.write_str("pid is already registered under another name"),
            Self::NotRegistered => f.write_str("name is not registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Trait for managing the process name registry from BIFs.
///
/// Implementations are provided by the scheduler (or test mocks) and injected
/// into [`super::ProcessContext`] before BIF execution.
pub trait RegistryFacility: Send + Sync {
    /// Register `name` to `pid`. Fails if `name` is already taken or `pid`
    /// is already registered under another name.
    fn register(&self, name: Atom, pid: u64) -> Result<(), RegistryError>;

    /// Remove the registration for `name`. Fails if `name` is not registered.
    fn unregister(&self, name: Atom) -> Result<(), RegistryError>;

    /// Look up the PID registered under `name`. Returns `None` if not registered.
    fn whereis(&self, name: Atom) -> Option<u64>;
}

/// Record of a registry operation, used by test mocks to verify BIF behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryRecord {
    /// A name was registered to a PID.
    Register {
        /// The atom name being registered.
        name: Atom,
        /// The PID being registered.
        pid: u64,
    },
    /// A name was unregistered.
    Unregister {
        /// The atom name being unregistered.
        name: Atom,
    },
    /// A name was looked up.
    Whereis {
        /// The atom name being looked up.
        name: Atom,
    },
}
