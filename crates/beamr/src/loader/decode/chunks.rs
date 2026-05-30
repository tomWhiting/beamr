use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;

use super::compact::CompactDecoder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    pub module: Atom,
    pub function: Atom,
    pub arity: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    pub function: Atom,
    pub arity: u8,
    pub label: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaEntry {
    pub function: Atom,
    pub arity: u8,
    pub label: u32,
    pub num_free: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineInfo {
    pub file: u32,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Integer(i64),
    BigInteger(Vec<u8>),
    Atom(Atom),
    Binary(Vec<u8>),
    Tuple(Vec<Literal>),
    Nil,
    List(Vec<Literal>, Box<Literal>),
    Map(Vec<(Literal, Literal)>),
    String(Vec<u8>),
}

pub fn decode_atom_chunk(bytes: &[u8], atom_table: &AtomTable) -> Result<Vec<Atom>, LoadError> {
    let mut cursor = Cursor::new(bytes);
    let raw_count = cursor.read_i32()?;
    let count = raw_count.unsigned_abs() as usize;
    let compact_lengths = raw_count < 0;
    let mut atoms = Vec::with_capacity(count);

    for _ in 0..count {
        let len = if compact_lengths {
            let mut decoder = CompactDecoder::new(cursor.remaining(), &[], &[]);
            let (tag, value) = decoder.read_raw_compact_i64()?;
            if tag != 0 || value < 0 {
                return Err(LoadError::DecodeError("malformed AtU8 atom length".into()));
            }
            cursor.advance(decoder.offset())?;
            usize::try_from(value)
                .map_err(|_| LoadError::DecodeError("atom length out of range".into()))?
        } else {
            usize::from(cursor.read_u8()?)
        };
        let text = cursor.read_bytes(len)?;
        let name = std::str::from_utf8(text)
            .map_err(|_| LoadError::DecodeError("atom name is not valid UTF-8".into()))?;
        atoms.push(atom_table.intern(name));
    }

    if !cursor.remaining().is_empty() {
        return Err(LoadError::DecodeError("trailing atom chunk data".into()));
    }

    Ok(atoms)
}

pub fn decode_import_chunk(bytes: &[u8], atoms: &[Atom]) -> Result<Vec<ImportEntry>, LoadError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    let mut imports = Vec::with_capacity(count);
    for _ in 0..count {
        let module = resolve_atom(cursor.read_u32()?, atoms)?;
        let function = resolve_atom(cursor.read_u32()?, atoms)?;
        let arity = checked_arity(cursor.read_u32()?)?;
        imports.push(ImportEntry {
            module,
            function,
            arity,
        });
    }
    cursor.expect_empty("import")?;
    Ok(imports)
}

pub fn decode_export_chunk(bytes: &[u8], atoms: &[Atom]) -> Result<Vec<ExportEntry>, LoadError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let function = resolve_atom(cursor.read_u32()?, atoms)?;
        let arity = checked_arity(cursor.read_u32()?)?;
        let label = cursor.read_u32()?;
        exports.push(ExportEntry {
            function,
            arity,
            label,
        });
    }
    cursor.expect_empty("export")?;
    Ok(exports)
}

pub fn decode_lambda_chunk(bytes: &[u8], atoms: &[Atom]) -> Result<Vec<LambdaEntry>, LoadError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    let mut lambdas = Vec::with_capacity(count);
    for _ in 0..count {
        let function = resolve_atom(cursor.read_u32()?, atoms)?;
        let arity = checked_arity(cursor.read_u32()?)?;
        let label = cursor.read_u32()?;
        let _index = cursor.read_u32()?;
        let num_free = cursor.read_u32()?;
        let _old_uniq = cursor.read_u32()?;
        let _old_index = cursor.read_u32()?;
        lambdas.push(LambdaEntry {
            function,
            arity,
            label,
            num_free,
        });
    }
    cursor.expect_empty("lambda")?;
    Ok(lambdas)
}

