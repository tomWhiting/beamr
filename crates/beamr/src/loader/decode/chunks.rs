use std::io::{Read, Take};

use flate2::read::ZlibDecoder;

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;

use super::compact::CompactDecoder;
use super::etf::decode_external_term;
use super::MAX_TABLE_ENTRIES;

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
    /// Stable identifier derived from module/function/arity/free-count.
    ///
    /// Raw FunT decoding does not know the module name, so this is populated
    /// by the loader after the atom chunk has identified the module.
    pub unique_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineInfo {
    pub file: u32,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Integer(i64),
    Float(f64),
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
    // Each atom is at least a 1-byte length prefix, so the count cannot exceed
    // the remaining bytes — reject an attacker count before preallocating.
    ensure_count(count, cursor.remaining().len(), "atom count")?;
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
    // Each import entry is 3 × u32 = 12 bytes.
    ensure_count(count, cursor.remaining().len() / 12, "import count")?;
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
    // Each export entry is 3 × u32 = 12 bytes.
    ensure_count(count, cursor.remaining().len() / 12, "export count")?;
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
    // Each lambda (FunT) entry is 6 × u32 = 24 bytes.
    ensure_count(count, cursor.remaining().len() / 24, "lambda count")?;
    let mut lambdas = Vec::with_capacity(count);
    for _ in 0..count {
        let function = resolve_atom(cursor.read_u32()?, atoms)?;
        let arity = checked_arity(cursor.read_u32()?)?;
        let label = cursor.read_u32()?;
        let _index = cursor.read_u32()?;
        let num_free = cursor.read_u32()?;
        let _old_uniq = cursor.read_u32()?;
        lambdas.push(LambdaEntry {
            function,
            arity,
            label,
            num_free,
            unique_id: 0,
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
    // Each line is at least one compact-encoded byte.
    ensure_count(num_lines as usize, cursor.remaining().len(), "line count")?;
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
        decompress_bounded(cursor.remaining(), uncompressed_size as usize)?
    };

    let mut literal_cursor = Cursor::new(&payload);
    let count = literal_cursor.read_u32()? as usize;
    // Each literal carries at least a 4-byte size prefix.
    ensure_count(count, literal_cursor.remaining().len() / 4, "literal count")?;
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
        let literal = decode_external_term(&mut term_cursor, atom_table, 0)?;
        term_cursor.expect_empty("literal ETF")?;
        literals.push(literal);
    }
    literal_cursor.expect_empty("literal")?;
    Ok(literals)
}

/// Inflate a zlib stream, refusing to materialise more than the declared
/// uncompressed size. `ZlibDecoder` over `read_to_end` is *unbounded*: a small
/// highly-compressible payload (a "zip bomb") inflates to gigabytes before any
/// size check fires. Bounding the reader to `declared_size + 1` means an
/// oversized stream errors out at the cap instead of exhausting memory.
fn decompress_bounded(bytes: &[u8], declared_size: usize) -> Result<Vec<u8>, LoadError> {
    let limit = u64::try_from(declared_size)
        .map_err(|_| LoadError::DecodeError("literal declared size overflow".into()))?;
    let decoder = ZlibDecoder::new(bytes);
    // Read at most declared_size + 1: any overflow byte trips the check below.
    let mut reader: Take<ZlibDecoder<&[u8]>> = decoder.take(limit.saturating_add(1));
    let mut out = Vec::with_capacity(declared_size.min(1024 * 1024));
    reader
        .read_to_end(&mut out)
        .map_err(|error| LoadError::DecodeError(format!("zlib literal decode failed: {error}")))?;
    if out.len() != declared_size {
        return Err(LoadError::DecodeError(format!(
            "literal chunk decompressed to {} bytes, expected {declared_size}",
            out.len()
        )));
    }
    Ok(out)
}

/// Validate a length-prefixed count read from untrusted bytes before it is used
/// to preallocate. `feasible` is the maximum number of elements the remaining
/// input could possibly contain (remaining bytes / min bytes-per-element). A
/// crafted `u32` count would otherwise force a multi-gigabyte
/// `Vec::with_capacity` before the element read loop ever fails on truncation.
fn ensure_count(count: usize, feasible: usize, label: &str) -> Result<(), LoadError> {
    if count > MAX_TABLE_ENTRIES {
        return Err(LoadError::DecodeError(format!("{label} exceeds limit")));
    }
    if count > feasible {
        return Err(LoadError::DecodeError(format!(
            "{label} impossible for payload size"
        )));
    }
    Ok(())
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

/// Bounds-checked byte cursor shared by the chunk decoders and the ETF
/// sub-decoder (`super::etf`). Visible to the parent module so the ETF decoder
/// in its sibling file can reuse the same truncation-safe reads.
pub(super) struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(super) fn remaining(&self) -> &'a [u8] {
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

    pub(super) fn read_u8(&mut self) -> Result<u8, LoadError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| LoadError::DecodeError("truncated chunk data".into()))?;
        self.offset += 1;
        Ok(byte)
    }

    pub(super) fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
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

    pub(super) fn read_u16(&mut self) -> Result<u16, LoadError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn read_i32(&mut self) -> Result<i32, LoadError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_literal_chunk_decodes_new_float_ext() {
        let atom_table = AtomTable::with_common_atoms();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&10u32.to_be_bytes());
        bytes.push(131);
        bytes.push(70);
        bytes.extend_from_slice(&1.5f64.to_bits().to_be_bytes());

        let literals = decode_literal_chunk(&bytes, &atom_table).expect("literal chunk");
        assert_eq!(literals, vec![Literal::Float(1.5)]);
    }

    #[test]
    fn decode_literal_chunk_rejects_truncated_new_float_ext() {
        let atom_table = AtomTable::with_common_atoms();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&9u32.to_be_bytes());
        bytes.push(131);
        bytes.push(70);
        bytes.extend_from_slice(&1.5f64.to_bits().to_be_bytes()[..7]);

        assert!(decode_literal_chunk(&bytes, &atom_table).is_err());
    }

    #[test]
    fn decode_import_chunk_rejects_impossible_count_without_huge_alloc() {
        // 4-byte header declares u32::MAX import entries (each 12 bytes) but the
        // body is empty. Must reject before `Vec::with_capacity` ever runs.
        let bytes = u32::MAX.to_be_bytes().to_vec();
        match decode_import_chunk(&bytes, &[]) {
            Err(LoadError::DecodeError(message)) => assert!(
                message.contains("exceeds limit") || message.contains("impossible"),
                "unexpected error: {message}"
            ),
            other => panic!("expected count-limit DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn decode_atom_chunk_rejects_impossible_count_without_huge_alloc() {
        // Positive i32 count of ~2.1 billion atoms, empty body.
        let bytes = i32::MAX.to_be_bytes().to_vec();
        let atom_table = AtomTable::with_common_atoms();
        assert!(matches!(
            decode_atom_chunk(&bytes, &atom_table),
            Err(LoadError::DecodeError(_))
        ));
    }

    #[test]
    fn decode_literal_chunk_rejects_decompression_bomb() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        // Compress 1 MiB of zeros (a tiny zlib stream, ~1000:1 ratio) but
        // declare an uncompressed size of only 16 bytes. The bounded decoder
        // must error at the cap rather than inflating the full megabyte.
        let inflated = vec![0u8; 1024 * 1024];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&inflated).expect("zlib write");
        let compressed = encoder.finish().expect("zlib finish");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes()); // declared uncompressed size
        bytes.extend_from_slice(&compressed);

        let atom_table = AtomTable::with_common_atoms();
        match decode_literal_chunk(&bytes, &atom_table) {
            Err(LoadError::DecodeError(message)) => assert!(
                message.contains("decompressed to"),
                "unexpected error: {message}"
            ),
            other => panic!("expected bounded decompression DecodeError, got {other:?}"),
        }
    }
}
