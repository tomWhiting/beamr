# ADR-004: Low-Bit Term Tagging, Not NaN-Boxing

**Status:** Accepted
**Date:** 2026-05-29

## Context

Every value in the BEAM VM is a "term" -- a tagged machine word that
encodes the type and either the value itself (small integers, atoms) or
a pointer to heap-allocated data (tuples, lists, binaries).

Two tagging schemes were evaluated:

- **NaN-boxing:** Exploits the IEEE 754 NaN space to pack pointers and
  small values into 64-bit floats. Efficient for float-heavy workloads
  because floats are unboxed. Complex bit manipulation; diverges from
  BEAM's own representation.

- **Low-bit tagging:** Uses the lowest 2-4 bits of a machine word for
  type tags. Matches BEAM's native tagging scheme. Integers, atoms, and
  pointers are the common cases and decode with a single mask operation.

## Decision

We use classic low-bit term tagging. The lowest bits of a 64-bit word
encode the term type (integer, atom, boxed pointer, list pointer, etc.).

## Consequences

**Positive:**
- Matches BEAM's own tagging conventions. Bytecode assumptions about
  term layout translate directly.
- Decoding the hot-path types (small integer, atom, pointer) is a
  single bitwise AND plus branch.
- Well-understood scheme with decades of prior art in Erlang/OTP.

**Negative:**
- Floats must be heap-allocated (boxed). This adds an indirection for
  float operations. Acceptable because Gleam workflow code is not
  float-heavy -- the hot path is atoms, integers, and data structures.
- Slightly less pointer space than NaN-boxing on 64-bit platforms, but
  the 46+ usable address bits are more than sufficient.
