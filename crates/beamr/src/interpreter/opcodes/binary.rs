//! Binary construction and matching opcode handlers.

use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::loader::decode::{BinaryOp, Literal};
use crate::module::Module;
use crate::process::{CodePosition, Process};
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::boxed::{BoxedHeader, BoxedTag};
use crate::term::Term;

use super::core;

const BUILDER_META_WORDS: usize = 3;
const MATCH_CONTEXT_WORDS: usize = 4;

pub fn binary_op(
    process: &mut Process,
    module: &Module,
    op: BinaryOp,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    match op {
        BinaryOp::BsInitWritable | BinaryOp::BsCreateBin => bs_init_or_create(process, operands),
        BinaryOp::BsStartMatch3 | BinaryOp::BsStartMatch4 => bs_start_match(process, module, operands),
        BinaryOp::BsGetInteger2 => bs_get_integer(process, module, operands),
        BinaryOp::BsGetBinary2 => bs_get_binary(process, module, operands),
        BinaryOp::BsMatchString => bs_match_string(process, module, operands),
        BinaryOp::BsTestTail2 => bs_test_tail(process, module, operands),
        other => Err(ExecError::UnsupportedOpcode {
            name: binary_opcode_name(other),
        }),
    }
}

fn bs_init_or_create(
    process: &mut Process,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    match operands {
        [size, destination] => {
            let capacity = core::operand_usize(size, "binary builder size")?;
            let term = allocate_builder(process, capacity)?;
            core::write_term(process, destination, term)?;
            Ok(InstructionOutcome::Continue)
        }
        [destination, size, segments @ ..] => {
            let capacity = core::operand_usize(size, "binary builder size")?;
            let builder = allocate_builder(process, capacity)?;
            for segment in segments {
                append_create_bin_segment(process, builder, segment)?;
            }
            let binary = finalize_builder(process, builder)?;
            core::write_term(process, destination, binary)?;
            Ok(InstructionOutcome::Continue)
        }
        _ => Err(ExecError::InvalidOperand("bs_init2 operands")),
    }
}

fn bs_start_match(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, source, destination) = match operands {
        [fail, source, destination] => (fail, source, destination),
        [fail, source, _live, destination] => (fail, source, destination),
        _ => return Err(ExecError::InvalidOperand("bs_start_match operands")),
    };
    let source = core::read_term(process, source)?;
    let Some(binary) = Binary::new(source) else {
        return jump_label(module, fail);
    };

    let ptr = process
        .heap_mut()
        .alloc(MATCH_CONTEXT_WORDS)
        .map_err(ExecError::from)?;
    let heap = heap_slice(ptr, MATCH_CONTEXT_WORDS);
    heap[0] = BoxedHeader::new(BoxedTag::MatchContext, MATCH_CONTEXT_WORDS - 1);
    heap[1] = 0;
    heap[2] = (binary.len() * u8::BITS as usize) as u64;
    heap[3] = source.raw();
    core::write_term(process, destination, Term::boxed_ptr(heap.as_ptr()))?;
    Ok(InstructionOutcome::Continue)
}

fn bs_get_integer(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit, flags, destination) = parse_get_operands(operands, "bs_get_integer2")?;
    let size_bits = segment_bits(size, unit)?;
    let endian = Endian::from_flags(flags);
    let context_term = core::read_term(process, context)?;
    let context = MatchContext::new(context_term).ok_or(ExecError::Badarg)?;
    if size_bits % u8::BITS as usize != 0 || context.position_bits() % u8::BITS as usize != 0 {
        return Err(ExecError::Badarg);
    }
    if !context.has_bits(size_bits) {
        return jump_label(module, fail);
    }

    let bytes = context.slice(size_bits).ok_or(ExecError::Badarg)?;
    let value = decode_integer(bytes, endian)?;
    let term = Term::try_small_int(value).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, term)?;
    context.set_position_bits(context.position_bits() + size_bits);
    Ok(InstructionOutcome::Continue)
}

