//! Selective receive and save pointer.
//!
//! When a process reads its mailbox, it scans for the first message
//! matching a pattern. Non-matching messages are skipped and a save
//! pointer tracks scan progress to avoid rescanning from scratch.
//! If no message matches, the process suspends until mail arrives
//! or a timeout fires.

pub(crate) fn _scaffold() {}
