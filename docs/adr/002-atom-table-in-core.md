# ADR-002: Atom Table Lives in Core, Loader Accepts a Handle

**Status:** Accepted
**Date:** 2026-05-29

## Context

The atom table maps atom indices to their string representations and
back. It is populated during loading but queried throughout the lifetime
of the VM -- during pattern matching, message dispatch, and error
reporting.

Two options were considered:

- **(A) Atom table in a micro-crate** shared between loader and runtime.
  Adds a crate for a single data structure. The table's API depends on
  concurrency primitives already in core.

- **(B) Atom table in core**, with the loader receiving a handle
  (`&AtomTable` or `Arc<AtomTable>`) to populate during module loading.

## Decision

The atom table is defined and owned by the core crate. The loader
receives a handle to it and registers atoms during BEAM file parsing.

## Consequences

**Positive:**
- Clear ownership: the runtime owns the table, the loader borrows it.
- No cross-crate type sharing needed. The atom table's concurrency
  strategy (e.g., DashMap, RwLock) is an internal core detail.
- One fewer crate to version, test, and document.

**Negative:**
- The loader module is coupled to core's atom table API. This is
  acceptable because the loader already lives inside core (see ADR-001).
- If atom table internals change, loader code may need updating. The
  blast radius is contained within a single crate.
