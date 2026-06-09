//! Validation helpers for control-flow-related JIT translation.

use crate::loader::decode::TypeTestOp;

use super::compiler::JitError;
use super::ir_common::{validate_read_operand, validate_write_operand};

pub(crate) fn validate_import_operand(
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

pub(crate) fn validate_supported_type_test(op: TypeTestOp) -> Result<(), JitError> {
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

pub(crate) fn validate_fmove_operands(
    source: &crate::loader::decode::compact::Operand,
    dest: &crate::loader::decode::compact::Operand,
) -> Result<(), JitError> {
    match (source, dest) {
        (
            crate::loader::decode::Operand::FloatRegister(_),
            crate::loader::decode::Operand::FloatRegister(_),
        ) => {
            validate_float_register_operand(source, "fmove source")?;
            validate_float_register_operand(dest, "fmove destination")
        }
        (crate::loader::decode::Operand::FloatRegister(_), _) => {
            validate_float_register_operand(source, "fmove source")?;
            validate_write_operand(dest)
        }
        (_, crate::loader::decode::Operand::FloatRegister(_)) => {
            validate_read_operand(source)?;
            validate_float_register_operand(dest, "fmove destination")
        }
        _ => Err(JitError::UnsupportedOperand {
            operand: format!("fmove source {source:?} destination {dest:?}"),
        }),
    }
}

pub(crate) fn validate_float_register_operand(
    operand: &crate::loader::decode::compact::Operand,
    context: &'static str,
) -> Result<(), JitError> {
    let crate::loader::decode::Operand::FloatRegister(index) = operand else {
        return Err(JitError::UnsupportedOperand {
            operand: format!("{context} {operand:?}"),
        });
    };
    if *index < 16 {
        Ok(())
    } else {
        Err(JitError::UnsupportedOperand {
            operand: format!("{context} fr{index}"),
        })
    }
}
