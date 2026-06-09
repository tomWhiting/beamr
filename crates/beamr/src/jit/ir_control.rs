//! Control flow structures for the JIT compiler.

use crate::loader::Instruction;
use crate::loader::decode::{BifOp, TypeTestOp};
use cranelift_frontend::FunctionBuilder;
use std::collections::{HashMap, HashSet};

use super::compiler::JitError;
use super::ir_arithmetic::{ArithmeticOp, ParsedBif};
use super::ir_common::{
    ensure_known_label, validate_label_operand, validate_read_operand, validate_write_operand,
};
use super::ir_guards::{
    immediate_raw_term, immediate_usize, parse_select_pairs, validate_tag_atom,
};

pub(crate) struct TranslationPlan {
    pub(crate) labels: HashMap<u32, usize>,
    pub(crate) block_starts: HashSet<usize>,
}

impl TranslationPlan {
    pub(crate) fn new(instructions: &[Instruction]) -> Result<Self, JitError> {
        if instructions.is_empty() {
            return Err(JitError::EmptyFunction);
        }

        let mut labels = HashMap::new();
        let mut block_starts = HashSet::from([0, instructions.len()]);
        for (index, instruction) in instructions.iter().enumerate() {
            match instruction {
                Instruction::Label { label } => {
                    labels.insert(*label, index);
                    block_starts.insert(index);
                }
                Instruction::Return => {}
                Instruction::Move {
                    source,
                    destination,
                } => {
                    validate_read_operand(source)?;
                    validate_write_operand(destination)?;
                }
                Instruction::Swap { left, right } => {
                    validate_read_operand(left)?;
                    validate_read_operand(right)?;
                    validate_write_operand(left)?;
                    validate_write_operand(right)?;
                }
                Instruction::TypeTest { op, fail, value } => {
                    validate_supported_type_test(*op)?;
                    validate_label_operand(fail)?;
                    validate_read_operand(value)?;
                    block_starts.insert(index + 1);
                }
                Instruction::PutList {
                    head,
                    tail,
                    destination,
                } => {
                    validate_read_operand(head)?;
                    validate_read_operand(tail)?;
                    validate_write_operand(destination)?;
                    block_starts.insert(index + 1);
                }
                Instruction::GetList { source, head, tail } => {
                    validate_read_operand(source)?;
                    validate_write_operand(head)?;
                    validate_write_operand(tail)?;
                }
                Instruction::GetHd {
                    source,
                    destination,
                }
                | Instruction::GetTl {
                    source,
                    destination,
                } => {
                    validate_read_operand(source)?;
                    validate_write_operand(destination)?;
                }
                Instruction::PutTuple2 {
                    destination,
                    elements,
                } => {
                    validate_write_operand(destination)?;
                    let crate::loader::decode::Operand::List(elements) = elements else {
                        return Err(JitError::UnsupportedOperand {
                            operand: format!(
                                "put_tuple2 elements must be a list, got {elements:?}"
                            ),
                        });
                    };
                    for element in elements {
                        validate_read_operand(element)?;
                    }
                    block_starts.insert(index + 1);
                }
                Instruction::GetTupleElement {
                    source,
                    index,
                    destination,
                } => {
                    validate_read_operand(source)?;
                    let _ = immediate_usize(index, "get_tuple_element index")?;
                    validate_write_operand(destination)?;
                }
                Instruction::Comparison {
                    fail, left, right, ..
                } => {
                    validate_label_operand(fail)?;
                    validate_read_operand(left)?;
                    validate_read_operand(right)?;
                    block_starts.insert(index + 1);
                }
                Instruction::TestArity { fail, tuple, arity } => {
                    validate_label_operand(fail)?;
                    validate_read_operand(tuple)?;
                    let _ = immediate_usize(arity, "test_arity arity")?;
                    block_starts.insert(index + 1);
                }
                Instruction::IsTaggedTuple {
                    fail,
                    value,
                    arity,
                    tag,
                } => {
                    validate_label_operand(fail)?;
                    validate_read_operand(value)?;
                    let _ = immediate_usize(arity, "is_tagged_tuple arity")?;
                    validate_tag_atom(tag)?;
                    block_starts.insert(index + 1);
                }
                Instruction::SelectVal { value, fail, list } => {
                    validate_read_operand(value)?;
                    validate_label_operand(fail)?;
                    for (candidate, target) in parse_select_pairs(list)? {
                        let _ = immediate_raw_term(candidate)?;
                        validate_label_operand(target)?;
                    }
                    block_starts.insert(index + 1);
                }
                Instruction::Jump { target } => {
                    validate_label_operand(target)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Try { destination, label } => {
                    validate_write_operand(destination)?;
                    validate_label_operand(label)?;
                    block_starts.insert(index + 1);
                }
                Instruction::TryEnd { source } | Instruction::TryCase { source } => {
                    validate_write_operand(source)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Call { label, .. } | Instruction::CallOnly { label, .. } => {
                    validate_label_operand(label)?;
                    block_starts.insert(index + 1);
                }
                Instruction::CallExt { import, .. } | Instruction::CallExtOnly { import, .. } => {
                    validate_import_operand(import)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Bif { op, operands } => {
                    let parsed = ParsedBif::parse(*op, operands)?;
                    let _ = ArithmeticOp::from_import(parsed.import)?;
                    validate_label_operand(parsed.fail)?;
                    validate_read_operand(parsed.left)?;
                    validate_read_operand(parsed.right)?;
                    validate_write_operand(parsed.destination)?;
                    block_starts.insert(index + 1);
                }
                other => {
                    return Err(JitError::UnsupportedOpcode {
                        opcode: opcode_name(other),
                    });
                }
            }
        }

        for instruction in instructions {
            match instruction {
                Instruction::TypeTest { fail, .. }
                | Instruction::Comparison { fail, .. }
                | Instruction::TestArity { fail, .. }
                | Instruction::IsTaggedTuple { fail, .. } => ensure_known_label(&labels, fail)?,
                Instruction::SelectVal { fail, list, .. } => {
                    ensure_known_label(&labels, fail)?;
                    for (_, target) in parse_select_pairs(list)? {
                        ensure_known_label(&labels, target)?;
                    }
                }
                Instruction::Jump { target }
                | Instruction::Try { label: target, .. }
                | Instruction::Call { label: target, .. }
                | Instruction::CallOnly { label: target, .. } => {
                    ensure_known_label(&labels, target)?
                }
                Instruction::Bif { op, operands } => {
                    if matches!(op, BifOp::Bif2 | BifOp::GcBif2) {
                        let parsed = ParsedBif::parse(*op, operands)?;
                        ensure_known_label(&labels, parsed.fail)?;
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            labels,
            block_starts,
        })
    }
}

fn validate_import_operand(
    import: &crate::loader::decode::compact::Operand,
) -> Result<(), JitError> {
    match import {
        crate::loader::decode::compact::Operand::Unsigned(value) => usize::try_from(*value)
            .map(|_| ())
            .map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            }),
        crate::loader::decode::compact::Operand::Integer(value) => usize::try_from(*value)
            .map(|_| ())
            .map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            }),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("external call import must be an index, got {other:?}"),
        }),
    }
}

