//! Encoder for Erlang's external term format (ETF).

use crate::atom::{Atom, AtomTable};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BigInt, Closure, Cons, Float, Map, Reference, Tuple};
use crate::term::{Tag, Term};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

use super::tags;

const MAX_ETF_DEPTH: usize = 256;
const NONODE_NOHOST: &str = "nonode@nohost";
const IOVEC_BINARY_REFERENCE_THRESHOLD: usize = 64;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EncodeError {
    UnsupportedTerm,
    AtomResolveFailed,
    TooDeep,
    CompressionFailed,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct EncodeOptions {
    pub compression_level: Option<u32>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum IoSegment {
    Owned(Vec<u8>),
    Reference(Term),
}

pub fn encode_term(term: Term, atom_table: &AtomTable) -> Result<Vec<u8>, EncodeError> {
    let mut out = vec![tags::VERSION];
    encode_term_inner(term, atom_table, &mut out, 0)?;
    Ok(out)
}

pub fn encode_term_iovec(
    term: Term,
    atom_table: &AtomTable,
) -> Result<Vec<IoSegment>, EncodeError> {
    let mut out = SegmentBuilder::new();
    out.bytes().push(tags::VERSION);
    encode_term_iovec_inner(term, atom_table, &mut out, 0)?;
    Ok(out.finish())
}

pub fn encode_term_with_options(
    term: Term,
    atom_table: &AtomTable,
    options: EncodeOptions,
) -> Result<Vec<u8>, EncodeError> {
    let uncompressed = encode_term(term, atom_table)?;
    let Some(level) = options.compression_level else {
        return Ok(uncompressed);
    };
    if level == 0 {
        return Ok(uncompressed);
    }

    let payload = uncompressed.get(1..).ok_or(EncodeError::UnsupportedTerm)?;
    let uncompressed_size =
        u32::try_from(payload.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder
        .write_all(payload)
        .map_err(|_| EncodeError::CompressionFailed)?;
    let compressed_payload = encoder
        .finish()
        .map_err(|_| EncodeError::CompressionFailed)?;
    let mut compressed = Vec::with_capacity(6 + compressed_payload.len());
    compressed.push(tags::VERSION);
    compressed.push(tags::COMPRESSED_EXT);
    compressed.extend_from_slice(&uncompressed_size.to_be_bytes());
    compressed.extend_from_slice(&compressed_payload);
    if compressed.len() < uncompressed.len() {
        Ok(compressed)
    } else {
        Ok(uncompressed)
    }
}

fn encode_term_inner(
    term: Term,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    if depth > MAX_ETF_DEPTH {
        return Err(EncodeError::TooDeep);
    }

    match term.tag() {
        Tag::SmallInt => encode_integer(
            term.as_small_int().ok_or(EncodeError::UnsupportedTerm)?,
            out,
        ),
        Tag::Atom => encode_atom(
            term.as_atom().ok_or(EncodeError::UnsupportedTerm)?,
            atom_table,
            out,
        ),
        Tag::Pid => encode_pid(term.as_pid().ok_or(EncodeError::UnsupportedTerm)?, out),
        Tag::Nil => {
            out.push(tags::NIL_EXT);
            Ok(())
        }
        Tag::List => encode_list(term, atom_table, out, depth),
        Tag::Boxed => encode_boxed(term, atom_table, out, depth),
    }
}

fn encode_integer(value: i64, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    if let Ok(byte) = u8::try_from(value) {
        out.push(tags::SMALL_INTEGER_EXT);
        out.push(byte);
    } else if let Ok(integer) = i32::try_from(value) {
        out.push(tags::INTEGER_EXT);
        out.extend_from_slice(&integer.to_be_bytes());
    } else {
        encode_i64_big(value, out)?;
    }
    Ok(())
}

fn encode_i64_big(value: i64, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let negative = value.is_negative();
    let magnitude = value.unsigned_abs();
    encode_big_bytes(negative, magnitude.to_le_bytes().as_slice(), out)
}

fn encode_bigint(bigint: BigInt, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut bytes = Vec::with_capacity(bigint.limb_count() * std::mem::size_of::<u64>());
    for limb in bigint.limbs() {
        bytes.extend_from_slice(&limb.to_le_bytes());
    }
    encode_big_bytes(bigint.is_negative(), &bytes, out)
}

fn encode_big_bytes(negative: bool, bytes: &[u8], out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let trimmed_len = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(1, |index| index + 1);
    let magnitude = &bytes[..trimmed_len];

    if let Ok(length) = u8::try_from(magnitude.len()) {
        out.push(tags::SMALL_BIG_EXT);
        out.push(length);
    } else {
        let length = u32::try_from(magnitude.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
        out.push(tags::LARGE_BIG_EXT);
        out.extend_from_slice(&length.to_be_bytes());
    }
    out.push(u8::from(negative));
    out.extend_from_slice(magnitude);
    Ok(())
}

fn encode_atom(atom: Atom, atom_table: &AtomTable, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let name = atom_table
        .resolve(atom)
        .ok_or(EncodeError::AtomResolveFailed)?;
    encode_atom_name(name, out)
}

fn encode_atom_name(name: &str, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let bytes = name.as_bytes();
    if let Ok(length) = u8::try_from(bytes.len()) {
        out.push(tags::SMALL_ATOM_UTF8_EXT);
        out.push(length);
    } else {
        let length = u16::try_from(bytes.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
        out.push(tags::ATOM_UTF8_EXT);
        out.extend_from_slice(&length.to_be_bytes());
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn encode_pid(pid: u64, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    out.push(tags::NEW_PID_EXT);
    encode_atom_name(NONODE_NOHOST, out)?;
    let id = u32::try_from(pid).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    Ok(())
}

fn encode_reference(reference: Reference, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    out.push(tags::NEWER_REFERENCE_EXT);
    out.extend_from_slice(&2_u16.to_be_bytes());
    encode_atom_name(NONODE_NOHOST, out)?;
    out.extend_from_slice(&0_u32.to_be_bytes());
    let id = reference.id();
    out.extend_from_slice(&((id >> u32::BITS) as u32).to_be_bytes());
    out.extend_from_slice(&(id as u32).to_be_bytes());
    Ok(())
}

struct SegmentBuilder {
    segments: Vec<IoSegment>,
    current: Vec<u8>,
}

impl SegmentBuilder {
    fn new() -> Self {
        Self {
            segments: Vec::new(),
            current: Vec::new(),
        }
    }

    fn bytes(&mut self) -> &mut Vec<u8> {
        &mut self.current
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.segments
                .push(IoSegment::Owned(std::mem::take(&mut self.current)));
        }
    }

    fn push_reference(&mut self, term: Term) {
        self.flush();
        self.segments.push(IoSegment::Reference(term));
    }

    fn finish(mut self) -> Vec<IoSegment> {
        self.flush();
        self.segments
    }
}

fn encode_term_iovec_inner(
    term: Term,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    if depth > MAX_ETF_DEPTH {
        return Err(EncodeError::TooDeep);
    }

    match term.tag() {
        Tag::SmallInt => encode_integer(
            term.as_small_int().ok_or(EncodeError::UnsupportedTerm)?,
            out.bytes(),
        ),
        Tag::Atom => encode_atom(
            term.as_atom().ok_or(EncodeError::UnsupportedTerm)?,
            atom_table,
            out.bytes(),
        ),
        Tag::Pid => encode_pid(
            term.as_pid().ok_or(EncodeError::UnsupportedTerm)?,
            out.bytes(),
        ),
        Tag::Nil => {
            out.bytes().push(tags::NIL_EXT);
            Ok(())
        }
        Tag::List => encode_list_iovec(term, atom_table, out, depth),
        Tag::Boxed => encode_boxed_iovec(term, atom_table, out, depth),
    }
}

fn encode_boxed_iovec(
    term: Term,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    if let Some(float) = Float::new(term) {
        out.bytes().push(tags::NEW_FLOAT_EXT);
        out.bytes()
            .extend_from_slice(&float.value().to_bits().to_be_bytes());
        return Ok(());
    }
    if let Some(tuple) = Tuple::new(term) {
        return encode_tuple_iovec(tuple, atom_table, out, depth);
    }
    if let Some(binary) = BinaryRef::new(term) {
        return encode_binary_iovec(term, binary, out);
    }
    if let Some(map) = Map::new(term) {
        return encode_map_iovec(map, atom_table, out, depth);
    }
    if let Some(bigint) = BigInt::new(term) {
        return encode_bigint(bigint, out.bytes());
    }
    if let Some(reference) = Reference::new(term) {
        return encode_reference(reference, out.bytes());
    }
    if let Some(closure) = Closure::new(term) {
        return encode_closure_iovec(closure, atom_table, out, depth);
    }
    Err(EncodeError::UnsupportedTerm)
}

fn encode_boxed(
    term: Term,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    if let Some(float) = Float::new(term) {
        out.push(tags::NEW_FLOAT_EXT);
        out.extend_from_slice(&float.value().to_bits().to_be_bytes());
        return Ok(());
    }
    if let Some(tuple) = Tuple::new(term) {
        return encode_tuple(tuple, atom_table, out, depth);
    }
    if let Some(binary) = BinaryRef::new(term) {
        return encode_binary(binary, out);
    }
    if let Some(map) = Map::new(term) {
        return encode_map(map, atom_table, out, depth);
    }
    if let Some(bigint) = BigInt::new(term) {
        return encode_bigint(bigint, out);
    }
    if let Some(reference) = Reference::new(term) {
        return encode_reference(reference, out);
    }
    if let Some(closure) = Closure::new(term) {
        return encode_closure(closure, atom_table, out, depth);
    }
    Err(EncodeError::UnsupportedTerm)
}

fn encode_tuple(
    tuple: Tuple,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    let arity = tuple.arity();
    if let Ok(small_arity) = u8::try_from(arity) {
        out.push(tags::SMALL_TUPLE_EXT);
        out.push(small_arity);
    } else {
        let large_arity = u32::try_from(arity).map_err(|_| EncodeError::UnsupportedTerm)?;
        out.push(tags::LARGE_TUPLE_EXT);
        out.extend_from_slice(&large_arity.to_be_bytes());
    }

    for index in 0..arity {
        let element = tuple.get(index).ok_or(EncodeError::UnsupportedTerm)?;
        encode_term_inner(element, atom_table, out, depth + 1)?;
    }
    Ok(())
}

fn encode_tuple_iovec(
    tuple: Tuple,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    let arity = tuple.arity();
    if let Ok(small_arity) = u8::try_from(arity) {
        out.bytes().push(tags::SMALL_TUPLE_EXT);
        out.bytes().push(small_arity);
    } else {
        let large_arity = u32::try_from(arity).map_err(|_| EncodeError::UnsupportedTerm)?;
        out.bytes().push(tags::LARGE_TUPLE_EXT);
        out.bytes().extend_from_slice(&large_arity.to_be_bytes());
    }
    for index in 0..arity {
        encode_term_iovec_inner(
            tuple.get(index).ok_or(EncodeError::UnsupportedTerm)?,
            atom_table,
            out,
            depth + 1,
        )?;
    }
    Ok(())
}

fn encode_list(
    term: Term,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    let (elements, tail) = collect_list(term)?;
    if tail.is_nil() && elements.len() <= u16::MAX as usize {
        let mut bytes = Vec::with_capacity(elements.len());
        for element in &elements {
            let Some(value) = element.as_small_int() else {
                bytes.clear();
                break;
            };
            let Ok(byte) = u8::try_from(value) else {
                bytes.clear();
                break;
            };
            bytes.push(byte);
        }
        if bytes.len() == elements.len() {
            out.push(tags::STRING_EXT);
            out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            out.extend_from_slice(&bytes);
            return Ok(());
        }
    }

    let length = u32::try_from(elements.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.push(tags::LIST_EXT);
    out.extend_from_slice(&length.to_be_bytes());
    for element in elements {
        encode_term_inner(element, atom_table, out, depth + 1)?;
    }
    encode_term_inner(tail, atom_table, out, depth + 1)
}

fn encode_list_iovec(
    term: Term,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    let (elements, tail) = collect_list(term)?;
    if tail.is_nil() && elements.len() <= u16::MAX as usize {
        let mut bytes = Vec::with_capacity(elements.len());
        for element in &elements {
            let Some(value) = element.as_small_int() else {
                bytes.clear();
                break;
            };
            let Ok(byte) = u8::try_from(value) else {
                bytes.clear();
                break;
            };
            bytes.push(byte);
        }
        if bytes.len() == elements.len() {
            out.bytes().push(tags::STRING_EXT);
            out.bytes()
                .extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            out.bytes().extend_from_slice(&bytes);
            return Ok(());
        }
    }

    let length = u32::try_from(elements.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.bytes().push(tags::LIST_EXT);
    out.bytes().extend_from_slice(&length.to_be_bytes());
    for element in elements {
        encode_term_iovec_inner(element, atom_table, out, depth + 1)?;
    }
    encode_term_iovec_inner(tail, atom_table, out, depth + 1)
}

fn collect_list(term: Term) -> Result<(Vec<Term>, Term), EncodeError> {
    let mut elements = Vec::new();
    let mut current = term;
    while current.is_list() {
        let cons = Cons::new(current).ok_or(EncodeError::UnsupportedTerm)?;
        elements.push(cons.head());
        current = cons.tail();
    }
    Ok((elements, current))
}

fn encode_binary(binary: BinaryRef, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let bytes = binary.as_bytes();
    let length = u32::try_from(bytes.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.push(tags::BINARY_EXT);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn encode_binary_iovec(
    term: Term,
    binary: BinaryRef,
    out: &mut SegmentBuilder,
) -> Result<(), EncodeError> {
    let length = u32::try_from(binary.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    if binary.len() < IOVEC_BINARY_REFERENCE_THRESHOLD {
        return encode_binary(binary, out.bytes());
    }

    out.bytes().push(tags::BINARY_EXT);
    out.bytes().extend_from_slice(&length.to_be_bytes());
    out.flush();
    out.push_reference(term);
    Ok(())
}

fn encode_map(
    map: Map,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    let len = u32::try_from(map.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.push(tags::MAP_EXT);
    out.extend_from_slice(&len.to_be_bytes());
    for index in 0..map.len() {
        let key = map.key(index).ok_or(EncodeError::UnsupportedTerm)?;
        let value = map.value(index).ok_or(EncodeError::UnsupportedTerm)?;
        encode_term_inner(key, atom_table, out, depth + 1)?;
        encode_term_inner(value, atom_table, out, depth + 1)?;
    }
    Ok(())
}

fn encode_map_iovec(
    map: Map,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    let len = u32::try_from(map.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.bytes().push(tags::MAP_EXT);
    out.bytes().extend_from_slice(&len.to_be_bytes());
    for index in 0..map.len() {
        encode_term_iovec_inner(
            map.key(index).ok_or(EncodeError::UnsupportedTerm)?,
            atom_table,
            out,
            depth + 1,
        )?;
        encode_term_iovec_inner(
            map.value(index).ok_or(EncodeError::UnsupportedTerm)?,
            atom_table,
            out,
            depth + 1,
        )?;
    }
    Ok(())
}

fn encode_closure(
    closure: Closure,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    if closure.num_free() != 0 {
        return Err(EncodeError::UnsupportedTerm);
    }

    let function_index =
        u32::try_from(closure.function_index()).map_err(|_| EncodeError::UnsupportedTerm)?;

    out.push(tags::EXPORT_EXT);
    let module = closure.module().ok_or(EncodeError::UnsupportedTerm)?;
    encode_term_inner(Term::atom(module), atom_table, out, depth + 1)?;
    encode_term_inner(
        Term::atom(Atom::new(function_index)),
        atom_table,
        out,
        depth + 1,
    )?;
    encode_term_inner(
        Term::small_int(i64::from(closure.arity())),
        atom_table,
        out,
        depth + 1,
    )
}

fn encode_closure_iovec(
    closure: Closure,
    atom_table: &AtomTable,
    out: &mut SegmentBuilder,
    depth: usize,
) -> Result<(), EncodeError> {
    if closure.num_free() != 0 {
        return Err(EncodeError::UnsupportedTerm);
    }
    let function_index =
        u32::try_from(closure.function_index()).map_err(|_| EncodeError::UnsupportedTerm)?;

    out.bytes().push(tags::EXPORT_EXT);
    let module = closure.module().ok_or(EncodeError::UnsupportedTerm)?;
    encode_term_iovec_inner(Term::atom(module), atom_table, out, depth + 1)?;
    encode_term_iovec_inner(
        Term::atom(Atom::new(function_index)),
        atom_table,
        out,
        depth + 1,
    )?;
    encode_term_iovec_inner(
        Term::small_int(i64::from(closure.arity())),
        atom_table,
        out,
        depth + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::term::binary::packed_word_count;
    use crate::term::binary::write_binary;
    use crate::term::boxed::{write_bigint, write_cons, write_float, write_map, write_tuple};
    use crate::term::shared_binary::{SharedBinary, write_proc_bin};
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    fn atoms() -> AtomTable {
        AtomTable::with_common_atoms()
    }

    #[test]
    fn encode_small_integer_uses_small_integer_ext() {
        assert_eq!(
            encode_term(Term::small_int(42), &atoms()),
            Ok(vec![tags::VERSION, tags::SMALL_INTEGER_EXT, 42])
        );
    }

    #[test]
    fn encode_i32_integer_uses_integer_ext() {
        assert_eq!(
            encode_term(Term::small_int(1000), &atoms()),
            Ok(vec![tags::VERSION, tags::INTEGER_EXT, 0, 0, 3, 232])
        );
    }

    #[test]
    fn encode_float_uses_new_float_ext() {
        let mut heap = [0_u64; 2];
        let term = write_float(&mut heap, 3.14).expect("float fits");
        let mut expected = vec![tags::VERSION, tags::NEW_FLOAT_EXT];
        expected.extend_from_slice(&3.14_f64.to_bits().to_be_bytes());
        assert_eq!(encode_term(term, &atoms()), Ok(expected));
    }

    #[test]
    fn encode_atom_uses_small_atom_utf8_ext() {
        assert_eq!(
            encode_term(Term::atom(Atom::OK), &atoms()),
            Ok(vec![
                tags::VERSION,
                tags::SMALL_ATOM_UTF8_EXT,
                2,
                b'o',
                b'k'
            ])
        );
    }

    #[test]
    fn encode_nil_uses_nil_ext() {
        assert_eq!(
            encode_term(Term::NIL, &atoms()),
            Ok(vec![tags::VERSION, tags::NIL_EXT])
        );
    }

    #[test]
    fn encode_tuple_recursively_encodes_elements() {
        let mut heap = [0_u64; 3];
        let term = write_tuple(&mut heap, &[Term::atom(Atom::OK), Term::small_int(42)])
            .expect("tuple fits");
        assert_eq!(
            encode_term(term, &atoms()),
            Ok(vec![
                tags::VERSION,
                tags::SMALL_TUPLE_EXT,
                2,
                tags::SMALL_ATOM_UTF8_EXT,
                2,
                b'o',
                b'k',
                tags::SMALL_INTEGER_EXT,
                42,
            ])
        );
    }

    #[test]
    fn encode_non_byte_list_uses_list_ext() {
        let mut tail_heap = [0_u64; 2];
        let tail = write_cons(&mut tail_heap, Term::small_int(300), Term::NIL).expect("cons");
        let mut mid_heap = [0_u64; 2];
        let mid = write_cons(&mut mid_heap, Term::small_int(2), tail).expect("cons");
        let mut head_heap = [0_u64; 2];
        let list = write_cons(&mut head_heap, Term::small_int(1), mid).expect("cons");

        assert_eq!(
            encode_term(list, &atoms()),
            Ok(vec![
                tags::VERSION,
                tags::LIST_EXT,
                0,
                0,
                0,
                3,
                tags::SMALL_INTEGER_EXT,
                1,
                tags::SMALL_INTEGER_EXT,
                2,
                tags::INTEGER_EXT,
                0,
                0,
                1,
                44,
                tags::NIL_EXT,
            ])
        );
    }

    #[test]
    fn encode_byte_list_uses_string_ext() {
        let bytes = [72, 101, 108, 108, 111];
        let mut cells = [[0_u64; 2]; 5];
        let mut tail = Term::NIL;
        for (index, byte) in bytes.iter().enumerate().rev() {
            tail = write_cons(&mut cells[index], Term::small_int(*byte), tail).expect("cons");
        }

        assert_eq!(
            encode_term(tail, &atoms()),
            Ok(vec![
                tags::VERSION,
                tags::STRING_EXT,
                0,
                5,
                b'H',
                b'e',
                b'l',
                b'l',
                b'o',
            ])
        );
    }

    #[test]
    fn encode_map_uses_map_ext() {
        let table = AtomTable::with_common_atoms();
        let key = table.intern("a");
        let mut heap = [0_u64; 4];
        let map = write_map(&mut heap, &[Term::atom(key)], &[Term::small_int(1)]).expect("map");

        assert_eq!(
            encode_term(map, &table),
            Ok(vec![
                tags::VERSION,
                tags::MAP_EXT,
                0,
                0,
                0,
                1,
                tags::SMALL_ATOM_UTF8_EXT,
                1,
                b'a',
                tags::SMALL_INTEGER_EXT,
                1,
            ])
        );
    }

    #[test]
    fn encode_binary_uses_binary_ext() {
        let mut heap = [0_u64; 3];
        let binary = write_binary(&mut heap, &[1, 2, 3]).expect("binary");
        assert_eq!(
            encode_term(binary, &atoms()),
            Ok(vec![tags::VERSION, tags::BINARY_EXT, 0, 0, 0, 3, 1, 2, 3])
        );
    }

    fn flatten_segments(segments: &[IoSegment]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for segment in segments {
            match segment {
                IoSegment::Owned(segment_bytes) => bytes.extend_from_slice(segment_bytes),
                IoSegment::Reference(term) => bytes.extend_from_slice(
                    BinaryRef::new(*term)
                        .expect("reference segment should be binary")
                        .as_bytes(),
                ),
            }
        }
        bytes
    }

    fn inline_binary(bytes: &[u8]) -> (Vec<u64>, Term) {
        let mut heap = vec![0_u64; 2 + packed_word_count(bytes.len())];
        let term = write_binary(&mut heap, bytes).expect("binary fits");
        (heap, term)
    }

    #[test]
    fn encode_term_iovec_small_binary_stays_in_single_owned_segment() {
        let (_heap, binary) = inline_binary(b"small");
        let segments = encode_term_iovec(binary, &atoms()).expect("iovec");
        let encoded = encode_term(binary, &atoms()).expect("encoded");

        assert_eq!(segments, vec![IoSegment::Owned(encoded.clone())]);
        assert_eq!(flatten_segments(&segments), encoded);
    }

    #[test]
    fn encode_term_iovec_large_binary_emits_header_and_reference() {
        let bytes = vec![7_u8; IOVEC_BINARY_REFERENCE_THRESHOLD];
        let (_heap, binary) = inline_binary(&bytes);
        let segments = encode_term_iovec(binary, &atoms()).expect("iovec");

        assert_eq!(segments.len(), 2);
        assert_eq!(
            segments[0],
            IoSegment::Owned(vec![tags::VERSION, tags::BINARY_EXT, 0, 0, 0, 64])
        );
        assert_eq!(segments[1], IoSegment::Reference(binary));
        assert_eq!(
            flatten_segments(&segments),
            encode_term(binary, &atoms()).expect("encoded")
        );
    }

    #[test]
    fn encode_term_iovec_references_proc_bins() {
        let shared = SharedBinary::new(vec![9_u8; IOVEC_BINARY_REFERENCE_THRESHOLD + 1]);
        let mut heap = [0_u64; 3];
        let binary = write_proc_bin(&mut heap, &shared).expect("proc bin");
        let segments = encode_term_iovec(binary, &atoms()).expect("iovec");

        assert!(matches!(
            segments.as_slice(),
            [IoSegment::Owned(_), IoSegment::Reference(term)]
                if *term == binary
        ));
        assert_eq!(
            flatten_segments(&segments),
            encode_term(binary, &atoms()).expect("encoded")
        );
    }

    #[test]
    fn encode_term_iovec_nested_large_binaries_match_flat_encoding() {
        let table = AtomTable::with_common_atoms();
        let key_atom = table.intern("payload");
        let bytes = vec![1_u8; IOVEC_BINARY_REFERENCE_THRESHOLD];
        let (_binary_heap, binary) = inline_binary(&bytes);
        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(&mut tuple_heap, &[Term::atom(Atom::OK), binary]).expect("tuple");
        let mut cons_heap = [0_u64; 2];
        let list = write_cons(&mut cons_heap, binary, Term::NIL).expect("list");
        let mut map_heap = [0_u64; 4];
        let map = write_map(&mut map_heap, &[Term::atom(key_atom)], &[binary]).expect("map");

        for term in [tuple, list, map] {
            let segments = encode_term_iovec(term, &table).expect("iovec");
            assert!(segments.iter().any(|segment| matches!(
                segment,
                IoSegment::Reference(reference) if *reference == binary
            )));
            assert_eq!(
                flatten_segments(&segments),
                encode_term(term, &table).expect("encoded")
            );
        }
    }

    #[test]
    fn compression_wraps_payload_when_smaller() {
        let table = AtomTable::with_common_atoms();
        let mut cells = vec![[0_u64; 2]; 512];
        let mut list = Term::NIL;
        for cell in cells.iter_mut().rev() {
            list = write_cons(cell, Term::small_int(0), list).expect("cons");
        }
        let uncompressed = encode_term(list, &table).expect("uncompressed");
        let compressed = encode_term_with_options(
            list,
            &table,
            EncodeOptions {
                compression_level: Some(6),
            },
        )
        .expect("compressed");

        assert!(compressed.len() < uncompressed.len());
        assert_eq!(compressed[0], tags::VERSION);
        assert_eq!(compressed[1], tags::COMPRESSED_EXT);
        let declared =
            u32::from_be_bytes([compressed[2], compressed[3], compressed[4], compressed[5]])
                as usize;
        assert_eq!(declared, uncompressed.len() - 1);
        let mut decoder = ZlibDecoder::new(&compressed[6..]);
        let mut inflated = Vec::new();
        decoder.read_to_end(&mut inflated).expect("inflate");
        assert_eq!(inflated, uncompressed[1..]);
    }

    #[test]
    fn compression_level_zero_returns_uncompressed() {
        let table = AtomTable::with_common_atoms();
        let uncompressed = encode_term(Term::small_int(42), &table).expect("uncompressed");
        let encoded = encode_term_with_options(
            Term::small_int(42),
            &table,
            EncodeOptions {
                compression_level: Some(0),
            },
        )
        .expect("encoded");
        assert_eq!(encoded, uncompressed);
    }

    #[test]
    fn compression_does_not_expand_small_terms() {
        let table = AtomTable::with_common_atoms();
        let uncompressed = encode_term(Term::atom(Atom::OK), &table).expect("uncompressed");
        let encoded = encode_term_with_options(
            Term::atom(Atom::OK),
            &table,
            EncodeOptions {
                compression_level: Some(6),
            },
        )
        .expect("encoded");
        assert_eq!(encoded, uncompressed);
    }

    #[test]
    fn encode_bigint_uses_big_ext_with_little_endian_magnitude() {
        let mut heap = [0_u64; 4];
        let bigint = write_bigint(&mut heap, true, &[0x0102]).expect("bigint");
        assert_eq!(
            encode_term(bigint, &atoms()),
            Ok(vec![tags::VERSION, tags::SMALL_BIG_EXT, 2, 1, 0x02, 0x01])
        );
    }

    #[test]
    fn deeply_nested_terms_return_too_deep() {
        let mut tuples = vec![[0_u64; 2]; MAX_ETF_DEPTH + 2];
        let mut term = Term::NIL;
        for tuple in tuples.iter_mut().rev() {
            term = write_tuple(tuple, &[term]).expect("tuple");
        }

        assert_eq!(encode_term(term, &atoms()), Err(EncodeError::TooDeep));
        assert_eq!(encode_term_iovec(term, &atoms()), Err(EncodeError::TooDeep));
    }

    #[test]
    fn deeply_nested_list_returns_too_deep() {
        let mut cells = vec![[0_u64; 2]; MAX_ETF_DEPTH + 2];
        let mut term = Term::NIL;
        for cell in cells.iter_mut().rev() {
            term = write_cons(cell, term, Term::NIL).expect("cons");
        }

        assert_eq!(encode_term(term, &atoms()), Err(EncodeError::TooDeep));
        assert_eq!(encode_term_iovec(term, &atoms()), Err(EncodeError::TooDeep));
    }
}
