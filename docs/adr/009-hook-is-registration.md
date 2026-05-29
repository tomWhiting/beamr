# ADR-009: Reduction-Boundary Hook Is a Registration Point

**Status:** Accepted
**Date:** 2026-05-29

## Context

At every reduction boundary (after each instruction or group of
instructions), the VM has an opportunity to perform bookkeeping:
check for pending signals, update counters, or emit diagnostics.

The beamr-meridian crate needs to observe reduction boundaries for
telemetry and workflow diagnostics. However, core must not depend on
beamr-meridian or know what "diagnostics" means -- this is the project's
foundational dependency rule.

Two approaches:

- **Hard-coded call:** Core calls a specific diagnostics function at
  each reduction boundary. Violates the dependency rule.

- **Registration point:** Core provides a hook seam. External code
  registers a callback. Core invokes whatever is registered (or nothing).

## Decision

The reduction-boundary hook is a registration point, not a hard-coded
call. Core provides the seam; what runs in it is registered from outside
by beamr-meridian or any other consumer.

Core does not know what diagnostics are. It knows that something may
want to run at reduction boundaries and provides the mechanism.

## Consequences

**Positive:**
- Preserves the one rule: beamr-core depends on nothing of Meridian's.
- Hook overhead is zero when nothing is registered (a null check or
  Option::is_none branch, which the branch predictor will eliminate).
- Other consumers can register their own hooks for testing, profiling,
  or debugging without modifying core.

**Negative:**
- Indirect call overhead when a hook is registered (function pointer or
  trait object dispatch). Negligible compared to instruction execution.
- Hook API must be designed carefully: it becomes a stability surface
  that external crates depend on.
