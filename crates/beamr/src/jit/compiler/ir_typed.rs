use crate::jit::type_info::{FunctionSignature, TypeDescriptor};
use crate::loader::decode::TypeTestOp;
use crate::loader::decode::compact::Operand;
use crate::term::Term;
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

use crate::jit::ir_arithmetic::{ArithmeticLowering, ArithmeticOp};
use crate::jit::ir_common::{
    Register, SMALL_INT_SHIFT, read_operand_term, read_register_term, register_operand,
    write_register_term,
};
use crate::jit::ir_exceptions::return_status_raw;
use crate::jit::ir_guards::immediate_usize;

use super::JitError;

pub(super) fn lower_typed_int_arithmetic(
    builder: &mut FunctionBuilder<'_>,
    register_file: cranelift_codegen::ir::Value,
    lowering: ArithmeticLowering<'_>,
    deopt: cranelift_codegen::ir::Block,
) -> Result<(), JitError> {
    let left = read_register_term(builder, register_file, register_operand(lowering.left)?);
    let right = read_register_term(builder, register_file, register_operand(lowering.right)?);
    let result = match lowering.op {
        ArithmeticOp::Add => {
            let value = builder.ins().iadd(left, right);
            let overflow = signed_add_overflow(builder, left, right, value);
            branch_to_block_if(builder, overflow, deopt);
            branch_to_deopt_if_not_small_int(builder, value, deopt);
            value
        }
        ArithmeticOp::Subtract => {
            let value = builder.ins().isub(left, right);
            let overflow = signed_sub_overflow(builder, left, right, value);
            branch_to_block_if(builder, overflow, deopt);
            branch_to_deopt_if_not_small_int(builder, value, deopt);
            value
        }
        ArithmeticOp::Multiply => {
            let (value, overflow) = builder.ins().smul_overflow(left, right);
            branch_to_block_if(builder, overflow, deopt);
            branch_to_deopt_if_not_small_int(builder, value, deopt);
            value
        }
        ArithmeticOp::Div | ArithmeticOp::Rem => {
            let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
            branch_to_block_if(builder, zero, lowering.fail);
            let min_divisor = builder.ins().icmp_imm(IntCC::Equal, right, -1);
            let min_dividend = builder.ins().icmp_imm(IntCC::Equal, left, i64::MIN);
            let division_overflow = builder.ins().band(min_dividend, min_divisor);
            branch_to_block_if(builder, division_overflow, deopt);
            if matches!(lowering.op, ArithmeticOp::Div) {
                builder.ins().sdiv(left, right)
            } else {
                builder.ins().srem(left, right)
            }
        }
    };
    write_register_term(
        builder,
        register_file,
        register_operand(lowering.destination)?,
        result,
    );
    builder.ins().jump(lowering.success, &[]);
    Ok(())
}

pub(super) fn lower_typed_type_test(
    builder: &mut FunctionBuilder<'_>,
    typed_state: &TypedRegisterState,
    op: TypeTestOp,
    value: &Operand,
    fail: cranelift_codegen::ir::Block,
    success: cranelift_codegen::ir::Block,
) -> Result<bool, JitError> {
    let Some(type_) = typed_state.operand_type(value) else {
        return Ok(false);
    };
    match typed_guard_decision(type_, op) {
        GuardDecision::AlwaysTrue => builder.ins().jump(success, &[]),
        GuardDecision::AlwaysFalse => builder.ins().jump(fail, &[]),
        GuardDecision::Unknown => return Ok(false),
    };
    Ok(true)
}

pub(super) fn lower_typed_test_arity(
    builder: &mut FunctionBuilder<'_>,
    typed_state: &TypedRegisterState,
    tuple: &Operand,
    arity: &Operand,
    fail: cranelift_codegen::ir::Block,
    success: cranelift_codegen::ir::Block,
) -> Result<bool, JitError> {
    let Some(TypeDescriptor::Tuple(elements)) = typed_state.operand_type(tuple) else {
        return Ok(false);
    };
    let expected = immediate_usize(arity, "test_arity arity")?;
    if elements.len() == expected {
        builder.ins().jump(success, &[]);
    } else {
        builder.ins().jump(fail, &[]);
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum GuardDecision {
    AlwaysTrue,
    AlwaysFalse,
    Unknown,
}

fn typed_guard_decision(type_: &TypeDescriptor, op: TypeTestOp) -> GuardDecision {
    match op {
        TypeTestOp::IsInteger => bool_decision(matches!(type_, TypeDescriptor::Int)),
        TypeTestOp::IsAtom => {
            bool_decision(matches!(type_, TypeDescriptor::Atom | TypeDescriptor::Bool))
        }
        TypeTestOp::IsList => bool_decision(matches!(
            type_,
            TypeDescriptor::List(_) | TypeDescriptor::Nil
        )),
        TypeTestOp::IsTuple => bool_decision(matches!(type_, TypeDescriptor::Tuple(_))),
        TypeTestOp::IsBoolean => bool_decision(matches!(type_, TypeDescriptor::Bool)),
        TypeTestOp::IsNonemptyList => {
            if matches!(type_, TypeDescriptor::Nil) {
                GuardDecision::AlwaysFalse
            } else {
                GuardDecision::Unknown
            }
        }
        TypeTestOp::IsBinary => bool_decision(matches!(
            type_,
            TypeDescriptor::String | TypeDescriptor::BitArray
        )),
        TypeTestOp::IsFloat => bool_decision(matches!(type_, TypeDescriptor::Float)),
        _ => GuardDecision::Unknown,
    }
}

fn bool_decision(value: bool) -> GuardDecision {
    if value {
        GuardDecision::AlwaysTrue
    } else {
        GuardDecision::AlwaysFalse
    }
}

fn signed_add_overflow(
    builder: &mut FunctionBuilder<'_>,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
    result: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let left_xor_result = builder.ins().bxor(left, result);
    let right_xor_result = builder.ins().bxor(right, result);
    let both_changed_sign = builder.ins().band(left_xor_result, right_xor_result);
    builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, both_changed_sign, 0)
}

fn signed_sub_overflow(
    builder: &mut FunctionBuilder<'_>,
    left: cranelift_codegen::ir::Value,
    right: cranelift_codegen::ir::Value,
    result: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let left_xor_right = builder.ins().bxor(left, right);
    let left_xor_result = builder.ins().bxor(left, result);
    let both_changed_sign = builder.ins().band(left_xor_right, left_xor_result);
    builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, both_changed_sign, 0)
}

