//! Select facility — mailbox access for selector BIFs.
//!
//! The `SelectFacility` trait provides a narrow interface for BIFs that need
//! to scan the process mailbox. It is implemented by a lightweight adapter
//! that the interpreter constructs before calling select-family BIFs.

use crate::term::Term;

/// Provides mailbox scanning and mutation for select BIFs.
///
/// The interpreter creates an implementation of this trait from the process
/// mailbox, installs it on ProcessContext before calling select, and reads
/// back the result afterward.
pub trait SelectFacility: Send + Sync {
    /// Returns the number of scannable messages in the mailbox.
    fn message_count(&self) -> usize;

    /// Peeks at the message at `index` (0-based from save pointer).
    fn peek_message(&self, index: usize) -> Option<Term>;

    /// Removes the message at `index` from the mailbox.
    fn remove_message(&self, index: usize);
}

/// A simple select facility backed by a snapshot of mailbox messages.
///
/// The interpreter copies messages from the mailbox into this snapshot,
/// and records which message index was removed by the BIF.
pub struct MailboxSnapshot {
    messages: Vec<Term>,
    removed_index: std::sync::Mutex<Option<usize>>,
}

impl MailboxSnapshot {
    /// Create a snapshot from a list of messages.
    #[must_use]
    pub fn new(messages: Vec<Term>) -> Self {
        Self {
            messages,
            removed_index: std::sync::Mutex::new(None),
        }
    }

    /// Returns the index of the removed message, if any.
    #[must_use]
    pub fn removed_index(&self) -> Option<usize> {
        *self.removed_index.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SelectFacility for MailboxSnapshot {
    fn message_count(&self) -> usize {
        self.messages.len()
    }

    fn peek_message(&self, index: usize) -> Option<Term> {
        self.messages.get(index).copied()
    }

    fn remove_message(&self, index: usize) {
        *self.removed_index.lock().unwrap_or_else(|e| e.into_inner()) = Some(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::term::Term;

    #[test]
    fn snapshot_provides_message_count_and_peek() {
        let messages = vec![Term::small_int(1), Term::small_int(2), Term::small_int(3)];
        let snapshot = MailboxSnapshot::new(messages);

        assert_eq!(snapshot.message_count(), 3);
        assert_eq!(snapshot.peek_message(0), Some(Term::small_int(1)));
        assert_eq!(snapshot.peek_message(1), Some(Term::small_int(2)));
        assert_eq!(snapshot.peek_message(2), Some(Term::small_int(3)));
        assert_eq!(snapshot.peek_message(3), None);
    }

    #[test]
    fn snapshot_records_removed_index() {
        let messages = vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)];
        let snapshot = MailboxSnapshot::new(messages);

        assert_eq!(snapshot.removed_index(), None);
        snapshot.remove_message(1);
        assert_eq!(snapshot.removed_index(), Some(1));
    }

    #[test]
    fn empty_snapshot() {
        let snapshot = MailboxSnapshot::new(Vec::new());
        assert_eq!(snapshot.message_count(), 0);
        assert_eq!(snapshot.peek_message(0), None);
        assert_eq!(snapshot.removed_index(), None);
    }
}
