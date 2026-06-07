//! Completion ring abstraction shared by I/O lifecycle code.

use std::os::fd::RawFd;

/// I/O operation accepted by a completion ring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IoOp {
    /// Close a raw file descriptor asynchronously.
    Close { fd: RawFd },
}

/// Minimal completion-ring interface needed for FD resource lifecycle work.
pub trait CompletionRing: Send + Sync {
    /// Submit an operation and return its ring-assigned operation id.
    fn submit(&self, op: IoOp) -> u64;
}