fn branch_to_deopt_if_not_small_int(
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
    deopt: cranelift_codegen::ir::Block,
) {
    let below_min = builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, value, Term::SMALL_INT_MIN);
    branch_to_block_if(builder, below_min, deopt);
    let above_max = builder
        .ins()
        .icmp_imm(IntCC::SignedGreaterThan, value, Term::SMALL_INT_MAX);
    branch_to_block_if(builder, above_max, deopt);
}

fn branch_to_block_if(
    builder: &mut FunctionBuilder<'_>,
    condition: cranelift_codegen::ir::Value,
    target: cranelift_codegen::ir::Block,
) {
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(condition, target, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

pub(super) fn float_fail_block(
    builder: &mut FunctionBuilder<'_>,
    status: u8,
    raw: i64,
    continuation: cranelift_codegen::ir::Block,
) -> cranelift_codegen::ir::Block {
    let current = builder.current_block().unwrap_or(continuation);
    let fail = builder.create_block();
    builder.switch_to_block(fail);
    return_status_raw(builder, status, raw);
    builder.switch_to_block(current);
    fail
}
pub(super) struct TypedRegisterState {
    registers: HashMap<Register, TypeDescriptor>,
}

impl TypedRegisterState {
    pub(super) fn new(signature: Option<&FunctionSignature>) -> Self {
        let mut registers = HashMap::new();
        if let Some(signature) = signature {
            for (index, type_) in signature.param_types.iter().enumerate() {
                if let Some(type_) = supported_type(type_)
                    && let Ok(index) = u32::try_from(index)
                {
                    registers.insert(Register::X(index), type_);
                }
            }
        }
        Self { registers }
    }

    pub(super) fn initialize_entry_values(
        &self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
    ) {
        for (register, type_) in &self.registers {
            if matches!(type_, TypeDescriptor::Int) {
                let tagged = read_register_term(builder, register_file, *register);
                let payload = builder.ins().sshr_imm(tagged, SMALL_INT_SHIFT);
                write_register_term(builder, register_file, *register, payload);
            }
        }
    }

    pub(super) fn read_return_value(
        &self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value {
        let value = read_register_term(builder, register_file, Register::X(0));
        if matches!(
            self.registers.get(&Register::X(0)),
            Some(TypeDescriptor::Int)
        ) {
            builder.ins().ishl_imm(value, SMALL_INT_SHIFT)
        } else {
            value
        }
    }

    pub(super) fn materialize_all_for_untyped_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
    ) {
        let registers = self.registers.keys().copied().collect::<Vec<_>>();
        self.materialize_registers(builder, register_file, registers);
    }

    pub(super) fn materialize_operands_for_untyped_lowering<'a>(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
        operands: impl IntoIterator<Item = &'a Operand>,
    ) {
        let registers = operands
            .into_iter()
            .filter_map(|operand| register_operand(operand).ok())
            .collect::<Vec<_>>();
        self.materialize_registers(builder, register_file, registers);
    }

    pub(super) fn materialize_tuple_elements_for_untyped_lowering(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
        elements: &Operand,
    ) -> Result<(), JitError> {
        let Operand::List(elements) = elements else {
            return Err(JitError::UnsupportedOperand {
                operand: format!("put_tuple2 elements must be a list, got {elements:?}"),
            });
        };
        let registers = elements
            .iter()
            .filter_map(|operand| register_operand(operand).ok())
            .collect::<Vec<_>>();
        self.materialize_registers(builder, register_file, registers);
        Ok(())
    }

    pub(super) fn materialize_registers(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
        registers: impl IntoIterator<Item = Register>,
    ) {
        for register in registers {
            if matches!(self.registers.remove(&register), Some(TypeDescriptor::Int)) {
                let payload = read_register_term(builder, register_file, register);
                let tagged = builder.ins().ishl_imm(payload, SMALL_INT_SHIFT);
                write_register_term(builder, register_file, register, tagged);
            }
        }
    }

    pub(super) fn read_operand_value(
        &self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
        operand: &Operand,
    ) -> Result<cranelift_codegen::ir::Value, JitError> {
        if matches!(self.operand_type(operand), Some(TypeDescriptor::Int)) {
            Ok(read_register_term(
                builder,
                register_file,
                register_operand(operand)?,
            ))
        } else {
            read_operand_term(builder, register_file, operand)
        }
    }

    pub(super) fn operand_type(&self, operand: &Operand) -> Option<&TypeDescriptor> {
        let register = register_operand(operand).ok()?;
        self.registers.get(&register)
    }

    pub(super) fn set_operand_type(&mut self, operand: &Operand, type_: TypeDescriptor) {
        self.set_optional_operand_type(operand, Some(type_));
    }

    pub(super) fn set_optional_operand_type(
        &mut self,
        operand: &Operand,
        type_: Option<TypeDescriptor>,
    ) {
        if let Ok(register) = register_operand(operand) {
            if let Some(type_) = type_.as_ref().and_then(supported_type) {
                self.registers.insert(register, type_);
            } else {
                self.registers.remove(&register);
            }
        }
    }

    pub(super) fn clear_operand(&mut self, operand: &Operand) {
        if let Ok(register) = register_operand(operand) {
            self.registers.remove(&register);
        }
    }

    pub(super) fn copy(&mut self, source: &Operand, destination: &Operand) {
        if let Some(type_) = self.operand_type(source).cloned() {
            self.set_operand_type(destination, type_);
        } else {
            self.clear_operand(destination);
        }
    }

    pub(super) fn swap(&mut self, left: &Operand, right: &Operand) {
        let Ok(left_register) = register_operand(left) else {
            return;
        };
        let Ok(right_register) = register_operand(right) else {
            return;
        };
        let left_type = self.registers.remove(&left_register);
        let right_type = self.registers.remove(&right_register);
        if let Some(type_) = right_type {
            self.registers.insert(left_register, type_);
        }
        if let Some(type_) = left_type {
            self.registers.insert(right_register, type_);
        }
    }

    pub(super) fn operands_are_int(&self, left: &Operand, right: &Operand) -> bool {
        matches!(self.operand_type(left), Some(TypeDescriptor::Int))
            && matches!(self.operand_type(right), Some(TypeDescriptor::Int))
    }

    pub(super) fn can_write_typed(&self, operand: &Operand) -> bool {
        register_operand(operand).is_ok()
    }

    pub(super) fn list_type_from_head(&self, head: &Operand) -> Option<TypeDescriptor> {
        self.operand_type(head)
            .cloned()
            .map(|head_type| TypeDescriptor::List(Box::new(head_type)))
    }

    pub(super) fn list_head_type(&self, source: &Operand) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::List(inner)) = self.operand_type(source) {
            Some(inner.as_ref().clone())
        } else {
            None
        }
    }

    pub(super) fn list_tail_type(&self, source: &Operand) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::List(inner)) = self.operand_type(source) {
            Some(TypeDescriptor::List(inner.clone()))
        } else {
            None
        }
    }

    pub(super) fn tuple_type_from_elements(&self, elements: &Operand) -> Option<TypeDescriptor> {
        let Operand::List(elements) = elements else {
            return None;
        };
        elements
            .iter()
            .map(|element| self.operand_type(element).cloned())
            .collect::<Option<Vec<_>>>()
            .map(TypeDescriptor::Tuple)
    }

    pub(super) fn tuple_element_type(
        &self,
        source: &Operand,
        index: usize,
    ) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::Tuple(elements)) = self.operand_type(source) {
            elements.get(index).cloned()
        } else {
            None
        }
    }

    pub(super) fn mark_loaded_operand_type(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
        operand: &Operand,
        type_: Option<TypeDescriptor>,
    ) {
        let Ok(register) = register_operand(operand) else {
            return;
        };
        let Some(type_) = type_.as_ref().and_then(supported_type) else {
            self.registers.remove(&register);
            return;
        };
        if matches!(type_, TypeDescriptor::Int) {
            let tagged = read_register_term(builder, register_file, register);
            let payload = builder.ins().sshr_imm(tagged, SMALL_INT_SHIFT);
            write_register_term(builder, register_file, register, payload);
        }
        self.registers.insert(register, type_);
    }
}

pub(super) fn supported_type(type_: &TypeDescriptor) -> Option<TypeDescriptor> {
    match type_ {
        TypeDescriptor::Int | TypeDescriptor::Bool | TypeDescriptor::Atom | TypeDescriptor::Nil => {
            Some(type_.clone())
        }
        TypeDescriptor::List(inner) => {
            supported_type(inner).map(|inner| TypeDescriptor::List(Box::new(inner)))
        }
        TypeDescriptor::Tuple(elements) => elements
            .iter()
            .map(supported_type)
            .collect::<Option<Vec<_>>>()
            .map(TypeDescriptor::Tuple),
        _ => None,
    }
}
