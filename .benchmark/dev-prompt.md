Implement every R# in this brief. Run cargo check, cargo clippy -- -D warnings, and cargo test on affected crates. Fix any failures before submitting.



## Brief: B-005 — Implement term representation — tagging and immediates

Define the Term type in crates/beamr/src/term/mod.rs as a 64-bit value with low-bit tagging. Implement encoding and decoding for each immediate type: small integers (value in non-tag bits), atoms (Atom index in non-tag bits), pids (process id data in non-tag bits), and nil (a distinguished constant). Define tag constants and provide safe construction/extraction methods. The Term type must be Copy and fit in a machine word. Re-export the public API from term/mod.rs.

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

## Scout Context

### R1
Files: crates/beamr/src/term/mod.rs:1-10, crates/beamr/src/lib.rs:6-18, docs/design/beamr/DESIGN.md:108-112, docs/design/beamr/DESIGN.md:342-346, docs/design/beamr/briefs/B-005.md:31-49
Approach: Define `Term` as a private-field newtype over `u64`, derive `Copy`, `Clone`, `Debug`, and strongly consider `Eq`/`PartialEq` now because R5 acceptance compares `Term` values. Reserve 3 low bits via private constants such as `TAG_BITS = 3`, `TAG_MASK = 0b111`, and stable primary tags for small-int, atom, pid, nil, boxed, and list. Keep numeric tag constants private unless a later module requires `pub(crate)` raw helpers; external dispatch should use the public `Tag` enum and methods.
Notes: B-006 and B-007 depend on this layout staying stable: B-006 needs distinct boxed/list primary tags, and B-007 says not to modify the `Term` struct/tag layout. No cluster `INDEX.md` was present in this worktree, so cluster discipline/decisions beyond `DESIGN.md` could not be read.

### R2
Files: crates/beamr/src/term/mod.rs:1-10, docs/design/beamr/briefs/B-005.md:51-66, docs/design/beamr/CHECKLIST.md:46-48, docs/design/beamr/briefs/B-008.md:124-136
Approach: Use 3 tag bits, so payload range is `-(1_i64 << 60)..=(1_i64 << 60) - 1`. Encode as `((value as u64) << TAG_BITS) | SMALL_INT_TAG`; decode after `is_small_int()` with `(self.0 as i64) >> TAG_BITS` so right shift sign-extends. Provide `try_small_int` for runtime overflow paths; `small_int` can assert/panic on invalid input until big integer boxing exists, but must never truncate.
Notes: Prefer `SMALL_INT_TAG = 0` because it keeps sign-extension decode simple. If `small_int` panics for out-of-range, keep that away from normal interpreter/runtime arithmetic paths; future BIFs should use checked arithmetic plus `try_small_int`.

### R3
Files: crates/beamr/src/term/mod.rs:1-10, crates/beamr/src/atom/mod.rs:1-6, crates/beamr/src/atom/table.rs:1-9, docs/design/beamr/briefs/B-002.md:30-39, docs/design/beamr/briefs/B-002.md:85-98
Approach: Import `crate::atom::Atom`. Encode atom terms as `((atom.index() as u64) << TAG_BITS) | ATOM_TAG`. Decode by checking the tag, shifting right by `TAG_BITS`, verifying the payload fits `u32`, then constructing an `Atom` through a crate-private constructor such as `Atom::from_index(u32)`; if B-002 does not provide that constructor, add it to the prerequisite Atom implementation as `pub(crate)`. The constructor/extractor should be pure bit operations and allocate nothing.
Notes: This R# has a hard prerequisite dead-end in current code: B-005 atom tests cannot compile until B-002’s `Atom` API exists. Also distinguish `Term::atom(Atom::NIL)` from `Term::NIL`; atom nil is not empty-list nil.

### R4
Files: crates/beamr/src/term/mod.rs:1-10, docs/design/beamr/briefs/B-005.md:84-97, docs/design/beamr/briefs/B-010.md:36-45, docs/design/beamr/CHECKLIST.md:49
Approach: Encode pid as `(pid << TAG_BITS) | PID_TAG` after checking `pid <= PID_MAX`. Decode with `(self.0 >> TAG_BITS)` when `is_pid()` is true. Mirror the small-int API style: a convenience `pid` constructor for valid local pids, and a checked `try_pid` to avoid silent high-bit truncation if future callers receive arbitrary `u64` values.
Notes: If future process IDs exceed 61 bits, they cannot be immediate local pids under this scheme and will need a boxed/remote pid story. That is outside B-005, but do not mask/truncate high bits now.

