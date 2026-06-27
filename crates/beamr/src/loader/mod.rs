//! .beam file loader — the front door.
//!
//! Reads compiled Gleam/Erlang modules, decodes the chunked binary format,
//! and produces parsed module data. Import resolution and module registry
//! insertion are handled by later loader stages.

pub mod decode;
// Embedded-archive support compresses `.beam` modules with zstd; that dependency
// (and the C `zstd-sys` build) only exists under the `embedded` feature and does
// not build for wasm32.
#[cfg(feature = "embedded")]
pub mod embed;
pub mod load;
pub mod parser;
pub mod validate;

#[cfg(feature = "jit")]
pub use crate::jit::aot::load_companion_into_cache;
pub use decode::{
    ExportEntry, ImportEntry, Instruction, LambdaEntry, LineInfo, Literal, decode_instructions,
};
#[cfg(feature = "embedded")]
pub use embed::{
    EmbedError, EmbeddedArchive, archive_pack, embedded_archive, embedded_module_bytes,
    embedded_module_names, load_embedded_module, load_embedded_module_with_policy,
};
pub use load::{
    DeniedImportEntry, ParsedModule, UnresolvedImport, UnresolvedImportEntry,
    UnresolvedImportReport, lambda_unique_id, load_beam_chunks, load_module,
    load_module_with_origin, load_module_with_origin_and_policy, load_module_with_policy,
    prepare_module, prepare_module_with_origin, prepare_module_with_origin_and_policy,
    prepare_module_with_policy, resolve_imports,
};
pub use parser::{FourCC, parse_beam_chunks};