pub fn decode_string_chunk(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

pub fn decode_line_chunk(bytes: &[u8]) -> Result<Vec<LineInfo>, LoadError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = Cursor::new(bytes);
    let _version = cursor.read_u32()?;
    let _flags = cursor.read_u32()?;
    let _num_line_instrs = cursor.read_u32()?;
    let num_lines = cursor.read_u32()?;
    let _num_fnames = cursor.read_u32()?;
    let mut current_file = 0;
    let mut lines = Vec::with_capacity(num_lines as usize);

    for _ in 0..num_lines {
        let mut decoder = CompactDecoder::new(cursor.remaining(), &[], &[]);
        let (tag, value) = decoder.read_raw_compact_i64()?;
        cursor.advance(decoder.offset())?;
        match tag {
            0 | 1 => lines.push(LineInfo {
                file: current_file,
                line: u32::try_from(value).map_err(|_| {
                    LoadError::DecodeError(format!("line number {value} out of range"))
                })?,
            }),
            2 => {
                if value < 0 {
                    return Err(LoadError::DecodeError(format!(
                        "line file index {value} out of range"
                    )));
                }
                current_file = u32::try_from(value).map_err(|_| {
                    LoadError::DecodeError(format!("line file index {value} out of range"))
                })?;
                let mut line_decoder = CompactDecoder::new(cursor.remaining(), &[], &[]);
                let (line_tag, line_value) = line_decoder.read_raw_compact_i64()?;
                cursor.advance(line_decoder.offset())?;
                if line_tag != 1 {
                    return Err(LoadError::DecodeError(format!(
                        "expected line number after file ref, got compact tag {line_tag}"
                    )));
                }
                lines.push(LineInfo {
                    file: current_file,
                    line: u32::try_from(line_value).map_err(|_| {
                        LoadError::DecodeError(format!("line number {line_value} out of range"))
                    })?,
                });
            }
            other => {
                return Err(LoadError::DecodeError(format!(
                    "unsupported line compact tag {other}"
                )));
            }
        }
    }

    if lines.len() != num_lines as usize {
        return Err(LoadError::DecodeError(format!(
            "Line chunk declared {num_lines} lines but decoded {}",
            lines.len()
        )));
    }
    Ok(lines)
}

pub fn decode_literal_chunk(
    bytes: &[u8],
    atom_table: &AtomTable,
) -> Result<Vec<Literal>, LoadError> {
    let mut cursor = Cursor::new(bytes);
    let uncompressed_size = cursor.read_u32()?;
    let payload = if uncompressed_size == 0 {
        cursor.remaining().to_vec()
    } else {
        let mut decoder = ZlibDecoder::new(cursor.remaining());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|error| {
            LoadError::DecodeError(format!("zlib literal decode failed: {error}"))
        })?;
        if out.len() != uncompressed_size as usize {
            return Err(LoadError::DecodeError(format!(
                "literal chunk decompressed to {} bytes, expected {uncompressed_size}",
                out.len()
            )));
        }
        out
    };

    let mut literal_cursor = Cursor::new(&payload);
    let count = literal_cursor.read_u32()? as usize;
    let mut literals = Vec::with_capacity(count);
    for _ in 0..count {
        let size = literal_cursor.read_u32()? as usize;
        let term_bytes = literal_cursor.read_bytes(size)?;
        let mut term_cursor = Cursor::new(term_bytes);
        if term_cursor.read_u8()? != 131 {
            return Err(LoadError::DecodeError(
                "ETF literal missing version byte".into(),
            ));
        }
        let literal = decode_external_term(&mut term_cursor, atom_table)?;
        term_cursor.expect_empty("literal ETF")?;
        literals.push(literal);
    }
    literal_cursor.expect_empty("literal")?;
    Ok(literals)
}

