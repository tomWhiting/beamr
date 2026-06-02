---
name: rust-code-organization
description: Rust coding standards and module organization for Yggdrasil
---

## Rust Code Organization Standards

### Module Structure

- `mod.rs` contains only `pub mod` declarations and re-exports. Logic goes in named files.
- Use folder modules over flat files when a module has more than one concern.
- `lib.rs` and `main.rs` are thin entry points.
- Nothing over 500 lines of code (excluding tests, comments, whitespace). Break into sub-modules.

### Error Handling

- `thiserror` for library errors with domain-specific error types.
- `anyhow` only in the CLI crate for top-level reporting.
- Never `.unwrap()` or `.expect()` in library code.
- Mutex/lock poison always handled explicitly.

### Code Quality

- No silent failures. Every error handled, logged, or propagated.
- No partial implementations or deferred work.
- Handle all edge cases. Validate at boundaries.
- Test failure paths, not just happy paths.

### Naming and Documentation

- Every public type and function has doc comments.
- Module-level `//!` comments describe the domain.
- Follow Rust API guidelines for naming conventions.

### Linting

Strict clippy: `unsafe_code = "deny"`, pedantic enabled, warnings on `unwrap_used`/`expect_used`/`panic`/`todo`. All code must pass:

- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
