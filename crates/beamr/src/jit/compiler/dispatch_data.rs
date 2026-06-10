//! Data-structure, float, binary, and map instruction lowering for the dispatch loop.

use crate::atom::Atom;
use crate::jit::ir_allocation::{
    LoweringContext, lower_get_hd, lower_get_list, lower_get_tl, lower_get_tuple_element,
    lower_put_list, lower_put_tuple2, tuple_root_operands,
};
use crate::jit::ir_binary::{
    BinaryLoweringContext, binary_allocation_roots, fail_operand, lower_binary_op,
};
use crate::jit::ir_common::label_operand;
use crate::jit::ir_control::{BlockMap, opcode_name};
use crate::jit::ir_exceptions::JIT_STATUS_EXCEPTION;
use crate::jit::ir_float::{
    FloatBinaryOp, FloatLoweringContext, FloatRegisterMap, float_boxing_roots, translate_fconv,
    translate_float_binary, translate_fmove, translate_fnegate,
};
use crate::jit::ir_guards::immediate_usize;
use crate::jit::ir_map::{
    MapLoweringContext, map_allocation_roots, parse_get_map_elements_operands,
    parse_has_map_fields_operands, parse_put_map_operands, translate_get_map_elements,
    translate_has_map_fields, translate_put_map_assoc, translate_put_map_exact,
};
use crate::jit::safepoint::SafepointBuilder;
use crate::loader::Instruction;
use crate::loader::decode::MapOp;
use crate::loader::decode::compact::Operand;
use crate::term::Term;
use cranelift_frontend::FunctionBuilder;

use super::JitError;
use super::dispatch_helpers::{allocation_binary_op, clear_binary_outputs, supported_binary_op};
use super::ir_helpers::CompileHelpers;
use super::ir_typed::{TypedRegisterState, float_fail_block};

