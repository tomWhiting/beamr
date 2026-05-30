//! Supervision — links, monitors, and letting it crash.
//!
//! The VM provides four primitives: links (bidirectional, fatal by
//! default), monitors (unidirectional, non-fatal), exit signals
//! (carry the reason along links/monitors), and the trap-exit flag
//! (converts fatal signals into messages). Supervisor strategies
//! are Gleam library code (gleam_otp), not VM machinery (per D7).
pub mod link;
pub mod monitor;
