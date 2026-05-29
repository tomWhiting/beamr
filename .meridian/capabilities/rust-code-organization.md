---
name: rust-code-organization
description: Rust code organization standards — folder modules, mod.rs for re-exports only, no god files, modular crate structure. Adds guidelines for how Rust code should be organized within crates.
tools: Read, Glob, Grep
---

# Rust Code Organization

## Crate Structure

Every crate follows the same pattern:

```
crates/my-crate/
  Cargo.toml
  src/
    lib.rs          # or main.rs for binaries
    feature_a/      # folder module for each major feature
      mod.rs        # ONLY pub mod declarations and re-exports
      types.rs      # types and structs
      service.rs    # business logic
      helpers.rs    # internal utilities
      tests.rs      # unit tests (#[cfg(test)] mod tests)
    feature_b/
      mod.rs
      ...
    common.rs       # truly shared utilities (if small enough for one file)
    common/         # or a folder module if it grows
      mod.rs
      ...
  tests/            # integration tests
    feature_a.rs
```

## Rules

### mod.rs is for organization, not code

A `mod.rs` file should contain ONLY:
- `pub mod` declarations
- `pub use` re-exports for ergonomic imports
- Module-level doc comments

It should NOT contain:
- Struct/enum definitions
- Function implementations
- Trait definitions
- Constants or statics
- Anything that is "the actual code"

If you're writing logic in `mod.rs`, move it to a named file in the same directory.

**Bad:**
```rust
// feature_a/mod.rs — 2000 lines of types, logic, and helpers
pub struct Thing { ... }
impl Thing { ... }
fn helper() { ... }
```

**Good:**
```rust
// feature_a/mod.rs — 5 lines
pub mod types;
pub mod service;
mod helpers;

pub use types::Thing;
```

### No god files

No single file should exceed ~500 lines. If it does, it needs to be broken into a folder module. There are no exceptions for "it's all related" — if it's all related, it belongs in a folder module where the relationship is expressed through `mod.rs` re-exports.

Signs a file needs splitting:
- Multiple `impl` blocks for the same type with unrelated methods
- Multiple types that could stand alone
- Sections separated by comment banners (`// === SECTION ===`)
- You need to scroll to understand the file's scope
- The file has more than 3-4 top-level items (structs, enums, traits, functions)

### lib.rs / main.rs is an entry point

`lib.rs` should contain:
- `pub mod` declarations
- `pub use` re-exports for the crate's public API
- Top-level doc comments (`//!`)
- Crate-level attributes (`#![...]`)

It should NOT contain implementations, types, or logic. If you're tempted to put "just this one struct" in `lib.rs`, make a file for it.

`main.rs` for binaries follows the same pattern — declare modules, call into them. The `main()` function should be thin.

### Folder modules over flat files

Prefer folder modules over accumulating files at the `src/` root:

**Bad:**
```
src/
  lib.rs
  auth.rs           # 800 lines
  auth_middleware.rs # 400 lines
  auth_types.rs     # 300 lines
  user.rs           # 600 lines
  user_store.rs     # 500 lines
```

**Good:**
```
src/
  lib.rs
  auth/
    mod.rs           # pub mod middleware, types, service
    middleware.rs
    types.rs
    service.rs
  user/
    mod.rs
    types.rs
    store.rs
```

### Tests live close to the code

- Unit tests: `#[cfg(test)] mod tests` at the bottom of the file they test, OR in a dedicated `tests.rs` within the folder module
- Integration tests: `tests/` directory at the crate root
- Test helpers shared across tests: `tests/common/mod.rs`

### Re-export for ergonomic imports

Use `pub use` in `mod.rs` and `lib.rs` so consumers don't need to know the internal file structure:

```rust
// lib.rs
pub mod auth;
pub use auth::{AuthService, AuthToken, Middleware};
```

### When to create a new crate vs a module

Create a new crate when:
- The code has a distinct public API consumed by multiple other crates
- It could be versioned or tested independently
- It represents a clear architectural boundary (storage, engine, services)

Use a module when:
- The code is internal to a crate
- It shares types/state with sibling modules in the same crate
- It wouldn't make sense to publish or version separately
