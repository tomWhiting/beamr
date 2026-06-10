//! Free helper functions used by instruction dispatch lowering.

use crate::jit::ir_common::write_operand_term;
use crate::jit::ir_exceptions::{
    CompiledFrameInfo, ExceptionDispatch, ExceptionHelpers, JIT_STATUS_DEOPT, JIT_STATUS_NORMAL,
    JIT_STATUS_YIELD, dispatch_exception_status, return_status,
};
use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use cranelift_codegen::CodegenError;
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_frontend::FunctionBuilder;

use super::JitError;
use super::ir_typed::TypedRegisterState;

pub(super) fn branch_to_yield_if_exhausted(
    builder: &mut FunctionBuilder<'_>,
    exhausted: cranelift_codegen::ir::Value,
    yield_block: cranelift_codegen::ir::Block,
    continuation: cranelift_codegen::ir::Block,
) {
    let should_yield = builder.ins().icmp_imm(IntCC::NotEqual, exhausted, 0);
    builder
        .ins()
        .brif(should_yield, yield_block, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

pub(super) fn charge_reduction_or_yield(
    builder: &mut FunctionBuilder<'_>,
    charge_helper: cranelift_codegen::ir::FuncRef,
    process: cranelift_codegen::ir::Value,
    yield_block: cranelift_codegen::ir::Block,
) {
    let exhausted = builder.ins().call(charge_helper, &[process]);
    let exhausted = builder.inst_results(exhausted)[0];
    let continuation = builder.create_block();
    branch_to_yield_if_exhausted(builder, exhausted, yield_block, continuation);
}

fn branch_to_status_blocks(
    builder: &mut FunctionBuilder<'_>,
    status: cranelift_codegen::ir::Value,
    deopt_block: cranelift_codegen::ir::Block,
    yield_block: cranelift_codegen::ir::Block,
    continuation: cranelift_codegen::ir::Block,
) {
    let is_deopt = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(JIT_STATUS_DEOPT));
    let check_yield = builder.create_block();
    builder
        .ins()
        .brif(is_deopt, deopt_block, &[], check_yield, &[]);
    builder.switch_to_block(check_yield);
    let is_yield = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(JIT_STATUS_YIELD));
    builder
        .ins()
        .brif(is_yield, yield_block, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

pub(super) fn supported_binary_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::BsStartMatch3
            | BinaryOp::BsStartMatch4
            | BinaryOp::BsGetInteger2
            | BinaryOp::BsGetBinary2
            | BinaryOp::BsTestTail2
            | BinaryOp::BsTestUnit
            | BinaryOp::BsGetUtf8
            | BinaryOp::BsGetUtf16
            | BinaryOp::BsGetUtf32
            | BinaryOp::BsInitWritable
            | BinaryOp::BsCreateBin
            | BinaryOp::BsGetTail
            | BinaryOp::BsMatch
    )
}

pub(super) fn allocation_binary_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::BsStartMatch3
            | BinaryOp::BsStartMatch4
            | BinaryOp::BsGetBinary2
            | BinaryOp::BsInitWritable
            | BinaryOp::BsCreateBin
            | BinaryOp::BsGetTail
            | BinaryOp::BsMatch
    )
}

pub(super) fn clear_binary_outputs(
    _state: &mut TypedRegisterState,
    _op: BinaryOp,
    _operands: &[Operand],
) {
    // Binary lowerings are emitted after `materialize_all_for_untyped_call`, so
    // typed-register metadata has already been conservatively cleared.
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_helper_return(
    builder: &mut FunctionBuilder<'_>,
    status: cranelift_codegen::ir::Value,
    returned: cranelift_codegen::ir::Value,
    deopt_block: cranelift_codegen::ir::Block,
    yield_block: cranelift_codegen::ir::Block,
    exception_helpers: ExceptionHelpers,
    frame: Option<crate::jit::ir_exceptions::TryCatchFrame>,
    compiled_frame: CompiledFrameInfo,
    process: cranelift_codegen::ir::Value,
    register_file: cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let status_continuation = builder.create_block();
    branch_to_status_blocks(
        builder,
        status,
        deopt_block,
        yield_block,
        status_continuation,
    );
    let normal_continuation = builder.create_block();
    dispatch_exception_status(
        builder,
        ExceptionDispatch {
            helpers: exception_helpers,
            frame,
            compiled_frame,
            process,
            register_file,
            status,
            value: returned,
            continuation: normal_continuation,
        },
    );
    write_operand_term(
        builder,
        register_file,
        &crate::loader::decode::Operand::X(0),
        returned,
    )?;
    return_status(builder, JIT_STATUS_NORMAL, returned);
    Ok(())
}

pub(super) fn make_fun_lambda_index(operands: &[Operand]) -> Result<usize, JitError> {
    match operands {
        [index] => immediate_usize(index, "make_fun lambda index"),
        [index, _uniq, _old_index] => immediate_usize(index, "make_fun lambda index"),
        _ => Err(JitError::UnsupportedOperand {
            operand: format!("make_fun operands {operands:?}"),
        }),
    }
}

pub(super) fn import_index(import: &Operand) -> Result<usize, JitError> {
    match import {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            })
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            })
        }
        other => Err(JitError::UnsupportedOperand {
            operand: format!("external call import must be an index, got {other:?}"),
        }),
    }
}

pub(super) fn immediate_u8(operand: &Operand, context: &'static str) -> Result<u8, JitError> {
    match operand {
        Operand::Unsigned(value) => {
            u8::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} out of range: {value}"),
            })
        }
        Operand::Integer(value) => u8::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("{context} out of range: {value}"),
        }),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{context} must be an integer, got {other:?}"),
        }),
    }
}

fn immediate_usize(operand: &Operand, context: &'static str) -> Result<usize, JitError> {
    match operand {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} out of range: {value}"),
            })
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} out of range: {value}"),
            })
        }
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{context} must be an integer, got {other:?}"),
        }),
    }
}

pub(super) fn cranelift_error(error: cranelift_module::ModuleError) -> JitError {
    match error {
        cranelift_module::ModuleError::Compilation(CodegenError::Verifier(errors)) => {
            JitError::CraneliftError(errors.to_string())
        }
        other => JitError::CraneliftError(other.to_string()),
    }
}
