//! Instruction operand validation.
//!
//! After decoding, validates that instruction operands are well-formed:
//! register indices within range, label targets exist in the code,
//! arities match function signatures, and atom indices resolve in the
//! atom table. Invalid instructions produce actionable error messages
//! naming the instruction and the specific operand that failed.

pub(crate) fn _scaffold() {}
