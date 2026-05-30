Review and harden the implementation. You have two jobs:

1. HARDEN: Fix naming drift, missing error handling, convention violations, edge cases. Use Edit and Write directly.
2. REVIEW: Verify acceptance criteria for each R#. Check the ACTUAL CODE (use git diff HEAD~1), not the dev summary. Tick checklist items. Confirm stories.



## Brief: B-002 — Implement the global atom table

## Requirements

### R1: Atom newtype

beamr::atom SHALL define an Atom newtype wrapping a u32 index. Atom SHALL implement Copy, Clone, Eq, PartialEq, Hash, and Debug. Atom SHALL NOT expose its inner index publicly — Atom::index() SHALL be pub(crate), not pub. External crates SHALL compare atoms by equality, not by index value.

Modify: crates/beamr/src/atom/mod.rs

Acceptance:
- Atom is a public struct in beamr::atom
- Atom implements Copy, Clone, Eq, PartialEq, Hash, Debug
- Atom has no public fields (inner index is private)
- Atom::index() is pub(crate) — accessible within beamr but not from beamr-cli or other external crates
- Two Atoms with the same index are equal; two with different indices are not
- Attempting to call atom.index() from beamr-cli produces a compile error

Checklist:
- C12: Global atom table implemented as a concurrent map supporting lock-free reads

### R2: AtomTable concurrent intern map

beamr::atom::table SHALL define an AtomTable struct that supports concurrent interning and lookup. WHEN a new string is interned THE SYSTEM SHALL assign it the next sequential index and store the mapping in both directions. WHEN an already-interned string is interned again THE SYSTEM SHALL return the existing index without creating a duplicate. It SHALL NOT require an exclusive lock for read operations.

Modify: crates/beamr/src/atom/table.rs

Acceptance:
- AtomTable is a public struct in beamr::atom::table
- AtomTable::new() creates an empty table (before pre-registration)
- AtomTable::intern("hello") returns an Atom; calling intern("hello") again returns the same Atom
- AtomTable::intern("hello") and AtomTable::intern("world") return different Atoms
- AtomTable::resolve(atom) returns Some(&str) for a valid atom
- AtomTable::resolve(invalid_atom) returns None for an index that was never interned

Checklist:
- C12: Global atom table implemented as a concurrent map supporting lock-free reads
- C13: Inserting a new atom string returns a unique Atom index
- C14: Inserting an already-interned atom string returns the same index
- C15: Lookup by index returns the original atom string

### R3: Thread-safe concurrent access

WHILE multiple scheduler threads are running THE SYSTEM SHALL allow concurrent intern and resolve operations on the same AtomTable without data races. Concurrent inserts of the same string from different threads SHALL converge on a single index — no duplicates. The implementation SHALL use a lock-free or sharded concurrent map (e.g. dashmap). It SHALL NOT use a single Mutex<HashMap> for the hot read path.

Modify: crates/beamr/src/atom/table.rs crates/beamr/Cargo.toml

Acceptance:
- Spawning 8 threads that each intern the same 100 strings results in exactly 100 unique atoms in the table
- Spawning 8 threads that each intern distinct strings results in all strings present with unique indices
- No Mutex<HashMap> wrapping the primary lookup or insert path (verified by code inspection)

Checklist:
- C16: Concurrent inserts from multiple threads never produce duplicate entries for the same string

### R4: Pre-registered common atoms

AtomTable SHALL provide a with_common_atoms() constructor that pre-registers the following atoms at known, stable indices: ok, error, true, false, nil, undefined, normal, kill, EXIT, badarg, badarith, badmatch, function_clause, case_clause, if_clause, undef, badfun, badarity, noproc. The indices of these atoms SHALL be accessible as associated constants on the Atom type (e.g. Atom::OK, Atom::ERROR). Pre-registered atoms SHALL NOT be re-assignable — their indices are fixed for the lifetime of the table.

Modify: crates/beamr/src/atom/table.rs crates/beamr/src/atom/mod.rs

Acceptance:
- AtomTable::with_common_atoms() returns a table with all 19 listed atoms pre-interned
- Atom::OK, Atom::ERROR, Atom::TRUE, Atom::FALSE, Atom::NIL are public associated constants
- table.resolve(Atom::OK) returns Some("ok")
- table.resolve(Atom::ERROR) returns Some("error")
- table.intern("ok") on a with_common_atoms() table returns Atom::OK (does not create a duplicate)

Checklist:
- C17: Common atoms pre-registered at table creation: ok, error, true, false, nil, undefined, normal, kill, EXIT, badarg, badarith, badmatch, function_clause, case_clause, if_clause, undef, badfun, badarity, noproc

### R5: Module wiring and public API re-exports

crates/beamr/src/atom/mod.rs SHALL re-export the public API: Atom and AtomTable. It SHALL contain only pub mod and pub use declarations — no logic. The atom module SHALL be usable as beamr::atom::Atom and beamr::atom::AtomTable from external crates.

