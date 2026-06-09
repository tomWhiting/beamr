//! Ahead-of-time compilation support for BEAM modules.
//!
//! The current AOT bundle is a host-target-validated cache envelope around the
//! original BEAM bytes plus the function identities that compiled successfully.
//! Native pointers are intentionally not persisted because Cranelift JIT
//! function addresses are process-local. Loading a bundle validates the target
//! and module checksum, then recompiles the recorded functions through the same
//! [`JitCompiler`] path used by demand JIT compilation.

use super::cache::{JitCache, JitCacheKey};
use super::compiler::{JitCompiler, JitError, JitSettings};
use super::profiler::JitProfiler;
use crate::atom::{Atom, AtomTable};
use crate::error::LoadError;
use crate::loader::{ExportEntry, Instruction, ParsedModule, load_beam_chunks};
use crate::module::Module;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use super::{NativeCode, RootLocation, StackMapEntry};

const MAGIC: &[u8; 10] = b"BEAMR_AOT\0";
const VERSION: u8 = 1;

/// Host AOT compiler that reuses the normal untyped JIT pipeline.
pub struct AotCompiler {
    compiler: JitCompiler,
}

/// Result of compiling exported functions from one BEAM module.
pub struct AotResult {
    module: Atom,
    module_checksum: u64,
    module_bytes: Vec<u8>,
    compiled: Vec<(Atom, u8, NativeCode)>,
    skipped: Vec<(Atom, u8, String)>,
}

/// Serialized AOT cache bundle helpers.
pub struct NativeCodeBundle;

/// Error returned by AOT compilation, serialization, or cache loading.
#[derive(Debug)]
pub enum AotError {
    /// The `.beam` file could not be read.
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Source I/O error.
        source: std::io::Error,
    },
    /// BEAM parsing failed.
    Load(LoadError),
    /// The host JIT compiler could not be created or failed fatally.
    Jit(JitError),
    /// The bundle magic header is not recognised.
    InvalidMagic,
    /// The bundle version is not supported.
    UnsupportedVersion(u8),
    /// The bundle target hash does not match the requested target.
    TargetMismatch { expected: u64, actual: u64 },
    /// The bundle checksum does not match the current `.beam` bytes.
    ChecksumMismatch { expected: u64, actual: u64 },
    /// The bundle payload is truncated or malformed.
    Malformed(String),
    /// The bundle references a function absent from its embedded BEAM bytes.
    MissingFunction { function: Atom, arity: u8 },
}

impl AotCompiler {
    /// Creates an AOT compiler backed by [`JitCompiler`].
    pub fn new() -> Result<Self, AotError> {
        Ok(Self {
            compiler: JitCompiler::new(JitSettings).map_err(AotError::Jit)?,
        })
    }

    /// Compiles all exported functions in `beam_path`, skipping unsupported functions.
    pub fn compile_module(&self, beam_path: &Path) -> Result<AotResult, AotError> {
        let bytes = std::fs::read(beam_path).map_err(|source| AotError::Io {
            path: beam_path.to_path_buf(),
            source,
        })?;
        self.compile_module_bytes(bytes)
    }

    fn compile_module_bytes(&self, bytes: Vec<u8>) -> Result<AotResult, AotError> {
        let atom_table = AtomTable::with_common_atoms();
        let parsed = load_beam_chunks(&bytes, &atom_table).map_err(AotError::Load)?;
        let mut compiled = Vec::new();
        let mut skipped = Vec::new();

        for export in &parsed.exports {
            let instructions = match exported_instructions(&parsed, export) {
                Ok(instructions) => instructions,
                Err(error) => {
                    skipped.push((export.function, export.arity, error));
                    continue;
                }
            };

            match self
                .compiler
                .compile(instructions, parsed.name, export.function, export.arity)
            {
                Ok(native) => compiled.push((export.function, export.arity, native)),
                Err(error) if is_skippable_jit_error(&error) => {
                    eprintln!(
                        "beamr AOT: skipping {:?}/{}, {}",
                        export.function, export.arity, error
                    );
                    skipped.push((export.function, export.arity, error.to_string()));
                }
                Err(error) => return Err(AotError::Jit(error)),
            }
        }

        Ok(AotResult {
            module: parsed.name,
            module_checksum: module_checksum(&bytes),
            module_bytes: bytes,
            compiled,
            skipped,
        })
    }
}

