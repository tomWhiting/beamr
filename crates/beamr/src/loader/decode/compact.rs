use crate::atom::Atom;
use crate::error::LoadError;
use crate::term::Term;
use crate::term::bigint_convert::{from_twos_complement_be, to_sign_magnitude_le};

use super::Literal;
use super::budget::DecodeBudget;

/// Decoded compact operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Integer(i64),
    Unsigned(u64),
    Atom(Option<Atom>),
    X(u32),
    Y(u32),
    Label(u32),
    Character(u64),
    Literal(usize),
    List(Vec<Operand>),
    FloatRegister(u32),
    Allocation(Vec<Allocation>),
    TypedRegister {
        register: Box<Operand>,
        type_index: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allocation {
    Words(u64),
    Floats(u64),
    Funs(u64),
    Unknown { tag: u64, value: u64 },
}

/// One decoded compact-encoded value: either an `i64` or, for payloads wider
/// than eight bytes, a borrowed big-endian two's-complement byte slice.
enum CompactValue<'a> {
    Small(i64),
    Big(&'a [u8]),
}

pub(crate) struct CompactDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    atoms: &'a [Atom],
    literals: &'a [Literal],
    /// Big integer operands materialised while decoding; they become
    /// constant-pool literals appended after the module's `LitT` entries.
    extra_literals: Vec<Literal>,
    /// Bounds memory spent on oversized integer operands from hostile input.
    budget: DecodeBudget,
}

impl<'a> CompactDecoder<'a> {
    pub(crate) fn new(bytes: &'a [u8], atoms: &'a [Atom], literals: &'a [Literal]) -> Self {
        Self {
            bytes,
            offset: 0,
            atoms,
            literals,
            extra_literals: Vec::new(),
            budget: DecodeBudget::default(),
        }
    }

    /// Returns the big-integer literals materialised so far, clearing them.
    ///
    /// Operands reference these by index `literals.len() + position`, so the
    /// caller must append them after the module's decoded literal table.
    pub(crate) fn take_extra_literals(&mut self) -> Vec<Literal> {
        std::mem::take(&mut self.extra_literals)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    pub(crate) fn offset(&self) -> usize {
        self.offset
    }

    pub(crate) fn read_opcode(&mut self) -> Result<u8, LoadError> {
        self.read_byte()
    }

    pub(crate) fn read_operand(&mut self) -> Result<Operand, LoadError> {
        let (tag, value) = self.read_tagged_value()?;
        let value = match value {
            CompactValue::Small(value) => value,
            // Only signed integer operands (tag 1) may exceed eight bytes;
            // they become constant-pool big integer literals.
            CompactValue::Big(payload) if tag == 1 => return self.big_integer_operand(payload),
            CompactValue::Big(payload) => {
                return Err(LoadError::DecodeError(format!(
                    "compact integer with {} bytes is too large for tag {tag}",
                    payload.len()
                )));
            }
        };
        // BEAM compact term tags per the OTP standard:
        //   0 = unsigned literal, 1 = integer (signed), 2 = atom,
        //   3 = X register, 4 = Y register, 5 = label,
        //   6 = character, 7 = extended
        match tag {
            0 => Ok(Operand::Unsigned(unsigned_u64(
                value,
                "unsigned compact operand",
            )?)),
            1 => Ok(self.integer_operand(value)),
            2 => self.atom_operand(value),
            3 => unsigned_u32(value, "x register").map(Operand::X),
            4 => unsigned_u32(value, "y register").map(Operand::Y),
            5 => unsigned_u32(value, "label").map(Operand::Label),
            6 => Ok(Operand::Character(unsigned_u64(value, "character")?)),
            7 => self.read_extended(value),
            other => Err(LoadError::DecodeError(format!(
                "unsupported compact tag {other}"
            ))),
        }
    }

    pub(crate) fn read_raw_compact_i64(&mut self) -> Result<(u8, i64), LoadError> {
        self.read_tagged_integer()
    }

    fn read_extended(&mut self, subtag: i64) -> Result<Operand, LoadError> {
        match subtag {
            1 => {
                let len = self.read_unsigned()?;
                let mut operands = Vec::with_capacity(usize_from_u32(len, "extended list length")?);
                for _ in 0..len {
                    operands.push(self.read_operand()?);
                }
                Ok(Operand::List(operands))
            }
            2 => self.read_unsigned().map(Operand::FloatRegister),
            3 => {
                let len = self.read_unsigned()?;
                let mut entries =
                    Vec::with_capacity(usize_from_u32(len, "allocation list length")?);
                for _ in 0..len {
                    let tag = self.read_unsigned_u64()?;
                    let value = self.read_unsigned_u64()?;
                    let entry = match tag {
                        0 => Allocation::Words(value),
                        1 => Allocation::Floats(value),
                        2 => Allocation::Funs(value),
                        other => Allocation::Unknown { tag: other, value },
                    };
                    entries.push(entry);
                }
                Ok(Operand::Allocation(entries))
            }
            0 | 4 => {
                let index = self.read_unsigned_u64()?;
                let index = usize_from_u64(index, "literal index")?;
                if index >= self.literals.len() {
                    return Err(LoadError::DecodeError(format!(
                        "literal index {index} out of range"
                    )));
                }
                Ok(Operand::Literal(index))
            }
            5 => {
                let register = self.read_operand()?;
                let type_index = self.read_unsigned_u64()?;
                Ok(Operand::TypedRegister {
                    register: Box::new(register),
                    type_index,
                })
            }
            other => Err(LoadError::DecodeError(format!(
                "unsupported compact extended tag {other}"
            ))),
        }
    }

    fn atom_operand(&self, value: i64) -> Result<Operand, LoadError> {
        if value == 0 {
            return Ok(Operand::Atom(None));
        }
        let index = value
            .checked_sub(1)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| LoadError::DecodeError(format!("atom index {value} out of range")))?;
        let atom =
            self.atoms.get(index).copied().ok_or_else(|| {
                LoadError::DecodeError(format!("atom index {value} out of range"))
            })?;
        Ok(Operand::Atom(Some(atom)))
    }

