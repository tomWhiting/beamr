/// Messages and mailboxes — the only way processes touch.
///
/// Each process has a lock-free MPSC message queue (per D11).
/// Send copies a term into the receiver's heap and appends to
/// the mailbox. Receive pattern-matches against queued messages
/// with selective receive semantics.
pub mod selective;
