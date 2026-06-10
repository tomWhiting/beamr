//! Replay event recorder helpers.
//!
//! The recorder assigns total-order sequence numbers to causal deliveries while
//! preserving per-process logical-clock metadata for validation and debugging.

use crate::replay::{
    RecordedDeliveryKind, RecordedMessageDelivery, RecordedSchedule, ReplayEvent, ReplayLog,
};
use crate::term::Term;

/// Mutable builder for deterministic replay logs.
#[derive(Clone, Debug, Default)]
pub struct ReplayRecorder {
    next_delivery_order: u64,
    events: Vec<ReplayEvent>,
}

impl ReplayRecorder {
    /// Create an empty recorder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_delivery_order: 0,
            events: Vec::new(),
        }
    }

    /// Record an already-observed mailbox delivery in total causal order.
    pub fn record_message_delivery(
        &mut self,
        kind: RecordedDeliveryKind,
        sender_pid: Option<u64>,
        receiver_pid: u64,
        sender_clock: u64,
        receiver_clock: u64,
        message: Term,
    ) -> RecordedMessageDelivery {
        let recorded = RecordedMessageDelivery {
            order: self.next_delivery_order,
            kind,
            sender_pid,
            receiver_pid,
            sender_clock,
            receiver_clock,
            message,
        };
        self.next_delivery_order = self.next_delivery_order.saturating_add(1);
        self.events.push(ReplayEvent::MessageDelivery(recorded));
        recorded
    }

    /// Record a scheduler slice boundary.
    pub fn record_schedule(
        &mut self,
        pid: u64,
        scheduler_index: usize,
        reduction_budget: u32,
        reductions_consumed: u32,
    ) -> RecordedSchedule {
        let recorded = RecordedSchedule {
            pid,
            scheduler_index,
            reduction_budget,
            reductions_consumed,
        };
        self.events.push(ReplayEvent::Schedule(recorded));
        recorded
    }

    /// Borrow recorded events in append order.
    #[must_use]
    pub fn events(&self) -> &[ReplayEvent] {
        &self.events
    }

    /// Finish recording and return an immutable replay log.
    #[must_use]
    pub fn into_log(self) -> ReplayLog {
        ReplayLog::new(self.events)
    }
}
