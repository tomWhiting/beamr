//! Deterministic replay support.
//!
//! Replay mode consumes an immutable recorded event log and feeds the recorded
//! decisions back into the runtime at nondeterministic decision points.

mod debugger;
mod driver;
// The on-disk replay-log codec uses distribution ETF (net) and std::fs; it does
// not build for wasm. The in-memory driver/recorder/debugger do.
#[cfg(all(feature = "net", feature = "fs"))]
mod file;
#[cfg(all(test, feature = "net", feature = "fs"))]
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
#[cfg(all(feature = "net", feature = "fs"))]
pub use file::ReplayLogFileError;
pub use recorder::ReplayRecorder;
