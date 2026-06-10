//! Replay driver and recorded decision event log.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::atom::Atom;
use crate::native::ExceptionClass;
use crate::term::Term;
use crate::timer::{ExpiredTimer, TimerRef};

/// Immutable event log consumed by [`ReplayDriver`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReplayLog {
    events: Arc<[ReplayEvent]>,
}

impl ReplayLog {
    /// Build a replay log from recorded events in decision order.
    #[must_use]
    pub fn new(events: Vec<ReplayEvent>) -> Self {
        Self {
            events: Arc::from(events),
        }
    }

    /// Return the number of recorded events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns true when no events were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn get(&self, index: usize) -> Option<&ReplayEvent> {
        self.events.get(index)
    }
}

impl From<Vec<ReplayEvent>> for ReplayLog {
    fn from(events: Vec<ReplayEvent>) -> Self {
        Self::new(events)
    }
}

/// Recorded nondeterministic decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayEvent {
    /// A selective receive chose the message at `index`.
    Select(RecordedSelect),
    /// A message became visible to a receiver mailbox.
    MessageDelivery(RecordedMessageDelivery),
    /// A scheduler time slice was selected for execution.
    Schedule(RecordedSchedule),
    /// Timers expired when the clock was observed at `now`.
    TimerExpiry(RecordedTimerExpiry),
    /// A native call returned without being re-executed.
    NativeCall(RecordedNativeCall),
}

/// Kind of causal delivery recorded in the single-node replay log.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordedDeliveryKind {
    /// Ordinary process-to-process message delivery.
    Message,
    /// Trapped exit signal delivered as an `EXIT` tuple.
    ExitSignal,
    /// Monitor notification delivered as a `DOWN` tuple.
    DownMessage,
    /// Runtime-owned I/O/group-leader message delivery.
    RuntimeMessage,
}

/// Recorded mailbox delivery with both total-order and per-process clock data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordedMessageDelivery {
    /// Monotonic total-order delivery index assigned during recording.
    pub order: u64,
    /// Delivery class.
    pub kind: RecordedDeliveryKind,
    /// Local sender process when the delivery has one.
    pub sender_pid: Option<u64>,
    /// Receiver process whose mailbox observed the message.
    pub receiver_pid: u64,
    /// Sender logical clock after the send event, or zero for runtime-originated messages.
    pub sender_clock: u64,
    /// Receiver logical clock after delivery.
    pub receiver_clock: u64,
    /// Delivered message term as visible in the receiver heap/mailbox.
    pub message: Term,
}

/// Recorded scheduler slice boundary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordedSchedule {
    /// Process chosen by the recorded scheduler.
    pub pid: u64,
    /// Scheduler worker index that ran the slice during recording.
    pub scheduler_index: usize,
    /// Reduction budget assigned at the start of the slice.
    pub reduction_budget: u32,
    /// Reductions consumed before the context switch.
    pub reductions_consumed: u32,
}

/// Recorded selective receive result.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordedSelect {
    /// Process that performed the receive.
    pub pid: u64,
    /// Zero-based mailbox index selected by the recorded run.
    pub index: usize,
    /// Message visible at the recorded index.
    pub message: Term,
}

/// Recorded timer expiry batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedTimerExpiry {
    /// Instant used for the deterministic timer tick.
    pub now: Instant,
    /// Expired timers returned at that instant.
    pub expired: Vec<ExpiredTimer>,
}

/// Recorded native call result.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordedNativeCall {
    /// Calling process id.
    pub pid: u64,
    /// Native module atom.
    pub module: Atom,
    /// Native function atom.
    pub function: Atom,
    /// Native arity.
    pub arity: u8,
    /// Recorded outcome.
    pub outcome: NativeOutcome,
}

/// Recorded native result, including exception metadata for failures.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NativeOutcome {
    /// Native return value or raised reason.
    pub result: Result<Term, Term>,
    /// Exception class to use when `result` is `Err`.
    pub exception_class: ExceptionClass,
    /// Stacktrace to use when `result` is `Err`.
    pub exception_stacktrace: Term,
}

impl NativeOutcome {
    /// Build a successful native outcome.
    #[must_use]
    pub const fn ok(value: Term) -> Self {
        Self {
            result: Ok(value),
            exception_class: ExceptionClass::Error,
            exception_stacktrace: Term::NIL,
        }
    }

    /// Build a failing native outcome with exception metadata.
    #[must_use]
    pub const fn err(reason: Term, exception_class: ExceptionClass, stacktrace: Term) -> Self {
        Self {
            result: Err(reason),
            exception_class,
            exception_stacktrace: stacktrace,
        }
    }
}

