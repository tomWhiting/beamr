use std::io::{Read, Take};

use flate2::read::ZlibDecoder;

use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;

use super::bounded::BoundedCursor;
use super::budget::DecodeBudget;
use super::compact::CompactDecoder;
use super::etf::decode_external_term;

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

pub(crate) fn decode_atom_chunk(
    bytes: &[u8],
    atom_table: &AtomTable,
    budget: &mut DecodeBudget,
) -> Result<Vec<Atom>, LoadError> {
    let mut cursor = BoundedCursor::new(bytes);
    let raw_count = cursor.read_i32()?;
    let count = raw_count.unsigned_abs() as usize;
    let compact_lengths = raw_count < 0;
    // Each atom is at least a 1-byte length prefix, so the count cannot exceed
    // the remaining bytes — reject an attacker count before preallocating.
    cursor.ensure_count(count, 1, "atom count")?;
    budget.charge_bytes(
        count
            .checked_mul(std::mem::size_of::<Atom>())
            .ok_or_else(|| LoadError::DecodeError("atom table allocation size overflow".into()))?,
    )?;
    let mut atoms = Vec::with_capacity(count);

    for _ in 0..count {
        budget.charge_node()?;
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
        let atom = match atom_table.lookup(name) {
            Some(atom) => atom,
            None => {
                budget.charge_atom()?;
                atom_table.intern(name)
            }
        };
        atoms.push(atom);
    }

    if !cursor.remaining().is_empty() {
        return Err(LoadError::DecodeError("trailing atom chunk data".into()));
    }

    Ok(atoms)
}

