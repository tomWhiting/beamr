//! Segment handling for the OTP 25+ `bs_create_bin` instruction.
//!
//! Decoded operands are `[Fail, Alloc, Live, Unit, Dst, List]` where the
//! list carries six operands per segment:
//! `[Type, Seg, Unit, Flags, Src, Size]`.
//!
//! Segment type atoms (`integer`, `binary`, `float`, `string`, `append`,
//! `private_append`, `utf16`, `utf32`) arrive as module-local atom values
//! whose names are not resolvable here (no atom table flows through opcode
//! dispatch), so segments are classified structurally:
//!
//! * `Size` = atom `undefined` (stable common atom) marks a UTF segment;
//!   `utf8` is identifiable via the stable common atom, while `utf16`/`utf32`
//!   cannot be told apart without atom names and raise `badarg`.
//! * Any other atom `Size` means `all`: the whole source binary is copied
//!   (covers `append`, `private_append`, and `binary` with size `all`).
//! * A numeric `Size` dispatches on the source operand: a string-table
//!   offset or binary literal copies bytes, otherwise the runtime type of
//!   the source term selects integer, float, or sized-binary encoding.
//!
//! Construction flags are `nil` or a literal list such as `[little]`;
//! `big` is normalised away and `signed` dropped by the compiler, so a
//! non-empty flag list selects little-endian (`native` is little-endian on
//! every supported target).

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::Literal;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BigInt, Float};
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};

use crate::interpreter::opcodes::core;

use super::super::matching::{Endian, literal_bytes};
use super::super::{heap_slice, jump_label};

/// Operand count for one `bs_create_bin` segment specification.
const SEGMENT_FIELDS: usize = 6;

/// Execute a decoded `bs_create_bin` instruction in its compiler-emitted
/// `[Fail, Alloc, Live, Unit, Dst, List]` form.
pub(super) fn bs_create_bin(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let [fail, _alloc, _live, _unit, destination, Operand::List(fields)] = operands else {
        return Err(ExecError::InvalidOperand("bs_create_bin operands"));
    };
    match construct(process, module, fields) {
        Ok(bytes) => {
            let binary = allocate_binary(process, &bytes)?;
            core::write_term(process, destination, binary)?;
            Ok(InstructionOutcome::Continue)
        }
        Err(ExecError::Badarg) => match fail {
            Operand::Label(label) if *label != 0 => jump_label(module, fail),
            _ => Err(ExecError::Badarg),
        },
        Err(other) => Err(other),
    }
}

/// Build the byte content for all segments without touching the heap.
fn construct(
    process: &Process,
    module: &Module,
    fields: &[Operand],
) -> Result<Vec<u8>, ExecError> {
    if !fields.len().is_multiple_of(SEGMENT_FIELDS) {
        return Err(ExecError::InvalidOperand("bs_create_bin segment"));
    }
    let mut writer = BitWriter::default();
    for segment in fields.chunks_exact(SEGMENT_FIELDS) {
        append_segment(process, module, &mut writer, segment)?;
    }
    writer.into_bytes()
}

fn append_segment(
    process: &Process,
    module: &Module,
    writer: &mut BitWriter,
    segment: &[Operand],
) -> Result<(), ExecError> {
    let [type_atom, _seg, unit, flags, source, size] = segment else {
        return Err(ExecError::InvalidOperand("bs_create_bin segment"));
    };
    match size {
        Operand::Atom(Some(atom)) if *atom == Atom::UNDEFINED => {
            if matches!(type_atom, Operand::Atom(Some(t)) if *t == Atom::UTF8) {
                push_utf8(writer, core::read_term(process, module, source)?)
            } else {
                // utf16/utf32 segments need atom-name resolution that the
                // opcode dispatch does not provide yet; raise rather than
                // encode with the wrong width.
                Err(ExecError::Badarg)
            }
        }
        // Any other atom size is `all`: copy the entire source binary.
        Operand::Atom(Some(_)) => {
            push_whole_binary(writer, core::read_term(process, module, source)?)
        }
        _ => {
            let bits = segment_bits(process, module, size, unit)?;
            let endian = segment_endian(module, flags);
            match source {
                // String segments reference the module string table by
                // offset; integer immediates always decode as `Integer`.
                Operand::Unsigned(_) => push_literal_bytes(module, writer, source, bits),
                Operand::Literal(index)
                    if matches!(
                        module.literals.get(*index),
                        Some(Literal::Binary(_) | Literal::String(_))
                    ) =>
                {
                    push_literal_bytes(module, writer, source, bits)
                }
                _ => {
                    let term = core::read_term(process, module, source)?;
                    push_term(writer, term, bits, endian)
                }
            }
        }
    }
}

