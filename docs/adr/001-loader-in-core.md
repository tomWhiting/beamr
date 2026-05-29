# ADR-001: Loader Is a Module Inside the Core Crate

**Status:** Accepted
**Date:** 2026-05-29

## Context

The BEAM loader's outputs -- Module, Term, Atom -- are runtime types
defined in the core crate. We evaluated three options for where the
loader code should live:

- **(A) Loader as a separate crate depending on core.**
  Illusory independence. The loader cannot compile without core, so the
  separation buys nothing except an extra crate boundary to maintain.

- **(B) Shared types crate (beamr-types or similar).**
  Gravity-well problem. Term needs heap concepts, heap needs scheduler
  context -- the "shared" crate quickly pulls in most of core anyway.

- **(C) Loader as a module inside core.**
  Simplest dependency graph. The loader is a consumer of core types and
  a producer of core structures. Colocating it acknowledges this reality.

## Decision

The loader is a module (`core::loader`) inside the core crate.

No separate crate, no shared-types crate. The module boundary within
core is sufficient to keep the loader's concerns isolated. If a real
seam emerges later (e.g., the loader becomes independently testable
against a trait-abstracted runtime), extraction is straightforward.

## Consequences

**Positive:**
- Simplest possible dependency graph -- one crate, one compilation unit.
- No cross-crate type sharing or version coordination.
- Refactoring is cheaper: move a module, not a crate.

**Negative:**
- Cannot compile the loader without compiling all of core. Acceptable
  because core compiles in seconds, not minutes, at this project scale.
- If the loader grows substantially, the core crate gets larger. This is
  a future-us problem with a known solution (extract to crate).
