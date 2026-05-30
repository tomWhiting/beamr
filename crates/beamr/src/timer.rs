//! Timer wheel for one-shot timeouts.
//!
//! The wheel uses millisecond ticks, bucketed timer references, and an index map.
//! Scheduling and cancellation update only one bucket/index entry each, giving
//! O(1) insertion and cancellation without a sorted list, binary heap, or
//! priority queue.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::term::Term;

const DEFAULT_BUCKETS: usize = 1024;

/// Unique timer reference used for cancellation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TimerRef(u64);

impl TimerRef {
    /// Return the opaque integer id backing this reference.
    #[must_use]
    pub const fn id(self) -> u64 {
        self.0
    }

    /// Reconstruct a timer reference from an id term/payload.
    #[must_use]
    pub const fn from_id(id: u64) -> Self {
        Self(id)
    }
}

/// Timer entry metadata stored in the wheel index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimerEntry {
    /// Process that receives the message when the timer expires.
    pub target_pid: u64,
    /// Message to deliver.
    pub message: Term,
    /// Absolute expiry instant.
    pub expires_at: Instant,
    bucket: usize,
    slot: usize,
}

/// Expired timer returned by [`TimerWheel::tick`] / [`TimerWheel::tick_at`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExpiredTimer {
    /// Reference of the fired timer.
    pub reference: TimerRef,
    /// Process that receives the message.
    pub target_pid: u64,
    /// Message to deliver.
    pub message: Term,
    /// Absolute expiry instant.
    pub expires_at: Instant,
}

/// Millisecond-granularity O(1) timer wheel for one-shot timers.
#[derive(Debug)]
pub struct TimerWheel {
    buckets: Vec<Vec<TimerRef>>,
    entries: HashMap<TimerRef, TimerEntry>,
    next_ref: u64,
    start: Instant,
}