/// Append a sized segment whose source is a runtime term, dispatching on
/// the term's type (integer, float, or binary).
fn push_term(
    writer: &mut BitWriter,
    term: Term,
    bits: usize,
    endian: Endian,
) -> Result<(), ExecError> {
    if let Some(float) = Float::new(term) {
        return push_float(writer, float.value(), bits, endian);
    }
    if let Some(binary) = BinaryRef::new(term) {
        let bytes = binary.as_bytes();
        if bits > bytes.len() * u8::BITS as usize {
            return Err(ExecError::Badarg);
        }
        writer.push_bits(bytes, bits);
        return Ok(());
    }
    push_integer(writer, term, bits, endian)
}

fn push_literal_bytes(
    module: &Module,
    writer: &mut BitWriter,
    source: &Operand,
    bits: usize,
) -> Result<(), ExecError> {
    let bytes = literal_bytes(module, source, bits.div_ceil(u8::BITS as usize))?;
    writer.push_bits(bytes, bits);
    Ok(())
}

fn push_whole_binary(writer: &mut BitWriter, term: Term) -> Result<(), ExecError> {
    let binary = BinaryRef::new(term).ok_or(ExecError::Badarg)?;
    let bytes = binary.as_bytes();
    writer.push_bits(bytes, bytes.len() * u8::BITS as usize);
    Ok(())
}

fn push_utf8(writer: &mut BitWriter, term: Term) -> Result<(), ExecError> {
    let code = term.as_small_int().ok_or(ExecError::Badarg)?;
    let code = u32::try_from(code).map_err(|_| ExecError::Badarg)?;
    let character = char::from_u32(code).ok_or(ExecError::Badarg)?;
    let mut buffer = [0_u8; 4];
    let encoded = character.encode_utf8(&mut buffer);
    writer.push_bits(encoded.as_bytes(), encoded.len() * u8::BITS as usize);
    Ok(())
}

fn push_integer(
    writer: &mut BitWriter,
    term: Term,
    bits: usize,
    endian: Endian,
) -> Result<(), ExecError> {
    if bits == 0 {
        // Still badarg if the source is not an integer at all.
        return integer_le_bytes(term, 1).map(|_| ());
    }
    let len = bits.div_ceil(u8::BITS as usize);
    let le = integer_le_bytes(term, len)?;
    match endian {
        Endian::Little => {
            let full = bits / u8::BITS as usize;
            writer.push_bits(&le[..full], full * u8::BITS as usize);
            let rem = bits % u8::BITS as usize;
            if rem != 0 {
                // The most significant fragment is stored last, MSB-first.
                writer.push_bits(&[le[full] << (u8::BITS as usize - rem)], rem);
            }
        }
        Endian::Big => {
            let mut be: Vec<u8> = le.iter().rev().copied().collect();
            shift_left(&mut be, len * u8::BITS as usize - bits);
            writer.push_bits(&be, bits);
        }
    }
    Ok(())
}

/// Two's-complement little-endian bytes of an integer term, truncated or
/// sign-extended to `len` bytes (matching BEAM construction semantics,
/// where oversized values wrap instead of failing).
fn integer_le_bytes(term: Term, len: usize) -> Result<Vec<u8>, ExecError> {
    if let Some(value) = term.as_small_int() {
        let fill = if value < 0 { 0xff } else { 0x00 };
        let mut out = vec![fill; len];
        let le = value.to_le_bytes();
        let copy = len.min(le.len());
        out[..copy].copy_from_slice(&le[..copy]);
        return Ok(out);
    }
    let big = BigInt::new(term).ok_or(ExecError::Badarg)?;
    let mut out = vec![0_u8; len];
    for (limb_index, limb) in big.limbs().iter().enumerate() {
        for (byte_index, byte) in limb.to_le_bytes().iter().enumerate() {
            let index = limb_index * std::mem::size_of::<u64>() + byte_index;
            if index < len {
                out[index] = *byte;
            }
        }
    }
    if big.is_negative() {
        let mut carry = true;
        for byte in &mut out {
            *byte = !*byte;
            if carry {
                let (sum, overflow) = byte.overflowing_add(1);
                *byte = sum;
                carry = overflow;
            }
        }
    }
    Ok(out)
}

fn push_float(
    writer: &mut BitWriter,
    value: f64,
    bits: usize,
    endian: Endian,
) -> Result<(), ExecError> {
    let mut bytes = match bits {
        64 => value.to_bits().to_be_bytes().to_vec(),
        32 => {
            let narrowed = value as f32;
            if value.is_finite() && !narrowed.is_finite() {
                return Err(ExecError::Badarg);
            }
            narrowed.to_bits().to_be_bytes().to_vec()
        }
        16 => {
            let half = f16_bits(value).ok_or(ExecError::Badarg)?;
            half.to_be_bytes().to_vec()
        }
        _ => return Err(ExecError::Badarg),
    };
    if matches!(endian, Endian::Little) {
        bytes.reverse();
    }
    writer.push_bits(&bytes, bits);
    Ok(())
}