/// Mismatch between the live replay point and the recorded event log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayMismatch {
    message: String,
}

impl ReplayMismatch {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for ReplayMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReplayMismatch {}

/// Deterministic event consumer used by replay mode.
#[derive(Clone, Debug)]
pub struct ReplayDriver {
    log: ReplayLog,
    cursor: usize,
}

impl ReplayDriver {
    /// Create a replay driver over an immutable recorded log.
    #[must_use]
    pub fn new(log: ReplayLog) -> Self {
        Self { log, cursor: 0 }
    }

    /// Return the number of events already consumed.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Return true when all recorded events have been consumed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.cursor >= self.log.len()
    }

    /// Consume a recorded selective receive decision.
    pub fn next_select(&mut self, pid: u64) -> Result<RecordedSelect, ReplayMismatch> {
        let event = self.peek_event("select")?;
        match event.clone() {
            ReplayEvent::Select(recorded) if recorded.pid == pid => {
                self.advance_cursor();
                Ok(recorded)
            }
            ReplayEvent::Select(recorded) => Err(self.mismatch(format!(
                "select pid mismatch: expected pid {}, recorded pid {}",
                pid, recorded.pid
            ))),
            other => Err(self.mismatch(format!(
                "event kind mismatch at select decision: recorded {:?}",
                other
            ))),
        }
    }

    /// Consume a recorded mailbox delivery in total causal order.
    pub fn next_message_delivery(
        &mut self,
        kind: RecordedDeliveryKind,
        sender_pid: Option<u64>,
        receiver_pid: u64,
        message: Term,
    ) -> Result<RecordedMessageDelivery, ReplayMismatch> {
        let event = self.peek_event("message delivery")?;
        match event.clone() {
            ReplayEvent::MessageDelivery(recorded)
                if recorded.kind == kind
                    && recorded.sender_pid == sender_pid
                    && recorded.receiver_pid == receiver_pid
                    && recorded.message == message =>
            {
                self.advance_cursor();
                Ok(recorded)
            }
            ReplayEvent::MessageDelivery(recorded) => Err(self.mismatch(format!(
                "message delivery mismatch: expected kind/sender/receiver/message ({kind:?}, {sender_pid:?}, {receiver_pid}, {message:?}), recorded ({:?}, {:?}, {}, {:?})",
                recorded.kind, recorded.sender_pid, recorded.receiver_pid, recorded.message
            ))),
            other => Err(self.mismatch(format!(
                "event kind mismatch at message delivery: recorded {:?}",
                other
            ))),
        }
    }

    /// Inspect the next recorded scheduler slice without consuming it.
    #[must_use]
    pub fn peek_schedule(&self) -> Option<RecordedSchedule> {
        match self.log.get(self.cursor) {
            Some(ReplayEvent::Schedule(recorded)) => Some(*recorded),
            _ => None,
        }
    }

    /// Consume a recorded scheduler slice selection.
    pub fn next_schedule(
        &mut self,
        scheduler_index: usize,
    ) -> Result<RecordedSchedule, ReplayMismatch> {
        let event = self.peek_event("schedule")?;
        match event.clone() {
            ReplayEvent::Schedule(recorded) if recorded.scheduler_index == scheduler_index => {
                self.advance_cursor();
                Ok(recorded)
            }
            ReplayEvent::Schedule(recorded) => Err(self.mismatch(format!(
                "schedule worker mismatch: expected scheduler {}, recorded scheduler {} for pid {}",
                scheduler_index, recorded.scheduler_index, recorded.pid
            ))),
            other => Err(self.mismatch(format!(
                "event kind mismatch at schedule decision: recorded {:?}",
                other
            ))),
        }
    }

    /// Validate the reductions consumed by a slice selected from the replay log.
    pub fn validate_schedule_reductions(
        &self,
        recorded: RecordedSchedule,
        actual_reductions: u32,
    ) -> Result<(), ReplayMismatch> {
        if recorded.reductions_consumed == actual_reductions {
            Ok(())
        } else {
            Err(self.mismatch(format!(
                "schedule reduction mismatch for pid {}: expected {}, actual {}",
                recorded.pid, recorded.reductions_consumed, actual_reductions
            )))
        }
    }

    /// Consume a recorded timer expiry batch.
    pub fn next_timer_expiry(&mut self) -> Result<RecordedTimerExpiry, ReplayMismatch> {
        let event = self.peek_event("timer expiry")?;
        match event.clone() {
            ReplayEvent::TimerExpiry(recorded) => {
                self.advance_cursor();
                Ok(recorded)
            }
            other => Err(self.mismatch(format!(
                "event kind mismatch at timer decision: recorded {:?}",
                other
            ))),
        }
    }

