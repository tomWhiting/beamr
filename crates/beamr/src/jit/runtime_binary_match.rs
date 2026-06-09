//! Binary matching runtime helpers callable from JIT-generated code.
use super::runtime::{alloc_words, process_from_abi};
use crate::process::Process;
use crate::term::Term;
use crate::term::{
    binary_ref::BinaryRef,
    boxed::{BoxedHeader, BoxedTag, ProcBin},
};
use crate::term::{
    shared_binary::{alloc_binary, alloc_binary_word_count},
    sub_binary::{SUB_BINARY_WORDS, write_sub_binary},
};

const MATCH_CONTEXT_WORDS: usize = 4;
pub(super) const BINARY_HELPER_FAILURE: u64 = u64::MAX;

pub(crate) extern "C" fn jit_bs_start_match(process: *mut Process, binary: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let source = Term::from_raw(binary);
    let Some(binary) = BinaryRef::new(source) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(total_bits) = binary.len().checked_mul(u8::BITS as usize) else {
        return BINARY_HELPER_FAILURE;
    };
    let ptr = alloc_words(process, MATCH_CONTEXT_WORDS);
    if ptr.is_null() {
        return 0;
    }
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, MATCH_CONTEXT_WORDS) };
    heap[0] = BoxedHeader::new(BoxedTag::MatchContext, MATCH_CONTEXT_WORDS - 1);
    heap[1] = 0;
    heap[2] = total_bits as u64;
    heap[3] = source.raw();
    Term::boxed_ptr(heap.as_ptr()).raw()
}

pub(crate) extern "C" fn jit_bs_get_integer(match_ctx: u64, size_bits: u64, flags: u64) -> u64 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Ok(size_bits) = usize::try_from(size_bits) else {
        return BINARY_HELPER_FAILURE;
    };
    if !size_bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
        || !context.has_bits(size_bits)
    {
        return BINARY_HELPER_FAILURE;
    }
    let Some(bytes) = context.slice(size_bits) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(value) = decode_integer(bytes, SegmentFlags::from_raw(flags)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(term) = Term::try_small_int(value) else {
        return BINARY_HELPER_FAILURE;
    };
    context.set_position_bits(context.position_bits() + size_bits);
    term.raw()
}

pub(crate) extern "C" fn jit_bs_get_binary(
    process: *mut Process,
    match_ctx: u64,
    size_bits: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let bits = if size_bits == u64::MAX {
        context.remaining_bits()
    } else {
        let Ok(bits) = usize::try_from(size_bits) else {
            return BINARY_HELPER_FAILURE;
        };
        bits
    };
    if !bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
        || !context.has_bits(bits)
    {
        return BINARY_HELPER_FAILURE;
    }
    let Some(bytes) = context.slice(bits) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(binary) = allocate_extracted_binary(process, context, bytes, bits) else {
        return 0;
    };
    context.set_position_bits(context.position_bits() + bits);
    binary.raw()
}

pub(crate) extern "C" fn jit_bs_test_tail(match_ctx: u64, expected_bits: u64) -> u8 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return 0;
    };
    let Ok(expected_bits) = usize::try_from(expected_bits) else {
        return 0;
    };
    u8::from(context.remaining_bits() == expected_bits)
}

pub(crate) extern "C" fn jit_bs_test_unit(match_ctx: u64, unit: u64) -> u8 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return 0;
    };
    let Ok(unit) = usize::try_from(unit) else {
        return 0;
    };
    u8::from(unit != 0 && context.remaining_bits().is_multiple_of(unit))
}

pub(crate) extern "C" fn jit_bs_get_utf8(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf8)
}

pub(crate) extern "C" fn jit_bs_get_utf16(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf16)
}

pub(crate) extern "C" fn jit_bs_get_utf32(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf32)
}

#[derive(Copy, Clone)]
struct JitMatchContext {
    ptr: *mut u64,
}

impl JitMatchContext {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::MatchContext)).then_some(Self { ptr })
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
    fn source_term(self) -> Term {
        Term::from_raw(read_word(self.ptr, 3))
    }
    fn source(self) -> Option<BinaryRef> {
        BinaryRef::new(self.source_term())
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
        if !bits.is_multiple_of(u8::BITS as usize)
            || !self.position_bits().is_multiple_of(u8::BITS as usize)
        {
            return None;
        }
        let start = self.position_bits() / u8::BITS as usize;
        let len = bits / u8::BITS as usize;
        let end = start.checked_add(len)?;
        self.source()?.as_bytes().get(start..end)
    }
}

#[derive(Copy, Clone)]
pub(super) enum Endian {
    Big,
    Little,
}

impl Endian {
    pub(super) fn from_raw(flags: u64) -> Self {
        if flags & 0x02 != 0 || flags & 0x01 != 0 {
            Self::Little
        } else {
            Self::Big
        }
    }
}

#[derive(Copy, Clone)]
struct SegmentFlags {
    endian: Endian,
    signed: bool,
}