    fn read_unsigned(&mut self) -> Result<u32, LoadError> {
        let (tag, value) = self.read_tagged_integer()?;
        if tag != 0 {
            return Err(LoadError::DecodeError(format!(
                "expected unsigned compact operand, got tag {tag}"
            )));
        }
        unsigned_u32(value, "unsigned compact operand")
    }

    fn read_unsigned_u64(&mut self) -> Result<u64, LoadError> {
        let (tag, value) = self.read_tagged_integer()?;
        if tag != 0 {
            return Err(LoadError::DecodeError(format!(
                "expected unsigned compact operand, got tag {tag}"
            )));
        }
        unsigned_u64(value, "unsigned compact operand")
    }

    fn read_tagged_integer(&mut self) -> Result<(u8, i64), LoadError> {
        match self.read_tagged_value()? {
            (tag, CompactValue::Small(value)) => Ok((tag, value)),
            // Raw compact readers (atom lengths, line numbers, nested counts)
            // never accept multi-word values.
            (_, CompactValue::Big(payload)) => Err(LoadError::DecodeError(format!(
                "compact integer with {} bytes is too large for this context",
                payload.len()
            ))),
        }
    }

    fn read_tagged_value(&mut self) -> Result<(u8, CompactValue<'a>), LoadError> {
        let first = self.read_byte()?;
        let tag = first & 0x07;
        let value = if (first & 0x08) == 0 {
            // Single-byte: 4-bit value in bits [7:4].
            CompactValue::Small(i64::from(first >> 4))
        } else if (first & 0x10) == 0 {
            // Two-byte: 11-bit value from bits [7:5] of byte 1 and all of byte 2.
            let high = i64::from(first >> 5);
            let low = i64::from(self.read_byte()?);
            CompactValue::Small((high << 8) | low)
        } else {
            let descriptor = first >> 5;
            let byte_count = if descriptor < 7 {
                usize::from(descriptor) + 2
            } else {
                let extra_len = self.read_unsigned()?;
                usize_from_u32(extra_len, "compact big integer length")?
                    .checked_add(9)
                    .ok_or_else(|| LoadError::DecodeError("compact integer too large".into()))?
            };
            if byte_count <= 8 {
                CompactValue::Small(self.read_small_signed(byte_count, tag == 1)?)
            } else {
                CompactValue::Big(self.read_bytes(byte_count)?)
            }
        };
        Ok((tag, value))
    }

