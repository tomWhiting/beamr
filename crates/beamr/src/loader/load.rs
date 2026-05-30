use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;

use super::decode::{
    ExportEntry, ImportEntry, Instruction, LambdaEntry, LineInfo, Literal, decode_atom_chunk,
    decode_code_chunk, decode_export_chunk, decode_import_chunk, decode_lambda_chunk,
    decode_line_chunk, decode_literal_chunk, decode_string_chunk,
};
use super::parser::parse_beam_chunks;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModule {
    pub name: Atom,
    pub atoms: Vec<Atom>,
    pub instructions: Vec<Instruction>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    pub lambdas: Vec<LambdaEntry>,
    pub literals: Vec<Literal>,
    pub string_table: Vec<u8>,
    pub line_info: Vec<LineInfo>,
}

pub fn load_beam_chunks(bytes: &[u8], atom_table: &AtomTable) -> Result<ParsedModule, LoadError> {
    let chunks = parse_beam_chunks(bytes)?;

    let atom_chunk = find_chunk(&chunks, b"AtU8")
        .or_else(|| find_chunk(&chunks, b"Atom"))
        .ok_or_else(|| LoadError::MissingChunk("Atom/AtU8".into()))?;
    let atoms = decode_atom_chunk(atom_chunk, atom_table)?;
    let name = atoms
        .first()
        .copied()
        .ok_or_else(|| LoadError::DecodeError("atom chunk is empty".into()))?;

    let literals = match find_chunk(&chunks, b"LitT") {
        Some(bytes) => decode_literal_chunk(bytes, atom_table)?,
        None => Vec::new(),
    };

    let code_chunk =
        find_chunk(&chunks, b"Code").ok_or_else(|| LoadError::MissingChunk("Code".into()))?;
    let instructions = decode_code_chunk(code_chunk, &atoms, &literals)?;

    let imports = match find_chunk(&chunks, b"ImpT") {
        Some(bytes) => decode_import_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let exports = match find_chunk(&chunks, b"ExpT") {
        Some(bytes) => decode_export_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let lambdas = match find_chunk(&chunks, b"FunT") {
        Some(bytes) => decode_lambda_chunk(bytes, &atoms)?,
        None => Vec::new(),
    };
    let string_table = find_chunk(&chunks, b"StrT")
        .map(decode_string_chunk)
        .unwrap_or_default();
    let line_info = match find_chunk(&chunks, b"Line") {
        Some(bytes) => decode_line_chunk(bytes)?,
        None => Vec::new(),
    };

    Ok(ParsedModule {
        name,
        atoms,
        instructions,
        imports,
        exports,
        lambdas,
        literals,
        string_table,
        line_info,
    })
}

fn find_chunk<'a>(chunks: &'a [([u8; 4], &'a [u8])], tag: &[u8; 4]) -> Option<&'a [u8]> {
    chunks
        .iter()
        .find_map(|(chunk_tag, bytes)| (chunk_tag == tag).then_some(*bytes))
}
