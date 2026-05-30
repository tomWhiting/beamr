//! Messages and mailboxes — the only way processes touch.
//!
//! Each process has a lock-free MPSC message arrival queue. Send copies a term
//! into the receiver's heap and appends the copied term to the mailbox. Receive
//! pattern-matches against queued messages with selective receive semantics.
pub mod selective;

use std::{collections::VecDeque, sync::Arc};

use crossbeam_queue::SegQueue;

use crate::{
    process::heap::{Heap, HeapFull},
    term::{
        Term,
        binary::Binary,
        boxed::{self, BigInt, Closure, Cons, Float, Map, Reference, Tuple},
    },
};

/// Error returned when a message cannot be copied into the receiver heap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SendError {
    /// The receiver heap did not have enough free words for the copied term.
    HeapFull(HeapFull),
    /// The source term points at an unsupported or malformed heap object.
    InvalidBoxedTerm,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeapFull(error) => error.fmt(f),
            Self::InvalidBoxedTerm => f.write_str("invalid boxed term"),
        }
    }
}

impl std::error::Error for SendError {}

impl From<HeapFull> for SendError {
    fn from(value: HeapFull) -> Self {
        Self::HeapFull(value)
    }
}

/// The receiving side of a process mailbox.
#[derive(Debug, Default)]
pub struct Mailbox {
    arrival: Arc<SegQueue<Term>>,
    scan_list: VecDeque<Term>,
    save_pointer: usize,
}

impl Mailbox {
    /// Create an empty mailbox with no arrived or buffered messages.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a cloneable sender handle for this mailbox's arrival queue.
    #[must_use]
    pub fn sender(&self) -> MailboxSender {
        MailboxSender {
            arrival: Arc::clone(&self.arrival),
            wake: None,
        }
    }

    /// Return the total number of arrived and scan-buffered messages.
    #[must_use]
    pub fn message_count(&self) -> usize {
        self.arrival.len() + self.scan_list.len()
    }

    /// Move all currently arrived messages into the owner-only scan list.
    pub fn drain_arrival(&mut self) {
        while let Some(message) = self.arrival.pop() {
            self.scan_list.push_back(message);
        }
    }

    #[cfg(test)]
    fn scan_len(&self) -> usize {
        self.scan_list.len()
    }

    #[cfg(test)]
    fn save_pointer(&self) -> usize {
        self.save_pointer
    }
}

/// Cloneable handle used by other processes to enqueue copied messages.
#[derive(Clone)]
pub struct MailboxSender {
    arrival: Arc<SegQueue<Term>>,
    wake: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl std::fmt::Debug for MailboxSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxSender")
            .field("arrival", &self.arrival)
            .field("has_wake_notifier", &self.wake.is_some())
            .finish()
    }
}

impl MailboxSender {
    /// Attach a wake notifier that runs after each successful enqueue.
    ///
    /// The scheduler uses this hook to move a waiting receiver back to a run
    /// queue. The hook is intentionally invoked only after `copy_term` succeeds
    /// and the message is visible in the arrival queue, so failed sends cannot
    /// cause spurious wake-ups.
    #[must_use]
    pub fn with_wake_notifier(mut self, wake: impl Fn() + Send + Sync + 'static) -> Self {
        self.wake = Some(Arc::new(wake));
        self
    }

    /// Deep-copy `message` into `receiver_heap` and enqueue the copied term.
    ///
    /// This method only touches the lock-free arrival queue. It does not borrow,
    /// lock, or mutate the receiver's private scan list.
    pub fn send(&self, message: Term, receiver_heap: &mut Heap) -> Result<(), SendError> {
        let copied = copy_term(message, receiver_heap)?;
        self.arrival.push(copied);
        if let Some(wake) = &self.wake {
            wake();
        }
        Ok(())
    }

    /// Enqueue an already-owned term.
    ///
    /// This is useful for tests that only send immediate values and do not need
    /// a receiver heap. General process sends should call [`MailboxSender::send`]
    /// so boxed terms never alias the sender heap.
    #[cfg(test)]
    pub(crate) fn enqueue_owned(&self, message: Term) {
        self.arrival.push(message);
    }
}

fn copy_term(term: Term, heap: &mut Heap) -> Result<Term, SendError> {
    if term.is_list() {
        copy_cons(term, heap)
    } else if term.is_boxed() {
        copy_boxed(term, heap)
    } else {
        Ok(term)
    }
}