fn bs_get_binary(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit, _flags, destination) = parse_get_operands(operands, "bs_get_binary2")?;
    let size_bits = segment_bits(size, unit)?;
    let context_term = core::read_term(process, context)?;
    let context = MatchContext::new(context_term).ok_or(ExecError::Badarg)?;
    if size_bits % u8::BITS as usize != 0 || context.position_bits() % u8::BITS as usize != 0 {
        return Err(ExecError::Badarg);
    }
    if !context.has_bits(size_bits) {
        return jump_label(module, fail);
    }

    let bytes = context.slice(size_bits).ok_or(ExecError::Badarg)?;
    let words = 2 + packed_word_count(bytes.len());
    if process.heap().available() < words {
        return Err(ExecError::GcNeeded {
            requested: words,
            available: process.heap().available(),
        });
    }
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    let binary = write_binary(heap, bytes).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, binary)?;
    context.set_position_bits(context.position_bits() + size_bits);
    Ok(InstructionOutcome::Continue)
}

fn bs_match_string(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, bit_len, literal) = match operands {
        [fail, context, bit_len, literal] => (fail, context, bit_len, literal),
        _ => return Err(ExecError::InvalidOperand("bs_match_string operands")),
    };
    let bit_len = core::operand_usize(bit_len, "bs_match_string bit length")?;
    if bit_len % u8::BITS as usize != 0 {
        return Err(ExecError::Badarg);
    }
    let expected = literal_bytes(module, literal, bit_len / u8::BITS as usize)?;
    let context_term = core::read_term(process, context)?;
    let context = MatchContext::new(context_term).ok_or(ExecError::Badarg)?;
    if context.position_bits() % u8::BITS as usize != 0 || !context.has_bits(bit_len) {
        return jump_label(module, fail);
    }
    let candidate = context.slice(bit_len).ok_or(ExecError::Badarg)?;
    if candidate != expected {
        return jump_label(module, fail);
    }
    context.set_position_bits(context.position_bits() + bit_len);
    Ok(InstructionOutcome::Continue)
}

fn bs_test_tail(
    process: &Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, expected) = match operands {
        [fail, context, expected] => (fail, context, expected),
        _ => return Err(ExecError::InvalidOperand("bs_test_tail2 operands")),
    };
    let expected = core::operand_usize(expected, "bs_test_tail2 remaining bits")?;
    let context_term = core::read_term(process, context)?;
    let context = MatchContext::new(context_term).ok_or(ExecError::Badarg)?;
    if context.remaining_bits() == expected {
        Ok(InstructionOutcome::Continue)
    } else {
        jump_label(module, fail)
    }
}

fn append_create_bin_segment(
    process: &mut Process,
    builder: Term,
    segment: &Operand,
) -> Result<(), ExecError> {
    let Operand::List(fields) = segment else {
        return Err(ExecError::InvalidOperand("bs_create_bin segment"));
    };
    match fields.as_slice() {
        [Operand::Atom(None), value, size, unit, flags] => {
            bs_put_integer(process, builder, value, size, unit, flags)
        }
        [Operand::Atom(None), source] => bs_put_binary(process, builder, source),
        _ => Err(ExecError::InvalidOperand("bs_create_bin segment")),
    }
}

pub(crate) fn bs_put_integer(
    process: &mut Process,
    builder: Term,
    value: &Operand,
    size: &Operand,
    unit: &Operand,
    flags: &Operand,
) -> Result<(), ExecError> {
    let value = core::read_term(process, value)?;
    let value = value.as_small_int().ok_or(ExecError::Badarg)?;
    let size_bits = segment_bits(size, unit)?;
    let endian = Endian::from_flags(flags);
    if size_bits == 0 || size_bits % u8::BITS as usize != 0 {
        return Err(ExecError::Badarg);
    }
    let byte_count = size_bits / u8::BITS as usize;
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    let start = builder.write_position_bits();
    if start % u8::BITS as usize != 0 || !builder.can_append(size_bits) {
        return Err(ExecError::Badarg);
    }
    let bytes = encode_integer(value, byte_count, endian)?;
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    Ok(())
}

