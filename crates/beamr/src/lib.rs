//! Beamr — a Rust runtime with the BEAM's execution model.
//!
//! Loads `.beam` bytecode produced by the Gleam toolchain (via `erlc`)
//! and executes it with preemptive scheduling, per-process isolation,
//! supervision primitives, and a native function interface.
pub mod atom;
pub mod error;
pub mod gc;
pub mod hook;
pub mod interpreter;
pub mod loader;
pub mod mailbox;
pub mod module;
pub mod native;
pub mod process;
pub mod scheduler;
pub mod supervision;
pub mod term;
pub mod timer;