    fn read_small_signed(&mut self, byte_count: usize, signed: bool) -> Result<i64, LoadError> {
        debug_assert!(byte_count <= 8);
        let bytes = self.read_bytes(byte_count)?;
        let negative = signed && bytes.first().is_some_and(|byte| (byte & 0x80) != 0);
        let fill = if negative { 0xff } else { 0x00 };
        let mut out = [fill; 8];
        let start = 8 - byte_count;
        out[start..].copy_from_slice(bytes);
        Ok(i64::from_be_bytes(out))
    }

    /// Wraps a signed integer operand, diverting values that cannot live in a
    /// small-integer immediate into the constant pool.
    fn integer_operand(&mut self, value: i64) -> Operand {
        if Term::try_small_int(value).is_some() {
            Operand::Integer(value)
        } else {
            self.push_extra_literal(Literal::Integer(value))
        }
    }

    /// Converts an oversized (more than eight bytes) signed compact integer
    /// payload into an operand, materialising a constant-pool literal when the
    /// value cannot live in a small-integer immediate.
    fn big_integer_operand(&mut self, payload: &[u8]) -> Result<Operand, LoadError> {
        self.budget.charge_node()?;
        self.budget.charge_bytes(payload.len())?;
        let value = from_twos_complement_be(payload);
        // Non-minimal encodings can still hold word-sized values; demote them
        // so equality against runtime small integers stays canonical.
        if let Some(small) = value.to_small_i64() {
            return Ok(self.integer_operand(small));
        }
        let (negative, magnitude_le) = to_sign_magnitude_le(&value);
        let mut bytes = Vec::with_capacity(1 + magnitude_le.len());
        bytes.push(u8::from(negative));
        bytes.extend_from_slice(&magnitude_le);
        Ok(self.push_extra_literal(Literal::BigInteger(bytes)))
    }

    fn push_extra_literal(&mut self, literal: Literal) -> Operand {
        let index = self.literals.len() + self.extra_literals.len();
        self.extra_literals.push(literal);
        Operand::Literal(index)
    }

    fn read_byte(&mut self) -> Result<u8, LoadError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| LoadError::DecodeError("truncated compact term".into()))?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("compact term offset overflow".into()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| LoadError::DecodeError("truncated compact term".into()))?;
        self.offset = end;
        Ok(slice)
    }
}

pub(crate) fn unsigned_u32(value: i64, context: &str) -> Result<u32, LoadError> {
    u32::try_from(value)
        .map_err(|_| LoadError::DecodeError(format!("{context} value {value} out of range")))
}

pub(crate) fn unsigned_u64(value: i64, context: &str) -> Result<u64, LoadError> {
    u64::try_from(value)
        .map_err(|_| LoadError::DecodeError(format!("{context} value {value} out of range")))
}

fn usize_from_u32(value: u32, context: &str) -> Result<usize, LoadError> {
    usize::try_from(value)
        .map_err(|_| LoadError::DecodeError(format!("{context} {value} out of range")))
}

