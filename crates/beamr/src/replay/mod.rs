//! Deterministic replay support.
//!
//! Replay mode consumes an immutable recorded event log and feeds the recorded
//! decisions back into the runtime at nondeterministic decision points.

mod debugger;
mod driver;
mod file;
#[cfg(test)]
mod file_tests;
mod recorder;

pub use debugger::{
    FunctionInspection, HeapInspection, MailboxInspection, ProcessSnapshot, RegisterInspection,
    RegisterKind, ReplayDebugger, ReplayStepOutcome, StackFrameInspection,
};
pub use driver::{
    CliReplayResult, NativeOutcome, RecordedDeliveryKind, RecordedMessageDelivery,
    RecordedNativeCall, RecordedSchedule, RecordedSelect, RecordedTimerExpiry, ReplayDriver,
    ReplayEvent, ReplayLog, ReplayMismatch,
};
pub use file::ReplayLogFileError;
pub use recorder::ReplayRecorder;