impl AotResult {
    /// Module atom compiled by this result.
    #[must_use]
    pub const fn module(&self) -> Atom {
        self.module
    }

    /// Deterministic checksum of the source BEAM bytes.
    #[must_use]
    pub const fn module_checksum(&self) -> u64 {
        self.module_checksum
    }

    /// Compiled exported functions as function atom, arity, and native code.
    #[must_use]
    pub fn compiled_functions(&self) -> &[(Atom, u8, NativeCode)] {
        &self.compiled
    }

    /// Exported functions skipped by AOT as function atom, arity, and reason.
    #[must_use]
    pub fn skipped_functions(&self) -> &[(Atom, u8, String)] {
        &self.skipped
    }
}

impl NativeCodeBundle {
    /// Serializes an AOT result into the `.beamr_native` cache format.
    #[must_use]
    pub fn serialize(aot_result: &AotResult) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        output.push(VERSION);
        write_u64(&mut output, target_hash(&host_target()));
        write_u64(&mut output, aot_result.module_checksum);
        write_atom(&mut output, aot_result.module);
        write_bytes(&mut output, &aot_result.module_bytes);
        write_u32(&mut output, aot_result.compiled.len() as u32);
        for (function, arity, native) in &aot_result.compiled {
            write_atom(&mut output, *function);
            output.push(*arity);
            write_stack_maps(&mut output, native.stack_maps());
        }
        output
    }

    /// Deserializes and recompiles native entries for `target`.
    pub fn deserialize(
        bytes: &[u8],
        target: &str,
    ) -> Result<Vec<(Atom, u8, NativeCode)>, AotError> {
        let bundle = DecodedBundle::read(bytes, target)?;
        let compiler = JitCompiler::new(JitSettings).map_err(AotError::Jit)?;
        let atom_table = AtomTable::with_common_atoms();
        let parsed = load_beam_chunks(&bundle.module_bytes, &atom_table).map_err(AotError::Load)?;
        recompile_entries(&compiler, &parsed, &bundle.entries)
    }

    /// Loads a bundle while also validating it against the supplied BEAM bytes.
    pub fn deserialize_for_module(
        bytes: &[u8],
        target: &str,
        beam_bytes: &[u8],
    ) -> Result<(Atom, Vec<(Atom, u8, NativeCode)>), AotError> {
        let bundle = DecodedBundle::read(bytes, target)?;
        let actual = module_checksum(beam_bytes);
        if bundle.module_checksum != actual {
            return Err(AotError::ChecksumMismatch {
                expected: actual,
                actual: bundle.module_checksum,
            });
        }
        let compiler = JitCompiler::new(JitSettings).map_err(AotError::Jit)?;
        let atom_table = AtomTable::with_common_atoms();
        let parsed = load_beam_chunks(&bundle.module_bytes, &atom_table).map_err(AotError::Load)?;
        let module = parsed.name;
        Ok((
            module,
            recompile_entries(&compiler, &parsed, &bundle.entries)?,
        ))
    }
}

/// Attempts to load a filesystem companion bundle into the JIT cache and profiler.
pub fn load_companion_into_cache(
    beam_path: &Path,
    beam_bytes: &[u8],
    module: &Module,
    cache: &JitCache,
    profiler: &JitProfiler,
) -> Result<usize, AotError> {
    let native_path = beam_path.with_extension("beamr_native");
    if !native_path.exists() {
        return Ok(0);
    }
    let bytes = std::fs::read(&native_path).map_err(|source| AotError::Io {
        path: native_path,
        source,
    })?;
    let target = host_target();
    let (_, entries) = NativeCodeBundle::deserialize_for_module(&bytes, &target, beam_bytes)?;
    let mut loaded = 0;
    for (function, arity, code) in entries {
        cache.insert(JitCacheKey::new(module.name, function, arity, 0), code);
        profiler.mark_compiled(module.name, function, arity);
        loaded += 1;
    }
    Ok(loaded)
}