fn usize_from_u64(value: u64, context: &str) -> Result<usize, LoadError> {
    usize::try_from(value)
        .map_err(|_| LoadError::DecodeError(format!("{context} {value} out of range")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPRO: i128 = 100_000_000_000_000_000_000; // 10^20, needs 9 bytes.

    /// Encodes a signed compact integer (tag 1) with an explicit payload width.
    fn encode_signed(value: i128, byte_count: usize) -> Vec<u8> {
        assert!((9..=16).contains(&byte_count), "test helper covers 9..=16");
        let mut bytes = vec![
            0b1111_1001, // tag 1, extended-size descriptor
            ((byte_count - 9) as u8) << 4,
        ];
        bytes.extend_from_slice(&value.to_be_bytes()[16 - byte_count..]);
        bytes
    }

    fn decode_one(bytes: &[u8], literals: &[Literal]) -> (Operand, Vec<Literal>) {
        let mut decoder = CompactDecoder::new(bytes, &[], literals);
        let operand = decoder.read_operand().expect("operand decodes");
        assert!(decoder.is_empty(), "all bytes must be consumed");
        (operand, decoder.take_extra_literals())
    }

    fn magnitude_le(value: i128) -> Vec<u8> {
        let mut bytes = value.unsigned_abs().to_le_bytes().to_vec();
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        bytes
    }

    #[test]
    fn nine_byte_positive_integer_becomes_big_literal_operand() {
        let (operand, extras) = decode_one(&encode_signed(REPRO, 9), &[]);
        assert_eq!(operand, Operand::Literal(0));
        let mut expected = vec![0_u8];
        expected.extend(magnitude_le(REPRO));
        assert_eq!(extras, vec![Literal::BigInteger(expected)]);
    }

    #[test]
    fn nine_byte_negative_integer_keeps_sign_through_twos_complement() {
        let (operand, extras) = decode_one(&encode_signed(-REPRO, 9), &[]);
        assert_eq!(operand, Operand::Literal(0));
        let mut expected = vec![1_u8];
        expected.extend(magnitude_le(REPRO));
        assert_eq!(extras, vec![Literal::BigInteger(expected)]);
    }

    #[test]
    fn big_literal_indices_start_after_existing_literal_table() {
        let existing = vec![Literal::Nil, Literal::Nil];
        let (operand, extras) = decode_one(&encode_signed(REPRO, 10), &existing);
        assert_eq!(operand, Operand::Literal(2));
        assert_eq!(extras.len(), 1);
    }

    #[test]
    fn non_minimal_wide_encoding_of_small_value_stays_inline() {
        let (operand, extras) = decode_one(&encode_signed(5, 9), &[]);
        assert_eq!(operand, Operand::Integer(5));
        assert!(extras.is_empty());
        let (operand, extras) = decode_one(&encode_signed(-5, 12), &[]);
        assert_eq!(operand, Operand::Integer(-5));
        assert!(extras.is_empty());
    }

    #[test]
    fn word_sized_value_beyond_small_range_becomes_integer_literal() {
        // Eight-byte payload (descriptor 6): fits i64 but not a small immediate.
        let value = 1_i64 << 62;
        let mut bytes = vec![0b1101_1001];
        bytes.extend_from_slice(&value.to_be_bytes());
        let (operand, extras) = decode_one(&bytes, &[]);
        assert_eq!(operand, Operand::Literal(0));
        assert_eq!(extras, vec![Literal::Integer(value)]);
    }

    #[test]
    fn nine_byte_value_that_fits_i64_demotes_to_integer_literal() {
        let (operand, extras) = decode_one(&encode_signed(i128::from(i64::MIN), 9), &[]);
        assert_eq!(operand, Operand::Literal(0));
        assert_eq!(extras, vec![Literal::Integer(i64::MIN)]);
    }

    #[test]
    fn raw_compact_reads_still_reject_oversized_integers() {
        let bytes = encode_signed(REPRO, 9);
        let mut decoder = CompactDecoder::new(&bytes, &[], &[]);
        let error = decoder.read_raw_compact_i64().expect_err("must reject");
        assert!(matches!(error, LoadError::DecodeError(message) if message.contains("too large")));
    }

    #[test]
    fn oversized_unsigned_operands_are_rejected() {
        let mut bytes = vec![0b1111_1000, 0x00]; // tag 0, 9-byte payload
        bytes.extend_from_slice(&REPRO.to_be_bytes()[7..]);
        let mut decoder = CompactDecoder::new(&bytes, &[], &[]);
        let error = decoder.read_operand().expect_err("must reject");
        assert!(matches!(error, LoadError::DecodeError(message) if message.contains("too large")));
    }

    #[test]
    fn truncated_big_integer_payload_is_rejected() {
        let mut bytes = encode_signed(REPRO, 9);
        bytes.truncate(bytes.len() - 1);
        let mut decoder = CompactDecoder::new(&bytes, &[], &[]);
        assert!(decoder.read_operand().is_err());
    }
}
