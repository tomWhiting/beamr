//! Supervision — links, monitors, and letting it crash.
//!
//! The VM provides four primitives: links (bidirectional, fatal by
//! default), monitors (unidirectional, non-fatal), exit signals
//! (carry the reason along links/monitors), and the trap-exit flag
//! (converts fatal signals into messages). Supervisor strategies
//! are Gleam library code (gleam_otp), not VM machinery (per D7).
pub mod link;
pub mod monitor;

pub use link::{LinkSet, enqueue_exit_message_pub, link, terminal_reason, unlink};
pub use monitor::{MonitorSet, enqueue_down_message_pub};
