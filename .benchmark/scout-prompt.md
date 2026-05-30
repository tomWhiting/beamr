Explore the codebase and gather implementation context for each R# in this brief. You are read-only — do not modify files.

For each R#, find:
- 2-5 key files the implementer should look at (with line ranges)
- Conventions to match (sibling patterns, naming, error handling)
- A concrete implementation approach
- Any gotchas or edge cases the brief might not have considered

The implementing agent has the same tools you do — focus on saving them time, not cataloguing every file. Be concise.



## Brief: B-005 — Implement term representation — tagging and immediates

Terms are the substrate — every value flowing through the interpreter is a term. This brief delivers the 64-bit tagged value type and all immediate term encodings (small integer, atom, pid, nil). Immediates require no heap allocation and are the hot path for pattern matching and arithmetic. The tagging scheme must be correct before any boxed types or the interpreter can build on it.

## Requirements

### R1: Term tagged value type

beamr::term SHALL define a Term type as a 64-bit value with low-bit tagging to distinguish types. Term SHALL implement Copy, Clone, Debug. Term SHALL NOT expose its raw u64 as a public field — all access is through typed constructors and extractors. The tag layout SHALL reserve at least 3 low bits for primary tag discrimination.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term is a public struct in beamr::term wrapping a u64
- Term implements Copy, Clone, Debug
- Term has no public fields
- std::mem::size_of::<Term>() == 8
- Tag constants are defined for: small integer, atom, pid, nil, boxed, list

Checklist:
- C33: Term is a 64-bit value with low-bit tagging to distinguish types

Stories:
- S19: As an implementation agent, I want the term representation, heap, and GC to be defined in a single crate with clear module boundaries so that I can understand the term lifecycle without cross-crate indirection.

### R2: Small integer immediate encoding

WHEN a small integer is encoded as a Term THE SYSTEM SHALL store the value in the non-tag bits and set the small-integer tag. WHEN a Term with a small-integer tag is decoded THE SYSTEM SHALL return the original integer value. The range SHALL cover at least i61 (signed, fitting in 64 bits minus tag). It SHALL NOT silently truncate values outside the representable range — out-of-range values require big integer boxing (B-006 scope).

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term::small_int(0) round-trips: term.as_small_int() == Some(0)
- Term::small_int(42) round-trips: term.as_small_int() == Some(42)
- Term::small_int(-1) round-trips: term.as_small_int() == Some(-1)
- Term::small_int(i64::MAX >> 3) round-trips correctly (max representable value)
- term.is_small_int() returns true for small integer terms, false for atoms

Checklist:
- C34: Small integer encoded as immediate: value fits in the non-tag bits, round-trips encode/decode

### R3: Atom immediate encoding

WHEN an Atom is encoded as a Term THE SYSTEM SHALL store the atom's u32 index in the non-tag bits and set the atom tag. WHEN a Term with an atom tag is decoded THE SYSTEM SHALL return the original Atom. It SHALL NOT require heap allocation — atom terms are always immediate.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term::atom(Atom::OK) round-trips: term.as_atom() == Some(Atom::OK)
- Term::atom(Atom::ERROR) round-trips: term.as_atom() == Some(Atom::ERROR)
- term.is_atom() returns true for atom terms, false for integers
- No heap allocation occurs when creating an atom term

Checklist:
- C35: Atom encoded as immediate: atom index in non-tag bits, round-trips encode/decode

### R4: Pid immediate encoding

WHEN a process identifier is encoded as a Term THE SYSTEM SHALL store the pid data in the non-tag bits and set the pid tag. WHEN a Term with a pid tag is decoded THE SYSTEM SHALL return the original pid data. It SHALL NOT require heap allocation for local pids.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term::pid(0) round-trips: term.as_pid() == Some(0)
- Term::pid(12345) round-trips: term.as_pid() == Some(12345)
- term.is_pid() returns true for pid terms, false for atoms and integers

Checklist:
- C36: Pid encoded as immediate: process id data in non-tag bits, round-trips encode/decode

### R5: Nil constant

beamr::term SHALL define Term::NIL as a public associated constant representing the empty list / nil value. It SHALL be a distinguished value that is not equal to any small integer, atom, or pid term. term.is_nil() SHALL return true only for this value.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term::NIL is a public associated constant
- Term::NIL.is_nil() returns true
- Term::small_int(0).is_nil() returns false
- Term::atom(Atom::NIL).is_nil() returns false (atom nil is not list nil)
- Term::NIL != Term::small_int(0)

Checklist:
- C37: Nil represented as a distinguished constant value

### R6: Tag dispatch and type predicates

Term SHALL provide a tag() method returning a Tag enum and type predicate methods: is_small_int(), is_atom(), is_pid(), is_nil(), is_boxed(), is_list(). WHEN the interpreter dispatches on a term's type THE SYSTEM SHALL use these predicates or the Tag enum. It SHALL NOT require raw bit manipulation outside the term module.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- Term::small_int(1).tag() returns Tag::SmallInt
- Term::atom(Atom::OK).tag() returns Tag::Atom
- Term::NIL.tag() returns Tag::Nil
- All is_* predicates agree with the Tag enum for every term type
- Tag enum is public and exhaustive for all supported types

Checklist:
- C33: Term is a 64-bit value with low-bit tagging to distinguish types

### R7: Unit tests for immediate terms

WHEN immediate terms are tested THE SYSTEM SHALL include round-trip tests for every immediate type, boundary tests for small integer range limits, and cross-type discrimination tests. Tests SHALL live in a #[cfg(test)] mod tests block within term/mod.rs.

Modify: crates/beamr/src/term/mod.rs

Acceptance:
- cargo test -p beamr term passes with all tests green
- Tests cover: small int encode/decode round-trip, atom encode/decode round-trip, pid encode/decode round-trip, nil constant, tag dispatch, cross-type is_* predicates return false

Checklist:
- C34: Small integer encoded as immediate: value fits in the non-tag bits, round-trips encode/decode
- C35: Atom encoded as immediate: atom index in non-tag bits, round-trips encode/decode
- C36: Pid encoded as immediate: process id data in non-tag bits, round-trips encode/decode
- C37: Nil represented as a distinguished constant value

## Boundaries

- SHALL NOT implement boxed types (tuples, lists, floats, binaries, etc.) — B-006 scope
- SHALL NOT implement term comparison or ordering — B-007 scope
- SHALL NOT implement heap allocation — boxed terms need a heap, delivered by process/heap.rs in a later brief
- SHALL NOT implement term-to-string display formatting beyond Debug — the interpreter needs it but it's not this brief's job

Full design document: docs/design/beamr/DESIGN.md
