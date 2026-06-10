//! Core instruction lowering: basic ops, guards, messages, exceptions, return.

use crate::jit::ir_arithmetic::{
    ArithmeticLowering, ArithmeticOp, ParsedBif, lower_arithmetic_bif, lower_comparison,
};
use crate::jit::ir_common::{
    JIT_DEOPT_SENTINEL, label_operand, read_operand_term, write_operand_term,
};
use crate::jit::ir_control::BlockMap;
use crate::jit::ir_exceptions::{
    ExceptionLoweringState, JIT_STATUS_DEOPT, JIT_STATUS_NORMAL, return_status, return_status_raw,
};
use crate::jit::ir_guards::{
    SelectPair, immediate_raw_term, lower_is_tagged_tuple, lower_select_val, lower_test_arity,
    lower_type_test, parse_select_pairs,
};
use crate::jit::ir_message::{
    MessageLoweringContext, translate_loop_rec, translate_loop_rec_end, translate_remove_message,
    translate_send, translate_timeout, translate_wait, translate_wait_timeout,
};
use crate::jit::safepoint::SafepointBuilder;
use crate::jit::type_info::TypeDescriptor;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use cranelift_codegen::ir::InstBuilder;
use cranelift_frontend::FunctionBuilder;

use super::JitError;
use super::ir_helpers::CompileHelpers;
use super::ir_typed::{
    TypedRegisterState, lower_typed_int_arithmetic, lower_typed_test_arity, lower_typed_type_test,
};

