//! Bounded, insertion-ordered store of process exit tombstones.
//!
//! A *tombstone* records the [`ExitReason`] of a process that has died. It is
//! the load-bearing exit-detection signal: [`Scheduler::run_until_exit`] parks
//! on a condvar and only returns once it observes the dead pid's tombstone, and
//! [`Scheduler::peek_exit_reason`] / the link/monitor already-dead guards read
//! it to discover a process has gone.
//!
//! Historically this was an unbounded `DashMap<u64, ExitReason>`: a tombstone
//! was written on every process death and *never* removed for the lifetime of
//! the scheduler. Under a workload that spawns a fresh process per connection
//! (or per request), that map grows without bound — a slow but real leak.
//!
//! [`BoundedTombstones`] caps the live tombstone count at [`TOMBSTONE_CAPACITY`]
//! entries using a pure insertion-order (FIFO) eviction policy: when a new
//! tombstone would push the count past the cap, the *oldest* tombstone is
//! evicted. The cap is deliberately huge (64Ki entries, low single-digit MB)
//! so that eviction is invisible to every legitimate reader:
//!
//! * `run_until_exit` always targets a pid whose tombstone was *just* inserted
//!   to wake that very caller; FIFO eviction only reclaims the oldest entries
//!   once [`TOMBSTONE_CAPACITY`] *newer* exits have accumulated, which cannot
//!   happen inside the sub-10ms condvar wake window — so a blocked
//!   `run_until_exit` can never miss its real exit.
//! * `peek_exit_reason` and the link/monitor guards observe recently-dead pids
//!   in practice (a just-closed connection, never one buried 64Ki exits deep),
//!   so for them too the cap is effectively unreachable.
//!
//! The satellite maps (`exit_results` / `exit_errors` / `exit_exceptions`) are
//! evicted *together* with the tombstone they pair with — see
//! [`SharedState::insert_exit_tombstone`] — so a satellite can never outlive
//! its tombstone and the "tombstone observed ⇒ paired result already present"
//! invariant is preserved.

use crate::process::ExitReason;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Maximum number of live exit tombstones retained at once.
///
/// At ~16 bytes per entry (a `u64` pid plus a `Copy` [`ExitReason`]) plus
/// DashMap overhead, 65536 entries caps the tombstone map at low single-digit
/// MB while leaving an enormous safety margin: a process that exited would have
/// to be followed by 65,536 *further* exits before its tombstone is reclaimed.
/// That dwarfs any plausible window of concurrently-interesting recently-dead
/// pids (a server with thousands of in-flight connections still has its
/// just-closed connection's tombstone well within the most-recent 64Ki), so the
/// FIFO eviction policy is effectively invisible to every legitimate reader
/// while still hard-bounding memory.
pub(super) const TOMBSTONE_CAPACITY: usize = 65_536;

/// A bounded, insertion-ordered concurrent map from pid to [`ExitReason`].
///
/// Reads are lock-free via the inner [`DashMap`] and preserve the exact
/// `Option`-returning semantics callers rely on (a miss returns `None`, same as
/// an unknown pid). Inserts additionally record the pid in a FIFO order queue
/// and, on overflow, evict the oldest pid — returning it so the caller can
/// evict the paired satellite entries.
pub(super) struct BoundedTombstones {
    reasons: DashMap<u64, ExitReason>,
    /// Insertion order of currently-live pids, oldest at the front. Guarded
    /// independently of the DashMap shards; only touched on the (lower-volume)
    /// insert path, never on reads.
    order: Mutex<VecDeque<u64>>,
    capacity: usize,
}

impl BoundedTombstones {
    /// Create a store with the default [`TOMBSTONE_CAPACITY`].
    pub(super) fn new() -> Self {
        Self::with_capacity(TOMBSTONE_CAPACITY)
    }