Modify: crates/beamr/src/atom/mod.rs

Acceptance:
- use beamr::atom::Atom compiles from beamr-cli
- use beamr::atom::AtomTable compiles from beamr-cli
- atom/mod.rs contains no fn, struct, enum, trait, or impl blocks

Checklist:
- C12: Global atom table implemented as a concurrent map supporting lock-free reads

### R6: Unit and concurrency tests

WHEN the atom table is tested THE SYSTEM SHALL include tests for: basic intern/resolve round-trip, idempotent interning, common atom constants, concurrent interning from multiple threads, and resolve for invalid indices. Tests SHALL live in a #[cfg(test)] mod tests block within table.rs.

Modify: crates/beamr/src/atom/table.rs

Acceptance:
- cargo test -p beamr atom passes with all tests green
- A test verifies intern("x") followed by resolve returns "x"
- A test verifies intern("x") called twice returns the same Atom
- A test verifies Atom::OK resolves to "ok" after with_common_atoms()
- A test spawns multiple threads doing concurrent inserts and asserts no duplicates
- A test verifies resolve on an out-of-range index returns None

Checklist:
- C13: Inserting a new atom string returns a unique Atom index
- C14: Inserting an already-interned atom string returns the same index
- C15: Lookup by index returns the original atom string
- C16: Concurrent inserts from multiple threads never produce duplicate entries for the same string

## Dev Results

### R1: implemented — Atom is a public tuple struct with a private u32 field, derives the required traits, exposes only crate-visible index construction/access, and equality is by derived value comparison over the private index.
Files: crates/beamr/src/atom/table.rs, crates/beamr/src/atom/mod.rs

### R2: implemented — AtomTable::new creates empty concurrent maps. intern first performs a DashMap read, then uses DashMap entry insertion so repeated strings return the existing Atom. New strings receive the next AtomicU32 index and are inserted in both directions. resolve uses the reverse DashMap and returns None for absent indices.
Files: crates/beamr/src/atom/table.rs

### R3: implemented — The implementation uses DashMap for both hot lookup directions and AtomicU32 for unique allocation; tests spawn 8 threads for same-string convergence and 8 threads for distinct-string uniqueness/resolution.
Files: crates/beamr/Cargo.toml, Cargo.lock, crates/beamr/src/atom/table.rs

### R4: implemented — Atom constants use stable zero-based indices in the required order. with_common_atoms pre-inserts each required string/Atom pair into both maps and advances next_index to 19; intern("ok") then returns Atom::OK.
Files: crates/beamr/src/atom/table.rs, crates/beamr/src/atom/mod.rs

### R5: implemented — The atom module now contains only the public submodule declaration and re-export declarations, making beamr::atom::Atom and beamr::atom::AtomTable available externally while keeping implementation logic in table.rs.
Files: crates/beamr/src/atom/mod.rs

### R6: implemented — table.rs includes all requested tests under a #[cfg(test)] mod tests block. The test suite covers intern/resolve, idempotent interning, distinct atoms, common constants, invalid atom lookup, and concurrent insertion behavior.
Files: crates/beamr/src/atom/table.rs, crates/beamr/src/error.rs, crates/beamr/src/gc/major.rs, crates/beamr/src/gc/minor.rs, crates/beamr/src/hook.rs, crates/beamr/src/interpreter/opcodes.rs, crates/beamr/src/interpreter/pattern.rs, crates/beamr/src/loader/decode.rs, crates/beamr/src/loader/parser.rs, crates/beamr/src/loader/validate.rs, crates/beamr/src/mailbox/selective.rs, crates/beamr/src/module.rs, crates/beamr/src/native/bifs.rs, crates/beamr/src/process/heap.rs, crates/beamr/src/process/registry.rs, crates/beamr/src/process/stack.rs, crates/beamr/src/scheduler/dirty.rs, crates/beamr/src/scheduler/run_queue.rs, crates/beamr/src/scheduler/steal.rs, crates/beamr/src/supervision/link.rs, crates/beamr/src/supervision/monitor.rs, crates/beamr/src/term/binary.rs, crates/beamr/src/term/boxed.rs, crates/beamr/src/term/compare.rs, crates/beamr/src/timer.rs

## Verification Criteria

- cargo check --workspace
- cargo clippy --workspace -- -D warnings
- cargo test -p beamr atom -- --nocapture
- Code inspection: `crates/beamr/src/atom/mod.rs` contains only docs, `pub mod table;`, and `pub use table::{Atom, AtomTable};` — no struct/fn/impl/etc.
- Code inspection: no `Mutex<HashMap>` or single global mutex in AtomTable primary intern/resolve path.
- Optional manual external API check: from beamr-cli or a scratch external crate, `use beamr::atom::{Atom, AtomTable};` compiles, while calling `atom.index()` does not.

Dev attestation: panics=true, unsafe=true, boundaries=true, tests=true

Full design document: docs/design/beamr/DESIGN.md

Set pass=true only if all acceptance criteria are met and no blocking issues remain.