/// Lower a core instruction (basic ops, guards, messages, exceptions, return).
///
/// Returns `Ok(Some(terminated))` if the instruction was handled, `Ok(None)` if the
/// instruction should be delegated to another lowering function.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_core_instruction(
    builder: &mut FunctionBuilder<'_>,
    register_file: cranelift_codegen::ir::Value,
    process: cranelift_codegen::ir::Value,
    blocks: &BlockMap,
    typed_state: &mut TypedRegisterState,
    safepoints: &mut SafepointBuilder,
    exceptions: &mut ExceptionLoweringState,
    helpers: CompileHelpers,
    index: usize,
    instruction: &Instruction,
    instructions: &[Instruction],
) -> Result<Option<bool>, JitError> {
    match instruction {
        Instruction::Label { .. } => Ok(Some(false)),
        Instruction::Move {
            source,
            destination,
        } => {
            let value = typed_state.read_operand_value(builder, register_file, source)?;
            write_operand_term(builder, register_file, destination, value)?;
            typed_state.copy(source, destination);
            Ok(Some(false))
        }
        Instruction::Swap { left, right } => {
            let left_value = read_operand_term(builder, register_file, left)?;
            let right_value = read_operand_term(builder, register_file, right)?;
            write_operand_term(builder, register_file, left, right_value)?;
            write_operand_term(builder, register_file, right, left_value)?;
            typed_state.swap(left, right);
            Ok(Some(false))
        }
        Instruction::Bif { op, operands } => {
            let bif = ParsedBif::parse(*op, operands)?;
            let arithmetic = ArithmeticOp::from_import(bif.import)?;
            let fail = blocks.label_block(label_operand(bif.fail)?)?;
            let next = blocks.block_after(index);
            let lowering = ArithmeticLowering {
                op: arithmetic,
                left: bif.left,
                right: bif.right,
                destination: bif.destination,
                fail,
                success: next,
            };
            if typed_state.operands_are_int(bif.left, bif.right)
                && typed_state.can_write_typed(bif.destination)
            {
                lower_typed_int_arithmetic(builder, register_file, lowering, blocks.deopt)?;
                typed_state.set_operand_type(bif.destination, TypeDescriptor::Int);
            } else {
                typed_state.materialize_operands_for_untyped_lowering(
                    builder,
                    register_file,
                    [bif.left, bif.right],
                );
                lower_arithmetic_bif(builder, register_file, lowering)?;
                typed_state.clear_operand(bif.destination);
            }
            Ok(Some(true))
        }
        Instruction::TypeTest { op, fail, value } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            let next = blocks.block_after(index);
            if !lower_typed_type_test(builder, typed_state, *op, value, fail, next)? {
                lower_type_test(builder, register_file, *op, value, fail, next)?;
            }
            Ok(Some(true))
        }
        Instruction::Comparison {
            op,
            fail,
            left,
            right,
        } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            let next = blocks.block_after(index);
            typed_state.materialize_operands_for_untyped_lowering(
                builder,
                register_file,
                [left, right],
            );
            lower_comparison(builder, register_file, *op, left, right, fail, next)?;
            Ok(Some(true))
        }
        Instruction::TestArity { fail, tuple, arity } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            let next = blocks.block_after(index);
            if !lower_typed_test_arity(builder, typed_state, tuple, arity, fail, next)? {
                lower_test_arity(builder, register_file, tuple, arity, fail, next)?;
            }
            Ok(Some(true))
        }
        Instruction::IsTaggedTuple {
            fail,
            value,
            arity,
            tag,
        } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            let next = blocks.block_after(index);
            lower_is_tagged_tuple(builder, register_file, value, arity, tag, fail, next)?;
            Ok(Some(true))
        }
        Instruction::SelectVal { value, fail, list } => {
            let fail = blocks.label_block(label_operand(fail)?)?;
            typed_state.materialize_operands_for_untyped_lowering(builder, register_file, [value]);
            let pairs = parse_select_pairs(list)?
                .into_iter()
                .map(|(candidate, target)| {
                    Ok(SelectPair {
                        candidate_raw: immediate_raw_term(candidate)?,
                        target: blocks.label_block(label_operand(target)?)?,
                    })
                })
                .collect::<Result<Vec<_>, JitError>>()?;
            lower_select_val(builder, register_file, value, fail, &pairs)?;
            Ok(Some(true))
        }
        Instruction::Jump { target } => {
            let target = blocks.label_block(label_operand(target)?)?;
            builder.ins().jump(target, &[]);
            Ok(Some(true))
        }
        // -- message passing --
        Instruction::Send => {
            safepoints.record_allocation_site(index, [Operand::X(0), Operand::X(1)])?;
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            translate_send(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
                &Operand::X(0),
                &Operand::X(1),
                &Operand::X(0),
            )?;
            typed_state.clear_operand(&Operand::X(0));
            Ok(Some(false))
        }
        Instruction::LoopRec { fail, destination } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let fail = blocks.label_block(label_operand(fail)?)?;
            translate_loop_rec(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
                fail,
                destination,
            )?;
            typed_state.clear_operand(destination);
            Ok(Some(false))
        }
        Instruction::LoopRecEnd { fail } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let loop_label = blocks.label_block(label_operand(fail)?)?;
            translate_loop_rec_end(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
                loop_label,
            );
            Ok(Some(true))
        }
        Instruction::RemoveMessage => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            translate_remove_message(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
            );
            Ok(Some(false))
        }
        Instruction::Wait { fail } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let loop_label = blocks.label_block(label_operand(fail)?)?;
            translate_wait(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
                helpers.charge,
                loop_label,
            );
            Ok(Some(true))
        }
        Instruction::WaitTimeout { fail, timeout } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let timeout_label = blocks.label_block(label_operand(fail)?)?;
            let loop_label = instructions[..index]
                .iter()
                .rposition(|candidate| matches!(candidate, Instruction::LoopRec { .. }))
                .map_or(blocks.block_for_instruction(index), |loop_index| {
                    blocks.block_for_instruction(loop_index)
                });
            translate_wait_timeout(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
                helpers.charge,
                timeout,
                timeout_label,
                loop_label,
            )?;
            Ok(Some(true))
        }
        Instruction::Timeout => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            translate_timeout(
                builder,
                MessageLoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                    yield_block: blocks.yield_block,
                },
                helpers.message,
            );
            Ok(Some(false))
        }
        Instruction::RecvMarkerReserve { .. }
        | Instruction::RecvMarkerBind { .. }
        | Instruction::RecvMarkerClear { .. }
        | Instruction::RecvMarkerUse { .. } => {
            return_status_raw(builder, JIT_STATUS_DEOPT, JIT_DEOPT_SENTINEL);
            Ok(Some(true))
        }
        // -- exception handling --
        Instruction::Try { destination, label } => {
            let catch_block = blocks.label_block(label_operand(label)?)?;
            let _frame = exceptions.translate_try(catch_block, destination)?;
            Ok(Some(false))
        }
        Instruction::TryEnd { source } => {
            let _ = crate::jit::ir_common::register_operand(source)?;
            exceptions.translate_try_end()?;
            builder.ins().call(helpers.exception.clear, &[process]);
            Ok(Some(false))
        }
        Instruction::TryCase { source } => {
            let caught = exceptions.translate_try_case(builder, register_file, source)?;
            write_operand_term(
                builder,
                register_file,
                &crate::loader::decode::Operand::X(0),
                caught.class,
            )?;
            write_operand_term(
                builder,
                register_file,
                &crate::loader::decode::Operand::X(1),
                caught.reason,
            )?;
            write_operand_term(
                builder,
                register_file,
                &crate::loader::decode::Operand::X(2),
                caught.trace,
            )?;
            Ok(Some(false))
        }
        Instruction::Return => {
            let value = typed_state.read_return_value(builder, register_file);
            return_status(builder, JIT_STATUS_NORMAL, value);
            Ok(Some(true))
        }
        _ => Ok(None),
    }
}
