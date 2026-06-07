//! Binary construction and matching opcode handlers.

mod construction;
mod matching;
mod utf;

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::{CodePosition, Process};
use crate::term::boxed::{BoxedHeader, BoxedTag};

use super::core;

#[cfg(test)]
pub(crate) use construction::{BinaryBuilder, bs_put_binary, bs_put_integer, finalize_builder};
#[cfg(test)]
pub(crate) use matching::{Endian, MatchContext, SegmentFlags, decode_integer};

const BUILDER_META_WORDS: usize = 3;
const MATCH_CONTEXT_WORDS: usize = 4;

pub fn binary_op(
    process: &mut Process,
    module: &Module,
    op: BinaryOp,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    match op {
        BinaryOp::BsInitWritable | BinaryOp::BsCreateBin => {
            construction::bs_init_or_create(process, module, operands)
        }
        BinaryOp::BsStartMatch3 | BinaryOp::BsStartMatch4 => {
            matching::bs_start_match(process, module, operands)
        }
        BinaryOp::BsGetInteger2 => matching::bs_get_integer(process, module, operands),
        BinaryOp::BsGetFloat2 => matching::bs_get_float(process, module, operands),
        BinaryOp::BsGetBinary2 => matching::bs_get_binary(process, module, operands),
        BinaryOp::BsSkipBits2 => matching::bs_skip_bits(process, module, operands),
        BinaryOp::BsTestTail2 => matching::bs_test_tail(process, module, operands),
        BinaryOp::BsTestUnit => matching::bs_test_unit(process, module, operands),
        BinaryOp::BsMatchString => matching::bs_match_string(process, module, operands),
        BinaryOp::BsGetUtf8 => utf::bs_get_utf8(process, module, operands),
        BinaryOp::BsSkipUtf8 => utf::bs_skip_utf8(process, module, operands),
        BinaryOp::BsGetUtf16 => utf::bs_get_utf16(process, module, operands),
        BinaryOp::BsSkipUtf16 => utf::bs_skip_utf16(process, module, operands),
        BinaryOp::BsGetUtf32 => utf::bs_get_utf32(process, module, operands),
        BinaryOp::BsSkipUtf32 => utf::bs_skip_utf32(process, module, operands),
        BinaryOp::BsGetTail => matching::bs_get_tail(process, module, operands),
        BinaryOp::BsGetPosition => matching::bs_get_position(process, module, operands),
        BinaryOp::BsSetPosition => matching::bs_set_position(process, module, operands),
        BinaryOp::BsMatch => matching::bs_match(process, module, operands),
    }
}

pub(super) fn jump_label(
    module: &Module,
    label: &Operand,
) -> Result<InstructionOutcome, ExecError> {
    let label = core::operand_label(label)?;
    Ok(InstructionOutcome::Jump(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, label)?,
    }))
}

pub(super) fn boxed_tag(ptr: *const u64) -> Option<BoxedTag> {
    BoxedHeader::tag(read_word(ptr.cast_mut(), 0))
}

pub(super) fn read_word(ptr: *mut u64, offset: usize) -> u64 {
    unsafe { *ptr.add(offset) }
}

pub(super) fn write_word(ptr: *mut u64, offset: usize, value: u64) {
    unsafe { *ptr.add(offset) = value }
}

pub(super) fn slice_from_words(ptr: *const u64, word_offset: usize, len: usize) -> &'static [u8] {
    unsafe { std::slice::from_raw_parts(ptr.add(word_offset).cast::<u8>(), len) }
}

pub(super) fn heap_slice<'a>(ptr: *mut u64, words: usize) -> &'a mut [u64] {
    unsafe { std::slice::from_raw_parts_mut(ptr, words) }
}

#[cfg(test)]
mod tests;
