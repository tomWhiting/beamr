//! Chunked .beam format parser.
//!
//! Parses the FOR1/BEAM header and iterates chunk headers (Atom/AtU8,
//! Code, StrT, ImpT, ExpT, FunT, LitT, Line). Each chunk is extracted
//! as raw bytes for downstream decoders. Validates the container
//! structure without interpreting chunk contents.

use crate::error::LoadError;

/// Four-byte BEAM chunk identifier, such as `Code`, `AtU8`, or `StrT`.
pub type FourCC = [u8; 4];

/// Parse a BEAM IFF container into borrowed raw chunk payloads.
pub fn parse_beam_chunks(bytes: &[u8]) -> Result<Vec<(FourCC, &[u8])>, LoadError> {
    if bytes.len() < 12 || &bytes[0..4] != b"FOR1" || &bytes[8..12] != b"BEAM" {
        return Err(LoadError::InvalidFormat);
    }

    let declared_size = read_u32_at(bytes, 4)? as usize;
    let total_size = declared_size
        .checked_add(8)
        .ok_or(LoadError::InvalidFormat)?;
    if total_size > bytes.len() || declared_size < 4 {
        return Err(LoadError::InvalidFormat);
    }

    let end = total_size;
    let mut offset = 12;
    let mut chunks = Vec::new();

    while offset < end {
        if end - offset < 8 {
            return Err(LoadError::InvalidFormat);
        }

        let tag = read_fourcc_at(bytes, offset)?;
        let length = read_u32_at(bytes, offset + 4)? as usize;
        let data_start = offset + 8;
        let data_end = data_start
            .checked_add(length)
            .ok_or(LoadError::InvalidFormat)?;
        if data_end > end {
            return Err(LoadError::InvalidFormat);
        }

        chunks.push((tag, &bytes[data_start..data_end]));

        let padding = (4 - (length % 4)) % 4;
        offset = data_end
            .checked_add(padding)
            .ok_or(LoadError::InvalidFormat)?;
        if offset > end {
            return Err(LoadError::InvalidFormat);
        }
    }

    Ok(chunks)
}

fn read_fourcc_at(bytes: &[u8], offset: usize) -> Result<FourCC, LoadError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(LoadError::InvalidFormat)?;
    let mut out = [0; 4];
    out.copy_from_slice(slice);
    Ok(out)
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, LoadError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(LoadError::InvalidFormat)?;
    let mut out = [0; 4];
    out.copy_from_slice(slice);
    Ok(u32::from_be_bytes(out))
}

#[cfg(test)]
mod tests {
    use super::parse_beam_chunks;
    use crate::atom::AtomTable;
    use crate::error::LoadError;
    use crate::loader::load_beam_chunks;

    #[test]
    fn parses_valid_container_chunks() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FOR1");
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(b"BEAM");
        bytes.extend_from_slice(b"StrT");
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(b"abc");
        bytes.push(0);

        let chunks = parse_beam_chunks(&bytes).expect("valid handcrafted BEAM container");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, *b"StrT");
        assert_eq!(chunks[0].1, b"abc");
    }

    #[test]
    fn rejects_bad_magic() {
        let bytes = b"NOPE\0\0\0\x04BEAM";
        assert_eq!(parse_beam_chunks(bytes), Err(LoadError::InvalidFormat));
    }

    #[test]
    fn rejects_wrong_form_type() {
        let bytes = b"FOR1\0\0\0\x04NOPE";
        assert_eq!(parse_beam_chunks(bytes), Err(LoadError::InvalidFormat));
    }

    #[test]
    fn loads_committed_fixture() {
        let bytes = include_bytes!("../../tests/fixtures/hello.beam");
        let table = AtomTable::with_common_atoms();
        let module = load_beam_chunks(bytes, &table).expect("fixture BEAM should decode");

        assert_eq!(module.atoms.len(), 7);
        assert_eq!(table.resolve(module.name), Some("hello"));
        assert_eq!(module.imports.len(), 3);
        assert!(module.instructions.len() > 10);

        let export_names = module
            .exports
            .iter()
            .filter_map(|entry| table.resolve(entry.function))
            .collect::<Vec<_>>();
        assert!(export_names.contains(&"main"));
        assert!(export_names.contains(&"add"));
    }
}