fn copy_cons(term: Term, heap: &mut Heap) -> Result<Term, SendError> {
    let cons = Cons::new(term).ok_or(SendError::InvalidBoxedTerm)?;
    let head = copy_term(cons.head(), heap)?;
    let tail = copy_term(cons.tail(), heap)?;
    let ptr = heap.alloc(2)?;
    // SAFETY: `Heap::alloc(2)` returned a live two-word region in the receiver
    // heap, and the temporary slice is used only to initialize that region.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, 2) };
    boxed::write_cons(words, head, tail).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_boxed(term: Term, heap: &mut Heap) -> Result<Term, SendError> {
    if let Some(tuple) = Tuple::new(term) {
        return copy_tuple(tuple, heap);
    }
    if let Some(float) = Float::new(term) {
        return copy_float(float, heap);
    }
    if let Some(bigint) = BigInt::new(term) {
        return copy_bigint(bigint, heap);
    }
    if let Some(closure) = Closure::new(term) {
        return copy_closure(closure, heap);
    }
    if let Some(map) = Map::new(term) {
        return copy_map(map, heap);
    }
    if let Some(reference) = Reference::new(term) {
        return copy_reference(reference, heap);
    }
    if let Some(binary) = Binary::new(term) {
        return copy_binary(binary, heap);
    }

    Err(SendError::InvalidBoxedTerm)
}

fn copy_tuple(tuple: Tuple, heap: &mut Heap) -> Result<Term, SendError> {
    let elements = (0..tuple.arity())
        .map(|index| {
            tuple
                .get(index)
                .ok_or(SendError::InvalidBoxedTerm)
                .and_then(|element| copy_term(element, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = alloc_words(heap, 1 + elements.len())?;
    boxed::write_tuple(words, &elements).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_float(float: Float, heap: &mut Heap) -> Result<Term, SendError> {
    let words = alloc_words(heap, 2)?;
    boxed::write_float(words, float.value()).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_bigint(bigint: BigInt, heap: &mut Heap) -> Result<Term, SendError> {
    let limbs = bigint.limbs();
    let words = alloc_words(heap, 3 + limbs.len())?;
    boxed::write_bigint(words, bigint.is_negative(), limbs).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_closure(closure: Closure, heap: &mut Heap) -> Result<Term, SendError> {
    let module = closure.module().ok_or(SendError::InvalidBoxedTerm)?;
    let free_vars = (0..closure.num_free())
        .map(|index| {
            closure
                .free_var(index)
                .ok_or(SendError::InvalidBoxedTerm)
                .and_then(|free_var| copy_term(free_var, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = alloc_words(heap, 5 + free_vars.len())?;
    boxed::write_closure(
        words,
        module,
        closure.function_index(),
        closure.arity(),
        &free_vars,
    )
    .ok_or(SendError::InvalidBoxedTerm)
}

fn copy_map(map: Map, heap: &mut Heap) -> Result<Term, SendError> {
    let keys = (0..map.len())
        .map(|index| {
            map.key(index)
                .ok_or(SendError::InvalidBoxedTerm)
                .and_then(|key| copy_term(key, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let values = (0..map.len())
        .map(|index| {
            map.value(index)
                .ok_or(SendError::InvalidBoxedTerm)
                .and_then(|value| copy_term(value, heap))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let words = alloc_words(heap, 2 + keys.len() + values.len())?;
    boxed::write_map(words, &keys, &values).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_reference(reference: Reference, heap: &mut Heap) -> Result<Term, SendError> {
    let words = alloc_words(heap, 2)?;
    boxed::write_reference(words, reference.id()).ok_or(SendError::InvalidBoxedTerm)
}

fn copy_binary(binary: Binary, heap: &mut Heap) -> Result<Term, SendError> {
    let bytes = binary.as_bytes();
    let words = alloc_words(
        heap,
        2 + crate::term::binary::packed_word_count(bytes.len()),
    )?;
    crate::term::binary::write_binary(words, bytes).ok_or(SendError::InvalidBoxedTerm)
}

fn alloc_words(heap: &mut Heap, word_count: usize) -> Result<&mut [u64], SendError> {
    let ptr = heap.alloc(word_count)?;
    // SAFETY: `Heap::alloc` reserves `word_count` contiguous words in the heap;
    // the returned slice is used immediately to initialize the allocation.
    Ok(unsafe { std::slice::from_raw_parts_mut(ptr, word_count) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::{Cons, Tuple};
    use std::thread;

    #[test]
    fn new_mailbox_is_empty_and_sender_is_clone_send() {
        fn assert_clone_send<T: Clone + Send>(_: &T) {}

        let mailbox = Mailbox::new();
        let sender = mailbox.sender();

        assert_eq!(mailbox.message_count(), 0);
        assert_eq!(mailbox.scan_len(), 0);
        assert_clone_send(&sender);
    }

    #[test]
    fn send_small_integer_arrives_with_same_value() {
        let mut mailbox = Mailbox::new();
        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);

        sender
            .send(Term::small_int(42), &mut receiver_heap)
            .expect("send should copy immediate");

        mailbox.drain_arrival();
        assert_eq!(mailbox.message_count(), 1);
        assert_eq!(mailbox.scan_list.pop_front(), Some(Term::small_int(42)));
    }

    #[test]
    fn message_count_and_drain_include_arrival_and_scan_list() {
        let mut mailbox = Mailbox::new();
        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);

        for value in 1..=3 {
            sender
                .send(Term::small_int(value), &mut receiver_heap)
                .expect("send should copy immediate");
        }

        assert_eq!(mailbox.message_count(), 3);
        mailbox.drain_arrival();
        assert_eq!(mailbox.message_count(), 3);
        assert_eq!(mailbox.scan_len(), 3);
    }

    #[test]
    fn sending_tuple_deep_copies_nested_boxed_elements() {
        let mut sender_heap = Heap::new(16);
        let nested = allocate_tuple(&mut sender_heap, &[Term::small_int(7)]);
        let tuple = allocate_tuple(&mut sender_heap, &[Term::small_int(1), nested]);
        let mut receiver_heap = Heap::new(16);
        let mut mailbox = Mailbox::new();

        mailbox
            .sender()
            .send(tuple, &mut receiver_heap)
            .expect("tuple copy should fit");
        mailbox.drain_arrival();
        let copied = mailbox.scan_list.pop_front().expect("copied tuple");

        assert_eq!(copied, tuple);
        assert_ne!(copied.heap_ptr(), tuple.heap_ptr());
        let copied_nested = Tuple::new(copied).and_then(|tuple| tuple.get(1)).unwrap();
        assert_ne!(copied_nested.heap_ptr(), nested.heap_ptr());
    }

    #[test]
    fn sending_list_deep_copies_all_cons_cells() {
        let mut sender_heap = Heap::new(16);
        let tail = allocate_cons(&mut sender_heap, Term::small_int(2), Term::NIL);
        let list = allocate_cons(&mut sender_heap, Term::small_int(1), tail);
        let mut receiver_heap = Heap::new(16);
        let mut mailbox = Mailbox::new();

        mailbox
            .sender()
            .send(list, &mut receiver_heap)
            .expect("list copy should fit");
        mailbox.drain_arrival();
        let copied = mailbox.scan_list.pop_front().expect("copied list");

        assert_eq!(copied, list);
        assert_ne!(copied.heap_ptr(), list.heap_ptr());
        let copied_tail = Cons::new(copied).expect("copied cons").tail();
        assert_ne!(copied_tail.heap_ptr(), tail.heap_ptr());
    }

    #[test]
    fn concurrent_senders_enqueue_without_loss() {
        let mut mailbox = Mailbox::new();
        let sender = mailbox.sender();
        let mut handles = Vec::new();

        for thread_id in 0..4 {
            let sender = sender.clone();
            handles.push(thread::spawn(move || {
                for offset in 0..25 {
                    sender.enqueue_owned(Term::small_int(thread_id * 100 + offset));
                }
            }));
        }

        for handle in handles {
            handle.join().expect("sender thread should not panic");
        }

        mailbox.drain_arrival();
        assert_eq!(mailbox.message_count(), 100);
    }

    #[test]
    fn fifo_order_is_preserved_within_one_sender() {
        let mut mailbox = Mailbox::new();
        let sender = mailbox.sender();
        let mut receiver_heap = Heap::new(8);

        for value in 0..5 {
            sender
                .send(Term::small_int(value), &mut receiver_heap)
                .expect("send should copy immediate");
        }

        mailbox.drain_arrival();
        let values = mailbox
            .scan_list
            .iter()
            .map(|term| term.as_small_int().expect("small int"))
            .collect::<Vec<_>>();
        assert_eq!(values, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn send_fails_without_enqueuing_when_receiver_heap_is_full() {
        let mut sender_heap = Heap::new(3);
        let tuple = allocate_tuple(&mut sender_heap, &[Term::small_int(1), Term::small_int(2)]);
        let mut receiver_heap = Heap::new(2);
        let mailbox = Mailbox::new();

        let error = mailbox
            .sender()
            .send(tuple, &mut receiver_heap)
            .expect_err("tuple requires three words");

        assert!(matches!(error, SendError::HeapFull(_)));
        assert_eq!(mailbox.message_count(), 0);
    }

    fn allocate_tuple(heap: &mut Heap, elements: &[Term]) -> Term {
        let words = alloc_words(heap, 1 + elements.len()).expect("test allocation should fit");
        boxed::write_tuple(words, elements).expect("tuple should fit")
    }

    fn allocate_cons(heap: &mut Heap, head: Term, tail: Term) -> Term {
        let words = alloc_words(heap, 2).expect("test allocation should fit");
        boxed::write_cons(words, head, tail).expect("cons should fit")
    }
}
