//! Per-thread run queue backed by a lock-free work-stealing deque.
//!
//! The owning scheduler thread pushes and pops from the back (LIFO for cache
//! locality). Stealers pop from the front (FIFO for fairness). Process IDs are
//! queued rather than process bodies because `Process` is intentionally `!Send`.

use crossbeam_deque::{Steal, Stealer, Worker};

/// A per-thread run queue that stores process IDs.
pub struct RunQueue {
    worker: Worker<u64>,
}

impl RunQueue {
    /// Create a new empty run queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            worker: Worker::new_lifo(),
        }
    }

    /// Push a process ID onto the owner side of the queue.
    pub fn push(&self, pid: u64) {
        self.worker.push(pid);
    }

    /// Pop a process ID from the owner side of the queue.
    #[must_use]
    pub fn pop(&self) -> Option<u64> {
        self.worker.pop()
    }

    /// Approximate number of queued process IDs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.worker.len()
    }

    /// Whether this queue is currently empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.worker.is_empty()
    }

    /// Create a stealer handle for other scheduler threads.
    #[must_use]
    pub fn stealer(&self) -> Stealer<u64> {
        self.worker.stealer()
    }

    /// Steal approximately half the items from `victim` into this queue.
    ///
    /// Queues with zero or one item are left alone so the owning thread keeps
    /// its last runnable process.
    pub fn steal_half_from(&self, victim: &Stealer<u64>) -> usize {
        let victim_len = victim.len();
        if victim_len <= 1 {
            return 0;
        }

        let limit = victim_len / 2;
        if limit == 0 {
            return 0;
        }

        let before = self.worker.len();
        match victim.steal_batch_with_limit_and_pop(&self.worker, limit) {
            Steal::Success(pid) => {
                self.worker.push(pid);
                self.worker.len().saturating_sub(before)
            }
            Steal::Empty | Steal::Retry => 0,
        }
    }
}

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::RunQueue;

    #[test]
    fn push_then_pop_returns_same_process() {
        let queue = RunQueue::new();
        queue.push(42);

        assert_eq!(queue.pop(), Some(42));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn owner_pop_is_lifo() {
        let queue = RunQueue::new();
        queue.push(1);
        queue.push(2);
        queue.push(3);

        assert_eq!(queue.pop(), Some(3));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(1));
    }

    #[test]
    fn steal_half_from_ten_takes_approximately_five() {
        let victim = RunQueue::new();
        for pid in 0..10 {
            victim.push(pid);
        }
        let stealer = victim.stealer();
        let thief = RunQueue::new();

        let stolen = thief.steal_half_from(&stealer);

        assert!((4..=6).contains(&stolen), "stole {stolen} items");
        assert!(!thief.is_empty());
        assert!(!victim.is_empty());
    }

    #[test]
    fn steal_from_empty_queue_returns_nothing() {
        let victim = RunQueue::new();
        let thief = RunQueue::new();

        assert_eq!(thief.steal_half_from(&victim.stealer()), 0);
        assert!(thief.is_empty());
    }

    #[test]
    fn steal_from_single_item_queue_returns_nothing() {
        let victim = RunQueue::new();
        victim.push(7);
        let thief = RunQueue::new();

        assert_eq!(thief.steal_half_from(&victim.stealer()), 0);
        assert_eq!(victim.len(), 1);
        assert!(thief.is_empty());
    }

    #[test]
    fn push_and_steal_from_different_threads_do_not_race() {
        let owner = RunQueue::new();
        for pid in 0..100 {
            owner.push(pid);
        }
        let stealer = owner.stealer();

        let thief_thread = std::thread::spawn(move || {
            let thief = RunQueue::new();
            let _stolen = thief.steal_half_from(&stealer);
            let mut items = Vec::new();
            while let Some(pid) = thief.pop() {
                items.push(pid);
            }
            items
        });

        let mut owner_items = Vec::new();
        while let Some(pid) = owner.pop() {
            owner_items.push(pid);
        }

        let thief_items = match thief_thread.join() {
            Ok(items) => items,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        let all: HashSet<_> = owner_items
            .iter()
            .chain(thief_items.iter())
            .copied()
            .collect();

        assert_eq!(all.len(), owner_items.len() + thief_items.len());
        assert!(all.len() <= 100);
    }
}
