//! Unified accessor for local and remote PID terms.

use crate::{
    atom::Atom,
    term::{Term, boxed::ExternalPid},
};

/// Borrowed PID accessor hiding immediate-local versus boxed-remote layout.
#[derive(Copy, Clone, Debug)]
pub enum PidRef {
    Local(u64),
    Remote(ExternalPid),
}

impl PidRef {
    /// Creates a PID accessor for local immediate or remote boxed PID terms.
    pub fn new(term: Term) -> Option<Self> {
        if let Some(pid) = term.as_pid() {
            return Some(Self::Local(pid));
        }
        ExternalPid::new(term).map(Self::Remote)
    }

    /// Returns the numeric process id component.
    #[must_use]
    pub fn pid_number(self) -> u64 {
        match self {
            Self::Local(pid) => pid,
            Self::Remote(pid) => pid.pid_number(),
        }
    }

    /// Returns the PID serial component; local immediate PIDs use serial 0.
    #[must_use]
    pub fn serial(self) -> u64 {
        match self {
            Self::Local(_) => 0,
            Self::Remote(pid) => pid.serial(),
        }
    }

    /// Returns the embedded remote node atom, or `None` for local PIDs.
    #[must_use]
    pub fn node(self) -> Option<Atom> {
        match self {
            Self::Local(_) => None,
            Self::Remote(pid) => pid.node(),
        }
    }

    /// Returns true when the PID uses the local immediate representation.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::write_external_pid;

    #[test]
    fn pid_ref_wraps_local_immediate_pid() {
        let pid = PidRef::new(Term::pid(42)).expect("local pid");

        assert!(pid.is_local());
        assert_eq!(pid.pid_number(), 42);
        assert_eq!(pid.serial(), 0);
        assert_eq!(pid.node(), None);
    }

    #[test]
    fn pid_ref_wraps_remote_boxed_pid() {
        let mut heap = [0_u64; 4];
        let term = write_external_pid(&mut heap, Atom::OK, 99, 7).expect("external pid fits");
        let pid = PidRef::new(term).expect("remote pid");

        assert!(!pid.is_local());
        assert_eq!(pid.pid_number(), 99);
        assert_eq!(pid.serial(), 7);
        assert_eq!(pid.node(), Some(Atom::OK));
    }
}
