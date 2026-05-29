---
type: design
cluster: test-workflow
title: Version Module
---

# Version Module — Design

## Intention

Every crate in the workspace should expose its version at runtime. This is
a deliberately trivial cluster used to validate the orchestrated-dev
workflow mechanics.

## Problem

The diagnostics crate has no way to report its own version at runtime.
Error messages and debug output would benefit from including the crate
version.

## Solution

Add a `version` module to the diagnostics crate that exports a `VERSION`
constant derived from Cargo.toml via `env!("CARGO_PKG_VERSION")`. Provide
a `version_info()` function that returns a formatted string including the
crate name and version.

Decision D1: Use `env!("CARGO_PKG_VERSION")` rather than a hardcoded
string. Rejected hardcoding because it drifts from Cargo.toml on every
release.

Decision D2: Single file, not a module directory. A version module is 10
lines — a directory would be over-engineering.

## Goals

- diagnostics crate exposes version at runtime
- Version string derived from Cargo.toml, not hardcoded

## Non-Goals

- Not adding version modules to other crates (future work)
- Not wiring version into CLI output (separate brief)

## Structure

```
crates/diagnostics/
└── src/
    ├── lib.rs          — add pub mod version
    └── version.rs      — VERSION const + version_info()
```

## Constraints

- No new dependencies.
- No changes to existing public API.
