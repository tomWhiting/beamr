//! Selective receive and save pointer.
//!
//! A receive attempt drains arrived messages into the scan list, scans from the
//! save pointer, removes the first matching message, and preserves the save
//! pointer on miss so old non-matching messages are not re-tested.

use crate::{mailbox::Mailbox, term::Term};

/// Attempt a selective receive against `mailbox`.
///
/// The matcher returns `true` for the first acceptable message. On success that
/// message is removed, unmatched message order is preserved, and the save
/// pointer resets to the beginning. On miss, the save pointer advances to the
/// end of the scan list and `None` is returned; scheduler code can then suspend
/// the process until a later arrival wakes it to retry from that point.
pub fn receive(mailbox: &mut Mailbox, mut matches: impl FnMut(Term) -> bool) -> Option<Term> {
    mailbox.drain_arrival();

    let mut index = mailbox.save_pointer.min(mailbox.scan_list.len());
    while index < mailbox.scan_list.len() {
        let message = mailbox.scan_list[index].term;
        if matches(message) {
            let matched = mailbox.scan_list.remove(index)?;
            mailbox.save_pointer = 0;
            return Some(matched.term);
        }
        index += 1;
    }

    mailbox.save_pointer = mailbox.scan_list.len();
    None
}

#[cfg(test)]
mod tests {
    use super::receive;
    use crate::{mailbox::Mailbox, process::heap::Heap, term::Term};

    #[test]
    fn empty_receive_returns_none_and_preserves_zero_save_pointer() {
        let mut mailbox = Mailbox::new();
        let mut tested = 0;

        assert_eq!(
            receive(&mut mailbox, |_| {
                tested += 1;
                true
            }),
            None
        );

        assert_eq!(tested, 0);
        assert_eq!(mailbox.save_pointer(), 0);
    }

    #[test]
    fn receive_with_matching_message_returns_and_removes_it() {
        let mut mailbox = mailbox_with_values([1, 2, 3]);

        let matched = receive(&mut mailbox, |term| term.as_small_int() == Some(2));

        assert_eq!(matched, Some(Term::small_int(2)));
        assert_eq!(mailbox.message_count(), 2);
        assert_eq!(scan_values(&mailbox), vec![1, 3]);
        assert_eq!(mailbox.save_pointer(), 0);
    }

    #[test]
    fn receive_with_no_matching_messages_returns_none_and_saves_end() {
        let mut mailbox = mailbox_with_values([1, 2, 3]);

        let matched = receive(&mut mailbox, |term| term.as_small_int() == Some(9));

        assert_eq!(matched, None);
        assert_eq!(mailbox.message_count(), 3);
        assert_eq!(scan_values(&mailbox), vec![1, 2, 3]);
        assert_eq!(mailbox.save_pointer(), 3);
    }

    #[test]
    fn new_messages_after_failed_scan_are_tested_from_save_pointer() {
        let mut mailbox = mailbox_with_values([1, 2]);
        let mut tested = Vec::new();
        assert_eq!(
            receive(&mut mailbox, |term| {
                tested.push(value(term));
                false
            }),
            None
        );
        assert_eq!(tested, vec![1, 2]);
        assert_eq!(mailbox.save_pointer(), 2);

        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);
        sender
            .send(Term::small_int(3), &mut receiver_heap)
            .expect("send should copy immediate");
        tested.clear();

        let matched = receive(&mut mailbox, |term| {
            tested.push(value(term));
            term.as_small_int() == Some(3)
        });

        assert_eq!(matched, Some(Term::small_int(3)));
        assert_eq!(tested, vec![3]);
        assert_eq!(scan_values(&mailbox), vec![1, 2]);
        assert_eq!(mailbox.save_pointer(), 0);
    }

    #[test]
    fn messages_before_save_pointer_are_not_retested() {
        let mut mailbox = mailbox_with_values([1, 2]);
        assert_eq!(receive(&mut mailbox, |_| false), None);

        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);
        sender
            .send(Term::small_int(4), &mut receiver_heap)
            .expect("send should copy immediate");
        let mut tested = Vec::new();

        assert_eq!(
            receive(&mut mailbox, |term| {
                tested.push(value(term));
                false
            }),
            None
        );

        assert_eq!(tested, vec![4]);
        assert_eq!(mailbox.save_pointer(), 3);
        assert_eq!(scan_values(&mailbox), vec![1, 2, 4]);
    }

    #[test]
    fn save_pointer_resets_after_success_following_previous_miss() {
        let mut mailbox = mailbox_with_values([1, 2]);
        assert_eq!(receive(&mut mailbox, |_| false), None);

        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);
        sender
            .send(Term::small_int(3), &mut receiver_heap)
            .expect("send should copy immediate");

        assert_eq!(
            receive(&mut mailbox, |term| term.as_small_int() == Some(3)),
            Some(Term::small_int(3))
        );
        assert_eq!(mailbox.save_pointer(), 0);
    }

    fn mailbox_with_values(values: impl IntoIterator<Item = i64>) -> Mailbox {
        let mut mailbox = Mailbox::new();
        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(64);
        for value in values {
            sender
                .send(Term::small_int(value), &mut receiver_heap)
                .expect("send should copy immediate");
        }
        mailbox.drain_arrival();
        mailbox
    }

    fn scan_values(mailbox: &Mailbox) -> Vec<i64> {
        mailbox
            .scan_list
            .iter()
            .map(|message| message.term)
            .map(value)
            .collect::<Vec<_>>()
    }

    fn value(term: Term) -> i64 {
        term.as_small_int()
            .expect("test message should be small int")
    }
}