fn decode_external_term(
    cursor: &mut Cursor<'_>,
    atom_table: &AtomTable,
) -> Result<Literal, LoadError> {
    let tag = cursor.read_u8()?;
    match tag {
        97 => Ok(Literal::Integer(i64::from(cursor.read_u8()?))),
        98 => Ok(Literal::Integer(i64::from(cursor.read_i32()?))),
        100 | 118 => {
            let len = usize::from(cursor.read_u16()?);
            let bytes = cursor.read_bytes(len)?;
            decode_atom_literal(bytes, atom_table)
        }
        119 => {
            let len = usize::from(cursor.read_u8()?);
            let bytes = cursor.read_bytes(len)?;
            decode_atom_literal(bytes, atom_table)
        }
        104 => {
            let arity = usize::from(cursor.read_u8()?);
            decode_tuple(cursor, arity, atom_table)
        }
        105 => {
            let arity = cursor.read_u32()? as usize;
            decode_tuple(cursor, arity, atom_table)
        }
        106 => Ok(Literal::Nil),
        107 => {
            let len = usize::from(cursor.read_u16()?);
            Ok(Literal::String(cursor.read_bytes(len)?.to_vec()))
        }
        108 => {
            let len = cursor.read_u32()? as usize;
            let mut elements = Vec::with_capacity(len);
            for _ in 0..len {
                elements.push(decode_external_term(cursor, atom_table)?);
            }
            let tail = decode_external_term(cursor, atom_table)?;
            Ok(Literal::List(elements, Box::new(tail)))
        }
        109 => {
            let len = cursor.read_u32()? as usize;
            Ok(Literal::Binary(cursor.read_bytes(len)?.to_vec()))
        }
        110 | 111 => decode_big_integer(cursor, tag),
        116 => {
            let len = cursor.read_u32()? as usize;
            let mut pairs = Vec::with_capacity(len);
            for _ in 0..len {
                let key = decode_external_term(cursor, atom_table)?;
                let value = decode_external_term(cursor, atom_table)?;
                pairs.push((key, value));
            }
            Ok(Literal::Map(pairs))
        }
        other => Err(LoadError::DecodeError(format!(
            "unsupported ETF literal tag {other}"
        ))),
    }
}

fn decode_atom_literal(bytes: &[u8], atom_table: &AtomTable) -> Result<Literal, LoadError> {
    let name = std::str::from_utf8(bytes)
        .map_err(|_| LoadError::DecodeError("ETF atom is not valid UTF-8".into()))?;
    Ok(Literal::Atom(atom_table.intern(name)))
}

fn decode_tuple(
    cursor: &mut Cursor<'_>,
    arity: usize,
    atom_table: &AtomTable,
) -> Result<Literal, LoadError> {
    let mut elements = Vec::with_capacity(arity);
    for _ in 0..arity {
        elements.push(decode_external_term(cursor, atom_table)?);
    }
    Ok(Literal::Tuple(elements))
}

fn decode_big_integer(cursor: &mut Cursor<'_>, tag: u8) -> Result<Literal, LoadError> {
    let len = if tag == 110 {
        usize::from(cursor.read_u8()?)
    } else {
        cursor.read_u32()? as usize
    };
    let sign = cursor.read_u8()?;
    let bytes = cursor.read_bytes(len)?;
    if len <= 8 {
        let mut value: i128 = 0;
        for (shift, byte) in bytes.iter().enumerate() {
            value += i128::from(*byte) << (shift * 8);
        }
        if sign == 1 {
            value = -value;
        } else if sign != 0 {
            return Err(LoadError::DecodeError(format!(
                "invalid bignum sign {sign}"
            )));
        }
        i64::try_from(value)
            .map(Literal::Integer)
            .map_err(|_| LoadError::DecodeError(format!("ETF bignum {value} is outside i64 range")))
    } else {
        let mut out = Vec::with_capacity(len + 1);
        out.push(sign);
        out.extend_from_slice(bytes);
        Ok(Literal::BigInteger(out))
    }
}

fn resolve_atom(index: u32, atoms: &[Atom]) -> Result<Atom, LoadError> {
    if index == 0 {
        return Err(LoadError::DecodeError(
            "atom index 0 is invalid here".into(),
        ));
    }
    atoms
        .get((index - 1) as usize)
        .copied()
        .ok_or_else(|| LoadError::DecodeError(format!("atom index {index} out of range")))
}

fn checked_arity(value: u32) -> Result<u8, LoadError> {
    u8::try_from(value).map_err(|_| LoadError::DecodeError(format!("arity {value} out of range")))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn advance(&mut self, len: usize) -> Result<(), LoadError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("cursor offset overflow".into()))?;
        if end > self.bytes.len() {
            return Err(LoadError::DecodeError("truncated chunk data".into()));
        }
        self.offset = end;
        Ok(())
    }

    fn expect_empty(&self, name: &str) -> Result<(), LoadError> {
        if self.remaining().is_empty() {
            Ok(())
        } else {
            Err(LoadError::DecodeError(format!(
                "trailing {name} chunk data"
            )))
        }
    }

    fn read_u8(&mut self) -> Result<u8, LoadError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| LoadError::DecodeError("truncated chunk data".into()))?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("cursor offset overflow".into()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| LoadError::DecodeError("truncated chunk data".into()))?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u16(&mut self) -> Result<u16, LoadError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i32(&mut self) -> Result<i32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}