pub(crate) fn bs_put_binary(
    process: &mut Process,
    builder: Term,
    source: &Operand,
) -> Result<(), ExecError> {
    let source = core::read_term(process, source)?;
    let binary = Binary::new(source).ok_or(ExecError::Badarg)?;
    let bytes = binary.as_bytes();
    let size_bits = bytes.len() * u8::BITS as usize;
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    let start = builder.write_position_bits();
    if start % u8::BITS as usize != 0 || !builder.can_append(size_bits) {
        return Err(ExecError::Badarg);
    }
    builder.write_bytes(start / u8::BITS as usize, bytes);
    builder.set_write_position_bits(start + size_bits);
    Ok(())
}

pub(crate) fn finalize_builder(
    process: &mut Process,
    builder: Term,
) -> Result<Term, ExecError> {
    let builder = BinaryBuilder::new(builder).ok_or(ExecError::Badarg)?;
    if builder.write_position_bits() % u8::BITS as usize != 0 {
        return Err(ExecError::Badarg);
    }
    let byte_len = builder.write_position_bits() / u8::BITS as usize;
    let bytes = builder.bytes(byte_len).ok_or(ExecError::Badarg)?;
    let words = 2 + packed_word_count(byte_len);
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    write_binary(heap, bytes).ok_or(ExecError::Badarg)
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
struct BinaryBuilder {
    ptr: *mut u64,
}

impl BinaryBuilder {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        if boxed_tag(ptr) == Some(BoxedTag::BinaryBuilder) {
            Some(Self { ptr })
        } else {
            None
        }
    }

    fn write_position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }

    fn set_write_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }

    fn capacity_bytes(self) -> usize {
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

#[derive(Copy, Clone)]
struct MatchContext {
    ptr: *mut u64,
}

impl MatchContext {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        if boxed_tag(ptr) == Some(BoxedTag::MatchContext) {
            Some(Self { ptr })
        } else {
            None
        }
    }

    fn position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }

    fn set_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }

    fn total_bits(self) -> usize {
        read_word(self.ptr, 2) as usize
    }

    fn source(self) -> Option<Binary> {
        Binary::new(Term::from_raw(read_word(self.ptr, 3)))
    }

    fn remaining_bits(self) -> usize {
        self.total_bits().saturating_sub(self.position_bits())
    }

    fn has_bits(self, bits: usize) -> bool {
        self.position_bits()
            .checked_add(bits)
            .is_some_and(|end| end <= self.total_bits())
    }

    fn slice(self, bits: usize) -> Option<&'static [u8]> {
        if !bits.is_multiple_of(u8::BITS as usize) || !self.position_bits().is_multiple_of(u8::BITS as usize) {
            return None;
        }
        let start = self.position_bits() / u8::BITS as usize;
        let len = bits / u8::BITS as usize;
        let bytes = self.source()?.as_bytes();
        bytes.get(start..start + len)
    }
}

#[derive(Copy, Clone)]
enum Endian {
    Big,
    Little,
}

impl Endian {
    fn from_flags(flags: &Operand) -> Self {
        match flags {
            Operand::Unsigned(1) | Operand::Integer(1) => Self::Little,
            Operand::List(items) if items.iter().any(is_little_flag) => Self::Little,
            _ => Self::Big,
        }
    }
}

