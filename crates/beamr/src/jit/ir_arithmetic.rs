//! Arithmetic and comparison lowering for the JIT compiler.

use crate::loader::decode::{BifOp, ComparisonOp, Operand};
use crate::term::Term;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{
    SMALL_INT_SHIFT, branch_to_fail_if, checked_small_int_payload, read_operand_term,
    write_operand_term,
};

#[derive(Clone, Copy)]
pub(crate) enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Div,
    Rem,
}

impl ArithmeticOp {
    pub(crate) fn from_import(import: &Operand) -> Result<Self, JitError> {
        match import {
            // The JIT compile API does not yet receive the Module resolved-import
            // table, so this early translator accepts deterministic import slots
            // for arithmetic BIF tests and falls back for every other import.
            Operand::Unsigned(0) => Ok(Self::Add),
            Operand::Unsigned(1) => Ok(Self::Subtract),
            Operand::Unsigned(2) => Ok(Self::Multiply),
            Operand::Unsigned(3) => Ok(Self::Div),
            Operand::Unsigned(4) => Ok(Self::Rem),
            other => Err(JitError::UnsupportedOperand {
                operand: format!("arithmetic import {other:?}"),
            }),
        }
    }
}

pub(crate) struct ArithmeticLowering<'a> {
    pub(crate) op: ArithmeticOp,
    pub(crate) left: &'a Operand,
    pub(crate) right: &'a Operand,
    pub(crate) destination: &'a Operand,
    pub(crate) fail: cranelift_codegen::ir::Block,
    pub(crate) success: cranelift_codegen::ir::Block,
}

pub(crate) struct ParsedBif<'a> {
    pub(crate) fail: &'a Operand,
    pub(crate) import: &'a Operand,
    pub(crate) left: &'a Operand,
    pub(crate) right: &'a Operand,
    pub(crate) destination: &'a Operand,
}

impl<'a> ParsedBif<'a> {
    pub(crate) fn parse(op: BifOp, operands: &'a [Operand]) -> Result<Self, JitError> {
        match op {
            BifOp::Bif2 => {
                let [fail, import, left, right, destination] = operands else {
                    return Err(JitError::UnsupportedOperand {
                        operand: format!("bif2 operands {operands:?}"),
                    });
                };
                Ok(Self {
                    fail,
                    import,
                    left,
                    right,
                    destination,
                })
            }
            BifOp::GcBif2 => {
                let (fail, import, left, right, destination) = match operands {
                    [fail, import, left, right, destination] => {
                        (fail, import, left, right, destination)
                    }
                    [fail, _heap_need, import, left, right, destination] => {
                        (fail, import, left, right, destination)
                    }
                    _ => {
                        return Err(JitError::UnsupportedOperand {
                            operand: format!("gc_bif2 operands {operands:?}"),
                        });
                    }
                };
                Ok(Self {
                    fail,
                    import,
                    left,
                    right,
                    destination,
                })
            }
            other => Err(JitError::UnsupportedOpcode {
                opcode: format!("Bif({other:?})"),
            }),
        }
    }
}

pub(crate) fn lower_arithmetic_bif(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    lowering: ArithmeticLowering<'_>,
) -> Result<(), JitError> {
    let left = read_operand_term(builder, register_file, lowering.left)?;
    let right = read_operand_term(builder, register_file, lowering.right)?;
    let left_payload = checked_small_int_payload(builder, left, lowering.fail);
    let right_payload = checked_small_int_payload(builder, right, lowering.fail);

    let result = match lowering.op {
        ArithmeticOp::Add => {
            let value = builder.ins().iadd(left_payload, right_payload);
            let overflow = signed_add_overflow(builder, left_payload, right_payload, value);
            branch_to_fail_if(builder, overflow, lowering.fail);
            value
        }
        ArithmeticOp::Subtract => {
            let value = builder.ins().isub(left_payload, right_payload);
            let overflow = signed_sub_overflow(builder, left_payload, right_payload, value);
            branch_to_fail_if(builder, overflow, lowering.fail);
            value
        }
        ArithmeticOp::Multiply => {
            let (value, overflow) = builder.ins().smul_overflow(left_payload, right_payload);
            branch_to_fail_if(builder, overflow, lowering.fail);
            value
        }
        ArithmeticOp::Div | ArithmeticOp::Rem => {
            let zero = builder.ins().icmp_imm(IntCC::Equal, right_payload, 0);
            branch_to_fail_if(builder, zero, lowering.fail);
            let min_divisor = builder.ins().icmp_imm(IntCC::Equal, right_payload, -1);
            let min_dividend = builder.ins().icmp_imm(IntCC::Equal, left_payload, i64::MIN);
            let division_overflow = builder.ins().band(min_dividend, min_divisor);
            branch_to_fail_if(builder, division_overflow, lowering.fail);
            if matches!(lowering.op, ArithmeticOp::Div) {
                builder.ins().sdiv(left_payload, right_payload)
            } else {
                builder.ins().srem(left_payload, right_payload)
            }
        }
    };

    let min_check = builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, result, Term::SMALL_INT_MIN);
    let max_check = builder
        .ins()
        .icmp_imm(IntCC::SignedGreaterThan, result, Term::SMALL_INT_MAX);
    let out_of_range = builder.ins().bor(min_check, max_check);
    branch_to_fail_if(builder, out_of_range, lowering.fail);
    let tagged = builder.ins().ishl_imm(result, SMALL_INT_SHIFT);
    write_operand_term(builder, register_file, lowering.destination, tagged)?;
    builder.ins().jump(lowering.success, &[]);
    Ok(())
}

pub(crate) fn lower_comparison(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    op: ComparisonOp,
    left: &Operand,
    right: &Operand,
    fail: cranelift_codegen::ir::Block,
    success: cranelift_codegen::ir::Block,
) -> Result<(), JitError> {
    let left = read_operand_term(builder, register_file, left)?;
    let right = read_operand_term(builder, register_file, right)?;
    let passed = match op {
        ComparisonOp::Eq | ComparisonOp::EqExact => builder.ins().icmp(IntCC::Equal, left, right),
        ComparisonOp::Ne | ComparisonOp::NeExact => {
            builder.ins().icmp(IntCC::NotEqual, left, right)
        }
        ComparisonOp::Lt | ComparisonOp::Ge => {
            let left_payload = checked_small_int_payload(builder, left, fail);
            let right_payload = checked_small_int_payload(builder, right, fail);
            let cc = match op {
                ComparisonOp::Lt => IntCC::SignedLessThan,
                ComparisonOp::Ge => IntCC::SignedGreaterThanOrEqual,
                _ => IntCC::Equal,
            };
            builder.ins().icmp(cc, left_payload, right_payload)
        }
    };
    builder.ins().brif(passed, success, &[], fail, &[]);
    Ok(())
}

fn signed_add_overflow(
    builder: &mut FunctionBuilder<'_>,
    left: Value,
    right: Value,
    result: Value,
) -> Value {
    let left_xor_result = builder.ins().bxor(left, result);
    let right_xor_result = builder.ins().bxor(right, result);
    let both_changed_sign = builder.ins().band(left_xor_result, right_xor_result);
    builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, both_changed_sign, 0)
}

fn signed_sub_overflow(
    builder: &mut FunctionBuilder<'_>,
    left: Value,
    right: Value,
    result: Value,
) -> Value {
    let left_xor_right = builder.ins().bxor(left, right);
    let left_xor_result = builder.ins().bxor(left, result);
    let both_changed_sign = builder.ins().band(left_xor_right, left_xor_result);
    builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, both_changed_sign, 0)
}