### R5
Files: crates/beamr/src/term/mod.rs:1-10, docs/design/beamr/briefs/B-005.md:99-114, docs/design/beamr/briefs/B-006.md:70-80, docs/design/beamr/briefs/B-010.md:36-45, docs/design/beamr/briefs/B-010.md:115-123
Approach: Choose nil’s canonical raw value as payload zero plus `NIL_TAG`, e.g. `Term(NIL_TAG)`. Implement `is_nil()` as exact equality with `Term::NIL`, not merely low-bit tag matching, so only the distinguished value is nil. Ensure `Term::atom(Atom::NIL)` uses `ATOM_TAG` and therefore has `is_nil() == false`. Derive or implement `PartialEq`/`Eq` so tests can assert `Term::NIL != Term::small_int(0)`.
Notes: If raw construction remains private, malformed nil-tagged values are unreachable publicly, but exact `is_nil` still better matches the requirement “true only for this value.”

### R6
Files: crates/beamr/src/term/mod.rs:1-10, docs/design/beamr/briefs/B-005.md:116-131, docs/design/beamr/briefs/B-006.md:34-37, docs/design/beamr/briefs/B-006.md:70-80, docs/design/beamr/briefs/B-007.md:120-124
Approach: Implement `tag()` by matching `self.0 & TAG_MASK` to public `Tag` variants. Implement all predicates as `self.tag() == Tag::...`, except `is_nil()` may use exact `self == Term::NIL` and `tag()` should return `Tag::Nil` only for the canonical nil value. Keep raw helper methods private or `pub(crate)` so later boxed/list modules can construct tagged pointers without external crates depending on bit layout.
Notes: There are 8 possible 3-bit patterns but only 6 supported tags. Since `Tag` should be exhaustive for supported types, avoid adding public `Invalid` unless the implementer wants explicit internal validation. If matching reserved tags, use a clear internal unreachable/reserved policy and do not expose raw constructors publicly.

### R7
Files: crates/beamr/src/term/mod.rs:1-10, docs/design/beamr/briefs/B-005.md:133-164, crates/beamr/Cargo.toml:1-7, Cargo.toml:1-5, docs/design/beamr/DESIGN.md:392-403
Approach: Add unit tests for size (`std::mem::size_of::<Term>() == 8`), small-int round trips (`0`, `42`, `-1`, max, and preferably min), checked range rejection via `try_small_int`, atom round trips for `Atom::OK`/`Atom::ERROR`, pid round trips for `0`/`12345`, nil distinctness including `Term::atom(Atom::NIL).is_nil() == false`, tag dispatch, and cross-type predicate false cases. Keep tests focused on immediate terms; do not create boxed/list terms or heap allocations in B-005 tests.
Notes: Avoid out-of-range expressions that overflow at compile time when testing small-int minimum underflow. Do not implement comparison/order tests here; B-007 owns term comparison beyond exact bit equality needed for this brief.

## Verification

- cargo test -p beamr term
- cargo check --workspace
- cargo clippy --workspace -- -D warnings
- Code inspection: `Term` is `pub struct Term(u64)` or equivalent private-field newtype, not a public raw field.
- Code inspection: no raw term bit manipulation is required outside `crates/beamr/src/term/`.
- Code inspection/prerequisite check: B-002 `Atom` type, constants, `pub(crate) index()`, and a crate-private decode constructor exist before running B-005 atom tests.

## Boundaries

- SHALL NOT implement boxed types (tuples, lists, floats, binaries, etc.) — B-006 scope
- SHALL NOT implement term comparison or ordering — B-007 scope
- SHALL NOT implement heap allocation — boxed terms need a heap, delivered by process/heap.rs in a later brief
- SHALL NOT implement term-to-string display formatting beyond Debug — the interpreter needs it but it's not this brief's job

Full design document: docs/design/beamr/DESIGN.md

For each R#, report: status, files changed, how satisfied, any deviation. For each C# and S# assigned to the R#, report whether delivered. Attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests pass.