impl SegmentFlags {
    fn from_raw(flags: u64) -> Self {
        Self {
            endian: Endian::from_raw(flags),
            signed: flags & 0x04 != 0,
        }
    }
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

fn decode_integer(bytes: &[u8], flags: SegmentFlags) -> Option<i64> {
    if bytes.len() > std::mem::size_of::<i64>() {
        return None;
    }
    let msb = match flags.endian {
        Endian::Big => bytes.first(),
        Endian::Little => bytes.last(),
    };
    let negative = flags.signed && msb.is_some_and(|byte| byte & 0x80 != 0);
    let fill = if negative { 0xff_u8 } else { 0x00_u8 };
    let mut full = [fill; 8];
    match flags.endian {
        Endian::Big => full[8 - bytes.len()..].copy_from_slice(bytes),
        Endian::Little => full[..bytes.len()].copy_from_slice(bytes),
    }
    Some(match flags.endian {
        Endian::Big => u64::from_be_bytes(full) as i64,
        Endian::Little => u64::from_le_bytes(full) as i64,
    })
}

pub(super) fn allocate_binary(process: &mut Process, bytes: &[u8]) -> Option<Term> {
    let words = alloc_binary_word_count(bytes.len());
    let ptr = alloc_words(process, words);
    if ptr.is_null() {
        return None;
    }
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    alloc_binary(heap, bytes)
}

fn allocate_extracted_binary(
    process: &mut Process,
    context: JitMatchContext,
    bytes: &[u8],
    bits: usize,
) -> Option<Term> {
    let source = context.source_term();
    if ProcBin::new(source).is_some() {
        let start = context.position_bits() / u8::BITS as usize;
        let length = bits / u8::BITS as usize;
        let ptr = alloc_words(process, SUB_BINARY_WORDS);
        if ptr.is_null() {
            return None;
        }
        let heap = unsafe { std::slice::from_raw_parts_mut(ptr, SUB_BINARY_WORDS) };
        return write_sub_binary(heap, source, start, length);
    }
    allocate_binary(process, bytes)
}

fn get_utf(
    match_ctx: u64,
    flags: u64,
    decoder: fn(JitMatchContext, Endian) -> Option<(u32, usize)>,
) -> u64 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some((codepoint, bits)) = decoder(context, Endian::from_raw(flags)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(term) = Term::try_small_int(i64::from(codepoint)) else {
        return BINARY_HELPER_FAILURE;
    };
    context.set_position_bits(context.position_bits() + bits);
    term.raw()
}

fn decode_utf8(context: JitMatchContext, _endian: Endian) -> Option<(u32, usize)> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return None;
    }
    let bytes = context.slice(context.remaining_bits())?;
    let first = bytes.first().copied()?;
    let (needed, mut codepoint, min) = if first <= 0x7f {
        (1, u32::from(first), 0)
    } else if (0xc2..=0xdf).contains(&first) {
        (2, u32::from(first & 0x1f), 0x80)
    } else if (0xe0..=0xef).contains(&first) {
        (3, u32::from(first & 0x0f), 0x800)
    } else if (0xf0..=0xf4).contains(&first) {
        (4, u32::from(first & 0x07), 0x10000)
    } else {
        return None;
    };
    if bytes.len() < needed {
        return None;
    }
    for byte in &bytes[1..needed] {
        if byte & 0xc0 != 0x80 {
            return None;
        }
        codepoint = (codepoint << 6) | u32::from(byte & 0x3f);
    }
    (codepoint >= min && valid_codepoint(codepoint))
        .then_some((codepoint, needed * u8::BITS as usize))
}

fn decode_utf16(context: JitMatchContext, endian: Endian) -> Option<(u32, usize)> {
    let first = read_u16(context, 0, endian)?;
    if (0xd800..=0xdbff).contains(&first) {
        let second = read_u16(context, 2, endian)?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return None;
        }
        let codepoint =
            0x10000 + (((u32::from(first) - 0xd800) << 10) | (u32::from(second) - 0xdc00));
        valid_codepoint(codepoint).then_some((codepoint, 32))
    } else if (0xdc00..=0xdfff).contains(&first) {
        None
    } else {
        Some((u32::from(first), 16))
    }
}

fn decode_utf32(context: JitMatchContext, endian: Endian) -> Option<(u32, usize)> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) || !context.has_bits(32) {
        return None;
    }
    let bytes = context.slice(32)?;
    let codepoint = match endian {
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    };
    valid_codepoint(codepoint).then_some((codepoint, 32))
}

fn read_u16(context: JitMatchContext, byte_offset: usize, endian: Endian) -> Option<u16> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return None;
    }
    let bits = (byte_offset + 2) * u8::BITS as usize;
    if !context.has_bits(bits) {
        return None;
    }
    let bytes = context.slice(bits)?;
    let pair = [bytes[byte_offset], bytes[byte_offset + 1]];
    Some(match endian {
        Endian::Big => u16::from_be_bytes(pair),
        Endian::Little => u16::from_le_bytes(pair),
    })
}

pub(super) fn valid_codepoint(codepoint: u32) -> bool {
    codepoint <= 0x10ffff && !(0xd800..=0xdfff).contains(&codepoint)
}

pub(super) fn set_badarg(process: &mut Process) {
    process.set_current_exception(Some(crate::process::Exception {
        class: Term::atom(crate::atom::Atom::ERROR),
        reason: Term::atom(crate::atom::Atom::BADARG),
        stacktrace: Term::NIL,
    }));
}
