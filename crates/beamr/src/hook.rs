//! Reduction-boundary hook — the bit that's ours.
//!
//! At every process yield (budget exhausted or blocking on receive),
//! the hook fires if a registrant is present. The core provides the
//! seam; what runs in it is registered from outside (per D9). The
//! core does not know what diagnostics are — it fires the callback
//! and waits for a response. If no hook is registered, the yield
//! path skips invocation with zero overhead.

pub(crate) fn _scaffold() {}