/// Returns the current host target identity used by AOT bundles.
#[must_use]
pub fn host_target() -> String {
    option_env!("TARGET").map_or_else(
        || {
            format!(
                "{}-{}-{}",
                std::env::consts::ARCH,
                std::env::consts::OS,
                std::env::consts::FAMILY
            )
        },
        str::to_owned,
    )
}

/// Deterministic FNV-1a checksum used for module cache validation.
#[must_use]
pub fn module_checksum(bytes: &[u8]) -> u64 {
    fnv1a64(bytes)
}

/// Deterministic target hash for the serialized header.
#[must_use]
pub fn target_hash(target: &str) -> u64 {
    fnv1a64(target.as_bytes())
}

fn recompile_entries(
    compiler: &JitCompiler,
    parsed: &ParsedModule,
    entries: &[(Atom, u8, Vec<StackMapEntry>)],
) -> Result<Vec<(Atom, u8, NativeCode)>, AotError> {
    let exports: HashMap<(Atom, u8), &ExportEntry> = parsed
        .exports
        .iter()
        .map(|export| ((export.function, export.arity), export))
        .collect();
    let mut compiled = Vec::with_capacity(entries.len());
    for (function, arity, _stack_maps) in entries {
        let export = exports
            .get(&(*function, *arity))
            .ok_or(AotError::MissingFunction {
                function: *function,
                arity: *arity,
            })?;
        let instructions = exported_instructions(parsed, export).map_err(AotError::Malformed)?;
        let native = compiler
            .compile(instructions, parsed.name, *function, *arity)
            .map_err(AotError::Jit)?;
        compiled.push((*function, *arity, native));
    }
    Ok(compiled)
}

fn exported_instructions<'a>(
    parsed: &'a ParsedModule,
    export: &ExportEntry,
) -> Result<&'a [Instruction], String> {
    let label_index = label_index(&parsed.instructions);
    let entry = label_index
        .get(&export.label)
        .copied()
        .ok_or_else(|| format!("export label {} is absent from module code", export.label))?;
    let start = match parsed.instructions.get(entry) {
        Some(Instruction::Label { .. }) | Some(Instruction::FuncInfo { .. }) => entry + 1,
        Some(_) => entry,
        None => return Err(format!("entry instruction {entry} is outside module code")),
    };
    let end = parsed
        .instructions
        .iter()
        .enumerate()
        .skip(start.saturating_add(1))
        .find_map(|(index, instruction)| match instruction {
            Instruction::FuncInfo { .. } => Some(index),
            _ => None,
        })
        .unwrap_or(parsed.instructions.len());
    Ok(&parsed.instructions[start..end])
}

fn label_index(instructions: &[Instruction]) -> HashMap<u32, usize> {
    instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, index)),
            _ => None,
        })
        .collect()
}

fn is_skippable_jit_error(error: &JitError) -> bool {
    matches!(
        error,
        JitError::UnsupportedOpcode { .. }
            | JitError::UnsupportedOperand { .. }
            | JitError::UnknownLabel { .. }
    )
}

fn write_atom(output: &mut Vec<u8>, atom: Atom) {
    write_u32(output, atom_index(atom));
}

fn atom_index(atom: Atom) -> u32 {
    let debug = format!("{atom:?}");
    debug
        .strip_prefix("Atom(")
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or(0)
}

