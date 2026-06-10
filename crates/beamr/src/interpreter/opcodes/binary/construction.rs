//! Binary construction handlers: writable builders, legacy put-style
//! segments, and dispatch for the OTP 25+ `bs_create_bin` instruction.
//!
//! Decoded `bs_create_bin` operands arrive as
//! `[Fail, Alloc, Live, Unit, Dst, List]` where the list holds six operands
//! per segment; that form is handled by the [`segments`] submodule. The
//! older synthetic forms (used by unit tests and the JIT lowering) are kept
//! for compatibility.

mod segments;

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::packed_word_count;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BoxedHeader, BoxedTag};
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};

use super::super::core;
use super::matching::{Endian, segment_bits};
use super::{BUILDER_META_WORDS, boxed_tag, heap_slice, read_word, slice_from_words, write_word};

pub(super) fn bs_init_or_create(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    match operands {
        // Real `bs_init_writable` takes no operands: the size hint sits in
        // x0 and the result replaces it. The result only ever feeds a later
        // `bs_create_bin` append segment, which copies its source, so an
        // ordinary empty binary preserves the semantics.
        [] => {
            let empty = segments::empty_binary(process)?;
            process.set_x_reg(0, empty);
            Ok(InstructionOutcome::Continue)
        }
        [size, destination] => {
            let capacity = core::operand_usize(size, "binary builder size")?;
            let term = allocate_builder(process, capacity)?;
            core::write_term(process, destination, term)?;
            Ok(InstructionOutcome::Continue)
        }
        // Real compiler output: [Fail, Alloc, Live, Unit, Dst, SegmentList].
        [Operand::Label(_), _, _, _, _, Operand::List(_)] => {
            segments::bs_create_bin(process, module, operands)
        }
        [destination, size, segments @ ..] => {
            let capacity = core::operand_usize(size, "binary builder size")?;
            let builder = allocate_builder(process, capacity)?;
            for segment in segments {
                append_create_bin_segment(process, module, builder, segment)?;
            }
            let binary = finalize_builder(process, builder)?;
            core::write_term(process, destination, binary)?;
            Ok(InstructionOutcome::Continue)
        }
        _ => Err(ExecError::InvalidOperand("bs_init2 operands")),
    }
}

fn append_create_bin_segment(
    process: &mut Process,
    module: &Module,
    builder: Term,
    segment: &Operand,
) -> Result<(), ExecError> {
    let Operand::List(fields) = segment else {
        return Err(ExecError::InvalidOperand("bs_create_bin segment"));
    };
    match fields.as_slice() {
        [Operand::Atom(None), value, size, unit, flags] => {
            bs_put_integer(process, module, builder, value, size, unit, flags)
        }
        [Operand::Atom(None), source] => bs_put_binary(process, module, builder, source),
        _ => Err(ExecError::InvalidOperand("bs_create_bin segment")),
    }
}

pub(crate) fn bs_put_integer(
    process: &mut Process,
    module: &Module,
    builder: Term,
    value: &Operand,
    size: &Operand,
    unit: &Operand,
    flags: &Operand,
) -> Result<(), ExecError> {
    let value = core::read_term(process, module, value)?;
    let value = value.as_small_int().ok_or(ExecError::Badarg)?;
    let size_bits = segment_bits(size, unit)?;
    let endian = Endian::from_flags(flags);
    if size_bits == 0 || !size_bits.is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    let byte_count = size_bits / u8::BITS as usize;
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        return Err(ExecError::Badarg);
    }
    let bytes = encode_integer(value, byte_count, endian)?;
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    Ok(())
}

pub(crate) fn bs_put_binary(
    process: &mut Process,
    module: &Module,
    builder: Term,
    source: &Operand,
) -> Result<(), ExecError> {
    let source = core::read_term(process, module, source)?;
    let binary = BinaryRef::new(source).ok_or(ExecError::Badarg)?;
    let bytes = binary.as_bytes();
    let size_bits = bytes.len() * u8::BITS as usize;
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        return Err(ExecError::Badarg);
    }
    builder.write_bytes(start / u8::BITS as usize, bytes);
    builder.set_write_position_bits(start + size_bits);
    Ok(())
}

pub(crate) fn finalize_builder(process: &mut Process, builder: Term) -> Result<Term, ExecError> {
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    if !builder
        .write_position_bits()
        .is_multiple_of(u8::BITS as usize)
    {
        return Err(ExecError::Badarg);
    }
    let byte_len = builder.write_position_bits() / u8::BITS as usize;
    let bytes = builder.bytes(byte_len).ok_or(ExecError::Badarg)?;
    let words = alloc_binary_word_count(byte_len);
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    alloc_binary(heap, bytes).ok_or(ExecError::Badarg)
}

fn allocate_builder(process: &mut Process, capacity: usize) -> Result<Term, ExecError> {
    let words = BUILDER_META_WORDS
        .checked_add(packed_word_count(capacity))
        .ok_or(ExecError::InvalidOperand("binary builder size"))?;
    if process.heap().available() < words {
        return Err(ExecError::GcNeeded {
            requested: words,
            available: process.heap().available(),
        });
    }
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    heap[0] = BoxedHeader::new(BoxedTag::BinaryBuilder, words - 1);
    heap[1] = 0;
    heap[2] = capacity as u64;
    Ok(Term::boxed_ptr(heap.as_ptr()))
}

#[derive(Copy, Clone)]
pub(crate) struct BinaryBuilder {
    ptr: *mut u64,
}

impl BinaryBuilder {
    pub(crate) fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::BinaryBuilder)).then_some(Self { ptr })
    }

    pub(crate) fn write_position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }

    fn set_write_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }

    pub(crate) fn capacity_bytes(self) -> usize {
        read_word(self.ptr, 2) as usize
    }

    fn can_append(self, bits: usize) -> bool {
        self.write_position_bits()
            .checked_add(bits)
            .is_some_and(|end| end <= self.capacity_bytes() * u8::BITS as usize)
    }

    fn write_bytes(self, start: usize, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().copied().enumerate() {
            let index = start + offset;
            let word_offset = BUILDER_META_WORDS + index / std::mem::size_of::<u64>();
            let shift = (index % std::mem::size_of::<u64>()) * u8::BITS as usize;
            let mut word = read_word(self.ptr, word_offset);
            word &= !(0xff_u64 << shift);
            word |= u64::from(byte) << shift;
            write_word(self.ptr, word_offset, word);
        }
    }

    fn bytes(self, len: usize) -> Option<&'static [u8]> {
        if len > self.capacity_bytes() {
            return None;
        }
        Some(slice_from_words(self.ptr, BUILDER_META_WORDS, len))
    }
}

fn encode_integer(value: i64, byte_count: usize, endian: Endian) -> Result<Vec<u8>, ExecError> {
    if byte_count > std::mem::size_of::<i64>() {
        return Err(ExecError::Badarg);
    }
    let bits = byte_count * u8::BITS as usize;
    if bits < i64::BITS as usize && (value < 0 || (value as u64) >= (1_u64 << bits)) {
        return Err(ExecError::Badarg);
    }
    Ok(match endian {
        Endian::Big => value.to_be_bytes()[std::mem::size_of::<i64>() - byte_count..].to_vec(),
        Endian::Little => value.to_le_bytes()[..byte_count].to_vec(),
    })
}
