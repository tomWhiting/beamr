//! Distribution-facing ETF term codec and pass-through message framing.
//!
//! This module intentionally supports the uncompressed distribution term subset.
//! `COMPRESSED_EXT` and atom-cache references are negotiated/implemented outside
//! this codec.

use std::fmt;
use std::io::Read;

use crate::atom::{Atom, AtomTable};
use crate::etf::tags;
use crate::process::heap::Heap;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{
    BigInt, Cons, ExternalPid, ExternalReference, Float, Map, Reference, Tuple, write_bigint,
    write_cons, write_external_pid, write_external_reference, write_float, write_map, write_tuple,
};
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};
use crate::term::{Tag, Term};

const MAX_ETF_DEPTH: usize = 256;
const NONODE_NOHOST: &str = "nonode@nohost";
const PASS_THROUGH: u8 = b'p';

/// Errors produced by distribution ETF encoding.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EncodeError {
    UnsupportedTerm,
    AtomResolveFailed,
    TooDeep,
}

/// Errors produced by distribution ETF decoding.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DecodeError {
    EmptyInput,
    BadVersion(u8),
    Truncated,
    TrailingBytes,
    UnsupportedTag(u8),
    InvalidUtf8,
    TooDeep,
    IntegerOutOfRange,
    InvalidBigSign(u8),
    InvalidExternalNode,
    InvalidReferenceArity(u16),
    HeapAllocationFailed,
    SizeLimitExceeded,
}

