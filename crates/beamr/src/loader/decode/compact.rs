use crate::atom::Atom;
use crate::error::LoadError;

use super::Literal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Integer(i64),
    Unsigned(u64),
    Atom(Option<Atom>),
    X(u32),
    Y(u32),
    Label(u32),
    Character(u64),
    Literal(Literal),
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

pub(crate) struct CompactDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    atoms: &'a [Atom],
    literals: &'a [Literal],
}

impl<'a> CompactDecoder<'a> {
    pub(crate) fn new(bytes: &'a [u8], atoms: &'a [Atom], literals: &'a [Literal]) -> Self {
        Self {
            bytes,
            offset: 0,
            atoms,
            literals,
        }
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
        let (tag, value) = self.read_tagged_integer()?;
        // BEAM compact term tags per the OTP standard:
        //   0 = unsigned literal, 1 = integer (signed), 2 = atom,
        //   3 = X register, 4 = Y register, 5 = label,
        //   6 = character, 7 = extended
        match tag {
            0 => Ok(Operand::Unsigned(unsigned_u64(
                value,
                "unsigned compact operand",
            )?)),
            1 => Ok(Operand::Integer(value)),
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
                let literal = self
                    .literals
                    .get(usize_from_u64(index, "literal index")?)
                    .cloned()
                    .ok_or_else(|| {
                        LoadError::DecodeError(format!("literal index {index} out of range"))
                    })?;
                Ok(Operand::Literal(literal))
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
        let first = self.read_byte()?;
        let tag = first & 0x07;
        let value = if (first & 0x08) == 0 {
            // Single-byte: 4-bit value in bits [7:4].
            i64::from(first >> 4)
        } else if (first & 0x10) == 0 {
            // Two-byte: 11-bit value from bits [7:5] of byte 1 and all of byte 2.
            let high = i64::from(first >> 5);
            let low = i64::from(self.read_byte()?);
            (high << 8) | low
        } else {
            let descriptor = first >> 5;
            if descriptor < 7 {
                let byte_count = usize::from(descriptor) + 2;
                self.read_big_signed(byte_count, tag == 1)?
            } else {
                let extra_len = self.read_unsigned()?;
                let byte_count = usize_from_u32(extra_len, "compact big integer length")?
                    .checked_add(9)
                    .ok_or_else(|| LoadError::DecodeError("compact integer too large".into()))?;
                self.read_big_signed(byte_count, tag == 1)?
            }
        };
        Ok((tag, value))
    }

    fn read_big_signed(&mut self, byte_count: usize, signed: bool) -> Result<i64, LoadError> {
        if byte_count > 8 {
            return Err(LoadError::DecodeError(format!(
                "compact integer with {byte_count} bytes is too large"
            )));
        }
        let bytes = self.read_bytes(byte_count)?;
        let negative = signed && bytes.first().is_some_and(|byte| (byte & 0x80) != 0);
        let fill = if negative { 0xff } else { 0x00 };
        let mut out = [fill; 8];
        let start = 8 - byte_count;
        out[start..].copy_from_slice(bytes);
        Ok(i64::from_be_bytes(out))
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
