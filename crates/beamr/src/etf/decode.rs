//! Runtime decoder for Erlang's external term format (ETF).

use std::io::{Read, Take};

use flate2::read::ZlibDecoder;

use crate::atom::AtomTable;
use crate::native::ProcessContext;
use crate::term::Term;

use super::tags;

const MAX_ETF_DEPTH: usize = 256;
const DEFAULT_MAX_HEAP_WORDS: usize = crate::process::heap::DEFAULT_MAX_HEAP_WORDS;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DecodeError {
    EmptyInput,
    BadVersion(u8),
    Truncated,
    TrailingBytes,
    UnsupportedTag(u8),
    InvalidUtf8,
    UnsafeAtom(String),
    TooDeep,
    IntegerOutOfRange,
    InvalidBigSign(u8),
    HeapAllocationFailed,
    InvalidExportFunction,
    SizeLimitExceeded,
    DecompressionFailed,
    DecompressedSizeMismatch { expected: usize, actual: usize },
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DecodeOptions {
    pub safe: bool,
    pub return_used: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DecodedTerm {
    pub term: Term,
    pub used: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RuntimeDecodeBudget {
    pub max_heap_words: usize,
}

impl RuntimeDecodeBudget {
    #[must_use]
    pub fn for_context(context: &ProcessContext<'_>) -> Self {
        let max_heap_words = context
            .process_heap()
            .map_or(DEFAULT_MAX_HEAP_WORDS, |heap| heap.max_capacity());
        Self { max_heap_words }
    }

    fn max_bytes(self) -> Result<usize, DecodeError> {
        self.max_heap_words
            .checked_mul(std::mem::size_of::<u64>())
            .ok_or(DecodeError::SizeLimitExceeded)
    }
}

pub fn decode_term(
    bytes: &[u8],
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
) -> Result<Term, DecodeError> {
    decode_term_with_options(bytes, context, atom_table, DecodeOptions::default())
        .map(|decoded| decoded.term)
}

pub fn decode_term_with_options(
    bytes: &[u8],
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
    options: DecodeOptions,
) -> Result<DecodedTerm, DecodeError> {
    let budget = RuntimeDecodeBudget::for_context(context);
    let mut cursor = Cursor::new(bytes);
    let version = cursor.read_u8()?;
    if version != tags::VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let tag = cursor.read_u8()?;
    let term = if tag == tags::COMPRESSED_EXT {
        let declared_size =
            usize::try_from(cursor.read_u32()?).map_err(|_| DecodeError::SizeLimitExceeded)?;
        let max_bytes = budget.max_bytes()?;
        if declared_size > max_bytes {
            return Err(DecodeError::SizeLimitExceeded);
        }
        let (inflated, compressed_used) = decompress_bounded(cursor.remaining(), declared_size)?;
        cursor.skip_bytes(compressed_used)?;
        let mut inflated_cursor = Cursor::new(&inflated);
        let inflated_tag = inflated_cursor.read_u8()?;
        let term = decode_after_tag(
            inflated_tag,
            &mut inflated_cursor,
            context,
            atom_table,
            options,
            0,
            budget,
        )?;
        inflated_cursor.expect_empty()?;
        term
    } else {
        decode_after_tag(tag, &mut cursor, context, atom_table, options, 0, budget)?
    };
    let used = cursor.position();
    if !options.return_used {
        cursor.expect_empty()?;
    }
    Ok(DecodedTerm { term, used })
}

fn decode_after_tag(
    tag: u8,
    cursor: &mut Cursor<'_>,
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
    options: DecodeOptions,
    depth: usize,
    budget: RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    if depth > MAX_ETF_DEPTH {
        return Err(DecodeError::TooDeep);
    }

    match tag {
        tag if tag == tags::NEW_FLOAT_EXT => {
            let bytes = cursor.read_bytes(8)?;
            let value = f64::from_bits(u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]));
            context
                .alloc_float(value)
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::SMALL_INTEGER_EXT => Ok(Term::small_int(i64::from(cursor.read_u8()?))),
        tag if tag == tags::INTEGER_EXT => decode_small_integer(i64::from(cursor.read_i32()?)),
        tag if tag == tags::ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u16()?);
            decode_atom(cursor.read_bytes(len)?, atom_table, options)
        }
        tag if tag == tags::SMALL_ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u8()?);
            decode_atom(cursor.read_bytes(len)?, atom_table, options)
        }
        tag if tag == tags::SMALL_TUPLE_EXT => {
            let arity = usize::from(cursor.read_u8()?);
            decode_tuple(cursor, arity, context, atom_table, options, depth, budget)
        }
        tag if tag == tags::LARGE_TUPLE_EXT => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, arity, context, atom_table, options, depth, budget)
        }
        tag if tag == tags::NIL_EXT => Ok(Term::NIL),
        tag if tag == tags::STRING_EXT => {
            let len = usize::from(cursor.read_u16()?);
            ensure_heap_words(
                len.checked_mul(2).ok_or(DecodeError::SizeLimitExceeded)?,
                budget,
            )?;
            let mut elements = Vec::with_capacity(len);
            for byte in cursor.read_bytes(len)? {
                elements.push(Term::small_int(i64::from(*byte)));
            }
            context
                .alloc_list(&elements)
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::LIST_EXT => {
            let len = cursor.read_u32()? as usize;
            ensure_heap_words(
                len.checked_mul(2).ok_or(DecodeError::SizeLimitExceeded)?,
                budget,
            )?;
            let mut elements = Vec::with_capacity(len);
            for _ in 0..len {
                elements.push(decode_one(
                    cursor,
                    context,
                    atom_table,
                    options,
                    depth + 1,
                    budget,
                )?);
            }
            let tail = decode_one(cursor, context, atom_table, options, depth + 1, budget)?;
            context
                .alloc_list_with_tail(&elements, tail)
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::BINARY_EXT => {
            let len = cursor.read_u32()? as usize;
            let words = packed_word_count(len)
                .and_then(|count| count.checked_add(2))
                .ok_or(DecodeError::SizeLimitExceeded)?;
            ensure_heap_words(words, budget)?;
            context
                .alloc_binary(cursor.read_bytes(len)?)
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::SMALL_BIG_EXT => {
            let len = usize::from(cursor.read_u8()?);
            decode_big_integer(cursor, len, context, budget)
        }
        tag if tag == tags::LARGE_BIG_EXT => {
            let len = cursor.read_u32()? as usize;
            decode_big_integer(cursor, len, context, budget)
        }
        tag if tag == tags::EXPORT_EXT => {
            let module = decode_one(cursor, context, atom_table, options, depth + 1, budget)?;
            let function = decode_one(cursor, context, atom_table, options, depth + 1, budget)?;
            if function.as_atom().is_none() {
                return Err(DecodeError::InvalidExportFunction);
            }
            let arity = decode_one(cursor, context, atom_table, options, depth + 1, budget)?;
            context
                .alloc_tuple(&[module, function, arity])
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::MAP_EXT => {
            let len = cursor.read_u32()? as usize;
            let words = len
                .checked_mul(2)
                .and_then(|count| count.checked_add(2))
                .ok_or(DecodeError::SizeLimitExceeded)?;
            ensure_heap_words(words, budget)?;
            let mut keys = Vec::with_capacity(len);
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                keys.push(decode_one(
                    cursor,
                    context,
                    atom_table,
                    options,
                    depth + 1,
                    budget,
                )?);
                values.push(decode_one(
                    cursor,
                    context,
                    atom_table,
                    options,
                    depth + 1,
                    budget,
                )?);
            }
            context
                .alloc_map(&keys, &values)
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
        tag if tag == tags::PID_EXT || tag == tags::NEW_PID_EXT => decode_pid(
            cursor,
            context,
            atom_table,
            options,
            depth,
            budget,
            tag == tags::NEW_PID_EXT,
        ),
        other => Err(DecodeError::UnsupportedTag(other)),
    }
}

fn decode_pid(
    cursor: &mut Cursor<'_>,
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
    options: DecodeOptions,
    depth: usize,
    budget: RuntimeDecodeBudget,
    has_creation_u32: bool,
) -> Result<Term, DecodeError> {
    let node = decode_one(cursor, context, atom_table, options, depth + 1, budget)?;
    let node = node
        .as_atom()
        .ok_or(DecodeError::UnsupportedTag(tags::NEW_PID_EXT))?;
    let id = u64::from(cursor.read_u32()?);
    let serial = u64::from(cursor.read_u32()?);
    if has_creation_u32 {
        let _creation = cursor.read_u32()?;
    } else {
        let _creation = cursor.read_u8()?;
    }
    context
        .alloc_external_pid(node, id, serial)
        .map_err(|_| DecodeError::HeapAllocationFailed)
}

fn decode_one(
    cursor: &mut Cursor<'_>,
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
    options: DecodeOptions,
    depth: usize,
    budget: RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let tag = cursor.read_u8()?;
    decode_after_tag(tag, cursor, context, atom_table, options, depth, budget)
}

fn decode_tuple(
    cursor: &mut Cursor<'_>,
    arity: usize,
    context: &mut ProcessContext<'_>,
    atom_table: &AtomTable,
    options: DecodeOptions,
    depth: usize,
    budget: RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    ensure_heap_words(
        arity.checked_add(1).ok_or(DecodeError::SizeLimitExceeded)?,
        budget,
    )?;
    let mut elements = Vec::with_capacity(arity);
    for _ in 0..arity {
        elements.push(decode_one(
            cursor,
            context,
            atom_table,
            options,
            depth + 1,
            budget,
        )?);
    }
    context
        .alloc_tuple(&elements)
        .map_err(|_| DecodeError::HeapAllocationFailed)
}

fn decode_atom(
    bytes: &[u8],
    atom_table: &AtomTable,
    options: DecodeOptions,
) -> Result<Term, DecodeError> {
    let name = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
    let atom = if options.safe {
        atom_table
            .lookup(name)
            .ok_or_else(|| DecodeError::UnsafeAtom(name.to_owned()))?
    } else {
        atom_table.intern(name)
    };
    Ok(Term::atom(atom))
}

fn decode_small_integer(value: i64) -> Result<Term, DecodeError> {
    Term::try_small_int(value).ok_or(DecodeError::IntegerOutOfRange)
}

fn decode_big_integer(
    cursor: &mut Cursor<'_>,
    len: usize,
    context: &mut ProcessContext<'_>,
    budget: RuntimeDecodeBudget,
) -> Result<Term, DecodeError> {
    let sign = cursor.read_u8()?;
    let negative = match sign {
        0 => false,
        1 => true,
        other => return Err(DecodeError::InvalidBigSign(other)),
    };
    let bytes = cursor.read_bytes(len)?;
    if len <= std::mem::size_of::<i64>() {
        let mut value: i128 = 0;
        for (shift, byte) in bytes.iter().enumerate() {
            value += i128::from(*byte) << (shift * u8::BITS as usize);
        }
        let signed = if negative { -value } else { value };
        let integer = i64::try_from(signed).map_err(|_| DecodeError::IntegerOutOfRange)?;
        if let Some(term) = Term::try_small_int(integer) {
            Ok(term)
        } else {
            let magnitude = integer.unsigned_abs();
            context
                .alloc_bigint(integer.is_negative(), &[magnitude])
                .map_err(|_| DecodeError::HeapAllocationFailed)
        }
    } else {
        let limb_count = len.div_ceil(std::mem::size_of::<u64>());
        ensure_heap_words(
            limb_count
                .checked_add(3)
                .ok_or(DecodeError::SizeLimitExceeded)?,
            budget,
        )?;
        let mut limbs = Vec::with_capacity(limb_count);
        for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
            let mut limb_bytes = [0_u8; std::mem::size_of::<u64>()];
            limb_bytes[..chunk.len()].copy_from_slice(chunk);
            limbs.push(u64::from_le_bytes(limb_bytes));
        }
        context
            .alloc_bigint(negative, &limbs)
            .map_err(|_| DecodeError::HeapAllocationFailed)
    }
}

fn ensure_heap_words(words: usize, budget: RuntimeDecodeBudget) -> Result<(), DecodeError> {
    if words > budget.max_heap_words {
        return Err(DecodeError::SizeLimitExceeded);
    }
    Ok(())
}

fn packed_word_count(bytes: usize) -> Option<usize> {
    bytes
        .checked_add(7)
        .map(|count| count / std::mem::size_of::<u64>())
}

fn decompress_bounded(bytes: &[u8], declared_size: usize) -> Result<(Vec<u8>, usize), DecodeError> {
    let limit = u64::try_from(declared_size).map_err(|_| DecodeError::SizeLimitExceeded)?;
    let decoder = ZlibDecoder::new(bytes);
    let mut reader: Take<ZlibDecoder<&[u8]>> = decoder.take(limit.saturating_add(1));
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|_| DecodeError::DecompressionFailed)?;
    let compressed_used = usize::try_from(reader.into_inner().total_in())
        .map_err(|_| DecodeError::SizeLimitExceeded)?;
    if out.len() != declared_size {
        return Err(DecodeError::DecompressedSizeMismatch {
            expected: declared_size,
            actual: out.len(),
        });
    }
    Ok((out, compressed_used))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn position(&self) -> usize {
        self.offset
    }

    fn remaining(&self) -> &'a [u8] {
        if self.offset <= self.bytes.len() {
            &self.bytes[self.offset..]
        } else {
            &[]
        }
    }

    fn skip_bytes(&mut self, len: usize) -> Result<(), DecodeError> {
        self.read_bytes(len)?;
        Ok(())
    }

    fn expect_empty(&self) -> Result<(), DecodeError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or(DecodeError::Truncated)?;
        self.offset = self.offset.checked_add(1).ok_or(DecodeError::Truncated)?;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i32(&mut self) -> Result<i32, DecodeError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.offset.checked_add(len).ok_or(DecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(DecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::etf::encode::{EncodeOptions, encode_term_with_options};
    use crate::process::Process;
    use crate::term::binary::Binary;
    use crate::term::boxed::{Tuple, write_tuple};

    fn context(process: &mut Process) -> ProcessContext<'_> {
        let mut context = ProcessContext::new();
        context.attach_process(process, 0);
        context
    }

    #[test]
    fn decodes_compressed_binary_from_encoder() {
        let atoms = AtomTable::with_common_atoms();
        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("tuple");
        let bytes = encode_term_with_options(
            tuple,
            &atoms,
            EncodeOptions {
                compression_level: Some(6),
            },
        )
        .expect("encode");
        let mut process = Process::new(1, 128);
        let mut ctx = context(&mut process);

        let decoded = decode_term(&bytes, &mut ctx, &atoms).expect("decode");
        let tuple = Tuple::new(decoded).expect("tuple result");
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
        assert_eq!(tuple.get(1), Some(Term::small_int(42)));
    }

    #[test]
    fn rejects_claimed_uncompressed_size_beyond_budget() {
        let atoms = AtomTable::with_common_atoms();
        let mut process = Process::new(1, 8);
        process.heap_mut().set_max_capacity(8);
        let mut ctx = context(&mut process);
        let bytes = [tags::VERSION, tags::COMPRESSED_EXT, 0, 0, 0, 65, 0x78, 0x9c];

        assert_eq!(
            decode_term(&bytes, &mut ctx, &atoms),
            Err(DecodeError::SizeLimitExceeded)
        );
    }

    #[test]
    fn safe_mode_rejects_novel_atoms() {
        let atoms = AtomTable::with_common_atoms();
        let mut process = Process::new(1, 64);
        let mut ctx = context(&mut process);
        let bytes = [
            tags::VERSION,
            tags::SMALL_ATOM_UTF8_EXT,
            5,
            b'n',
            b'o',
            b'v',
            b'e',
            b'l',
        ];

        assert_eq!(
            decode_term_with_options(
                &bytes,
                &mut ctx,
                &atoms,
                DecodeOptions {
                    safe: true,
                    return_used: false,
                },
            ),
            Err(DecodeError::UnsafeAtom("novel".to_owned()))
        );
    }

    #[test]
    fn export_ext_decodes_correct_function_atom() {
        let atoms = AtomTable::with_common_atoms();
        let mut process = Process::new(1, 64);
        let mut ctx = context(&mut process);
        let bytes = [
            tags::VERSION,
            tags::EXPORT_EXT,
            tags::SMALL_ATOM_UTF8_EXT,
            6,
            b'e',
            b'r',
            b'l',
            b'a',
            b'n',
            b'g',
            tags::SMALL_ATOM_UTF8_EXT,
            4,
            b's',
            b'e',
            b'l',
            b'f',
            tags::SMALL_INTEGER_EXT,
            0,
        ];

        let decoded = decode_term(&bytes, &mut ctx, &atoms).expect("decode export");

        let tuple = Tuple::new(decoded).expect("export tuple");
        assert_eq!(tuple.get(0), Some(Term::atom(atoms.intern("erlang"))));
        assert_eq!(tuple.get(1), Some(Term::atom(atoms.intern("self"))));
        assert_eq!(tuple.get(2), Some(Term::small_int(0)));
    }

    #[test]
    fn used_mode_tracks_consumed_prefix_size() {
        let atoms = AtomTable::with_common_atoms();
        let bytes = [tags::VERSION, tags::BINARY_EXT, 0, 0, 0, 3, 1, 2, 3, 99];
        let mut process = Process::new(1, 64);
        let mut ctx = context(&mut process);

        let decoded = decode_term_with_options(
            &bytes,
            &mut ctx,
            &atoms,
            DecodeOptions {
                safe: false,
                return_used: true,
            },
        )
        .expect("decode");
        assert_eq!(decoded.used, bytes.len() - 1);
        assert_eq!(
            Binary::new(decoded.term).expect("binary").as_bytes(),
            &[1, 2, 3]
        );
    }

    #[test]
    fn default_decode_rejects_trailing_bytes() {
        let atoms = AtomTable::with_common_atoms();
        let bytes = [tags::VERSION, tags::SMALL_INTEGER_EXT, 42, 99];
        let mut process = Process::new(1, 64);
        let mut ctx = context(&mut process);

        assert_eq!(
            decode_term(&bytes, &mut ctx, &atoms),
            Err(DecodeError::TrailingBytes)
        );
    }
}