/// Errors produced by distribution pass-through frame deframing.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Error {
    Io(String),
    TruncatedHeader,
    TruncatedBody { expected: usize, actual: usize },
    EmptyBody,
    InvalidPassThrough(u8),
    InvalidControl(DecodeError),
    LengthTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "distribution frame I/O error: {error}"),
            Self::TruncatedHeader => formatter.write_str("distribution frame header was truncated"),
            Self::TruncatedBody { expected, actual } => write!(
                formatter,
                "distribution frame body was truncated: expected {expected} bytes, got {actual}"
            ),
            Self::EmptyBody => formatter.write_str("distribution frame body is empty"),
            Self::InvalidPassThrough(tag) => {
                write!(formatter, "invalid distribution pass-through tag {tag}")
            }
            Self::InvalidControl(error) => write!(formatter, "invalid control ETF term: {error:?}"),
            Self::LengthTooLarge => {
                formatter.write_str("distribution frame length does not fit usize")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Encode a term to its ETF binary representation, including the ETF version byte.
///
/// For distribution-supported terms this returns BEAM-compatible ETF bytes. If a
/// term outside the supported subset is supplied, an empty vector is returned;
/// callers that need to distinguish that case should use [`encode_term_result`].
#[must_use]
pub fn encode_term(term: Term, atom_table: &AtomTable) -> Vec<u8> {
    encode_term_result(term, atom_table).unwrap_or_default()
}

/// Fallible distribution ETF encoder used by callers that need explicit errors.
pub fn encode_term_result(term: Term, atom_table: &AtomTable) -> Result<Vec<u8>, EncodeError> {
    let mut out = vec![tags::VERSION];
    encode_term_inner(term, atom_table, &mut out, 0)?;
    Ok(out)
}

/// Decode a complete ETF binary representation into a term allocated on `heap`.
pub fn decode_term(
    bytes: &[u8],
    heap: &mut Heap,
    atom_table: &AtomTable,
) -> Result<Term, DecodeError> {
    let decoded = decode_term_prefix(bytes, heap, atom_table)?;
    if decoded.used == bytes.len() {
        Ok(decoded.term)
    } else {
        Err(DecodeError::TrailingBytes)
    }
}

/// Write a distribution pass-through message frame.
///
/// The returned bytes are `u32_be(length) || 112 || control || payload?`.
#[must_use]
pub fn write_dist_message(control: &[u8], payload: Option<&[u8]>) -> Vec<u8> {
    let payload_len = payload.map_or(0, <[u8]>::len);
    let Some(body_len) = 1usize
        .checked_add(control.len())
        .and_then(|len| len.checked_add(payload_len))
    else {
        return Vec::new();
    };
    let Ok(length) = u32::try_from(body_len) else {
        return Vec::new();
    };
    let Some(capacity) = 4usize.checked_add(body_len) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(&length.to_be_bytes());
    out.push(PASS_THROUGH);
    out.extend_from_slice(control);
    if let Some(payload) = payload {
        out.extend_from_slice(payload);
    }
    out
}

/// Read and deframe one distribution pass-through message.
pub fn read_dist_message<R: Read>(stream: &mut R) -> Result<(Vec<u8>, Option<Vec<u8>>), Error> {
    let mut header = [0_u8; 4];
    read_exact_classified(stream, &mut header, Error::TruncatedHeader)?;
    let length = usize::try_from(u32::from_be_bytes(header)).map_err(|_| Error::LengthTooLarge)?;
    let mut body = vec![0_u8; length];
    read_exact_classified(
        stream,
        &mut body,
        Error::TruncatedBody {
            expected: length,
            actual: 0,
        },
    )?;
    let Some((&tag, remaining)) = body.split_first() else {
        return Err(Error::EmptyBody);
    };
    if tag != PASS_THROUGH {
        return Err(Error::InvalidPassThrough(tag));
    }

    let used = scan_term_prefix(remaining).map_err(Error::InvalidControl)?;
    let control = remaining[..used].to_vec();
    let payload = if used == remaining.len() {
        None
    } else {
        Some(remaining[used..].to_vec())
    };
    Ok((control, payload))
}

fn read_exact_classified<R: Read>(
    stream: &mut R,
    buf: &mut [u8],
    truncated: Error,
) -> Result<(), Error> {
    let mut read = 0;
    while read < buf.len() {
        match stream.read(&mut buf[read..]) {
            Ok(0) => {
                return match truncated {
                    Error::TruncatedBody { expected, .. } => Err(Error::TruncatedBody {
                        expected,
                        actual: read,
                    }),
                    other => Err(other),
                };
            }
            Ok(n) => read = read.checked_add(n).ok_or(Error::LengthTooLarge)?,
            Err(error) => return Err(Error::Io(error.to_string())),
        }
    }
    Ok(())
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
        Tag::Pid => encode_local_pid(term.as_pid().ok_or(EncodeError::UnsupportedTerm)?, out),
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

fn encode_local_pid(pid: u64, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    out.push(tags::NEW_PID_EXT);
    encode_atom_name(NONODE_NOHOST, out)?;
    let id = u32::try_from(pid).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    Ok(())
}

fn encode_external_pid(
    pid: ExternalPid,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    out.push(tags::NEW_PID_EXT);
    encode_atom(
        pid.node().ok_or(EncodeError::UnsupportedTerm)?,
        atom_table,
        out,
    )?;
    let id = u32::try_from(pid.pid_number()).map_err(|_| EncodeError::UnsupportedTerm)?;
    let serial = u32::try_from(pid.serial()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&serial.to_be_bytes());
    out.extend_from_slice(&0_u32.to_be_bytes());
    Ok(())
}

fn encode_local_reference(reference: Reference, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    out.push(tags::NEWER_REFERENCE_EXT);
    out.extend_from_slice(&2_u16.to_be_bytes());
    encode_atom_name(NONODE_NOHOST, out)?;
    out.extend_from_slice(&0_u32.to_be_bytes());
    let id = reference.id();
    out.extend_from_slice(&((id >> u32::BITS) as u32).to_be_bytes());
    out.extend_from_slice(&(id as u32).to_be_bytes());
    Ok(())
}

fn encode_external_reference(
    reference: ExternalReference,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    out.push(tags::NEWER_REFERENCE_EXT);
    out.extend_from_slice(&2_u16.to_be_bytes());
    encode_atom(
        reference.node().ok_or(EncodeError::UnsupportedTerm)?,
        atom_table,
        out,
    )?;
    out.extend_from_slice(&0_u32.to_be_bytes());
    let id = reference.id();
    out.extend_from_slice(&((id >> u32::BITS) as u32).to_be_bytes());
    out.extend_from_slice(&(id as u32).to_be_bytes());
    Ok(())
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
    if let Some(reference) = Reference::new(term) {
        return encode_local_reference(reference, out);
    }
    if let Some(pid) = ExternalPid::new(term) {
        return encode_external_pid(pid, atom_table, out);
    }
    if let Some(reference) = ExternalReference::new(term) {
        return encode_external_reference(reference, atom_table, out);
    }
    if let Some(bigint) = BigInt::new(term) {
        return encode_bigint(bigint, out);
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

fn encode_list(
    term: Term,
    atom_table: &AtomTable,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), EncodeError> {
    let (elements, tail) = collect_list(term)?;
    let length = u32::try_from(elements.len()).map_err(|_| EncodeError::UnsupportedTerm)?;
    out.push(tags::LIST_EXT);
    out.extend_from_slice(&length.to_be_bytes());
    for element in elements {
        encode_term_inner(element, atom_table, out, depth + 1)?;
    }
    encode_term_inner(tail, atom_table, out, depth + 1)
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

struct DecodedTerm {
    term: Term,
    used: usize,
}

fn decode_term_prefix(
    bytes: &[u8],
    heap: &mut Heap,
    atom_table: &AtomTable,
) -> Result<DecodedTerm, DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let version = cursor.read_u8().map_err(|error| match error {
        DecodeError::Truncated => DecodeError::EmptyInput,
        other => other,
    })?;
    if version != tags::VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let term = decode_one(&mut cursor, heap, atom_table, 0)?;
    Ok(DecodedTerm {
        term,
        used: cursor.position(),
    })
}

fn decode_one(
    cursor: &mut Cursor<'_>,
    heap: &mut Heap,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Term, DecodeError> {
    if depth > MAX_ETF_DEPTH {
        return Err(DecodeError::TooDeep);
    }

    let tag = cursor.read_u8()?;
    match tag {
        tag if tag == tags::NEW_FLOAT_EXT => {
            let bytes = cursor.read_bytes(8)?;
            let value = f64::from_bits(u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]));
            alloc_float_term(heap, value)
        }
        tag if tag == tags::SMALL_INTEGER_EXT => Ok(Term::small_int(i64::from(cursor.read_u8()?))),
        tag if tag == tags::INTEGER_EXT => decode_small_integer(i64::from(cursor.read_i32()?)),
        tag if tag == tags::ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u16()?);
            decode_atom(cursor.read_bytes(len)?, atom_table)
        }
        tag if tag == tags::SMALL_ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u8()?);
            decode_atom(cursor.read_bytes(len)?, atom_table)
        }
        tag if tag == tags::SMALL_TUPLE_EXT => {
            let arity = usize::from(cursor.read_u8()?);
            decode_tuple(cursor, arity, heap, atom_table, depth)
        }
        tag if tag == tags::LARGE_TUPLE_EXT => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, arity, heap, atom_table, depth)
        }
        tag if tag == tags::NIL_EXT => Ok(Term::NIL),
        tag if tag == tags::STRING_EXT => {
            let len = usize::from(cursor.read_u16()?);
            let mut elements = Vec::with_capacity(len);
            for byte in cursor.read_bytes(len)? {
                elements.push(Term::small_int(i64::from(*byte)));
            }
            alloc_list(heap, &elements, Term::NIL)
        }
        tag if tag == tags::LIST_EXT => {
            let len = cursor.read_u32()? as usize;
            let mut elements = Vec::with_capacity(len);
            for _ in 0..len {
                elements.push(decode_one(cursor, heap, atom_table, depth + 1)?);
            }
            let tail = decode_one(cursor, heap, atom_table, depth + 1)?;
            alloc_list(heap, &elements, tail)
        }
        tag if tag == tags::BINARY_EXT => {
            let len = cursor.read_u32()? as usize;
            let bytes = cursor.read_bytes(len)?;
            alloc_binary_term(heap, bytes)
        }
        tag if tag == tags::SMALL_BIG_EXT => {
            let len = usize::from(cursor.read_u8()?);
            decode_big_integer(cursor, len, heap)
        }
        tag if tag == tags::LARGE_BIG_EXT => {
            let len = cursor.read_u32()? as usize;
            decode_big_integer(cursor, len, heap)
        }
        tag if tag == tags::MAP_EXT => {
            let len = cursor.read_u32()? as usize;
            let mut keys = Vec::with_capacity(len);
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                keys.push(decode_one(cursor, heap, atom_table, depth + 1)?);
                values.push(decode_one(cursor, heap, atom_table, depth + 1)?);
            }
            alloc_map_term(heap, &keys, &values)
        }
        tag if tag == tags::NEW_PID_EXT => decode_new_pid(cursor, heap, atom_table, depth),
        tag if tag == tags::NEWER_REFERENCE_EXT => {
            decode_newer_reference(cursor, heap, atom_table, depth)
        }
        other => Err(DecodeError::UnsupportedTag(other)),
    }
}