fn write_stack_maps(output: &mut Vec<u8>, stack_maps: &[StackMapEntry]) {
    write_u32(output, stack_maps.len() as u32);
    for entry in stack_maps {
        write_u32(output, entry.offset_from_entry);
        write_u32(output, entry.live_roots.len() as u32);
        for root in &entry.live_roots {
            match root {
                RootLocation::Register(register) => {
                    output.push(0);
                    write_u16(output, *register);
                }
                RootLocation::StackSlot(slot) => {
                    output.push(1);
                    write_i32(output, *slot);
                }
            }
        }
    }
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    write_u64(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

fn write_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct DecodedBundle {
    module_checksum: u64,
    module_bytes: Vec<u8>,
    entries: Vec<(Atom, u8, Vec<StackMapEntry>)>,
}

impl DecodedBundle {
    fn read(bytes: &[u8], target: &str) -> Result<Self, AotError> {
        let mut reader = Reader::new(bytes);
        let magic = reader.read_exact(MAGIC.len())?;
        if magic != MAGIC {
            return Err(AotError::InvalidMagic);
        }
        let version = reader.read_u8()?;
        if version != VERSION {
            return Err(AotError::UnsupportedVersion(version));
        }
        let actual_target = reader.read_u64()?;
        let expected_target = target_hash(target);
        if actual_target != expected_target {
            return Err(AotError::TargetMismatch {
                expected: expected_target,
                actual: actual_target,
            });
        }
        let module_checksum = reader.read_u64()?;
        let _module_index = reader.read_u32()?;
        let module_bytes = reader.read_bytes()?;
        let atom_table = AtomTable::with_common_atoms();
        let parsed = load_beam_chunks(&module_bytes, &atom_table).map_err(AotError::Load)?;
        let functions_by_index: HashMap<u32, Atom> = parsed
            .exports
            .iter()
            .map(|export| (atom_index(export.function), export.function))
            .collect();
        let entry_count = reader.read_u32()? as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let function_index = reader.read_u32()?;
            let function = functions_by_index
                .get(&function_index)
                .copied()
                .ok_or_else(|| {
                    AotError::Malformed(format!("unknown function atom index {function_index}"))
                })?;
            let arity = reader.read_u8()?;
            let stack_maps = reader.read_stack_maps()?;
            entries.push((function, arity, stack_maps));
        }
        if !reader.is_empty() {
            return Err(AotError::Malformed(
                "trailing bytes after AOT bundle".into(),
            ));
        }
        Ok(Self {
            module_checksum,
            module_bytes,
            entries,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], AotError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| AotError::Malformed("bundle offset overflow".into()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| AotError::Malformed("truncated AOT bundle".into()))?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, AotError> {
        self.read_exact(1).map(|bytes| bytes[0])
    }

    fn read_u32(&mut self) -> Result<u32, AotError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, AotError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_i32(&mut self) -> Result<i32, AotError> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u16(&mut self) -> Result<u16, AotError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, AotError> {
        let len = usize::try_from(self.read_u64()?)
            .map_err(|_| AotError::Malformed("byte section length overflows usize".into()))?;
        self.read_exact(len).map(<[u8]>::to_vec)
    }

    fn read_stack_maps(&mut self) -> Result<Vec<StackMapEntry>, AotError> {
        let count = self.read_u32()? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let offset_from_entry = self.read_u32()?;
            let root_count = self.read_u32()? as usize;
            let mut live_roots = Vec::with_capacity(root_count);
            for _ in 0..root_count {
                let tag = self.read_u8()?;
                let root = match tag {
                    0 => RootLocation::Register(self.read_u16()?),
                    1 => RootLocation::StackSlot(self.read_i32()?),
                    other => {
                        return Err(AotError::Malformed(format!(
                            "unknown stack-map root tag {other}"
                        )));
                    }
                };
                live_roots.push(root);
            }
            entries.push(StackMapEntry {
                offset_from_entry,
                live_roots,
            });
        }
        Ok(entries)
    }
}

impl fmt::Display for AotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read '{}': {source}", path.display())
            }
            Self::Load(error) => write!(formatter, "load: {error}"),
            Self::Jit(error) => write!(formatter, "jit: {error}"),
            Self::InvalidMagic => formatter.write_str("invalid AOT bundle magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported AOT version {version}")
            }
            Self::TargetMismatch { expected, actual } => write!(
                formatter,
                "AOT target mismatch: expected hash {expected:#x}, got {actual:#x}"
            ),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "AOT module checksum mismatch: expected {expected:#x}, got {actual:#x}"
            ),
            Self::Malformed(message) => write!(formatter, "malformed AOT bundle: {message}"),
            Self::MissingFunction { function, arity } => {
                write!(
                    formatter,
                    "AOT bundle references missing function {function:?}/{arity}"
                )
            }
        }
    }
}

impl Error for AotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Load(error) => Some(error),
            Self::Jit(error) => Some(error),
            Self::InvalidMagic
            | Self::UnsupportedVersion(_)
            | Self::TargetMismatch { .. }
            | Self::ChecksumMismatch { .. }
            | Self::Malformed(_)
            | Self::MissingFunction { .. } => None,
        }
    }
}
