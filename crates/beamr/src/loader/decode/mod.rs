//! Bytecode instruction decoder.
//!
//! Decodes the Code chunk's raw bytes into structured `Instruction`
//! values. Handles compact term encoding for operands: tagged values,
//! extended tags, literals, atoms, labels, and register references.

mod bounded;
pub(crate) mod budget;
pub mod chunks;
mod code;
pub mod compact;
mod etf;
mod instruction;
mod opcode;

pub use chunks::{ExportEntry, ImportEntry, LambdaEntry, LineInfo, Literal, decode_string_chunk};
pub(crate) use chunks::{
    decode_atom_chunk, decode_export_chunk, decode_import_chunk, decode_lambda_chunk,
    decode_line_chunk, decode_literal_chunk,
};
pub use code::{decode_code_chunk, decode_instructions};
pub use compact::{Allocation, Operand};
pub use instruction::{BifOp, BinaryOp, ComparisonOp, Instruction, MapOp, TypeTestOp};