fn decode_tuple(
    cursor: &mut Cursor<'_>,
    arity: usize,
    heap: &mut Heap,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Term, DecodeError> {
    let mut elements = Vec::with_capacity(arity);
    for _ in 0..arity {
        elements.push(decode_one(cursor, heap, atom_table, depth + 1)?);
    }
    alloc_tuple_term(heap, &elements)
}

fn decode_atom(bytes: &[u8], atom_table: &AtomTable) -> Result<Term, DecodeError> {
    let name = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
    Ok(Term::atom(atom_table.intern(name)))
}

fn decode_small_integer(value: i64) -> Result<Term, DecodeError> {
    Term::try_small_int(value).ok_or(DecodeError::IntegerOutOfRange)
}

fn decode_big_integer(
    cursor: &mut Cursor<'_>,
    len: usize,
    heap: &mut Heap,
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
            alloc_bigint_term(heap, integer.is_negative(), &[magnitude])
        }
    } else {
        let limb_count = len.div_ceil(std::mem::size_of::<u64>());
        let mut limbs = Vec::with_capacity(limb_count);
        for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
            let mut limb_bytes = [0_u8; std::mem::size_of::<u64>()];
            limb_bytes[..chunk.len()].copy_from_slice(chunk);
            limbs.push(u64::from_le_bytes(limb_bytes));
        }
        alloc_bigint_term(heap, negative, &limbs)
    }
}

