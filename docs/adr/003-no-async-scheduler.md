# ADR-003: No Async Runtime in the Scheduler Hot Path

**Status:** Accepted
**Date:** 2026-05-29

## Context

BEAM processes are green threads managed by the VM's own scheduler, not
OS-level async tasks. The scheduler loop runs a process for a reduction
budget, then switches. This is a tight, predictable loop.

Introducing an async runtime (Tokio) into the scheduler hot path would
mean fighting two scheduling systems: BEAM's reduction-based preemption
and Tokio's cooperative task yielding. The interaction between them
creates subtle fairness bugs that are hard to reason about.

However, the VM does need non-blocking file and network I/O for BIFs
that interact with the outside world.

## Decision

The scheduler hot path uses plain OS threads and lock-free work-stealing
queues. No async runtime participates in process scheduling.

Tokio enters only at the edges: dirty schedulers (see ADR-010) use a
Tokio runtime for file I/O, network I/O, and other potentially blocking
native operations.

## Consequences

**Positive:**
- Reduction counting and preemption are straightforward to implement.
  One scheduling system, one set of fairness rules.
- No hidden yield points from `.await` inside the interpreter loop.
- Simpler debugging: stack traces show real threads, not async state
  machines.

**Negative:**
- Must implement work-stealing manually (or use crossbeam/deque).
- I/O-heavy BIFs require explicit dispatch to the dirty pool rather
  than just awaiting a future inline.
- Tokio is still a dependency, but only for the dirty scheduler pool,
  not the core interpreter.
