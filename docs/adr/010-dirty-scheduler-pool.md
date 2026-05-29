# ADR-010: Dirty Schedulers Are a Separate Thread Pool

**Status:** Accepted
**Date:** 2026-05-29

## Context

Some operations cannot complete within a single reduction budget: file
I/O, network calls, DNS resolution, native crypto, compression. If
these run on a normal scheduler thread, they block that thread and
starve all BEAM processes assigned to it.

BEAM/OTP solves this with dirty schedulers: separate thread pools
(dirty-cpu and dirty-io) where long-running or blocking native functions
execute without affecting normal scheduler fairness.

## Decision

Dirty schedulers are a separate thread pool, independent of the normal
scheduler threads. Native functions that may block are dispatched to the
dirty pool.

The dirty pool size is configurable independently of the normal
scheduler count. The normal scheduler thread count matches available CPU
cores; the dirty pool can be larger to absorb I/O concurrency.

## Consequences

**Positive:**
- Normal scheduler threads never block on native work. Fairness
  guarantees are preserved for all BEAM processes.
- I/O-bound BIFs can use Tokio (see ADR-003) within the dirty pool
  without contaminating the interpreter loop.
- Pool sizing is independently tunable: CPU-bound dirty work and
  I/O-bound dirty work can be separated if needed.

**Negative:**
- Additional thread pool to manage and configure. Adds operational
  surface area.
- Dispatching to the dirty pool requires suspending the calling process
  and resuming it when the dirty work completes. This is additional
  scheduler complexity.
- Thread pool overhead when no dirty work is in flight. Acceptable --
  idle threads consume minimal resources.