fn decode_new_pid(
    cursor: &mut Cursor<'_>,
    heap: &mut Heap,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Term, DecodeError> {
    let node = decode_node_atom(cursor, heap, atom_table, depth)?;
    let id = u64::from(cursor.read_u32()?);
    let serial = u64::from(cursor.read_u32()?);
    let _creation = cursor.read_u32()?;
    alloc_external_pid_term(heap, node, id, serial)
}

fn decode_newer_reference(
    cursor: &mut Cursor<'_>,
    heap: &mut Heap,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Term, DecodeError> {
    let len = cursor.read_u16()?;
    if len == 0 || len > 2 {
        return Err(DecodeError::InvalidReferenceArity(len));
    }
    let node = decode_node_atom(cursor, heap, atom_table, depth)?;
    let _creation = cursor.read_u32()?;
    let mut id = 0_u64;
    for _ in 0..len {
        id = (id << u32::BITS) | u64::from(cursor.read_u32()?);
    }
    alloc_external_reference_term(heap, node, id)
}

fn decode_node_atom(
    cursor: &mut Cursor<'_>,
    heap: &mut Heap,
    atom_table: &AtomTable,
    depth: usize,
) -> Result<Atom, DecodeError> {
    let node = decode_one(cursor, heap, atom_table, depth + 1)?;
    node.as_atom().ok_or(DecodeError::InvalidExternalNode)
}

fn alloc_tuple_term(heap: &mut Heap, elements: &[Term]) -> Result<Term, DecodeError> {
    let words = 1usize
        .checked_add(elements.len())
        .ok_or(DecodeError::SizeLimitExceeded)?;
    let slice = heap
        .alloc_slice(words)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_tuple(slice, elements).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_float_term(heap: &mut Heap, value: f64) -> Result<Term, DecodeError> {
    let slice = heap
        .alloc_slice(2)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_float(slice, value).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_binary_term(heap: &mut Heap, bytes: &[u8]) -> Result<Term, DecodeError> {
    let words = alloc_binary_word_count(bytes.len());
    let slice = heap
        .alloc_slice(words)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    alloc_binary(slice, bytes).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_bigint_term(heap: &mut Heap, negative: bool, limbs: &[u64]) -> Result<Term, DecodeError> {
    let words = 3usize
        .checked_add(limbs.len())
        .ok_or(DecodeError::SizeLimitExceeded)?;
    let slice = heap
        .alloc_slice(words)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_bigint(slice, negative, limbs).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_map_term(heap: &mut Heap, keys: &[Term], values: &[Term]) -> Result<Term, DecodeError> {
    let words = 2usize
        .checked_add(keys.len())
        .and_then(|count| count.checked_add(values.len()))
        .ok_or(DecodeError::SizeLimitExceeded)?;
    let slice = heap
        .alloc_slice(words)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_map(slice, keys, values).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_external_pid_term(
    heap: &mut Heap,
    node: Atom,
    id: u64,
    serial: u64,
) -> Result<Term, DecodeError> {
    let slice = heap
        .alloc_slice(4)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_external_pid(slice, node, id, serial).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_external_reference_term(
    heap: &mut Heap,
    node: Atom,
    id: u64,
) -> Result<Term, DecodeError> {
    let slice = heap
        .alloc_slice(3)
        .map_err(|_| DecodeError::HeapAllocationFailed)?;
    write_external_reference(slice, node, id).ok_or(DecodeError::HeapAllocationFailed)
}

fn alloc_list(heap: &mut Heap, elements: &[Term], mut tail: Term) -> Result<Term, DecodeError> {
    for element in elements.iter().rev().copied() {
        let slice = heap
            .alloc_slice(2)
            .map_err(|_| DecodeError::HeapAllocationFailed)?;
        tail = write_cons(slice, element, tail).ok_or(DecodeError::HeapAllocationFailed)?;
    }
    Ok(tail)
}

fn scan_term_prefix(bytes: &[u8]) -> Result<usize, DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let version = cursor.read_u8().map_err(|error| match error {
        DecodeError::Truncated => DecodeError::EmptyInput,
        other => other,
    })?;
    if version != tags::VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    scan_one(&mut cursor, 0)?;
    Ok(cursor.position())
}

fn scan_one(cursor: &mut Cursor<'_>, depth: usize) -> Result<(), DecodeError> {
    if depth > MAX_ETF_DEPTH {
        return Err(DecodeError::TooDeep);
    }
    let tag = cursor.read_u8()?;
    match tag {
        tag if tag == tags::NEW_FLOAT_EXT => cursor.skip_bytes(8),
        tag if tag == tags::SMALL_INTEGER_EXT => cursor.skip_bytes(1),
        tag if tag == tags::INTEGER_EXT => cursor.skip_bytes(4),
        tag if tag == tags::ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u16()?);
            let bytes = cursor.read_bytes(len)?;
            std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
            Ok(())
        }
        tag if tag == tags::SMALL_ATOM_UTF8_EXT => {
            let len = usize::from(cursor.read_u8()?);
            let bytes = cursor.read_bytes(len)?;
            std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
            Ok(())
        }
        tag if tag == tags::SMALL_TUPLE_EXT => {
            let arity = usize::from(cursor.read_u8()?);
            for _ in 0..arity {
                scan_one(cursor, depth + 1)?;
            }
            Ok(())
        }
        tag if tag == tags::LARGE_TUPLE_EXT => {
            let arity = cursor.read_u32()? as usize;
            for _ in 0..arity {
                scan_one(cursor, depth + 1)?;
            }
            Ok(())
        }
        tag if tag == tags::NIL_EXT => Ok(()),
        tag if tag == tags::STRING_EXT => {
            let len = usize::from(cursor.read_u16()?);
            cursor.skip_bytes(len)
        }
        tag if tag == tags::LIST_EXT => {
            let len = cursor.read_u32()? as usize;
            for _ in 0..len {
                scan_one(cursor, depth + 1)?;
            }
            scan_one(cursor, depth + 1)
        }
        tag if tag == tags::BINARY_EXT => {
            let len = cursor.read_u32()? as usize;
            cursor.skip_bytes(len)
        }
        tag if tag == tags::SMALL_BIG_EXT => {
            let len = usize::from(cursor.read_u8()?);
            scan_big(cursor, len)
        }
        tag if tag == tags::LARGE_BIG_EXT => {
            let len = cursor.read_u32()? as usize;
            scan_big(cursor, len)
        }
        tag if tag == tags::MAP_EXT => {
            let len = cursor.read_u32()? as usize;
            for _ in 0..len {
                scan_one(cursor, depth + 1)?;
                scan_one(cursor, depth + 1)?;
            }
            Ok(())
        }
        tag if tag == tags::NEW_PID_EXT => {
            scan_one(cursor, depth + 1)?;
            cursor.skip_bytes(12)
        }
        tag if tag == tags::NEWER_REFERENCE_EXT => {
            let len = cursor.read_u16()?;
            if len == 0 || len > 2 {
                return Err(DecodeError::InvalidReferenceArity(len));
            }
            scan_one(cursor, depth + 1)?;
            cursor.skip_bytes(4 + usize::from(len) * 4)
        }
        other => Err(DecodeError::UnsupportedTag(other)),
    }
}

fn scan_big(cursor: &mut Cursor<'_>, len: usize) -> Result<(), DecodeError> {
    let sign = cursor.read_u8()?;
    match sign {
        0 | 1 => cursor.skip_bytes(len),
        other => Err(DecodeError::InvalidBigSign(other)),
    }
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

    fn skip_bytes(&mut self, len: usize) -> Result<(), DecodeError> {
        self.read_bytes(len)?;
        Ok(())
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
    use crate::term::binary::Binary;
    use crate::term::boxed::{
        Float, Map, write_external_pid, write_external_reference, write_reference,
    };

    fn heap() -> Heap {
        Heap::new(2048)
    }

    fn test_reference(id: u64) -> Term {
        let words = Box::leak(Box::new([0_u64; 2]));
        write_reference(words, id).expect("reference allocation")
    }

    #[test]
    fn encodes_small_integer_and_integer_ext() {
        let atoms = AtomTable::with_common_atoms();
        assert_eq!(encode_term(Term::small_int(42), &atoms), vec![131, 97, 42]);
        assert_eq!(
            encode_term(Term::small_int(256), &atoms),
            vec![131, 98, 0, 0, 1, 0]
        );
        assert_eq!(
            encode_term(Term::small_int(-1), &atoms),
            vec![131, 98, 255, 255, 255, 255]
        );
    }

    #[test]
    fn encodes_atoms_with_utf8_tags() {
        let atoms = AtomTable::with_common_atoms();
        assert_eq!(
            encode_term(Term::atom(Atom::OK), &atoms),
            vec![131, tags::SMALL_ATOM_UTF8_EXT, 2, b'o', b'k']
        );
        let long_name = "a".repeat(256);
        let long_atom = atoms.intern(&long_name);
        let mut expected = vec![131, tags::ATOM_UTF8_EXT, 1, 0];
        expected.extend_from_slice(long_name.as_bytes());
        assert_eq!(encode_term(Term::atom(long_atom), &atoms), expected);
    }

    #[test]
    fn encodes_tuple_nil_list_binary_float_map_pid_and_reference() {
        let atoms = AtomTable::with_common_atoms();
        let mut local_heap = heap();
        let tuple = alloc_tuple_term(&mut local_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
            .expect("tuple allocation");
        assert_eq!(
            encode_term(tuple, &atoms),
            vec![131, 104, 2, 119, 2, b'o', b'k', 97, 1]
        );
        assert_eq!(encode_term(Term::NIL, &atoms), vec![131, 106]);

        let list = alloc_list(
            &mut local_heap,
            &[Term::small_int(65), Term::small_int(66)],
            Term::NIL,
        )
        .expect("list allocation");
        assert_eq!(
            encode_term(list, &atoms),
            vec![131, 108, 0, 0, 0, 2, 97, 65, 97, 66, 106]
        );

        let binary = alloc_binary_term(&mut local_heap, b"hi").expect("binary allocation");
        assert_eq!(
            encode_term(binary, &atoms),
            vec![131, 109, 0, 0, 0, 2, b'h', b'i']
        );

        let float = alloc_float_term(&mut local_heap, 1.5).expect("float allocation");
        let mut expected_float = vec![131, 70];
        expected_float.extend_from_slice(&1.5_f64.to_bits().to_be_bytes());
        assert_eq!(encode_term(float, &atoms), expected_float);

        let map = alloc_map_term(
            &mut local_heap,
            &[Term::atom(Atom::OK)],
            &[Term::small_int(2)],
        )
        .expect("map allocation");
        assert_eq!(
            encode_term(map, &atoms),
            vec![131, 116, 0, 0, 0, 1, 119, 2, b'o', b'k', 97, 2]
        );

        assert_eq!(
            encode_term(Term::pid(7), &atoms),
            vec![
                131, 88, 119, 13, b'n', b'o', b'n', b'o', b'd', b'e', b'@', b'n', b'o', b'h', b'o',
                b's', b't', 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let reference = test_reference(0x0102_0304_0506_0708);
        assert_eq!(
            encode_term(reference, &atoms),
            vec![
                131, 90, 0, 2, 119, 13, b'n', b'o', b'n', b'o', b'd', b'e', b'@', b'n', b'o', b'h',
                b'o', b's', b't', 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8
            ]
        );
    }

    #[test]
    fn decodes_round_trip_for_distribution_terms() {
        let atoms = AtomTable::with_common_atoms();
        let mut source_heap = heap();
        let terms = [
            Term::small_int(42),
            Term::small_int(256),
            Term::atom(Atom::OK),
            alloc_tuple_term(
                &mut source_heap,
                &[Term::atom(Atom::OK), Term::small_int(1)],
            )
            .expect("tuple allocation"),
            Term::NIL,
            alloc_list(
                &mut source_heap,
                &[Term::small_int(1), Term::small_int(2)],
                Term::NIL,
            )
            .expect("list allocation"),
            alloc_binary_term(&mut source_heap, b"payload").expect("binary allocation"),
            alloc_float_term(&mut source_heap, 3.25).expect("float allocation"),
            alloc_map_term(
                &mut source_heap,
                &[Term::atom(Atom::OK)],
                &[Term::small_int(9)],
            )
            .expect("map allocation"),
            Term::pid(99),
            test_reference(1234),
        ];

        for term in terms {
            let bytes = encode_term(term, &atoms);
            let mut target_heap = heap();
            let decoded = decode_term(&bytes, &mut target_heap, &atoms).expect("decode round trip");
            let encoded_again = encode_term(decoded, &atoms);
            assert_eq!(encoded_again, bytes);
        }
    }

    #[test]
    fn decodes_boxed_payloads_to_heap_terms() {
        let atoms = AtomTable::with_common_atoms();
        let mut heap = heap();
        let tuple = decode_term(&[131, 104, 1, 97, 7], &mut heap, &atoms).expect("tuple decode");
        assert_eq!(
            Tuple::new(tuple).and_then(|tuple| tuple.get(0)),
            Some(Term::small_int(7))
        );

        let binary = decode_term(&[131, 109, 0, 0, 0, 3, b'a', b'b', b'c'], &mut heap, &atoms)
            .expect("binary decode");
        assert_eq!(
            Binary::new(binary).map(|binary| binary.as_bytes()),
            Some(b"abc".as_slice())
        );

        let float = decode_term(
            &encode_term(
                alloc_float_term(&mut heap, 2.0).expect("float allocation"),
                &atoms,
            ),
            &mut heap,
            &atoms,
        )
        .expect("float decode");
        assert_eq!(Float::new(float).map(Float::value), Some(2.0));

        let map = decode_term(
            &[131, 116, 0, 0, 0, 1, 119, 2, b'o', b'k', 97, 8],
            &mut heap,
            &atoms,
        )
        .expect("map decode");
        let map = Map::new(map).expect("map accessor");
        assert_eq!(map.key(0), Some(Term::atom(Atom::OK)));
        assert_eq!(map.value(0), Some(Term::small_int(8)));
    }

    #[test]
    fn decodes_invalid_input_as_errors() {
        let atoms = AtomTable::with_common_atoms();
        let mut heap = heap();
        assert_eq!(
            decode_term(&[], &mut heap, &atoms),
            Err(DecodeError::EmptyInput)
        );
        assert_eq!(
            decode_term(&[130], &mut heap, &atoms),
            Err(DecodeError::BadVersion(130))
        );
        assert_eq!(
            decode_term(&[131], &mut heap, &atoms),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[131, 255], &mut heap, &atoms),
            Err(DecodeError::UnsupportedTag(255))
        );
        assert_eq!(
            decode_term(&[131, 97, 1, 2], &mut heap, &atoms),
            Err(DecodeError::TrailingBytes)
        );
        assert_eq!(
            decode_term(&[131, 119, 1, 0xFF], &mut heap, &atoms),
            Err(DecodeError::InvalidUtf8)
        );
        assert_eq!(
            decode_term(&[131, 104, 2, 97, 1], &mut heap, &atoms),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[131, 108, 0, 0, 0, 1, 97, 1], &mut heap, &atoms),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[131, 116, 0, 0, 0, 1, 97, 1], &mut heap, &atoms),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[131, 109, 0, 0, 0, 4, 1], &mut heap, &atoms),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_term(&[131, 88, 97, 1], &mut heap, &atoms),
            Err(DecodeError::InvalidExternalNode)
        );
        assert_eq!(
            decode_term(&[131, 90, 0, 0], &mut heap, &atoms),
            Err(DecodeError::InvalidReferenceArity(0))
        );
    }

    #[test]
    fn encodes_and_decodes_external_pid_and_reference() {
        let atoms = AtomTable::with_common_atoms();
        let node = atoms.intern("remote@host");
        let mut heap = heap();
        let mut pid_heap = [0_u64; 4];
        let pid = write_external_pid(&mut pid_heap, node, 10, 20).expect("external pid");
        let encoded_pid = encode_term(pid, &atoms);
        assert_eq!(
            encoded_pid,
            vec![
                131, 88, 119, 11, b'r', b'e', b'm', b'o', b't', b'e', b'@', b'h', b'o', b's', b't',
                0, 0, 0, 10, 0, 0, 0, 20, 0, 0, 0, 0
            ]
        );
        let decoded_pid = decode_term(&encoded_pid, &mut heap, &atoms).expect("decode pid");
        let decoded_pid = ExternalPid::new(decoded_pid).expect("external pid accessor");
        assert_eq!(decoded_pid.node(), Some(node));
        assert_eq!(decoded_pid.pid_number(), 10);
        assert_eq!(decoded_pid.serial(), 20);

        let mut reference_heap = [0_u64; 3];
        let reference = write_external_reference(&mut reference_heap, node, 0x1122_3344_5566_7788)
            .expect("external reference");
        let encoded_reference = encode_term(reference, &atoms);
        let decoded_reference =
            decode_term(&encoded_reference, &mut heap, &atoms).expect("decode reference");
        let decoded_reference =
            ExternalReference::new(decoded_reference).expect("external ref accessor");
        assert_eq!(decoded_reference.node(), Some(node));
        assert_eq!(decoded_reference.id(), 0x1122_3344_5566_7788);
    }

    #[test]
    fn frames_and_deframes_distribution_messages() {
        let control = vec![131, 97, 7];
        let frame = write_dist_message(&control, None);
        assert_eq!(&frame[..4], &[0, 0, 0, 4]);
        assert_eq!(frame[4], PASS_THROUGH);
        let (decoded_control, payload) = read_dist_message(&mut frame.as_slice()).expect("deframe");
        assert_eq!(decoded_control, control);
        assert_eq!(payload, None);

        let payload_bytes = b"payload";
        let frame = write_dist_message(&control, Some(payload_bytes));
        assert_eq!(&frame[..4], &[0, 0, 0, 11]);
        let (decoded_control, payload) =
            read_dist_message(&mut frame.as_slice()).expect("deframe payload");
        assert_eq!(decoded_control, control);
        assert_eq!(payload, Some(payload_bytes.to_vec()));
    }

    #[test]
    fn deframe_rejects_bad_frames() {
        assert_eq!(
            read_dist_message(&mut [0_u8, 0, 0].as_slice()),
            Err(Error::TruncatedHeader)
        );
        assert_eq!(
            read_dist_message(&mut [0_u8, 0, 0, 2, PASS_THROUGH].as_slice()),
            Err(Error::TruncatedBody {
                expected: 2,
                actual: 1
            })
        );
        assert_eq!(
            read_dist_message(&mut [0_u8, 0, 0, 0].as_slice()),
            Err(Error::EmptyBody)
        );
        assert_eq!(
            read_dist_message(&mut [0_u8, 0, 0, 1, b'x'].as_slice()),
            Err(Error::InvalidPassThrough(b'x'))
        );
        assert_eq!(
            read_dist_message(&mut [0_u8, 0, 0, 2, PASS_THROUGH, 131].as_slice()),
            Err(Error::InvalidControl(DecodeError::Truncated))
        );
    }
}
