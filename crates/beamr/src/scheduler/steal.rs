//! Work-stealing logic.
//!
//! Idle scheduler threads deterministically scan other scheduler queues in
//! round-robin order and steal approximately half of a victim's work. A queue
//! with a single process is never stolen from.

use crossbeam_deque::Stealer;

use crate::scheduler::run_queue::RunQueue;

/// Result of a steal attempt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StealResult {
    /// Work was stolen from `victim_index`.
    Stolen {
        /// Number of process IDs stolen.
        count: usize,
        /// Victim scheduler index.
        victim_index: usize,
    },
    /// No queue had stealable work.
    Empty,
}

/// Attempt to steal from other scheduler queues in deterministic round-robin
/// order. `last_victim` is the previously attempted victim and the returned
/// `usize` should be fed into the next call.
pub fn try_steal(
    my_queue: &RunQueue,
    my_index: usize,
    stealers: &[Stealer<u64>],
    last_victim: usize,
) -> (StealResult, usize) {
    let thread_count = stealers.len();
    if thread_count <= 1 {
        return (StealResult::Empty, last_victim);
    }

    let mut victim = last_victim;
    for _ in 0..thread_count - 1 {
        victim = next_victim(victim, my_index, thread_count);
        let count = my_queue.steal_half_from(&stealers[victim]);
        if count > 0 {
            return (
                StealResult::Stolen {
                    count,
                    victim_index: victim,
                },
                victim,
            );
        }
    }

    (StealResult::Empty, victim)
}

fn next_victim(current: usize, my_index: usize, thread_count: usize) -> usize {
    let mut victim = (current + 1) % thread_count;
    if victim == my_index {
        victim = (victim + 1) % thread_count;
    }
    victim
}

#[cfg(test)]
mod tests {
    use super::{StealResult, try_steal};
    use crate::scheduler::run_queue::RunQueue;

    #[test]
    fn empty_thread_steals_from_thread_with_processes() {
        let queues: Vec<_> = (0..4).map(|_| RunQueue::new()).collect();
        for pid in 0..10 {
            queues[2].push(pid);
        }
        let stealers: Vec<_> = queues.iter().map(RunQueue::stealer).collect();

        let (result, _) = try_steal(&queues[0], 0, &stealers, 0);

        match result {
            StealResult::Stolen {
                count,
                victim_index,
            } => {
                assert_eq!(victim_index, 2);
                assert!((4..=6).contains(&count), "stole {count}");
                assert!(!queues[0].is_empty());
            }
            StealResult::Empty => panic!("expected work stealing"),
        }
    }

    #[test]
    fn queue_with_one_process_is_not_stolen_from() {
        let queues: Vec<_> = (0..2).map(|_| RunQueue::new()).collect();
        queues[1].push(99);
        let stealers: Vec<_> = queues.iter().map(RunQueue::stealer).collect();

        let (result, _) = try_steal(&queues[0], 0, &stealers, 0);

        assert_eq!(result, StealResult::Empty);
        assert_eq!(queues[1].pop(), Some(99));
    }

    #[test]
    fn round_robin_victim_selection_visits_all_threads_before_repeating() {
        let queues: Vec<_> = (0..4).map(|_| RunQueue::new()).collect();
        for pid in 0..10 {
            queues[3].push(pid);
        }
        let stealers: Vec<_> = queues.iter().map(RunQueue::stealer).collect();

        let (result, next) = try_steal(&queues[0], 0, &stealers, 0);
        assert!(matches!(
            result,
            StealResult::Stolen {
                victim_index: 3,
                ..
            }
        ));
        assert_eq!(next, 3);

        for pid in 100..110 {
            queues[1].push(pid);
        }
        let (result, _) = try_steal(&queues[0], 0, &stealers, next);
        assert!(matches!(
            result,
            StealResult::Stolen {
                victim_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn all_empty_returns_empty() {
        let queues: Vec<_> = (0..3).map(|_| RunQueue::new()).collect();
        let stealers: Vec<_> = queues.iter().map(RunQueue::stealer).collect();

        assert_eq!(try_steal(&queues[0], 0, &stealers, 0).0, StealResult::Empty);
    }
}
