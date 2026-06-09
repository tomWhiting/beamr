//! GC safepoint metadata emitted for heap-allocating JIT instructions.
//!
//! Cranelift machine-code offsets are not exposed by the current scaffold at
//! lowering time, so allocation safepoints use a deterministic logical offset:
//! the zero-based BEAM instruction index that emitted the allocation. The value
//! is still stable per native function and preserves one descriptor per
//! allocation site until the compiler grows real code-offset plumbing.

use crate::loader::decode::Operand;

use super::ir_common::{Register, register_operand};
use super::{JitError, RootLocation, StackMapEntry};

/// Collects stack-map entries for allocation sites while lowering a function.
#[derive(Debug, Default)]
pub(crate) struct SafepointBuilder {
    entries: Vec<StackMapEntry>,
}

impl SafepointBuilder {
    /// Creates an empty allocation safepoint collector.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records a safepoint at `instruction_index` with roots from `operands`.
    pub(crate) fn record_allocation_site(
        &mut self,
        instruction_index: usize,
        operands: impl IntoIterator<Item = Operand>,
    ) -> Result<(), JitError> {
        let mut live_roots = Vec::new();
        for operand in operands {
            if let Some(root) = root_location(&operand)? {
                push_unique(&mut live_roots, root);
            }
        }

        self.entries.push(StackMapEntry {
            offset_from_entry: u32::try_from(instruction_index).map_err(|_| {
                JitError::CraneliftError(format!(
                    "JIT instruction index {instruction_index} does not fit stack-map offset"
                ))
            })?,
            live_roots,
        });
        Ok(())
    }

    /// Returns the collected stack maps in allocation order.
    pub(crate) fn finish(self) -> Vec<StackMapEntry> {
        self.entries
    }
}

fn root_location(operand: &Operand) -> Result<Option<RootLocation>, JitError> {
    match operand {
        Operand::Integer(_)
        | Operand::Unsigned(_)
        | Operand::Atom(_)
        | Operand::Label(_)
        | Operand::Character(_)
        | Operand::Literal(_)
        | Operand::List(_)
        | Operand::FloatRegister(_)
        | Operand::Allocation(_) => Ok(None),
        Operand::X(_) | Operand::Y(_) | Operand::TypedRegister { .. } => {
            register_operand(operand).map(|register| Some(location_for_register(register)))
        }
    }
}

fn location_for_register(register: Register) -> RootLocation {
    match register {
        Register::X(index) => RootLocation::Register(index as u16),
        Register::Y(index) => RootLocation::StackSlot(index as i32),
    }
}

fn push_unique(roots: &mut Vec<RootLocation>, root: RootLocation) {
    if !roots.iter().any(|existing| existing == &root) {
        roots.push(root);
    }
}

#[cfg(test)]
mod tests {
    use super::SafepointBuilder;
    use crate::jit::RootLocation;
    use crate::loader::decode::Operand;

    #[test]
    fn records_unique_register_roots_in_operand_order() {
        let mut builder = SafepointBuilder::new();
        builder
            .record_allocation_site(
                7,
                [
                    Operand::X(0),
                    Operand::Integer(1),
                    Operand::Y(2),
                    Operand::X(0),
                ],
            )
            .expect("safepoint recorded");

        let maps = builder.finish();
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].offset_from_entry, 7);
        assert_eq!(
            maps[0].live_roots,
            vec![RootLocation::Register(0), RootLocation::StackSlot(2)]
        );
    }
}