fn parse_get_operands<'a>(
    operands: &'a [Operand],
    context: &'static str,
) -> Result<(&'a Operand, &'a Operand, &'a Operand, &'a Operand, &'a Operand, &'a Operand), ExecError>
{
    match operands {
        [fail, match_context, _live, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        [fail, match_context, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        _ => Err(ExecError::InvalidOperand(context)),
    }
}

fn is_little_flag(flag: &Operand) -> bool {
    matches!(flag, Operand::Unsigned(1) | Operand::Integer(1))
}

fn segment_bits(size: &Operand, unit: &Operand) -> Result<usize, ExecError> {
    let size = core::operand_usize(size, "segment size")?;
    let unit = core::operand_usize(unit, "segment unit")?;
    size.checked_mul(unit)
        .ok_or(ExecError::InvalidOperand("segment size"))
}

fn encode_integer(value: i64, byte_count: usize, endian: Endian) -> Result<Vec<u8>, ExecError> {
    if byte_count > std::mem::size_of::<i64>() {
        return Err(ExecError::Badarg);
    }
    let bits = byte_count * u8::BITS as usize;
    if bits < i64::BITS as usize && (value < 0 || (value as u64) >= (1_u64 << bits)) {
        return Err(ExecError::Badarg);
    }
    let bytes = match endian {
        Endian::Big => value.to_be_bytes()[std::mem::size_of::<i64>() - byte_count..].to_vec(),
        Endian::Little => value.to_le_bytes()[..byte_count].to_vec(),
    };
    Ok(bytes)
}

fn decode_integer(bytes: &[u8], endian: Endian) -> Result<i64, ExecError> {
    if bytes.len() > std::mem::size_of::<i64>() {
        return Err(ExecError::Badarg);
    }
    let mut full = [0_u8; 8];
    match endian {
        Endian::Big => full[8 - bytes.len()..].copy_from_slice(bytes),
        Endian::Little => full[..bytes.len()].copy_from_slice(bytes),
    }
    Ok(match endian {
        Endian::Big => u64::from_be_bytes(full) as i64,
        Endian::Little => u64::from_le_bytes(full) as i64,
    })
}

fn literal_bytes<'a>(
    module: &'a Module,
    operand: &'a Operand,
    byte_len: usize,
) -> Result<&'a [u8], ExecError> {
    match operand {
        Operand::Literal(Literal::Binary(bytes) | Literal::String(bytes)) => bytes
            .get(..byte_len)
            .filter(|bytes| bytes.len() == byte_len)
            .ok_or(ExecError::Badarg),
        offset => {
            let offset = core::operand_usize(offset, "string table offset")?;
            module
                .string_table
                .get(offset..offset + byte_len)
                .ok_or(ExecError::Badarg)
        }
    }
}

fn jump_label(module: &Module, label: &Operand) -> Result<InstructionOutcome, ExecError> {
    let label = core::operand_label(label)?;
    Ok(InstructionOutcome::Jump(CodePosition {
        module: module.name,
        instruction_pointer: core::label_ip(module, label)?,
    }))
}

fn binary_opcode_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::BsGetFloat2 => "bs_get_float2",
        BinaryOp::BsSkipBits2 => "bs_skip_bits2",
        BinaryOp::BsTestUnit => "bs_test_unit",
        BinaryOp::BsGetUtf8 => "bs_get_utf8",
        BinaryOp::BsSkipUtf8 => "bs_skip_utf8",
        BinaryOp::BsGetUtf16 => "bs_get_utf16",
        BinaryOp::BsSkipUtf16 => "bs_skip_utf16",
        BinaryOp::BsGetUtf32 => "bs_get_utf32",
        BinaryOp::BsSkipUtf32 => "bs_skip_utf32",
        BinaryOp::BsGetTail => "bs_get_tail",
        BinaryOp::BsGetPosition => "bs_get_position",
        BinaryOp::BsSetPosition => "bs_set_position",
        BinaryOp::BsMatch => "bs_match",
        _ => "binary_op",
    }
}

fn boxed_tag(ptr: *const u64) -> Option<BoxedTag> {
    BoxedHeader::tag(read_word(ptr.cast_mut(), 0))
}

fn read_word(ptr: *mut u64, offset: usize) -> u64 {
    // SAFETY: callers construct these accessors only from live boxed heap terms
    // with a known layout and then read in-bounds metadata/data words.
    unsafe { *ptr.add(offset) }
}

fn write_word(ptr: *mut u64, offset: usize, value: u64) {
    // SAFETY: callers construct these accessors only from live mutable process
    // heap objects and write in-bounds metadata/data words.
    unsafe { *ptr.add(offset) = value }
}

fn slice_from_words(ptr: *const u64, word_offset: usize, len: usize) -> &'static [u8] {
    // SAFETY: inline data starts at `word_offset`; callers have checked that
    // `len` stays within the object's capacity. The returned slice is borrowed
    // only while the process heap object is live.
    unsafe { std::slice::from_raw_parts(ptr.add(word_offset).cast::<u8>(), len) }
}

fn heap_slice<'a>(ptr: *mut u64, words: usize) -> &'a mut [u64] {
    // SAFETY: `Heap::alloc(words)` returned a unique allocation with exactly
    // `words` contiguous words that this handler immediately initialises.
    unsafe { std::slice::from_raw_parts_mut(ptr, words) }
}

#[cfg(test)]
mod tests;
