//! Beamr — a Rust runtime with the BEAM's execution model.
//!
//! Loads `.beam` bytecode produced by the Gleam toolchain (via `erlc`)
//! and executes it with preemptive scheduling, per-process isolation,
//! supervision primitives, and a native function interface.
#![cfg_attr(all(not(feature = "std"), not(target_arch = "wasm32")), no_std)]

#[cfg(any(not(feature = "std"), target_arch = "wasm32"))]
extern crate alloc;

pub mod atom;
pub mod capability;
pub mod constant_pool;
#[cfg(feature = "net")]
pub mod distribution;
pub mod error;
pub mod etf;
pub mod ets;
pub mod gc;
#[cfg(feature = "threads")]
pub mod hook;
pub mod interpreter;
#[cfg(feature = "threads")]
#[path = "io/mod.rs"]
pub mod io;
#[cfg(feature = "jit")]
pub mod jit;
pub mod loader;
pub mod mailbox;
pub mod module;
pub mod namespace;
pub mod native;
pub mod process;
// The replay driver is a passive, in-memory event-log consumer (no threads, no
// tokio); it is reused by the cooperative runtime for deterministic delivery.
// Only its on-disk log format (`replay::file`) needs net/fs and stays gated.
#[cfg(any(feature = "threads", feature = "cooperative"))]
pub mod replay;
pub mod scheduler;
pub mod supervision;
#[cfg(feature = "telemetry")]
pub mod telemetry;
pub mod term;
// The passive timer wheel is polled (no thread, no tokio::sleep), so it is
// usable by the cooperative single-threaded runtime as well as the threaded one.
#[cfg(any(feature = "threads", feature = "cooperative"))]
pub mod timer;

// Ergonomic native-actor surface (NATIVE-003), re-exported at the crate root so
// downstream crates depend only on `beamr::` — not on scheduler internals.
// The platform-neutral trait/type surface is available in both the threaded
// native runtime and the cooperative (wasm) runtime so haematite/liminal import
// `beamr::Actor` etc. unchanged on both targets.
#[cfg(any(feature = "threads", feature = "cooperative"))]
pub use native::actor::{Actor, ActorContext, ActorError, ActorMessage};
// The external-driver request/reply handles (`ActorRef`/`SenderHandle`) and
// `spawn_actor` own the threaded `Scheduler` and block on a channel, so they are
// threaded-only for now; the cooperative spawn/`call_async` surface lands in a
// later WASM-runtime increment (WR-2/WR-6).
#[cfg(feature = "threads")]
pub use native::actor::{ActorRef, SenderHandle, spawn_actor};
#[cfg(any(feature = "threads", feature = "cooperative"))]
pub use native::native_process::{NativeContext, NativeHandler, NativeOutcome};
