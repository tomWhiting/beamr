//! Call and closure instruction lowering for the dispatch loop.

use crate::atom::Atom;
use crate::jit::ir_allocation::LoweringContext;
use crate::jit::ir_closure::{
    ClosureCall, ClosureMetadata, lower_call_fun, lower_make_fun2, make_fun_free_var_operands,
    make_fun_free_var_roots,
};
use crate::jit::ir_common::label_operand;
use crate::jit::ir_control::BlockMap;
use crate::jit::ir_exceptions::{CompiledFrameInfo, ExceptionLoweringState};
use crate::jit::safepoint::SafepointBuilder;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use cranelift_codegen::ir::{InstBuilder, types};
use cranelift_frontend::FunctionBuilder;

use super::dispatch_helpers::{
    charge_reduction_or_yield, handle_helper_return, immediate_u8, import_index,
    make_fun_lambda_index,
};
use super::ir_helpers::CompileHelpers;
use super::ir_typed::TypedRegisterState;
use super::{JitError, ModuleCompileMetadata};

/// Lower a call or closure instruction (CallExt, MakeFun, CallFun, Call, Apply).
///
/// Returns `Ok(Some(terminated))` if the instruction was handled, `Ok(None)` if the
/// instruction should be delegated to the data-structure lowering pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_call_instruction(
    builder: &mut FunctionBuilder<'_>,
    register_file: cranelift_codegen::ir::Value,
    process: cranelift_codegen::ir::Value,
    blocks: &BlockMap,
    typed_state: &mut TypedRegisterState,
    safepoints: &mut SafepointBuilder,
    exceptions: &mut ExceptionLoweringState,
    helpers: CompileHelpers,
    compiled_frame: CompiledFrameInfo,
    module: Atom,
    metadata: ModuleCompileMetadata<'_>,
    index: usize,
    instruction: &Instruction,
) -> Result<Option<bool>, JitError> {
    match instruction {
        Instruction::CallExt { arity: _, import }
        | Instruction::CallExtOnly { arity: _, import } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let import_idx = import_index(import)?;
            let call_arity = match instruction {
                Instruction::CallExt { arity, .. } | Instruction::CallExtOnly { arity, .. } => {
                    immediate_u8(arity, "external call arity")?
                }
                _ => 0,
            };
            let module_value = builder.ins().iconst(types::I64, i64::from(module.index()));
            let import_value = builder.ins().iconst(
                types::I64,
                i64::try_from(import_idx).map_err(|_| JitError::UnsupportedOperand {
                    operand: format!("import index out of range: {import_idx}"),
                })?,
            );
            let arity_value = builder.ins().iconst(types::I64, i64::from(call_arity));
            let returned = builder.ins().call(
                helpers.call_interpreted,
                &[
                    process,
                    module_value,
                    import_value,
                    arity_value,
                    register_file,
                ],
            );
            let results = builder.inst_results(returned).to_vec();
            handle_helper_return(
                builder,
                results[0],
                results[1],
                blocks.deopt,
                blocks.yield_block,
                helpers.exception,
                exceptions.current_frame(),
                compiled_frame,
                process,
                register_file,
            )?;
            Ok(Some(true))
        }
        Instruction::MakeFun { operands } => {
            let lambda_index = make_fun_lambda_index(operands)?;
            let lambda =
                metadata
                    .lambdas
                    .get(lambda_index)
                    .ok_or_else(|| JitError::UnsupportedOperand {
                        operand: format!("make_fun lambda index {lambda_index}"),
                    })?;
            let num_free =
                usize::try_from(lambda.num_free).map_err(|_| JitError::UnsupportedOperand {
                    operand: format!("make_fun num_free {}", lambda.num_free),
                })?;
            safepoints.record_allocation_site(
                index,
                make_fun_free_var_roots(&Operand::X(0), num_free)?,
            )?;
            let free_vars = make_fun_free_var_operands(num_free)?;
            typed_state.materialize_operands_for_untyped_lowering(
                builder,
                register_file,
                free_vars.iter(),
            );
            lower_make_fun2(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.closure.alloc,
                ClosureMetadata {
                    module,
                    function_index: u64::try_from(lambda_index).map_err(|_| {
                        JitError::UnsupportedOperand {
                            operand: format!("make_fun lambda index {lambda_index}"),
                        }
                    })?,
                    arity: lambda.arity,
                    generation: metadata.generation,
                    unique_id: lambda.unique_id,
                },
                &free_vars,
                &Operand::X(0),
            )?;
            typed_state.clear_operand(&Operand::X(0));
            Ok(Some(false))
        }
        Instruction::CallFun { arity } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let call_arity = immediate_u8(arity, "call_fun arity")?;
            let fun = Operand::X(u32::from(call_arity));
            let (status, returned) = lower_call_fun(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.closure.dispatch,
                ClosureCall {
                    fun: &fun,
                    arity: call_arity,
                },
            )?;
            handle_helper_return(
                builder,
                status,
                returned,
                blocks.deopt,
                blocks.yield_block,
                helpers.exception,
                exceptions.current_frame(),
                compiled_frame,
                process,
                register_file,
            )?;
            Ok(Some(true))
        }
        Instruction::CallFun2 {
            function: fun,
            arity,
            destination,
        } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let call_arity = immediate_u8(arity, "call_fun2 arity")?;
            let (status, returned) = lower_call_fun(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.closure.dispatch,
                ClosureCall {
                    fun,
                    arity: call_arity,
                },
            )?;
            handle_helper_return(
                builder,
                status,
                returned,
                blocks.deopt,
                blocks.yield_block,
                helpers.exception,
                exceptions.current_frame(),
                compiled_frame,
                process,
                register_file,
            )?;
            typed_state.clear_operand(destination);
            Ok(Some(true))
        }
        Instruction::Call { label, .. } | Instruction::CallOnly { label, .. } => {
            let target = blocks.label_block(label_operand(label)?)?;
            charge_reduction_or_yield(builder, helpers.charge, process, blocks.yield_block);
            builder.ins().jump(target, &[]);
            Ok(Some(true))
        }
        Instruction::Apply { arity } => {
            typed_state.materialize_all_for_untyped_call(builder, register_file);
            let call_arity = immediate_u8(arity, "closure apply arity")?;
            let fun = Operand::X(u32::from(call_arity));
            let (status, returned) = lower_call_fun(
                builder,
                LoweringContext {
                    register_file,
                    process,
                    deopt: blocks.deopt,
                },
                helpers.closure.dispatch,
                ClosureCall {
                    fun: &fun,
                    arity: call_arity,
                },
            )?;
            handle_helper_return(
                builder,
                status,
                returned,
                blocks.deopt,
                blocks.yield_block,
                helpers.exception,
                exceptions.current_frame(),
                compiled_frame,
                process,
                register_file,
            )?;
            Ok(Some(true))
        }
        _ => Ok(None),
    }
}