/// Convert to IEEE 754 half precision, refusing finite values that would
/// overflow to infinity (BEAM raises `badarg` for those).
fn f16_bits(value: f64) -> Option<u16> {
    let bits = (value as f32).to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;
    if exponent == 0xff {
        return Some(sign | 0x7c00 | (u16::from(mantissa != 0) * 0x0200));
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1f {
        return None;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return Some(sign);
        }
        let mantissa = mantissa | 0x0080_0000;
        let shift = 14 - half_exponent;
        return Some(sign | (mantissa >> shift) as u16);
    }
    Some(sign | ((half_exponent as u16) << 10) | (mantissa >> 13) as u16)
}

/// Resolve a segment's size-in-bits from its `Size` and `Unit` operands.
fn segment_bits(
    process: &Process,
    module: &Module,
    size: &Operand,
    unit: &Operand,
) -> Result<usize, ExecError> {
    let size = match size {
        Operand::Unsigned(value) => usize::try_from(*value).map_err(|_| ExecError::Badarg)?,
        Operand::Integer(value) => usize::try_from(*value).map_err(|_| ExecError::Badarg)?,
        Operand::X(_) | Operand::Y(_) | Operand::TypedRegister { .. } | Operand::Literal(_) => {
            let term = core::read_term(process, module, size)?;
            let value = term.as_small_int().ok_or(ExecError::Badarg)?;
            usize::try_from(value).map_err(|_| ExecError::Badarg)?
        }
        _ => return Err(ExecError::Badarg),
    };
    let unit = core::operand_usize(unit, "bs_create_bin segment unit")?;
    size.checked_mul(unit).ok_or(ExecError::Badarg)
}

/// Determine segment endianness from the `Flags` operand.
///
/// Real bytecode carries `nil` or a literal list such as `[little]` or
/// `[native]`; the compiler normalises `big` away and drops `signed` for
/// construction, so any non-empty list selects little-endian (all supported
/// targets are little-endian, making `native` equivalent).
fn segment_endian(module: &Module, flags: &Operand) -> Endian {
    match flags {
        Operand::Literal(index) => match module.literals.get(*index) {
            Some(Literal::List(items, _)) if !items.is_empty() => Endian::Little,
            Some(Literal::Integer(value)) if value & 0x02 != 0 => Endian::Little,
            _ => Endian::Big,
        },
        other => Endian::from_flags(other),
    }
}

/// Allocate an empty binary, used as the result of zero-operand
/// `bs_init_writable`.
pub(super) fn empty_binary(process: &mut Process) -> Result<Term, ExecError> {
    allocate_binary(process, &[])
}

fn allocate_binary(process: &mut Process, bytes: &[u8]) -> Result<Term, ExecError> {
    let words = alloc_binary_word_count(bytes.len());
    if process.heap().available() < words {
        return Err(ExecError::GcNeeded {
            requested: words,
            available: process.heap().available(),
        });
    }
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    let heap = heap_slice(ptr, words);
    alloc_binary(heap, bytes).ok_or(ExecError::Badarg)
}

/// Accumulates segment payloads at bit granularity, MSB-first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit_len: usize,
}

impl BitWriter {
    /// Append the first `bit_len` bits of `source`, MSB-first.
    fn push_bits(&mut self, source: &[u8], bit_len: usize) {
        let mut remaining = bit_len;
        for &byte in source {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(u8::BITS as usize);
            self.push_byte_bits(byte, take);
            remaining -= take;
        }
    }

    /// Append the top `count` bits (1..=8) of `byte`.
    fn push_byte_bits(&mut self, byte: u8, count: usize) {
        let masked = byte & high_mask(count);
        let offset = self.bit_len % u8::BITS as usize;
        if offset == 0 {
            self.bytes.push(masked);
        } else if let Some(last) = self.bytes.last_mut() {
            *last |= masked >> offset;
            if offset + count > u8::BITS as usize {
                self.bytes.push(masked << (u8::BITS as usize - offset));
            }
        }
        self.bit_len += count;
    }

    /// Finish writing, requiring a whole number of bytes (sub-byte results
    /// would be bitstrings, which this runtime cannot represent yet).
    fn into_bytes(mut self) -> Result<Vec<u8>, ExecError> {
        if !self.bit_len.is_multiple_of(u8::BITS as usize) {
            return Err(ExecError::Badarg);
        }
        self.bytes.truncate(self.bit_len / u8::BITS as usize);
        Ok(self.bytes)
    }
}

/// Mask keeping the top `count` bits of a byte.
fn high_mask(count: usize) -> u8 {
    if count >= u8::BITS as usize {
        0xff
    } else {
        !(0xff_u8 >> count)
    }
}

/// Shift a big-endian byte string left by `shift` bits (`shift < 8`).
fn shift_left(bytes: &mut [u8], shift: usize) {
    if shift == 0 {
        return;
    }
    let mut carry = 0_u8;
    for byte in bytes.iter_mut().rev() {
        let original = *byte;
        *byte = (original << shift) | carry;
        carry = original >> (u8::BITS as usize - shift);
    }
}

#[cfg(test)]
mod tests;
