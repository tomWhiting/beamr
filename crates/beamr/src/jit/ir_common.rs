//! Shared IR helpers used by all opcode translators.

use crate::loader::decode::Operand;
use crate::term::Term;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, MemFlags, Value, types};
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

use super::compiler::JitError;

pub(crate) const REGISTER_WORD_BYTES: i32 = 8;
pub(crate) const X_REGISTER_COUNT: u32 = 1024;
pub(crate) const JIT_DEOPT_SENTINEL: i64 = -1;
pub(crate) const SMALL_INT_TAG_MASK: i64 = 0b111;
pub(crate) const SMALL_INT_SHIFT: i64 = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Register {
    X(u32),
    Y(u32),
}

pub(crate) fn register_operand(operand: &Operand) -> Result<Register, JitError> {
    match operand {
        Operand::X(index) => Ok(Register::X(*index)),
        Operand::Y(index) => Ok(Register::Y(*index)),
        Operand::TypedRegister { register, .. } => register_operand(register),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{other:?}"),
        }),
    }
}

pub(crate) fn read_register_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    register: Register,
) -> Value {
    let offset = register_offset(register);
    builder
        .ins()
        .load(types::I64, MemFlags::trusted(), register_file, offset)
}

pub(crate) fn write_register_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    register: Register,
    value: Value,
) {
    let offset = register_offset(register);
    builder
        .ins()
        .store(MemFlags::trusted(), value, register_file, offset);
}

pub(crate) fn read_operand_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    operand: &Operand,
) -> Result<Value, JitError> {
    match operand {
        Operand::Integer(value) => small_int_constant(builder, *value),
        Operand::Unsigned(value) => {
            let value = i64::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("unsigned literal {value}"),
            })?;
            small_int_constant(builder, value)
        }
        Operand::Atom(Some(atom)) => Ok(builder
            .ins()
            .iconst(types::I64, Term::atom(*atom).raw() as i64)),
        Operand::Atom(None) => Ok(builder.ins().iconst(types::I64, Term::NIL.raw() as i64)),
        operand => Ok(read_register_term(
            builder,
            register_file,
            register_operand(operand)?,
        )),
    }
}

pub(crate) fn write_operand_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    operand: &Operand,
    value: Value,
) -> Result<(), JitError> {
    let register = register_operand(operand)?;
    write_register_term(builder, register_file, register, value);
    Ok(())
}

pub(crate) fn small_int_constant(
    builder: &mut FunctionBuilder<'_>,
    value: i64,
) -> Result<Value, JitError> {
    let term = Term::try_small_int(value).ok_or_else(|| JitError::UnsupportedOperand {
        operand: format!("small integer literal {value}"),
    })?;
    Ok(builder.ins().iconst(types::I64, term.raw() as i64))
}

pub(crate) fn checked_small_int_payload(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    fail: cranelift_codegen::ir::Block,
) -> Value {
    let tag = builder.ins().band_imm(value, SMALL_INT_TAG_MASK);
    let not_small_int = builder.ins().icmp_imm(IntCC::NotEqual, tag, 0);
    branch_to_fail_if(builder, not_small_int, fail);
    builder.ins().sshr_imm(value, SMALL_INT_SHIFT)
}

pub(crate) fn branch_to_fail_if(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    fail: cranelift_codegen::ir::Block,
) {
    let continuation = builder.create_block();
    builder.ins().brif(condition, fail, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

pub(crate) fn validate_read_operand(operand: &Operand) -> Result<(), JitError> {
    match operand {
        Operand::Integer(_) | Operand::Unsigned(_) | Operand::Atom(_) => Ok(()),
        _ => register_operand(operand).map(|_| ()),
    }
}

pub(crate) fn validate_write_operand(operand: &Operand) -> Result<(), JitError> {
    register_operand(operand).map(|_| ())
}

pub(crate) fn validate_label_operand(operand: &Operand) -> Result<(), JitError> {
    label_operand(operand).map(|_| ())
}

pub(crate) fn ensure_known_label(
    labels: &HashMap<u32, usize>,
    operand: &Operand,
) -> Result<(), JitError> {
    let label = label_operand(operand)?;
    if labels.contains_key(&label) {
        Ok(())
    } else {
        Err(JitError::UnknownLabel { label })
    }
}

pub(crate) fn label_operand(operand: &Operand) -> Result<u32, JitError> {
    match operand {
        Operand::Label(label) => Ok(*label),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("expected label, got {other:?}"),
        }),
    }
}

fn register_offset(register: Register) -> i32 {
    let index = match register {
        Register::X(index) => index,
        Register::Y(index) => X_REGISTER_COUNT + index,
    };
    (index as i32) * REGISTER_WORD_BYTES
}
