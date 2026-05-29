# ADR-006: BIFs Are Demand-Driven

**Status:** Accepted
**Date:** 2026-05-29

## Context

The BEAM VM provides hundreds of built-in functions (BIFs) across
modules like `erlang:`, `lists:`, `ets:`, and `maps:`. Implementing
all of them upfront would be a massive effort, and most are irrelevant
to Gleam workflow execution.

The loader already produces an unresolved-import report: a list of
every external function a loaded module references that is not satisfied
by another loaded module. This report is the exact set of BIFs the
current workload requires.

## Decision

BIFs are implemented on demand. The set of built-in functions we
implement is determined by the loader's unresolved-import report. If
no loaded workflow module imports a BIF, that BIF does not need to exist.

The import table is the leash: it physically constrains scope to what
workflows actually call.

## Consequences

**Positive:**
- Scope is bounded by real usage, not by the full BEAM specification.
  Prevents "implement all of erlang:" scope creep.
- Each BIF implementation is justified by a concrete workflow that needs
  it. No speculative work.
- The unresolved-import report doubles as a progress tracker: zero
  unresolved imports means the workflow can run.

**Negative:**
- Adding a new Gleam dependency to a workflow may introduce new BIF
  requirements. The loader's report makes this immediately visible.
- BIF coverage grows incrementally rather than being complete upfront.
  For a general-purpose VM this would be a problem; for a Gleam workflow
  engine it is a feature.