pub(crate) fn decode_import_chunk(
    bytes: &[u8],
    atoms: &[Atom],
    budget: &mut DecodeBudget,
) -> Result<Vec<ImportEntry>, LoadError> {
    let mut cursor = BoundedCursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    // Each import entry is 3 × u32 = 12 bytes.
    cursor.ensure_count(count, 12, "import count")?;
    budget.charge_bytes(
        count
            .checked_mul(std::mem::size_of::<ImportEntry>())
            .ok_or_else(|| {
                LoadError::DecodeError("import table allocation size overflow".into())
            })?,
    )?;
    let mut imports = Vec::with_capacity(count);
    for _ in 0..count {
        budget.charge_node()?;
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

pub(crate) fn decode_export_chunk(
    bytes: &[u8],
    atoms: &[Atom],
    budget: &mut DecodeBudget,
) -> Result<Vec<ExportEntry>, LoadError> {
    let mut cursor = BoundedCursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    // Each export entry is 3 × u32 = 12 bytes.
    cursor.ensure_count(count, 12, "export count")?;
    budget.charge_bytes(
        count
            .checked_mul(std::mem::size_of::<ExportEntry>())
            .ok_or_else(|| {
                LoadError::DecodeError("export table allocation size overflow".into())
            })?,
    )?;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        budget.charge_node()?;
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

pub(crate) fn decode_lambda_chunk(
    bytes: &[u8],
    atoms: &[Atom],
    budget: &mut DecodeBudget,
) -> Result<Vec<LambdaEntry>, LoadError> {
    let mut cursor = BoundedCursor::new(bytes);
    let count = cursor.read_u32()? as usize;
    // Each lambda (FunT) entry is 6 × u32 = 24 bytes.
    cursor.ensure_count(count, 24, "lambda count")?;
    budget.charge_bytes(
        count
            .checked_mul(std::mem::size_of::<LambdaEntry>())
            .ok_or_else(|| {
                LoadError::DecodeError("lambda table allocation size overflow".into())
            })?,
    )?;
    let mut lambdas = Vec::with_capacity(count);
    for _ in 0..count {
        budget.charge_node()?;
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

pub(crate) fn decode_line_chunk(
    bytes: &[u8],
    budget: &mut DecodeBudget,
) -> Result<Vec<LineInfo>, LoadError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = BoundedCursor::new(bytes);
    let _version = cursor.read_u32()?;
    let _flags = cursor.read_u32()?;
    let _num_line_instrs = cursor.read_u32()?;
    let num_lines = cursor.read_u32()?;
    let _num_fnames = cursor.read_u32()?;
    let mut current_file = 0;
    // Each line is at least one compact-encoded byte.
    let num_lines = num_lines as usize;
    cursor.ensure_count(num_lines, 1, "line count")?;
    budget.charge_bytes(
        num_lines
            .checked_mul(std::mem::size_of::<LineInfo>())
            .ok_or_else(|| LoadError::DecodeError("line table allocation size overflow".into()))?,
    )?;
    let mut lines = Vec::with_capacity(num_lines);

    for _ in 0..num_lines {
        budget.charge_node()?;
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

    if lines.len() != num_lines {
        return Err(LoadError::DecodeError(format!(
            "Line chunk declared {num_lines} lines but decoded {}",
            lines.len()
        )));
    }
    Ok(lines)
}

pub(crate) fn decode_literal_chunk(
    bytes: &[u8],
    atom_table: &AtomTable,
    budget: &mut DecodeBudget,
) -> Result<Vec<Literal>, LoadError> {
    let mut cursor = BoundedCursor::new(bytes);
    let uncompressed_size = cursor.read_u32()?;
    let payload = if uncompressed_size == 0 {
        budget.charge_bytes(cursor.remaining_len())?;
        cursor.remaining().to_vec()
    } else {
        decompress_bounded(cursor.remaining(), uncompressed_size as usize, budget)?
    };

    let mut literal_cursor = BoundedCursor::new(&payload);
    let count = literal_cursor.read_u32()? as usize;
    // Each literal carries at least a 4-byte size prefix.
    literal_cursor.ensure_count(count, 4, "literal count")?;
    budget.charge_bytes(
        count
            .checked_mul(std::mem::size_of::<Literal>())
            .ok_or_else(|| {
                LoadError::DecodeError("literal table allocation size overflow".into())
            })?,
    )?;
    let mut literals = Vec::with_capacity(count);
    for _ in 0..count {
        budget.charge_node()?;
        let size = literal_cursor.read_u32()? as usize;
        let term_bytes = literal_cursor.read_bytes(size)?;
        let mut term_cursor = BoundedCursor::new(term_bytes);
        if term_cursor.read_u8()? != 131 {
            return Err(LoadError::DecodeError(
                "ETF literal missing version byte".into(),
            ));
        }
        let literal = decode_external_term(&mut term_cursor, atom_table, budget)?;
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
fn decompress_bounded(
    bytes: &[u8],
    declared_size: usize,
    budget: &mut DecodeBudget,
) -> Result<Vec<u8>, LoadError> {
    let limit = u64::try_from(budget.bytes_remaining)
        .map_err(|_| LoadError::DecodeError("literal budget size overflow".into()))?;
    let decoder = ZlibDecoder::new(bytes);
    // Read at most budget + 1: any overflow byte trips the check below.
    let mut reader: Take<ZlibDecoder<&[u8]>> = decoder.take(limit.saturating_add(1));
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|error| LoadError::DecodeError(format!("zlib literal decode failed: {error}")))?;
    if out.len() > budget.bytes_remaining {
        return Err(LoadError::DecodeError(
            "literal chunk exceeds decode byte budget".into(),
        ));
    }
    if out.len() != declared_size {
        return Err(LoadError::DecodeError(format!(
            "literal chunk decompressed to {} bytes, expected {declared_size}",
            out.len()
        )));
    }
    budget.charge_bytes(out.len())?;
    Ok(out)
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

        let mut budget = DecodeBudget::default();
        let literals =
            decode_literal_chunk(&bytes, &atom_table, &mut budget).expect("literal chunk");
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

        let mut budget = DecodeBudget::default();
        assert!(decode_literal_chunk(&bytes, &atom_table, &mut budget).is_err());
    }

    #[test]
    fn decode_import_chunk_rejects_impossible_count_without_huge_alloc() {
        // 4-byte header declares u32::MAX import entries (each 12 bytes) but the
        // body is empty. Must reject before `Vec::with_capacity` ever runs.
        let bytes = u32::MAX.to_be_bytes().to_vec();
        let mut budget = DecodeBudget::default();
        match decode_import_chunk(&bytes, &[], &mut budget) {
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
        let mut budget = DecodeBudget::default();
        assert!(matches!(
            decode_atom_chunk(&bytes, &atom_table, &mut budget),
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
        let mut budget = DecodeBudget::default();
        match decode_literal_chunk(&bytes, &atom_table, &mut budget) {
            Err(LoadError::DecodeError(message)) => assert!(
                message.contains("decompressed to") || message.contains("byte budget"),
                "unexpected error: {message}"
            ),
            other => panic!("expected bounded decompression DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn decode_literal_chunk_rejects_zlib_output_over_byte_budget() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let inflated = vec![0u8; 64];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&inflated).expect("zlib write");
        let compressed = encoder.finish().expect("zlib finish");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(inflated.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&compressed);

        let atom_table = AtomTable::with_common_atoms();
        let mut budget = DecodeBudget::new(16, 16, 8, 16);
        match decode_literal_chunk(&bytes, &atom_table, &mut budget) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("byte budget"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected byte-budget DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn decode_atom_chunk_rejects_excessive_unique_atoms() {
        let atom_table = AtomTable::with_common_atoms();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.push(1);
        bytes.push(b'a');
        bytes.push(1);
        bytes.push(b'b');

        let mut budget = DecodeBudget::new(16, 16, 1024, 1);
        match decode_atom_chunk(&bytes, &atom_table, &mut budget) {
            Err(LoadError::DecodeError(message)) => {
                assert!(
                    message.contains("atom budget"),
                    "unexpected error: {message}"
                );
            }
            other => panic!("expected atom-budget DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn decode_atom_chunk_existing_atoms_do_not_charge_atom_budget() {
        let atom_table = AtomTable::with_common_atoms();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3i32.to_be_bytes());
        for name in ["ok", "error", "true"] {
            bytes.push(name.len() as u8);
            bytes.extend_from_slice(name.as_bytes());
        }

        let mut budget = DecodeBudget::new(16, 16, 1024, 0);
        let atoms = decode_atom_chunk(&bytes, &atom_table, &mut budget).expect("existing atoms");
        assert_eq!(atoms.len(), 3);
    }
}
