Implement every R# in this brief. Run cargo check, cargo clippy -- -D warnings, and cargo test on affected crates. Fix any failures before submitting.



## Brief: B-001 — Establish workspace, crate structure, and error types

The workspace Cargo.toml and both crate Cargo.tomls already exist in the scaffold. Verify and if needed adjust them to match the design: workspace declares beamr and beamr-cli as members, beamr crate uses edition 2024 with no Meridian/Yggdrasil/norn dependencies, beamr-cli depends on beamr only. Verify lib.rs declares all public modules. Define the crate-wide error types in error.rs: LoadError for loader failures, ExecError for interpreter/process failures, and a top-level BeamrError that unifies them. All errors implement std::fmt::Display and std::error::Error. Ensure cargo check and cargo clippy pass clean across the workspace.

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

## Scout Context

### R1
Files: Cargo.toml:1-6, docs/design/beamr/briefs/B-001.md:37-49, docs/design/beamr/DESIGN.md:378-389, crates/beamr-meridian/Cargo.toml:1-8
Approach: Leave Cargo.toml as-is unless formatting has drifted. Ensure members remains an explicit two-entry array with only crates/beamr and crates/beamr-cli, and keep resolver = "2".
Notes: Do not add [workspace.dependencies] or include beamr-meridian; future crate presence is the main trap.

### R2
Files: crates/beamr/Cargo.toml:1-7, docs/design/beamr/DESIGN.md:61-65, docs/design/beamr/USER-STORIES.md:31-34, docs/adr/009-hook-is-registration.md:12-15
Approach: Keep beamr/Cargo.toml dependency-free. Implement error traits manually rather than adding thiserror or anyhow.
Notes: Verification grep for meridian/yggdrasil/norn/libyggd currently returns no matches. Avoid any dependency whose name matches those prefixes.

### R3
Files: crates/beamr-cli/Cargo.toml:1-12, crates/beamr-cli/src/main.rs:1-10, docs/design/beamr/DESIGN.md:378-381, docs/design/beamr/briefs/B-001.md:69-83
Approach: Leave Cargo.toml as-is. Do not implement CLI behavior in B-001; only keep the binary crate compiling.
Notes: Do not add clap/anyhow/beamr-meridian/Meridian dependencies. Acceptance requires [dependencies] list only beamr.

### R4
Files: crates/beamr/src/lib.rs:1-19, crates/beamr/src/loader/mod.rs:1-9, crates/beamr/src/gc/mod.rs:1-8, docs/design/beamr/DESIGN.md:325-377, docs/design/beamr/DESIGN.md:405-409
Approach: Keep lib.rs declaration-only. If a strict reviewer interprets “only pub mod and pub use” literally, convert/remove top outer doc comments, but do not add API definitions here.
Notes: Order differs from brief list but the required set and count are correct. No pub use is required for B-001.

### R5
Files: crates/beamr/src/error.rs:1-7, crates/beamr/src/loader/mod.rs:1-9, crates/beamr/src/loader/parser.rs:1-8, crates/beamr/src/loader/decode.rs:1-9, crates/beamr/src/loader/validate.rs:1-9
Approach: Define #[derive(Debug, Clone, PartialEq, Eq)] pub enum LoadError { InvalidFormat, MissingChunk(String), DecodeError(String), ValidationError(String) }. Add a Display impl with non-empty human-readable messages, e.g. MissingChunk(chunk) => “missing required BEAM chunk: {chunk}”, and impl std::error::Error for LoadError.
Notes: MissingChunk(String) is explicitly required; do not change it to &'static str. UnresolvedImport is mentioned conceptually but not required for B-001; avoid adding premature payload types.

### R6
Files: crates/beamr/src/error.rs:1-7, crates/beamr/src/interpreter/opcodes.rs:1-10, crates/beamr/src/interpreter/pattern.rs:1-8, crates/beamr/src/module.rs:1-8, docs/design/beamr/CHECKLIST.md:75-109
Approach: Define ExecError with unit variants only: Badmatch, FunctionClause, Undef, Badarith, UserExit. Derive Debug, Clone, PartialEq, Eq (avoid Copy if you want fewer future API constraints), implement Display with non-empty messages, and impl std::error::Error.
Notes: Do not create Undef { module: String, function: String, arity: usize } or UserExit(String). Unit variants satisfy placeholder requirement and avoid breaking changes when Atom/Term land.

### R7
Files: crates/beamr/src/error.rs:1-7, crates/beamr/src/loader/mod.rs:1-9, crates/beamr/src/interpreter/mod.rs:1-8, docs/design/beamr/USER-STORIES.md:51-52, docs/design/beamr/DESIGN.md:74-76
Approach: After LoadError and ExecError, add BeamrError with Load and Exec variants. Implement From by wrapping in Self::Load/Self::Exec. Implement Display delegating to wrapped errors with prefixes like “load error: {error}” and “execution error: {error}”. Implement Error, ideally with source() returning Some(error).
Notes: No top-level re-export from lib.rs is required; acceptance names beamr::error::BeamrError. R7 depends on R5 and R6.

### R8
Files: crates/beamr/src/error.rs:1-7, crates/beamr/src/atom/table.rs:1-9, crates/beamr/src/hook.rs:1-10, crates/beamr/src/loader/parser.rs:1-8, crates/beamr/src/module.rs:1-8
Approach: After implementing error.rs, fix remaining scaffold clippy warnings by converting leading file-level /// comments in leaf scaffold files to //! inner module docs, leaving placeholder functions if needed. Then rerun cargo check and cargo clippy. Do not silence clippy with allow attributes; the right fix is documentation style.
Notes: Fixing only error.rs will not satisfy R8. B-001 boundaries forbid implementing loader/interpreter/process logic, but doc-comment cleanup is necessary scaffolding hygiene for clippy.

## Verification

- cargo check --workspace
- cargo clippy --workspace -- -D warnings
- grep -R 'meridian\|yggdrasil\|norn\|libyggd' crates/beamr/Cargo.toml || true
- Verify Cargo.toml [workspace] members are exactly crates/beamr and crates/beamr-cli and resolver = "2".
- Verify crates/beamr-cli/Cargo.toml [dependencies] contains only beamr = { path = "../beamr" } and [[bin]] name = "beamr".
- Verify crates/beamr/src/lib.rs has exactly 14 pub mod declarations and no fn/struct/enum/trait/impl blocks.
- Compile a small usage or test that formats LoadError::MissingChunk("Atom".into()), ExecError::Badarith, BeamrError::from(LoadError::InvalidFormat), and BeamrError::from(ExecError::Badarith).

## Boundaries

- SHALL NOT add any third-party dependencies to beamr/Cargo.toml (dependencies like dashmap are added by the briefs that need them)
- SHALL NOT implement any logic in lib.rs beyond module declarations
- SHALL NOT add beamr-meridian to the workspace members (future brief scope)
- SHALL NOT implement loader, interpreter, or process logic — only the error type definitions they will use

Full design document: docs/design/beamr/DESIGN.md

For each R#, report: status, files changed, how satisfied, any deviation. For each C# and S# assigned to the R#, report whether delivered. Attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests pass.