    /// Consume a recorded native result.
    pub fn next_native_call(
        &mut self,
        pid: u64,
        module: Atom,
        function: Atom,
        arity: u8,
    ) -> Result<RecordedNativeCall, ReplayMismatch> {
        let event = self.peek_event("native call")?;
        match event.clone() {
            ReplayEvent::NativeCall(recorded)
                if recorded.pid == pid
                    && recorded.module == module
                    && recorded.function == function
                    && recorded.arity == arity =>
            {
                self.advance_cursor();
                Ok(recorded)
            }
            ReplayEvent::NativeCall(recorded) => Err(self.mismatch(format!(
                "native call mismatch: expected pid/module/function/arity ({pid}, {:?}, {:?}, {arity}), recorded ({}, {:?}, {:?}, {})",
                module, function, recorded.pid, recorded.module, recorded.function, recorded.arity
            ))),
            other => Err(self.mismatch(format!(
                "event kind mismatch at native decision: recorded {:?}",
                other
            ))),
        }
    }

    /// Return a replay-backed select facility for the next recorded select.
    pub fn select_facility(
        shared: Arc<Mutex<Self>>,
        pid: u64,
    ) -> Result<Arc<ReplaySelectFacility>, ReplayMismatch> {
        let mut guard = match shared.lock() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        };
        let recorded = guard.next_select(pid)?;
        Ok(Arc::new(ReplaySelectFacility::new(recorded)))
    }

    fn peek_event(&self, decision: &'static str) -> Result<&ReplayEvent, ReplayMismatch> {
        let Some(event) = self.log.get(self.cursor) else {
            return Err(self.mismatch(format!("replay log exhausted before {decision} decision")));
        };
        Ok(event)
    }

    fn advance_cursor(&mut self) {
        self.cursor = self.cursor.saturating_add(1);
    }

    fn mismatch(&self, message: String) -> ReplayMismatch {
        ReplayMismatch::new(format!("{message} at replay cursor {}", self.cursor))
    }
}

/// Select facility that exposes only the recorded matched message at its
/// recorded index, preventing live mailbox order from influencing replay.
pub struct ReplaySelectFacility {
    recorded: RecordedSelect,
    removed_index: Mutex<Option<usize>>,
}

impl ReplaySelectFacility {
    fn new(recorded: RecordedSelect) -> Self {
        Self {
            recorded,
            removed_index: Mutex::new(None),
        }
    }

    /// Recorded removal, if the selector consumed the message.
    #[must_use]
    pub fn removed_index(&self) -> Option<usize> {
        *match self.removed_index.lock() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        }
    }
}

impl crate::native::SelectFacility for ReplaySelectFacility {
    fn message_count(&self) -> usize {
        self.recorded.index.saturating_add(1)
    }

    fn peek_message(&self, index: usize) -> Option<Term> {
        (index == self.recorded.index).then_some(self.recorded.message)
    }

    fn remove_message(&self, index: usize) {
        if index == self.recorded.index {
            *match self.removed_index.lock() {
                Ok(guard) => guard,
                Err(error) => error.into_inner(),
            } = Some(index);
        }
    }
}

impl From<RecordedTimerExpiry> for Vec<ExpiredTimer> {
    fn from(recorded: RecordedTimerExpiry) -> Self {
        recorded.expired
    }
}