/// Lower a data-structure, float, binary, or map instruction.
///
/// Called for any instruction not handled by `dispatch_core` or `dispatch_call`.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_data_instruction(
    builder: &mut FunctionBuilder<'_>,
    register_file: cranelift_codegen::ir::Value,
    process: cranelift_codegen::ir::Value,
    blocks: &BlockMap,
    typed_state: &mut TypedRegisterState,
    float_registers: &mut FloatRegisterMap,
    safepoints: &mut SafepointBuilder,
    helpers: CompileHelpers,
    index: usize,
    instruction: &Instruction,
) -> Result<bool, JitError> {
    match instruction {
        Instruction::Fmove { source, dest } => {
            if matches!(source, Operand::FloatRegister(_))
                && !matches!(dest, Operand::FloatRegister(_))
            {
                safepoints.record_allocation_site(index, float_boxing_roots(dest)?)?;
            }
            typed_state.materialize_operands_for_untyped_lowering(
                builder,
                register_file,
                [source, dest],
            );
            let next = blocks.block_after(index);
            let fail = float_fail_block(
                builder,
                JIT_STATUS_EXCEPTION,
                Term::atom(Atom::BADARITH).raw() as i64,
                next,
            );
            translate_fmove(
                builder,
                FloatLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    box_float: helpers.box_float,
                },
                float_registers,
                source,
                dest,
                fail,
            )?;
            typed_state.clear_operand(dest);
            Ok(false)
        }
        Instruction::Fconv { source, dest } => {
            typed_state.materialize_operands_for_untyped_lowering(builder, register_file, [source]);
            let next = blocks.block_after(index);
            let fail = float_fail_block(
                builder,
                JIT_STATUS_EXCEPTION,
                Term::atom(Atom::BADARITH).raw() as i64,
                next,
            );
            translate_fconv(builder, register_file, float_registers, source, dest, fail)?;
            Ok(false)
        }
        Instruction::Fadd {
            fail,
            left,
            right,
            dest,
        }
        | Instruction::Fsub {
            fail,
            left,
            right,
            dest,
        }
        | Instruction::Fmul {
            fail,
            left,
            right,
            dest,
        }
        | Instruction::Fdiv {
            fail,
            left,
            right,
            dest,
        } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            let op = match instruction {
                Instruction::Fadd { .. } => FloatBinaryOp::Add,
                Instruction::Fsub { .. } => FloatBinaryOp::Subtract,
                Instruction::Fmul { .. } => FloatBinaryOp::Multiply,
                Instruction::Fdiv { .. } => FloatBinaryOp::Divide,
                _ => FloatBinaryOp::Add,
            };
            translate_float_binary(builder, float_registers, op, left, right, dest, fail)?;
            Ok(false)
        }
        Instruction::Fnegate { fail, source, dest } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            translate_fnegate(builder, float_registers, source, dest, fail)?;
            Ok(false)
        }
        Instruction::PutList {
            head,
            tail,
            destination,
        } => {
            safepoints
                .record_allocation_site(index, [head.clone(), tail.clone(), destination.clone()])?;
            let destination_type = typed_state.list_type_from_head(head);
            typed_state.materialize_operands_for_untyped_lowering(
                builder,
                register_file,
                [head, tail],
            );
            lower_put_list(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.allocation.cons,
                head,
                tail,
                destination,
            )?;
            typed_state.set_optional_operand_type(destination, destination_type);
            Ok(false)
        }
        Instruction::GetList { source, head, tail } => {
            let head_type = typed_state.list_head_type(source);
            let tail_type = typed_state.list_tail_type(source);
            lower_get_list(builder, register_file, source, head, tail)?;
            typed_state.mark_loaded_operand_type(builder, register_file, head, head_type);
            typed_state.mark_loaded_operand_type(builder, register_file, tail, tail_type);
            Ok(false)
        }
        Instruction::GetHd {
            source,
            destination,
        } => {
            let destination_type = typed_state.list_head_type(source);
            lower_get_hd(builder, register_file, source, destination)?;
            typed_state.mark_loaded_operand_type(
                builder,
                register_file,
                destination,
                destination_type,
            );
            Ok(false)
        }
        Instruction::GetTl {
            source,
            destination,
        } => {
            let destination_type = typed_state.list_tail_type(source);
            lower_get_tl(builder, register_file, source, destination)?;
            typed_state.mark_loaded_operand_type(
                builder,
                register_file,
                destination,
                destination_type,
            );
            Ok(false)
        }
        Instruction::PutTuple2 {
            destination,
            elements,
        } => {
            safepoints
                .record_allocation_site(index, tuple_root_operands(destination, elements)?)?;
            let destination_type = typed_state.tuple_type_from_elements(elements);
            typed_state.materialize_tuple_elements_for_untyped_lowering(
                builder,
                register_file,
                elements,
            )?;
            lower_put_tuple2(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.allocation.tuple,
                destination,
                elements,
            )?;
            typed_state.set_optional_operand_type(destination, destination_type);
            Ok(false)
        }
        Instruction::GetTupleElement {
            source,
            index: elem_index,
            destination,
        } => {
            let elem_index = immediate_usize(elem_index, "get_tuple_element index")?;
            let destination_type = typed_state.tuple_element_type(source, elem_index);
            lower_get_tuple_element(builder, register_file, source, elem_index, destination)?;
            typed_state.mark_loaded_operand_type(
                builder,
                register_file,
                destination,
                destination_type,
            );
            Ok(false)
        }
        Instruction::BinaryOp { op, operands } if supported_binary_op(*op) => {
            if allocation_binary_op(*op) {
                safepoints
                    .record_allocation_site(index, binary_allocation_roots(*op, operands)?)?;
            }
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let fail = fail_operand(*op, operands)
                .map(label_operand)
                .transpose()?
                .map(|label| blocks.label_block(label))
                .transpose()?;
            lower_binary_op(
                builder,
                BinaryLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    exception: blocks.exception_block,
                },
                helpers.binary,
                *op,
                operands,
                fail,
            )?;
            clear_binary_outputs(typed_state, *op, operands);
            Ok(false)
        }
        Instruction::MapOp { op, operands } => {
            let context = MapLoweringContext {
                register_file,
                process,
            };
            match op {
                MapOp::PutMapAssoc => {
                    safepoints
                        .record_allocation_site(index, map_allocation_roots(*op, operands)?)?;
                    let (fail_operand, parsed) = parse_put_map_operands(operands)?;
                    typed_state.materialize_operands_for_untyped_lowering(
                        builder,
                        register_file,
                        std::iter::once(parsed.source).chain(parsed.pairs.iter()),
                    );
                    let fail = blocks.label_block(label_operand(fail_operand)?)?;
                    translate_put_map_assoc(builder, context, helpers.map, parsed, fail)?;
                    typed_state.clear_operand(parsed.destination);
                    Ok(false)
                }
                MapOp::PutMapExact => {
                    safepoints
                        .record_allocation_site(index, map_allocation_roots(*op, operands)?)?;
                    let (fail_operand, parsed) = parse_put_map_operands(operands)?;
                    typed_state.materialize_operands_for_untyped_lowering(
                        builder,
                        register_file,
                        std::iter::once(parsed.source).chain(parsed.pairs.iter()),
                    );
                    let fail = blocks.label_block(label_operand(fail_operand)?)?;
                    translate_put_map_exact(builder, context, helpers.map, parsed, fail)?;
                    typed_state.clear_operand(parsed.destination);
                    Ok(false)
                }
                MapOp::GetMapElements => {
                    let (fail_operand, parsed) = parse_get_map_elements_operands(operands)?;
                    typed_state.materialize_operands_for_untyped_lowering(
                        builder,
                        register_file,
                        std::iter::once(parsed.source).chain(parsed.pairs.iter().step_by(2)),
                    );
                    let fail = blocks.label_block(label_operand(fail_operand)?)?;
                    translate_get_map_elements(builder, context, helpers.map, parsed, fail)?;
                    for destination in parsed.pairs.iter().skip(1).step_by(2) {
                        typed_state.clear_operand(destination);
                    }
                    Ok(false)
                }
                MapOp::HasMapFields => {
                    let (fail_operand, parsed) = parse_has_map_fields_operands(operands)?;
                    typed_state.materialize_operands_for_untyped_lowering(
                        builder,
                        register_file,
                        std::iter::once(parsed.source).chain(parsed.keys.iter()),
                    );
                    let fail = blocks.label_block(label_operand(fail_operand)?)?;
                    translate_has_map_fields(builder, context, helpers.map, parsed, fail)?;
                    Ok(false)
                }
            }
        }
        other => Err(JitError::UnsupportedOpcode {
            opcode: opcode_name(other),
        }),
    }
}
