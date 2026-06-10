//! Deterministic replay support.
//!
//! Replay mode consumes an immutable recorded event log and feeds the recorded
//! decisions back into the runtime at nondeterministic decision points.

mod debugger;
mod driver;

pub use debugger::{
    FunctionInspection, HeapInspection, MailboxInspection, ProcessSnapshot, RegisterInspection,
    RegisterKind, ReplayDebugger, ReplayStepOutcome, StackFrameInspection,
};
pub use driver::{
    NativeOutcome, RecordedNativeCall, RecordedSelect, RecordedTimerExpiry, ReplayDriver,
    ReplayEvent, ReplayLog, ReplayMismatch,
};
