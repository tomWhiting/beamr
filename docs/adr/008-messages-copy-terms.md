# ADR-008: Messages Copy Terms Between Processes

**Status:** Accepted
**Date:** 2026-05-29

## Context

BEAM processes are isolated: each has its own heap, and no two processes
share mutable state. When process A sends a message to process B, the
message must somehow arrive in B's heap.

Two approaches exist:

- **Copy semantics:** The send operation deep-copies the term from A's
  heap into B's heap. This is what BEAM/OTP does. It preserves total
  isolation at the cost of copy overhead.

- **Shared-heap or zero-copy:** Processes share a common heap or use
  immutable references. Reduces copy cost but introduces shared state,
  complicating GC and breaking the isolation model.

## Decision

Message send copies terms. When a process sends a message, the term is
deep-copied into the receiver's heap. Per-process isolation is absolute.

As a future optimisation, large binaries (above a size threshold) may
use reference-counted shared storage, matching BEAM's refc binary
strategy. This is an optimisation, not a semantic change.

## Consequences

**Positive:**
- Isolation guarantee is simple and total. Each process can be GC'd
  independently without coordinating with other processes.
- No shared mutable state between processes. Data races are impossible
  at the term level.
- Matches BEAM semantics exactly, so Gleam code behaves as expected.

**Negative:**
- Copy cost on every message send. For workflow-scale messages (small to
  medium data structures), this is acceptable.
- Large messages are expensive. The refc binary optimisation mitigates
  the worst case but adds implementation complexity when we get there.
