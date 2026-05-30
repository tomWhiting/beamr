//! .beam file loader — the front door.
//!
//! Reads compiled Gleam/Erlang modules, decodes the chunked binary format,
//! and produces parsed module data. Import resolution and module registry
//! insertion are handled by later loader stages.

pub mod decode;
pub mod load;
pub mod parser;
pub mod validate;

pub use decode::{
    ExportEntry, ImportEntry, Instruction, LambdaEntry, LineInfo, Literal, decode_instructions,
};
pub use load::{ParsedModule, load_beam_chunks};
pub use parser::{FourCC, parse_beam_chunks};