impl From<(u64, u64, Term, Instant)> for ReplayEvent {
    fn from((reference, target_pid, message, expires_at): (u64, u64, Term, Instant)) -> Self {
        Self::TimerExpiry(RecordedTimerExpiry {
            now: expires_at,
            expired: vec![ExpiredTimer {
                reference: TimerRef::from_id(reference),
                target_pid,
                message,
                expires_at,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::select::SelectFacility;

    #[test]
    fn driver_consumes_select_decisions_in_order() {
        let log = ReplayLog::new(vec![ReplayEvent::Select(RecordedSelect {
            pid: 7,
            index: 2,
            message: Term::small_int(42),
        })]);
        let mut driver = ReplayDriver::new(log);

        match driver.next_select(7) {
            Ok(recorded) => {
                assert_eq!(recorded.index, 2);
                assert_eq!(recorded.message, Term::small_int(42));
            }
            Err(error) => assert!(error.to_string().is_empty()),
        }
        assert!(driver.is_complete());
    }

    #[test]
    fn replay_select_facility_exposes_recorded_index_only() {
        let recorded = RecordedSelect {
            pid: 1,
            index: 3,
            message: Term::small_int(99),
        };
        let facility = ReplaySelectFacility::new(recorded);

        assert_eq!(facility.message_count(), 4);
        assert_eq!(facility.peek_message(0), None);
        assert_eq!(facility.peek_message(3), Some(Term::small_int(99)));
        facility.remove_message(3);
        assert_eq!(facility.removed_index(), Some(3));
    }

    #[test]
    fn driver_consumes_message_deliveries_in_total_order() {
        let log = ReplayLog::new(vec![
            ReplayEvent::MessageDelivery(RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(1),
                receiver_pid: 2,
                sender_clock: 1,
                receiver_clock: 2,
                message: Term::small_int(10),
            }),
            ReplayEvent::MessageDelivery(RecordedMessageDelivery {
                order: 1,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(2),
                receiver_pid: 1,
                sender_clock: 3,
                receiver_clock: 4,
                message: Term::small_int(20),
            }),
        ]);
        let mut driver = ReplayDriver::new(log);

        let first = driver
            .next_message_delivery(
                RecordedDeliveryKind::Message,
                Some(1),
                2,
                Term::small_int(10),
            )
            .unwrap_or_else(|error| panic!("unexpected replay mismatch: {error}"));
        let second = driver
            .next_message_delivery(
                RecordedDeliveryKind::Message,
                Some(2),
                1,
                Term::small_int(20),
            )
            .unwrap_or_else(|error| panic!("unexpected replay mismatch: {error}"));

        assert_eq!(first.order, 0);
        assert_eq!(second.order, 1);
        assert_eq!(second.receiver_clock, 4);
        assert!(driver.is_complete());
    }

    #[test]
    fn driver_consumes_schedule_and_validates_reductions() {
        let log = ReplayLog::new(vec![ReplayEvent::Schedule(RecordedSchedule {
            pid: 3,
            scheduler_index: 0,
            reduction_budget: 17,
            reductions_consumed: 9,
        })]);
        let mut driver = ReplayDriver::new(log);

        assert_eq!(driver.peek_schedule().map(|recorded| recorded.pid), Some(3));
        let recorded = driver
            .next_schedule(0)
            .unwrap_or_else(|error| panic!("unexpected replay mismatch: {error}"));
        assert_eq!(recorded.reduction_budget, 17);
        assert!(driver.validate_schedule_reductions(recorded, 9).is_ok());
        assert!(driver.validate_schedule_reductions(recorded, 8).is_err());
    }

    #[test]
    fn driver_reports_log_exhaustion_for_schedule_without_advancing() {
        let mut driver = ReplayDriver::new(ReplayLog::default());

        let error = driver
            .next_schedule(0)
            .expect_err("empty log must report exhaustion");

        assert!(error.to_string().contains("replay log exhausted"));
        assert_eq!(driver.cursor(), 0);
    }

    #[test]
    fn driver_reports_schedule_worker_mismatch_without_advancing() {
        let mut driver = ReplayDriver::new(ReplayLog::new(vec![ReplayEvent::Schedule(
            RecordedSchedule {
                pid: 3,
                scheduler_index: 1,
                reduction_budget: 17,
                reductions_consumed: 9,
            },
        )]));

        let error = driver
            .next_schedule(0)
            .expect_err("wrong worker must mismatch");

        assert!(error.to_string().contains("schedule worker mismatch"));
        assert_eq!(driver.cursor(), 0);
    }

    #[test]
    fn driver_reports_message_delivery_mismatch_without_advancing() {
        let mut driver = ReplayDriver::new(ReplayLog::new(vec![ReplayEvent::MessageDelivery(
            RecordedMessageDelivery {
                order: 0,
                kind: RecordedDeliveryKind::Message,
                sender_pid: Some(1),
                receiver_pid: 2,
                sender_clock: 1,
                receiver_clock: 2,
                message: Term::small_int(10),
            },
        )]));

        let error = driver
            .next_message_delivery(
                RecordedDeliveryKind::Message,
                Some(2),
                1,
                Term::small_int(10),
            )
            .expect_err("wrong endpoints must mismatch");

        assert!(error.to_string().contains("message delivery mismatch"));
        assert_eq!(driver.cursor(), 0);
    }

    #[test]
    fn driver_reports_mismatch_without_advancing_log_mutation() {
        let log = ReplayLog::new(vec![ReplayEvent::NativeCall(RecordedNativeCall {
            pid: 1,
            module: Atom::MODULE,
            function: Atom::OK,
            arity: 0,
            outcome: NativeOutcome::ok(Term::atom(Atom::OK)),
        })]);
        let mut driver = ReplayDriver::new(log);

        match driver.next_select(1) {
            Ok(recorded) => assert_eq!(recorded.pid, u64::MAX),
            Err(error) => assert!(error.to_string().contains("event kind mismatch")),
        }
        assert_eq!(driver.cursor(), 0);
    }
}
