//! Beamr — a Rust runtime with the BEAM's execution model.
//!
//! Loads `.beam` bytecode produced by the Gleam toolchain (via `erlc`)
//! and executes it with preemptive scheduling, per-process isolation,
//! supervision primitives, and a native function interface.
pub mod atom;
pub mod constant_pool;
pub mod distribution;
pub mod error;
pub mod etf;
pub mod ets;
pub mod gc;
pub mod hook;
pub mod interpreter;
#[path = "io/mod.rs"]
pub mod io;
pub mod loader;
pub mod mailbox;
pub mod module;
pub mod namespace;
pub mod native;
pub mod process;
pub mod scheduler;
pub mod supervision;
pub mod term;
pub mod timer;
