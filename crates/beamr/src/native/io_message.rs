//! Scheduler-backed process message delivery for native I/O protocol BIFs.

use crate::term::Term;

/// Narrow native facility for sending a term to a local process pid.
pub trait IoMessageFacility: Send + Sync {
    /// Send `message` from `sender_pid` to `target_pid`.
    fn send_message(&self, sender_pid: u64, target_pid: u64, message: Term) -> bool;
}
