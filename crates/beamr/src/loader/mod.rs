//! .beam file loader — the front door.
//!
//! Reads compiled Gleam modules (produced by `gleam build` + `erlc`),
//! decodes the chunked binary format, resolves imports against loaded
//! modules and the BIF registry, and produces runnable `Module` values.
//! Unresolved imports become the demand-driven work queue (the leash).
pub mod decode;
pub mod parser;
pub mod validate;
