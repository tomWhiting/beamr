//! Binary construction runtime helpers callable from JIT-generated code.
use super::runtime::{alloc_words, process_from_abi};
use super::runtime_binary_match::{
    Endian, allocate_binary, boxed_tag, read_word, set_badarg, valid_codepoint, write_word,
};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::packed_word_count;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BoxedHeader, BoxedTag};

const BUILDER_META_WORDS: usize = 3;

pub(crate) extern "C" fn jit_bs_init(process: *mut Process, size_hint: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Ok(capacity) = usize::try_from(size_hint) else {
        return 0;
    };
    let Some(words) = BUILDER_META_WORDS.checked_add(packed_word_count(capacity)) else {
        return 0;
    };
    let ptr = alloc_words(process, words);
    if ptr.is_null() {
        return 0;
    }
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    heap[0] = BoxedHeader::new(BoxedTag::BinaryBuilder, words - 1);
    heap[1] = 0;
    heap[2] = capacity as u64;
    Term::boxed_ptr(heap.as_ptr()).raw()
}

pub(crate) extern "C" fn jit_bs_put_integer(
    process: *mut Process,
    builder: u64,
    value: u64,
    size_bits: u64,
    flags: u64,
) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(value) = Term::from_raw(value).as_small_int() else {
        set_badarg(process);
        return 1;
    };
    let Ok(size_bits) = usize::try_from(size_bits) else {
        set_badarg(process);
        return 1;
    };
    if size_bits == 0
        || !size_bits.is_multiple_of(u8::BITS as usize)
        || !builder
            .write_position_bits()
            .is_multiple_of(u8::BITS as usize)
        || !builder.can_append(size_bits)
    {
        set_badarg(process);
        return 1;
    }
    let byte_count = size_bits / u8::BITS as usize;
    let Some(bytes) = encode_integer(value, byte_count, Endian::from_raw(flags)) else {
        set_badarg(process);
        return 1;
    };
    let start = builder.write_position_bits();
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

pub(crate) extern "C" fn jit_bs_put_binary(process: *mut Process, builder: u64, source: u64) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(binary) = BinaryRef::new(Term::from_raw(source)) else {
        set_badarg(process);
        return 1;
    };
    let bytes = binary.as_bytes();
    let size_bits = bytes.len() * u8::BITS as usize;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        set_badarg(process);
        return 1;
    }
    builder.write_bytes(start / u8::BITS as usize, bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

pub(crate) extern "C" fn jit_bs_put_utf8(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
) -> u8 {
    put_utf(process, builder, codepoint, encode_utf8)
}

pub(crate) extern "C" fn jit_bs_put_utf16(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    flags: u64,
) -> u8 {
    put_utf(process, builder, codepoint, |codepoint, out| {
        encode_utf16(codepoint, Endian::from_raw(flags), out)
    })
}

pub(crate) extern "C" fn jit_bs_put_utf32(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    flags: u64,
) -> u8 {
    put_utf(process, builder, codepoint, |codepoint, out| {
        encode_utf32(codepoint, Endian::from_raw(flags), out)
    })
}

pub(crate) extern "C" fn jit_bs_finish(process: *mut Process, builder: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 0;
    };
    if !builder
        .write_position_bits()
        .is_multiple_of(u8::BITS as usize)
    {
        set_badarg(process);
        return 0;
    }
    let byte_len = builder.write_position_bits() / u8::BITS as usize;
    let Some(bytes) = builder.bytes(byte_len).map(<[u8]>::to_vec) else {
        set_badarg(process);
        return 0;
    };
    let Some(binary) = allocate_binary(process, &bytes) else {
        return 0;
    };
    binary.raw()
}

#[derive(Copy, Clone)]
struct JitBinaryBuilder {
    ptr: *mut u64,
}

impl JitBinaryBuilder {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::BinaryBuilder)).then_some(Self { ptr })
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
        Some(unsafe {
            std::slice::from_raw_parts(self.ptr.add(BUILDER_META_WORDS).cast::<u8>(), len)
        })
    }
}

fn encode_integer(value: i64, byte_count: usize, endian: Endian) -> Option<Vec<u8>> {
    if byte_count > std::mem::size_of::<i64>() {
        return None;
    }
    let bits = byte_count * u8::BITS as usize;
    if bits < i64::BITS as usize && (value < 0 || (value as u64) >= (1_u64 << bits)) {
        return None;
    }
    Some(match endian {
        Endian::Big => value.to_be_bytes()[std::mem::size_of::<i64>() - byte_count..].to_vec(),
        Endian::Little => value.to_le_bytes()[..byte_count].to_vec(),
    })
}

fn put_utf(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    encoder: impl FnOnce(u32, &mut Vec<u8>) -> bool,
) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(codepoint) = Term::from_raw(codepoint).as_small_int() else {
        set_badarg(process);
        return 1;
    };
    let Ok(codepoint) = u32::try_from(codepoint) else {
        set_badarg(process);
        return 1;
    };
    if !valid_codepoint(codepoint) {
        set_badarg(process);
        return 1;
    }
    let mut bytes = Vec::with_capacity(4);
    if !encoder(codepoint, &mut bytes) {
        set_badarg(process);
        return 1;
    }
    let size_bits = bytes.len() * u8::BITS as usize;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        set_badarg(process);
        return 1;
    }
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

fn encode_utf8(codepoint: u32, out: &mut Vec<u8>) -> bool {
    let Some(character) = char::from_u32(codepoint) else {
        return false;
    };
    let mut buffer = [0_u8; 4];
    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    true
}

fn encode_utf16(codepoint: u32, endian: Endian, out: &mut Vec<u8>) -> bool {
    let Some(character) = char::from_u32(codepoint) else {
        return false;
    };
    let mut units = [0_u16; 2];
    for unit in character.encode_utf16(&mut units) {
        let bytes = match endian {
            Endian::Big => unit.to_be_bytes(),
            Endian::Little => unit.to_le_bytes(),
        };
        out.extend_from_slice(&bytes);
    }
    true
}

fn encode_utf32(codepoint: u32, endian: Endian, out: &mut Vec<u8>) -> bool {
    if !valid_codepoint(codepoint) {
        return false;
    }
    let bytes = match endian {
        Endian::Big => codepoint.to_be_bytes(),
        Endian::Little => codepoint.to_le_bytes(),
    };
    out.extend_from_slice(&bytes);
    true
}