fn validate_supported_type_test(op: TypeTestOp) -> Result<(), JitError> {
    match op {
        TypeTestOp::IsInteger
        | TypeTestOp::IsAtom
        | TypeTestOp::IsPid
        | TypeTestOp::IsBinary
        | TypeTestOp::IsList
        | TypeTestOp::IsTuple => Ok(()),
        TypeTestOp::IsFloat => Err(JitError::UnsupportedOpcode {
            opcode: "TypeTest(IsFloat)".to_owned(),
        }),
        other => Err(JitError::UnsupportedOpcode {
            opcode: format!("TypeTest({other:?})"),
        }),
    }
}

pub(crate) struct BlockMap {
    blocks_by_index: Vec<cranelift_codegen::ir::Block>,
    label_blocks: HashMap<u32, cranelift_codegen::ir::Block>,
    pub(crate) entry: cranelift_codegen::ir::Block,
    pub(crate) deopt: cranelift_codegen::ir::Block,
    pub(crate) exception_block: cranelift_codegen::ir::Block,
    pub(crate) yield_block: cranelift_codegen::ir::Block,
}

impl BlockMap {
    pub(crate) fn new(
        builder: &mut FunctionBuilder<'_>,
        instructions: &[Instruction],
        plan: &TranslationPlan,
    ) -> Self {
        let mut blocks_by_index = Vec::with_capacity(instructions.len() + 1);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        let mut current = builder.create_block();
        for index in 0..=instructions.len() {
            if index > 0 && plan.block_starts.contains(&index) {
                current = builder.create_block();
            }
            blocks_by_index.push(current);
        }

        let mut label_blocks = HashMap::new();
        for (label, index) in &plan.labels {
            label_blocks.insert(*label, blocks_by_index[*index]);
        }

        Self {
            entry,
            blocks_by_index,
            label_blocks,
            deopt: builder.create_block(),
            exception_block: builder.create_block(),
            yield_block: builder.create_block(),
        }
    }

    pub(crate) fn block_for_instruction(&self, index: usize) -> cranelift_codegen::ir::Block {
        self.blocks_by_index[index]
    }

    pub(crate) fn block_after(&self, index: usize) -> cranelift_codegen::ir::Block {
        self.blocks_by_index[index + 1]
    }

    pub(crate) fn exit_block(&self) -> cranelift_codegen::ir::Block {
        self.blocks_by_index[self.blocks_by_index.len() - 1]
    }

    pub(crate) fn label_block(&self, label: u32) -> Result<cranelift_codegen::ir::Block, JitError> {
        self.label_blocks
            .get(&label)
            .copied()
            .ok_or(JitError::UnknownLabel { label })
    }
}

pub(crate) fn opcode_name(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Generic { opcode, name, .. } => format!("{name} ({opcode})"),
        other => format!("{other:?}"),
    }
}
