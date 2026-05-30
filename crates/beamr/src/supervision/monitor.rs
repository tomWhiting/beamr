//! Unidirectional monitor management.
//!
//! A monitor lets one process watch another without sharing fate.
//! When the monitored process exits, the monitoring process receives
//! a DOWN message with the exit reason. The monitored process is
//! unaware of the monitor. Monitors are identified by unique
//! references for cancellation.

pub(crate) fn _scaffold() {}