    /// Create a store with an explicit capacity. `capacity` must be non-zero;
    /// a zero capacity is clamped to 1 so the structure always stores at least
    /// the most recent tombstone.
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            reasons: DashMap::new(),
            order: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
        }
    }

    /// Read the exit reason for `pid`, or `None` if no tombstone is present.
    ///
    /// Lock-free and non-consuming: the tombstone is left in place. Takes the
    /// pid by reference to mirror the [`DashMap`] this replaced, keeping call
    /// sites unchanged.
    pub(super) fn get(&self, pid: &u64) -> Option<ExitReason> {
        self.reasons.get(pid).map(|entry| *entry)
    }

    /// Whether a tombstone exists for `pid`.
    pub(super) fn contains_key(&self, pid: &u64) -> bool {
        self.reasons.contains_key(pid)
    }

    /// Insert (or overwrite) the tombstone for `pid`.
    ///
    /// Returns `Some(evicted_pid)` when inserting this entry pushed the live
    /// count past the capacity and the oldest pid was evicted — the caller is
    /// responsible for evicting that pid's paired satellite entries so they
    /// cannot outlive their tombstone. Returns `None` when nothing was evicted
    /// (either below capacity, or this was an overwrite of an existing pid).
    pub(super) fn insert(&self, pid: u64, reason: ExitReason) -> Option<u64> {
        // Overwrite of an existing tombstone: update the reason in place and do
        // not touch the order queue, so a re-insert cannot duplicate the pid in
        // the FIFO or evict a different live pid.
        if self.reasons.insert(pid, reason).is_some() {
            return None;
        }
        let mut order = match self.order.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        order.push_back(pid);
        if order.len() <= self.capacity {
            return None;
        }
        // Over capacity: evict the oldest pid. Loop to skip any pid that was
        // already removed from the map (defensive — production never removes
        // tombstones out of band, but this keeps the structure self-correcting).
        while let Some(oldest) = order.pop_front() {
            if let Some((evicted, _)) = self.reasons.remove(&oldest) {
                return Some(evicted);
            }
        }
        None
    }

    /// Number of live tombstones. Test/diagnostic helper.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.reasons.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a) Inserting far more than the cap keeps the live count bounded at the
    /// cap and never above it.
    #[test]
    fn insert_over_cap_stays_bounded() {
        let cap = 8;
        let store = BoundedTombstones::with_capacity(cap);
        for pid in 0..1_000u64 {
            store.insert(pid, ExitReason::Normal);
            assert!(
                store.len() <= cap,
                "len {} exceeded cap {} after inserting pid {}",
                store.len(),
                cap,
                pid
            );
        }
        assert_eq!(store.len(), cap, "store settles exactly at the cap");
    }

    /// (b) The most-recent tombstones survive and read back their reason.
    #[test]
    fn most_recent_survive_and_are_readable() {
        let cap = 8;
        let store = BoundedTombstones::with_capacity(cap);
        for pid in 0..100u64 {
            // Vary the reason so we also confirm the right value comes back.
            let reason = if pid % 2 == 0 {
                ExitReason::Normal
            } else {
                ExitReason::Kill
            };
            store.insert(pid, reason);
        }
        // The last `cap` pids (92..=99) must all be present with their reason.
        for pid in 92..100u64 {
            let expected = if pid % 2 == 0 {
                ExitReason::Normal
            } else {
                ExitReason::Kill
            };
            assert_eq!(
                store.get(&pid),
                Some(expected),
                "recent pid {pid} must survive with its reason"
            );
            assert!(store.contains_key(&pid));
        }
    }

    /// (c) The oldest tombstones are evicted — `get` returns `None` for them —
    /// while recent ones still return `Some`, preserving exact Option
    /// semantics (a miss is indistinguishable from an unknown pid).
    #[test]
    fn oldest_are_evicted_recent_retained() {
        let cap = 4;
        let store = BoundedTombstones::with_capacity(cap);
        for pid in 0..10u64 {
            store.insert(pid, ExitReason::Normal);
        }
        // Oldest 6 (0..=5) evicted.
        for pid in 0..6u64 {
            assert_eq!(store.get(&pid), None, "old pid {pid} must be evicted");
            assert!(!store.contains_key(&pid));
        }
        // Newest 4 (6..=9) retained.
        for pid in 6..10u64 {
            assert_eq!(
                store.get(&pid),
                Some(ExitReason::Normal),
                "recent pid {pid} must be retained"
            );
        }
    }

    /// A re-insert (overwrite) of a live pid must not duplicate it in the FIFO
    /// order, must update the reason, and must not evict a different live pid.
    #[test]
    fn overwrite_does_not_duplicate_or_misevict() {
        let cap = 3;
        let store = BoundedTombstones::with_capacity(cap);
        store.insert(1, ExitReason::Normal);
        store.insert(2, ExitReason::Normal);
        store.insert(3, ExitReason::Normal);
        // Overwrite the oldest; reason updates, order is unchanged.
        store.insert(1, ExitReason::Kill);
        assert_eq!(store.get(&1), Some(ExitReason::Kill));
        assert_eq!(store.len(), cap);
        // Next fresh insert evicts pid 1 (still the oldest by first-insert
        // order), not pid 2 or 3.
        store.insert(4, ExitReason::Normal);
        assert_eq!(store.get(&1), None, "first-inserted pid is the one evicted");
        assert_eq!(store.get(&2), Some(ExitReason::Normal));
        assert_eq!(store.get(&3), Some(ExitReason::Normal));
        assert_eq!(store.get(&4), Some(ExitReason::Normal));
        assert_eq!(store.len(), cap);
    }
}