impl TimerWheel {
    /// Create a wheel with the default bucket count.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bucket_count(DEFAULT_BUCKETS)
    }

    /// Create a wheel with at least one bucket.
    #[must_use]
    pub fn with_bucket_count(bucket_count: usize) -> Self {
        let bucket_count = bucket_count.max(1);
        let buckets = (0..bucket_count).map(|_| Vec::new()).collect();
        Self {
            buckets,
            entries: HashMap::new(),
            next_ref: 1,
            start: Instant::now(),
        }
    }

    /// Schedule `message` for `target_pid` after `delay` from now.
    pub fn schedule(&mut self, delay: Duration, target_pid: u64, message: Term) -> TimerRef {
        self.schedule_at(Instant::now(), delay, target_pid, message)
    }

    /// Reserve a unique timer reference for callers that must include it in the message.
    pub fn reserve_reference(&mut self) -> TimerRef {
        self.allocate_ref()
    }

    /// Schedule `message` with a previously reserved reference.
    pub fn schedule_reserved(
        &mut self,
        reference: TimerRef,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> TimerRef {
        self.schedule_reserved_at(reference, Instant::now(), delay, target_pid, message)
    }

    /// Deterministic reserved-reference scheduling variant.
    pub fn schedule_reserved_at(
        &mut self,
        reference: TimerRef,
        now: Instant,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> TimerRef {
        if now < self.start {
            self.start = now;
        }
        let expires_at = now.checked_add(delay).unwrap_or(now);
        let bucket = self.bucket_for(expires_at);
        let slot = self.buckets[bucket].len();
        self.buckets[bucket].push(reference);
        self.entries.insert(
            reference,
            TimerEntry {
                target_pid,
                message,
                expires_at,
                bucket,
                slot,
            },
        );
        reference
    }

    /// Deterministic scheduling variant used by tests and scheduler ticks.
    pub fn schedule_at(
        &mut self,
        now: Instant,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> TimerRef {
        let reference = self.allocate_ref();
        self.schedule_reserved_at(reference, now, delay, target_pid, message)
    }

    /// Cancel a pending timer and return its remaining duration from now.
    pub fn cancel(&mut self, reference: TimerRef) -> Option<Duration> {
        self.cancel_at(reference, Instant::now())
    }

    /// Deterministic cancellation variant returning remaining duration from `now`.
    pub fn cancel_at(&mut self, reference: TimerRef, now: Instant) -> Option<Duration> {
        let entry = self.remove_entry(reference)?;
        Some(entry.expires_at.saturating_duration_since(now))
    }

    /// Process timers expired at the current instant.
    pub fn tick(&mut self) -> Vec<ExpiredTimer> {
        self.tick_at(Instant::now())
    }

    /// Process timers expired at `now`.
    pub fn tick_at(&mut self, now: Instant) -> Vec<ExpiredTimer> {
        let mut expired = Vec::new();
        for bucket_index in 0..self.buckets.len() {
            let mut slot = 0;
            while slot < self.buckets[bucket_index].len() {
                let reference = self.buckets[bucket_index][slot];
                let Some(entry) = self.entries.get(&reference) else {
                    self.swap_remove_bucket_slot(bucket_index, slot);
                    continue;
                };
                if entry.expires_at <= now {
                    if let Some(entry) = self.remove_entry(reference) {
                        expired.push(ExpiredTimer {
                            reference,
                            target_pid: entry.target_pid,
                            message: entry.message,
                            expires_at: entry.expires_at,
                        });
                    }
                } else {
                    slot += 1;
                }
            }
        }
        expired
    }

    /// Number of pending timers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no timers are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inspect a pending timer entry.
    #[must_use]
    pub fn get(&self, reference: TimerRef) -> Option<&TimerEntry> {
        self.entries.get(&reference)
    }

    fn allocate_ref(&mut self) -> TimerRef {
        let reference = TimerRef(self.next_ref);
        self.next_ref = self.next_ref.checked_add(1).unwrap_or(1);
        reference
    }

    fn bucket_for(&self, expires_at: Instant) -> usize {
        let elapsed_ms = expires_at.saturating_duration_since(self.start).as_millis();
        (elapsed_ms % self.buckets.len() as u128) as usize
    }

    fn remove_entry(&mut self, reference: TimerRef) -> Option<TimerEntry> {
        let entry = self.entries.remove(&reference)?;
        self.swap_remove_bucket_slot(entry.bucket, entry.slot);
        Some(entry)
    }

    fn swap_remove_bucket_slot(&mut self, bucket: usize, slot: usize) {
        let Some(bucket_entries) = self.buckets.get_mut(bucket) else {
            return;
        };
        if slot >= bucket_entries.len() {
            return;
        }
        let moved = bucket_entries.swap_remove(slot);
        if slot < bucket_entries.len() {
            let replacement = bucket_entries[slot];
            if let Some(entry) = self.entries.get_mut(&replacement) {
                entry.slot = slot;
            }
        }
        if let Some(entry) = self.entries.get_mut(&moved) {
            entry.slot = slot;
        }
    }
}

impl Default for TimerWheel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::TimerWheel;
    use crate::atom::Atom;
    use crate::term::Term;

    #[test]
    fn timer_schedule_and_tick_expire_due_timers() {
        let start = Instant::now();
        let mut wheel = TimerWheel::with_bucket_count(8);
        let reference =
            wheel.schedule_at(start, Duration::from_millis(10), 12, Term::atom(Atom::OK));

        assert!(wheel.tick_at(start + Duration::from_millis(9)).is_empty());
        let expired = wheel.tick_at(start + Duration::from_millis(10));

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].reference, reference);
        assert_eq!(expired[0].target_pid, 12);
        assert_eq!(expired[0].message, Term::atom(Atom::OK));
        assert!(wheel.is_empty());
    }

    #[test]
    fn timer_cancellation_is_constant_time_and_returns_remaining_time() {
        let start = Instant::now();
        let mut wheel = TimerWheel::with_bucket_count(4);
        let reference = wheel.schedule_at(start, Duration::from_millis(100), 1, Term::small_int(1));

        assert_eq!(
            wheel.cancel_at(reference, start + Duration::from_millis(40)),
            Some(Duration::from_millis(60))
        );
        assert_eq!(wheel.cancel_at(reference, start), None);
        assert!(wheel.tick_at(start + Duration::from_millis(100)).is_empty());
    }

    #[test]
    fn timer_cancellation_after_fire_returns_none() {
        let start = Instant::now();
        let mut wheel = TimerWheel::with_bucket_count(4);
        let reference = wheel.schedule_at(start, Duration::from_millis(1), 1, Term::small_int(1));

        assert_eq!(wheel.tick_at(start + Duration::from_millis(1)).len(), 1);
        assert_eq!(
            wheel.cancel_at(reference, start + Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn timer_handles_ten_thousand_concurrent_timers() {
        let start = Instant::now();
        let mut wheel = TimerWheel::with_bucket_count(256);
        for index in 0..10_000 {
            wheel.schedule_at(
                start,
                Duration::from_millis((index % 100) as u64),
                index,
                Term::small_int(index as i64),
            );
        }

        assert_eq!(wheel.len(), 10_000);
        let expired = wheel.tick_at(start + Duration::from_millis(100));
        assert_eq!(expired.len(), 10_000);
        assert!(wheel.is_empty());
    }
}
