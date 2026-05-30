//! Crate-wide error types for beamr.
//!
//! All runtime failures are represented as values, never panics.
//! Process-level errors become exit reasons; loader errors prevent
//! module registration; interpreter errors halt the faulting process.

pub(crate) fn _scaffold() {}
