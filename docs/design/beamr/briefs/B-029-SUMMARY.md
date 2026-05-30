---
phase: 3
plan: B-029
subsystem: native
tags: [bifs, stubs, otp, gleam_stdlib]
dependency-graph:
  requires: [B-026]
  provides: [stdlib-stubs]
  affects: [beamr-cli, native-registry]
tech-stack:
  patterns: [module-atom-registration, leaked-heap-allocation]
key-files:
  created:
    - crates/beamr/src/native/stdlib_stubs/mod.rs
    - crates/beamr/src/native/stdlib_stubs/tests.rs
  modified:
    - crates/beamr/src/native/mod.rs
    - crates/beamr-cli/src/main.rs
  deleted:
    - crates/beamr/src/native/process_bifs.rs
decisions:
  - Used leaked Box allocations for cons cells and binaries in BIFs lacking heap access
  - Structured stdlib_stubs as directory module with separate tests file
metrics:
  duration: 316s
  completed: 2026-05-30T17:13:51Z
---

# Phase 3 Plan B-029: Utility Stubs Summary

Stdlib stub BIFs registered under OTP module names (logger, unicode, sys, gleam_stdlib) with UTF-8 binary/list conversion and identity passthrough.

## Tasks Completed

| Task | Description | Commit | Key Files |
|------|-------------|--------|-----------|
| R1 | Create stdlib_stubs module and registration | fc36f1f | stdlib_stubs/mod.rs, native/mod.rs, main.rs |
| R2 | Implement all 5 stub BIFs | fc36f1f | stdlib_stubs/mod.rs |
| R3 | Unit tests for all stubs | fc36f1f | stdlib_stubs/tests.rs |

## Implementation Details

- **logger:warning/2**: Extracts binary format string via `Binary::new()`, falls back to debug formatting for non-binary terms, prints to stderr, returns `ok`
- **unicode:characters_to_binary/1**: Binary passthrough, empty list to empty binary, integer code-point list to UTF-8 binary via `char::from_u32` encoding
- **unicode:characters_to_list/1**: Decodes binary as UTF-8, builds proper cons-cell list of integer code points in reverse
- **sys:debug_options/1**: Accepts any single argument, returns `[]`
- **gleam_stdlib:identity/1**: Returns argument unchanged

BIFs are registered under non-erlang module atoms (`atom_table.intern("logger")`, etc.) via a single `register_stdlib_stubs()` call wired into the CLI load path alongside Gate 1-3 registration.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Removed stale process_bifs.rs causing E0761**
- **Found during:** Pre-build check
- **Issue:** Both `crates/beamr/src/native/process_bifs.rs` and `crates/beamr/src/native/process_bifs/mod.rs` existed, causing Rust module ambiguity error
- **Fix:** Deleted the old `process_bifs.rs` file (superseded by `process_bifs/` directory from B-024 refactor)
- **Files modified:** crates/beamr/src/native/process_bifs.rs (deleted)
- **Commit:** 79054f4

## Verification

- `cargo clippy --workspace -- -D warnings` passes
- `cargo test --workspace` passes (326 tests, 0 failures)
- All 5 MFAs resolve via `registry.lookup()` in registration tests
- No files exceed 500 lines (mod.rs: 189, tests.rs: 283)
