use std::env;
use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

const MAGIC: &[u8] = b"BEAMR_EMBED\0";
const VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4;
const ZSTD_LEVEL: i32 = 3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=BEAMR_EMBED_DIR");

    let out_dir = env::var_os("OUT_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OUT_DIR is not set"))?;
    let archive_path = Path::new(&out_dir).join("embedded_archive.bin");

    let archive = match env::var_os("BEAMR_EMBED_DIR") {
        Some(dir) => {
            let dir = Path::new(&dir);
            println!("cargo:rerun-if-changed={}", dir.display());
            archive_pack(dir)?
        }
        None => empty_archive_bytes(),
    };

    fs::write(archive_path, archive)?;
    Ok(())
}

fn archive_pack(beam_dir: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
        println!("cargo:rerun-if-changed={}", path.display());
        modules.push((name.to_owned(), path));
    }
    modules.sort_by(|left, right| left.0.cmp(&right.0));

    let mut packed = empty_archive_bytes();
    let count = u32::try_from(modules.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("too many embedded modules ({})", modules.len()),
        )
    })?;
    packed[MAGIC.len() + 1..HEADER_LEN].copy_from_slice(&count.to_le_bytes());

    for (name, path) in modules {
        let bytes = fs::read(&path)?;
        let compressed = zstd::stream::encode_all(Cursor::new(bytes), ZSTD_LEVEL)?;
        write_entry(&mut packed, &name, &compressed)?;
    }

    Ok(packed)
}

fn empty_archive_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes
}

fn write_entry(output: &mut Vec<u8>, name: &str, compressed: &[u8]) -> io::Result<()> {
    let name_len = u16::try_from(name.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded module name {name:?} is too long"),
        )
    })?;
    let beam_len = u32::try_from(compressed.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded module {name:?} compressed payload is too long"),
        )
    })?;
    output.extend_from_slice(&name_len.to_le_bytes());
    output.extend_from_slice(name.as_bytes());
    output.extend_from_slice(&beam_len.to_le_bytes());
    output.extend_from_slice(compressed);
    Ok(())
}
