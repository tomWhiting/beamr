//! Opcode dispatch table.
//!
//! Maps BEAM opcode numbers to handler functions. Organised by
//! instruction category: control flow (call, return, jump),
//! data movement (move, put_list, put_tuple), allocation
//! (allocate, test_heap), comparison and guards (is_eq, is_lt),
//! and binary matching. Only opcodes Gleam emits are implemented;
//! unknown opcodes produce an explicit error naming the opcode.

pub(crate) fn _scaffold() {}
