//! Dirty scheduler thread pool.
//!
//! A separate pool of OS threads for native functions that take
//! a long time (git push, cargo build). Long-running work goes
//! here so normal scheduler threads stay free and fair.
//! Pool size is configurable independently of the normal
//! scheduler thread count (per D10).

pub(crate) fn _scaffold() {}
