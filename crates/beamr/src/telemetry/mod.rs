//! Optional OpenTelemetry integration for Beamr runtime events.
//!
//! This module is compiled only with the `telemetry` feature so default builds
//! do not carry OpenTelemetry dependencies or call-site overhead.

pub mod lifecycle;
pub mod spans;
