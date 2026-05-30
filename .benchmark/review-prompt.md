Review and harden the implementation. You have two jobs:

1. HARDEN: Fix naming drift, missing error handling, convention violations, edge cases. Use Edit and Write directly.
2. REVIEW: Verify acceptance criteria for each R#. Check the ACTUAL CODE (use git diff HEAD~1), not the dev summary. Tick checklist items. Confirm stories.



## Brief: B-001 — Establish workspace, crate structure, and error types

## Requirements

### R1: Workspace Cargo.toml configuration

The workspace Cargo.toml at the repo root SHALL declare resolver = "2" and members = ["crates/beamr", "crates/beamr-cli"]. It SHALL NOT list any other crates as members (beamr-meridian is a future brief).

Modify: Cargo.toml

Acceptance:
- Cargo.toml [workspace] members array contains exactly "crates/beamr" and "crates/beamr-cli"
- resolver = "2" is set

Checklist:
- C1: Workspace Cargo.toml at repo root declares members: beamr, beamr-cli

### R2: beamr crate configuration

The beamr crate at crates/beamr/ SHALL use edition 2024 and SHALL NOT declare dependencies on any Meridian, Yggdrasil, or norn crates. It MAY declare dependencies on third-party utility crates (e.g. dashmap, crossbeam) as needed by later briefs.

Modify: crates/beamr/Cargo.toml

Acceptance:
- crates/beamr/Cargo.toml has edition = "2024"
- crates/beamr/Cargo.toml [dependencies] section contains no entries matching meridian*, yggdrasil*, norn*, or libyggd*

Checklist:
- C2: beamr crate exists at crates/beamr/ with edition 2024
- C5: beamr has no dependencies on any Meridian, Yggdrasil, or norn crates

Stories:
- S14: As the Meridian runtime, I want beamr to have zero dependencies on Meridian, Yggdrasil, or norn crates so that the VM is testable and buildable in isolation.

### R3: beamr-cli crate configuration

The beamr-cli crate at crates/beamr-cli/ SHALL be a binary crate with edition 2024 and SHALL depend on the beamr crate only. It SHALL NOT depend on beamr-meridian or any Meridian crates.

Modify: crates/beamr-cli/Cargo.toml

Acceptance:
- crates/beamr-cli/Cargo.toml has edition = "2024"
- crates/beamr-cli/Cargo.toml [dependencies] lists only beamr = { path = "../beamr" }
- [[bin]] section defines name = "beamr"

Checklist:
- C3: beamr-cli crate exists at crates/beamr-cli/ as a binary crate with edition 2024
- C6: beamr-cli depends on beamr only

### R4: Public module declarations in lib.rs

crates/beamr/src/lib.rs SHALL declare public modules: atom, loader, term, process, interpreter, scheduler, gc, mailbox, supervision, native, hook, timer, module, error. It SHALL NOT contain any function, trait, struct, or enum definitions — only pub mod and pub use declarations.

Modify: crates/beamr/src/lib.rs

Acceptance:
- lib.rs contains exactly 14 pub mod declarations matching the list above
- lib.rs contains no fn, struct, enum, trait, or impl blocks
- Every declared module directory/file exists (even if scaffold-only)

Checklist:
- C4: beamr/src/lib.rs declares public modules: atom, loader, term, process, interpreter, scheduler, gc, mailbox, supervision, native, hook, timer, module, error

### R5: LoadError type

WHEN the loader encounters a failure (invalid magic bytes, missing chunk, malformed instruction, unresolved import) THE SYSTEM SHALL return a LoadError variant from beamr::error. LoadError SHALL implement std::fmt::Display with a human-readable description and std::error::Error. It SHALL NOT panic on any loader failure.

Modify: crates/beamr/src/error.rs

Acceptance:
- LoadError is a public enum in beamr::error
- Variants include at minimum: InvalidFormat, MissingChunk(String), DecodeError(String), ValidationError(String)
- LoadError implements Display and std::error::Error
- format!("{}", LoadError::MissingChunk("Atom".into())) produces a non-empty human-readable string

Checklist:
- C7: LoadError enum defined in beamr::error with variants for loader failures

Stories:
- S22: As an implementation agent, I want explicit error types rather than panics for all loader and interpreter failures so that test failures produce actionable diagnostics.

### R6: ExecError type

WHEN the interpreter or a process encounters a runtime failure (badmatch, function_clause, undef, badarith) THE SYSTEM SHALL return an ExecError variant from beamr::error. ExecError SHALL define the variant shape (enum variants with names) but SHALL use opaque placeholder fields — concrete payload types (e.g. Atom, Term) are defined by later briefs (B-005 terms). ExecError SHALL implement Display and std::error::Error. It SHALL NOT lock variant payloads to String types that would require breaking changes when terms land.

Modify: crates/beamr/src/error.rs

Acceptance:
- ExecError is a public enum in beamr::error
- Variants exist for at minimum: Badmatch, FunctionClause, Undef, Badarith, UserExit
- Variant payloads use placeholder types (unit or a simple wrapper) — no String fields for module/function/reason that will later become Atom/Term
- ExecError implements Display and std::error::Error
- format!("{}", ExecError::Badarith) produces a non-empty human-readable string

Checklist:
- C8: ExecError enum defined in beamr::error with variants for runtime failures

Stories:
- S22: As an implementation agent, I want explicit error types rather than panics for all loader and interpreter failures so that test failures produce actionable diagnostics.

### R7: Top-level BeamrError unifying type

beamr::error SHALL define a BeamrError enum that wraps LoadError and ExecError. It SHALL implement From<LoadError> and From<ExecError> for ergonomic ? usage. It SHALL implement Display and std::error::Error.

