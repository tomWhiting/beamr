//! Deterministic replay support.
//!
//! Replay mode consumes an immutable recorded event log and feeds the recorded
//! decisions back into the runtime at nondeterministic decision points.

mod driver;

pub use driver::{
    NativeOutcome, RecordedNativeCall, RecordedSelect, RecordedTimerExpiry, ReplayDriver,
    ReplayEvent, ReplayLog, ReplayMismatch,
};
