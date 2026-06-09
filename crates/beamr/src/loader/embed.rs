//! Embedded `.beam` archive support.
//!
//! Archives contain zstd-compressed raw BEAM files. Runtime loading decompresses
//! each payload back to the original `.beam` bytes and then uses the normal BEAM
//! parser/loader path.

use std::fmt;
use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

use crate::atom::AtomTable;
use crate::error::LoadError;
use crate::module::{Module, ModuleOrigin, ModuleRegistry};
use crate::native::BifRegistry;
use crate::native::CapabilityPolicy;

use super::load::{
    UnresolvedImportReport, load_module_with_origin, load_module_with_origin_and_policy,
};

/// Archive magic number: `BEAMR_EMBED\0`.
pub const MAGIC: &[u8] = b"BEAMR_EMBED\0";
/// Current embedded archive format version.
pub const VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4;
const ZSTD_LEVEL: i32 = 3;

static EMBEDDED_ARCHIVE_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/embedded_archive.bin"));

/// Errors returned by embedded archive parsing, packing, and decompression.
#[derive(Debug)]
pub enum EmbedError {
    /// The archive header, version, lengths, count, or UTF-8 names are invalid.
    InvalidArchive,
    /// Filesystem IO failed while packing an archive.
    Io(io::Error),
    /// zstd compression or decompression failed.
    Compression(io::Error),
    /// A module name cannot be represented by this archive format.
    ModuleNameTooLong { name: String, len: usize },
    /// A compressed BEAM payload cannot be represented by this archive format.
    BeamDataTooLong { module: String, len: usize },
    /// The archive contains too many modules for its u32 count field.
    TooManyModules { count: usize },
}

impl Clone for EmbedError {
    fn clone(&self) -> Self {
        match self {
            Self::InvalidArchive => Self::InvalidArchive,
            Self::Io(error) => Self::Io(io::Error::new(error.kind(), error.to_string())),
            Self::Compression(error) => {
                Self::Compression(io::Error::new(error.kind(), error.to_string()))
            }
            Self::ModuleNameTooLong { name, len } => Self::ModuleNameTooLong {
                name: name.clone(),
                len: *len,
            },
            Self::BeamDataTooLong { module, len } => Self::BeamDataTooLong {
                module: module.clone(),
                len: *len,
            },
            Self::TooManyModules { count } => Self::TooManyModules { count: *count },
        }
    }
}

impl PartialEq for EmbedError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InvalidArchive, Self::InvalidArchive) => true,
            (Self::Io(left), Self::Io(right)) => left.kind() == right.kind(),
            (Self::Compression(left), Self::Compression(right)) => left.kind() == right.kind(),
            (
                Self::ModuleNameTooLong {
                    name: left_name,
                    len: left_len,
                },
                Self::ModuleNameTooLong {
                    name: right_name,
                    len: right_len,
                },
            ) => left_name == right_name && left_len == right_len,
            (
                Self::BeamDataTooLong {
                    module: left_module,
                    len: left_len,
                },
                Self::BeamDataTooLong {
                    module: right_module,
                    len: right_len,
                },
            ) => left_module == right_module && left_len == right_len,
            (Self::TooManyModules { count: left }, Self::TooManyModules { count: right }) => {
                left == right
            }
            _ => false,
        }
    }
}

impl Eq for EmbedError {}

impl fmt::Display for EmbedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArchive => write!(formatter, "invalid embedded BEAM archive"),
            Self::Io(error) => write!(formatter, "embedded archive IO error: {error}"),
            Self::Compression(error) => write!(formatter, "embedded archive zstd error: {error}"),
            Self::ModuleNameTooLong { name, len } => write!(
                formatter,
                "embedded module name {name:?} is too long ({len} bytes)"
            ),
            Self::BeamDataTooLong { module, len } => write!(
                formatter,
                "embedded module {module:?} compressed payload is too long ({len} bytes)"
            ),
            Self::TooManyModules { count } => {
                write!(formatter, "too many embedded modules ({count})")
            }
        }
    }
}

impl std::error::Error for EmbedError {}

impl From<io::Error> for EmbedError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug)]
struct EmbeddedEntry<'a> {
    name: &'a str,
    compressed_beam: &'a [u8],
}

/// Borrowed view of an embedded BEAM archive.
#[derive(Clone, Debug)]
pub struct EmbeddedArchive<'a> {
    entries: Vec<EmbeddedEntry<'a>>,
}

impl<'a> EmbeddedArchive<'a> {
    /// Parse an archive from bytes, validating magic, version, count, names, and bounds.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, EmbedError> {
        if bytes.len() < HEADER_LEN {
            return Err(EmbedError::InvalidArchive);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(EmbedError::InvalidArchive);
        }
        let mut cursor = MAGIC.len();
        if bytes[cursor] != VERSION {
            return Err(EmbedError::InvalidArchive);
        }
        cursor += 1;
        let count = read_u32(bytes, &mut cursor)? as usize;
        let mut entries = Vec::new();