Modify: crates/beamr/src/error.rs

Acceptance:
- BeamrError is a public enum with variants Load(LoadError) and Exec(ExecError)
- BeamrError::from(LoadError::InvalidFormat) compiles and produces BeamrError::Load(_)
- BeamrError::from(ExecError::Badarith) compiles and produces BeamrError::Exec(_)
- BeamrError implements Display and std::error::Error

Checklist:
- C9: BeamrError enum defined in beamr::error wrapping LoadError and ExecError with From impls

Stories:
- S22: As an implementation agent, I want explicit error types rather than panics for all loader and interpreter failures so that test failures produce actionable diagnostics.

### R8: Workspace compiles and passes clippy

WHEN all crate configurations and error types are in place THE SYSTEM SHALL pass cargo check --workspace and cargo clippy --workspace -- -D warnings with zero errors and zero warnings.


Acceptance:
- cargo check --workspace exits with code 0
- cargo clippy --workspace -- -D warnings exits with code 0

Checklist:
- C10: cargo check --workspace passes clean
- C11: cargo clippy --workspace -- -D warnings passes clean

## Dev Results

### R1: implemented — Verified the root Cargo.toml already declares resolver = "2" and exactly the two required workspace members: crates/beamr and crates/beamr-cli; no manifest change was needed.
Files: 

### R2: implemented — Verified crates/beamr/Cargo.toml already uses edition 2024 and has an empty [dependencies] section with no Meridian/Yggdrasil/norn/libyggd dependencies.
Files: 

### R3: implemented — Verified crates/beamr-cli/Cargo.toml already uses edition 2024, defines [[bin]] name = "beamr", and has only beamr = { path = "../beamr" } in [dependencies].
Files: 

### R4: implemented — lib.rs now contains exactly 14 public module declarations for atom, loader, term, process, interpreter, scheduler, gc, mailbox, supervision, native, hook, timer, module, and error, with no fn/struct/enum/trait/impl blocks. All declared module files/directories exist.
Files: crates/beamr/src/lib.rs

### R5: implemented — LoadError is public with InvalidFormat, MissingChunk(String), DecodeError(String), and ValidationError(String), implements std::fmt::Display and std::error::Error, and a test verifies MissingChunk("Atom") formats to a non-empty human-readable string.
Files: crates/beamr/src/error.rs

### R6: implemented — ExecError is public with unit variants Badmatch, FunctionClause, Undef, Badarith, and UserExit, avoiding String payloads that would constrain future Term/Atom payload design. It implements Display and Error, and a test verifies Badarith formats non-empty.
Files: crates/beamr/src/error.rs

### R7: implemented — BeamrError is public with Load(LoadError) and Exec(ExecError), implements From<LoadError>, From<ExecError>, Display, and Error::source. Tests verify both conversion paths compile and produce the expected wrapper variants.
Files: crates/beamr/src/error.rs

### R8: implemented — Ran cargo fmt --check, cargo check --workspace, cargo clippy --workspace -- -D warnings, and cargo test --workspace successfully. The scaffold doc-comment cleanup fixes clippy::empty_line_after_doc_comments without allow attributes or adding implementation logic.
Files: crates/beamr/src/atom/mod.rs, crates/beamr/src/atom/table.rs, crates/beamr/src/gc/major.rs, crates/beamr/src/gc/minor.rs, crates/beamr/src/gc/mod.rs, crates/beamr/src/hook.rs, crates/beamr/src/interpreter/mod.rs, crates/beamr/src/interpreter/opcodes.rs, crates/beamr/src/interpreter/pattern.rs, crates/beamr/src/loader/decode.rs, crates/beamr/src/loader/mod.rs, crates/beamr/src/loader/parser.rs, crates/beamr/src/loader/validate.rs, crates/beamr/src/mailbox/mod.rs, crates/beamr/src/mailbox/selective.rs, crates/beamr/src/module.rs, crates/beamr/src/native/bifs.rs, crates/beamr/src/native/mod.rs, crates/beamr/src/process/heap.rs, crates/beamr/src/process/mod.rs, crates/beamr/src/process/registry.rs, crates/beamr/src/process/stack.rs, crates/beamr/src/scheduler/dirty.rs, crates/beamr/src/scheduler/mod.rs, crates/beamr/src/scheduler/run_queue.rs, crates/beamr/src/scheduler/steal.rs, crates/beamr/src/supervision/link.rs, crates/beamr/src/supervision/mod.rs, crates/beamr/src/supervision/monitor.rs, crates/beamr/src/term/binary.rs, crates/beamr/src/term/boxed.rs, crates/beamr/src/term/compare.rs, crates/beamr/src/term/mod.rs, crates/beamr/src/timer.rs

## Verification Criteria

- cargo check --workspace
- cargo clippy --workspace -- -D warnings
- grep -R 'meridian\|yggdrasil\|norn\|libyggd' crates/beamr/Cargo.toml || true
- Verify Cargo.toml [workspace] members are exactly crates/beamr and crates/beamr-cli and resolver = "2".
- Verify crates/beamr-cli/Cargo.toml [dependencies] contains only beamr = { path = "../beamr" } and [[bin]] name = "beamr".
- Verify crates/beamr/src/lib.rs has exactly 14 pub mod declarations and no fn/struct/enum/trait/impl blocks.
- Compile a small usage or test that formats LoadError::MissingChunk("Atom".into()), ExecError::Badarith, BeamrError::from(LoadError::InvalidFormat), and BeamrError::from(ExecError::Badarith).

Dev attestation: panics=true, unsafe=true, boundaries=true, tests=true

Full design document: docs/design/beamr/DESIGN.md

Set pass=true only if all acceptance criteria are met and no blocking issues remain.
