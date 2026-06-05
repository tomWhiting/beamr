//! Bytecode instruction decoder.
//!
//! Decodes the Code chunk's raw bytes into structured `Instruction`
//! values. Handles compact term encoding for operands: tagged values,
//! extended tags, literals, atoms, labels, and register references.

pub mod chunks;
mod code;
pub mod compact;
mod etf;
mod instruction;
mod opcode;

/// Maximum recursion depth for the External Term Format literal decoder.
///
/// A crafted `LitT` chunk can nest lists/tuples/maps arbitrarily deep; each
/// level costs only a couple of bytes on the wire but one native stack frame
/// to decode. Bounding the depth turns an attacker-controlled stack overflow
/// (process abort before any code runs) into an explicit `DecodeError`.
pub(crate) const MAX_ETF_DEPTH: usize = 256;

/// Hard ceiling on any length-prefixed table/collection count read from
/// untrusted bytes, independent of the payload size. Prevents an attacker
/// `u32` count from forcing a multi-gigabyte `Vec::with_capacity` before the
/// element read loop has a chance to fail on truncated input.
pub(crate) const MAX_TABLE_ENTRIES: usize = 16_777_216;

pub use chunks::{
    ExportEntry, ImportEntry, LambdaEntry, LineInfo, Literal, decode_atom_chunk,
    decode_export_chunk, decode_import_chunk, decode_lambda_chunk, decode_line_chunk,
    decode_literal_chunk, decode_string_chunk,
};
pub use code::{decode_code_chunk, decode_instructions};
pub use compact::{Allocation, Operand};
pub use instruction::{BifOp, BinaryOp, ComparisonOp, Instruction, MapOp, TypeTestOp};