        for _ in 0..count {
            let name_len = read_u16(bytes, &mut cursor)? as usize;
            let name_bytes = read_slice(bytes, &mut cursor, name_len)?;
            let name = std::str::from_utf8(name_bytes).map_err(|_| EmbedError::InvalidArchive)?;
            let beam_len = read_u32(bytes, &mut cursor)? as usize;
            let compressed_beam = read_slice(bytes, &mut cursor, beam_len)?;
            entries.push(EmbeddedEntry {
                name,
                compressed_beam,
            });
        }

        if cursor != bytes.len() {
            return Err(EmbedError::InvalidArchive);
        }

        Ok(Self { entries })
    }

    /// Iterate over `(module_name, compressed_beam)` pairs in archive order.
    pub fn modules(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.entries
            .iter()
            .map(|entry| (entry.name, entry.compressed_beam))
    }

    /// Return all embedded module names in archive order.
    #[must_use]
    pub fn module_names(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.name).collect()
    }

    /// Decompress and return raw `.beam` bytes for `module_name`.
    pub fn get(&self, module_name: &str) -> Option<Vec<u8>> {
        self.entries
            .iter()
            .find(|entry| entry.name == module_name)
            .and_then(|entry| zstd::stream::decode_all(Cursor::new(entry.compressed_beam)).ok())
    }
}

/// Parse the statically linked embedded archive.
pub fn embedded_archive() -> Result<EmbeddedArchive<'static>, EmbedError> {
    EmbeddedArchive::parse(EMBEDDED_ARCHIVE_BYTES)
}

/// Return all statically embedded module names without loading them.
#[must_use]
pub fn embedded_module_names() -> Vec<&'static str> {
    embedded_archive()
        .map(|archive| archive.module_names())
        .unwrap_or_default()
}

/// Decompress a module from the statically linked archive, if it exists.
#[must_use]
pub fn embedded_module_bytes(module_name: &str) -> Option<Vec<u8>> {
    embedded_archive()
        .ok()
        .and_then(|archive| archive.get(module_name))
}

/// Load a module by name from the statically embedded archive.
pub fn load_embedded_module(
    module_name: &str,
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
) -> Result<Option<(std::sync::Arc<Module>, UnresolvedImportReport)>, LoadError> {
    let Some(bytes) = embedded_module_bytes(module_name) else {
        return Ok(None);
    };
    load_module_with_origin(
        &bytes,
        atom_table,
        module_registry,
        bif_registry,
        ModuleOrigin::Embedded,
    )
    .map(Some)
}

/// Load a module by name from the statically embedded archive with an explicit capability policy.
pub fn load_embedded_module_with_policy(
    module_name: &str,
    atom_table: &AtomTable,
    module_registry: &ModuleRegistry,
    bif_registry: &impl BifRegistry,
    capability_policy: &dyn CapabilityPolicy,
) -> Result<Option<(std::sync::Arc<Module>, UnresolvedImportReport)>, LoadError> {
    let Some(bytes) = embedded_module_bytes(module_name) else {
        return Ok(None);
    };
    load_module_with_origin_and_policy(
        &bytes,
        atom_table,
        module_registry,
        bif_registry,
        capability_policy,
        ModuleOrigin::Embedded,
    )
    .map(Some)
}

/// Pack all `.beam` files in `beam_dir` into the embedded archive format.
pub fn archive_pack(beam_dir: &Path) -> Result<Vec<u8>, EmbedError> {
    let mut modules = Vec::new();
    for entry in fs::read_dir(beam_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("beam") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        modules.push((name.to_owned(), path));
    }
    modules.sort_by(|left, right| left.0.cmp(&right.0));

    let mut packed = empty_archive_bytes();
    let count = u32::try_from(modules.len()).map_err(|_| EmbedError::TooManyModules {
        count: modules.len(),
    })?;
    packed[MAGIC.len() + 1..HEADER_LEN].copy_from_slice(&count.to_le_bytes());

    for (name, path) in modules {
        let bytes = fs::read(&path)?;
        let compressed = zstd::stream::encode_all(Cursor::new(bytes), ZSTD_LEVEL)
            .map_err(EmbedError::Compression)?;
        write_entry(&mut packed, &name, &compressed)?;
    }

    Ok(packed)
}

/// Return a valid empty embedded archive.
#[must_use]
pub fn empty_archive_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes
}

