Explore the codebase and gather implementation context for each R# in this brief. You are read-only — do not modify files.

For each R#, find:
- 2-5 key files the implementer should look at (with line ranges)
- Conventions to match (sibling patterns, naming, error handling)
- A concrete implementation approach
- Any gotchas or edge cases the brief might not have considered

The implementing agent has the same tools you do — focus on saving them time, not cataloguing every file. Be concise.



## Brief: B-001 — Establish workspace, crate structure, and error types

Set up the Cargo workspace and crate scaffolding so that every subsequent brief has a compilable foundation to build on. Establish the error type hierarchy so that all modules return values rather than panics from day one. This is the foundation everything else depends on — until it lands, no other brief can compile.

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

## Boundaries

- SHALL NOT add any third-party dependencies to beamr/Cargo.toml (dependencies like dashmap are added by the briefs that need them)
- SHALL NOT implement any logic in lib.rs beyond module declarations
- SHALL NOT add beamr-meridian to the workspace members (future brief scope)
- SHALL NOT implement loader, interpreter, or process logic — only the error type definitions they will use

Full design document: docs/design/beamr/DESIGN.md
