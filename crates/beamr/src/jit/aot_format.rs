//! Private binary encoding helpers for `.beamr_native` AOT cache bundles.

use super::aot::{AotError, MAGIC, VERSION};
use super::{RootLocation, StackMapEntry};
use crate::atom::{Atom, AtomTable};
use crate::loader::load_beam_chunks;
use std::collections::HashMap;

pub(crate) struct DecodedBundle {
    pub(crate) module_checksum: u64,
    pub(crate) module_bytes: Vec<u8>,
    pub(crate) entries: Vec<(Atom, u8, Vec<StackMapEntry>)>,
}

impl DecodedBundle {
    pub(crate) fn read(bytes: &[u8], target_hash: u64) -> Result<Self, AotError> {
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
        if actual_target != target_hash {
            return Err(AotError::TargetMismatch {
                expected: target_hash,
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
            .map(|export| (export.function.index(), export.function))
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

pub(crate) fn write_atom(output: &mut Vec<u8>, atom: Atom) {
    write_u32(output, atom.index());
}

pub(crate) fn write_stack_maps(output: &mut Vec<u8>, stack_maps: &[StackMapEntry]) {
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

pub(crate) fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    write_u64(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

pub(crate) fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
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