fn write_entry(output: &mut Vec<u8>, name: &str, compressed: &[u8]) -> Result<(), EmbedError> {
    let name_len = u16::try_from(name.len()).map_err(|_| EmbedError::ModuleNameTooLong {
        name: name.to_owned(),
        len: name.len(),
    })?;
    let beam_len = u32::try_from(compressed.len()).map_err(|_| EmbedError::BeamDataTooLong {
        module: name.to_owned(),
        len: compressed.len(),
    })?;
    output.extend_from_slice(&name_len.to_le_bytes());
    output.extend_from_slice(name.as_bytes());
    output.extend_from_slice(&beam_len.to_le_bytes());
    output.extend_from_slice(compressed);
    Ok(())
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, EmbedError> {
    let slice = read_slice(bytes, cursor, 2)?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, EmbedError> {
    let slice = read_slice(bytes, cursor, 4)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_slice<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], EmbedError> {
    let end = cursor.checked_add(len).ok_or(EmbedError::InvalidArchive)?;
    let slice = bytes.get(*cursor..end).ok_or(EmbedError::InvalidArchive)?;
    *cursor = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn pack_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = empty_archive_bytes();
        let count = u32::try_from(entries.len()).expect("test module count fits u32");
        bytes[MAGIC.len() + 1..HEADER_LEN].copy_from_slice(&count.to_le_bytes());
        for (name, beam) in entries {
            let compressed = zstd::stream::encode_all(Cursor::new(*beam), ZSTD_LEVEL)
                .expect("test zstd compression succeeds");
            write_entry(&mut bytes, name, &compressed).expect("test entry is encodable");
        }
        bytes
    }

    fn temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        dir.push(format!("beamr_embed_{name}_{nanos}"));
        fs::create_dir(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn parse_empty_bytes_returns_invalid_archive() {
        assert_eq!(EmbeddedArchive::parse(&[]), Err(EmbedError::InvalidArchive));
    }

    #[test]
    fn invalid_magic_or_version_returns_invalid_archive() {
        let mut bytes = empty_archive_bytes();
        bytes[0] = b'X';
        assert_eq!(
            EmbeddedArchive::parse(&bytes),
            Err(EmbedError::InvalidArchive)
        );

        let mut bytes = empty_archive_bytes();
        bytes[MAGIC.len()] = 2;
        assert_eq!(
            EmbeddedArchive::parse(&bytes),
            Err(EmbedError::InvalidArchive)
        );
    }

    #[test]
    fn archive_round_trip_returns_original_beam_bytes() {
        let archive_bytes = pack_entries(&[
            ("alpha", b"alpha beam"),
            ("beta", b"beta beam bytes"),
            ("gamma", b"\0beam\xffdata"),
        ]);
        let archive = EmbeddedArchive::parse(&archive_bytes).expect("archive parses");

        assert_eq!(archive.get("alpha"), Some(b"alpha beam".to_vec()));
        assert_eq!(archive.get("beta"), Some(b"beta beam bytes".to_vec()));
        assert_eq!(archive.get("gamma"), Some(b"\0beam\xffdata".to_vec()));
        assert_eq!(archive.get("missing"), None);
    }

    #[test]
    fn archive_pack_sorts_beam_files_and_skips_non_beam_files() {
        let dir = temp_dir("sorted");
        fs::write(dir.join("zeta.beam"), b"z").expect("write zeta");
        fs::write(dir.join("alpha.beam"), b"a").expect("write alpha");
        fs::write(dir.join("middle.beam"), b"m").expect("write middle");
        fs::write(dir.join("ignore.txt"), b"ignored").expect("write ignored");

        let packed = archive_pack(&dir).expect("pack archive");
        assert!(packed.starts_with(MAGIC));
        let count_offset = MAGIC.len() + 1;
        let count = u32::from_le_bytes([
            packed[count_offset],
            packed[count_offset + 1],
            packed[count_offset + 2],
            packed[count_offset + 3],
        ]);
        assert_eq!(count, 3);

        let archive = EmbeddedArchive::parse(&packed).expect("archive parses");
        assert_eq!(archive.module_names(), vec!["alpha", "middle", "zeta"]);
        assert_eq!(archive.get("alpha"), Some(b"a".to_vec()));
        assert_eq!(archive.get("middle"), Some(b"m".to_vec()));
        assert_eq!(archive.get("zeta"), Some(b"z".to_vec()));

        fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[test]
    fn archive_pack_empty_directory_is_valid_empty_archive() {
        let dir = temp_dir("empty");
        let packed = archive_pack(&dir).expect("pack empty archive");
        assert_eq!(packed, empty_archive_bytes());
        let archive = EmbeddedArchive::parse(&packed).expect("empty archive parses");
        assert_eq!(archive.module_names(), Vec::<&str>::new());
        fs::remove_dir_all(dir).expect("cleanup temp dir");
    }
}
