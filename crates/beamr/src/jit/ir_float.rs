//! Float register and arithmetic lowering for the JIT compiler.

use crate::loader::decode::Operand;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{
    SMALL_INT_SHIFT, SMALL_INT_TAG_MASK, branch_to_fail_if, read_operand_term, write_operand_term,
};

const FLOAT_REGISTER_COUNT: usize = 16;
const TERM_TAG_MASK: i64 = 0b111;
const BOXED_TAG: i64 = 0b100;
const BOXED_FLOAT_HEADER_TAG: i64 = 0x11;
const BOXED_FLOAT_PAYLOAD_OFFSET: i32 = 8;
const F64_EXPONENT_MASK: i64 = 0x7ff0_0000_0000_0000_u64 as i64;

/// Current Cranelift SSA values for BEAM float registers `fr0..fr15`.
#[derive(Clone, Debug)]
pub(crate) struct FloatRegisterMap {
    registers: [Value; FLOAT_REGISTER_COUNT],
}

impl FloatRegisterMap {
    /// Creates a map with all float registers bound to the supplied default f64 value.
    pub(crate) fn new(default: Value) -> Self {
        Self {
            registers: [default; FLOAT_REGISTER_COUNT],
        }
    }

    pub(crate) fn get(&self, fr_index: u32) -> Result<Value, JitError> {
        let index = float_register_index(fr_index)?;
        Ok(self.registers[index])
    }

    pub(crate) fn set(&mut self, fr_index: u32, value: Value) -> Result<(), JitError> {
        let index = float_register_index(fr_index)?;
        self.registers[index] = value;
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FloatBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Copy)]
pub(crate) struct FloatLoweringContext {
    pub(crate) register_file: Value,
    pub(crate) process: Value,
    pub(crate) deopt: Block,
    pub(crate) box_float: FuncRef,
}

pub(crate) fn translate_fconv(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    float_registers: &mut FloatRegisterMap,
    term_src: &Operand,
    fr_dest: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let term = read_operand_term(builder, register_file, term_src)?;
    let value = translate_term_to_float(builder, term, fail);
    let dest = float_register_operand(fr_dest)?;
    float_registers.set(dest, value)
}

pub(crate) fn translate_fmove(
    builder: &mut FunctionBuilder<'_>,
    context: FloatLoweringContext,
    float_registers: &mut FloatRegisterMap,
    source: &Operand,
    dest: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    match (source, dest) {
        (Operand::FloatRegister(source), Operand::FloatRegister(dest)) => {
            let value = float_registers.get(*source)?;
            float_registers.set(*dest, value)
        }
        (Operand::FloatRegister(source), _) => {
            let value = float_registers.get(*source)?;
            let call = builder
                .ins()
                .call(context.box_float, &[context.process, value]);
            let term = builder.inst_results(call)[0];
            branch_to_deopt_if_zero(builder, term, context.deopt);
            write_operand_term(builder, context.register_file, dest, term)
        }
        (_, Operand::FloatRegister(dest)) => {
            let term = read_operand_term(builder, context.register_file, source)?;
            let value = translate_boxed_float_to_float(builder, term, fail);
            float_registers.set(*dest, value)
        }
        _ => Err(JitError::UnsupportedOperand {
            operand: format!("fmove source {source:?} dest {dest:?}"),
        }),
    }
}

pub(crate) fn translate_float_binary(
    builder: &mut FunctionBuilder<'_>,
    float_registers: &mut FloatRegisterMap,
    op: FloatBinaryOp,
    left: &Operand,
    right: &Operand,
    dest: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let left = float_registers.get(float_register_operand(left)?)?;
    let right = float_registers.get(float_register_operand(right)?)?;

    if matches!(op, FloatBinaryOp::Divide) {
        branch_to_fail_if_float_zero(builder, right, fail);
    }

    let result = match op {
        FloatBinaryOp::Add => builder.ins().fadd(left, right),
        FloatBinaryOp::Subtract => builder.ins().fsub(left, right),
        FloatBinaryOp::Multiply => builder.ins().fmul(left, right),
        FloatBinaryOp::Divide => builder.ins().fdiv(left, right),
    };
    branch_to_fail_if_nan_or_inf(builder, result, fail);
    let dest = float_register_operand(dest)?;
    float_registers.set(dest, result)
}

pub(crate) fn translate_fnegate(
    builder: &mut FunctionBuilder<'_>,
    float_registers: &mut FloatRegisterMap,
    source: &Operand,
    dest: &Operand,
    fail: Block,
) -> Result<(), JitError> {
    let source = float_registers.get(float_register_operand(source)?)?;
    let result = builder.ins().fneg(source);
    branch_to_fail_if_nan_or_inf(builder, result, fail);
    let dest = float_register_operand(dest)?;
    float_registers.set(dest, result)
}

pub(crate) fn float_boxing_roots(dest: &Operand) -> Result<Vec<Operand>, JitError> {
    match dest {
        Operand::X(_) | Operand::Y(_) | Operand::TypedRegister { .. } => Ok(vec![dest.clone()]),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("float boxing destination {other:?}"),
        }),
    }
}

fn translate_term_to_float(builder: &mut FunctionBuilder<'_>, term: Value, fail: Block) -> Value {
    let tag = builder.ins().band_imm(term, SMALL_INT_TAG_MASK);
    let is_small = builder.ins().icmp_imm(IntCC::Equal, tag, 0);
    let small_block = builder.create_block();
    let boxed_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::F64);
    builder
        .ins()
        .brif(is_small, small_block, &[], boxed_block, &[]);

    builder.switch_to_block(small_block);
    let payload = builder.ins().sshr_imm(term, SMALL_INT_SHIFT);
    let value = builder.ins().fcvt_from_sint(types::F64, payload);
    builder.ins().jump(done, &[value.into()]);

    builder.switch_to_block(boxed_block);
    let value = translate_boxed_float_to_float(builder, term, fail);
    builder.ins().jump(done, &[value.into()]);

    builder.switch_to_block(done);
    builder.block_params(done)[0]
}

fn translate_boxed_float_to_float(
    builder: &mut FunctionBuilder<'_>,
    term: Value,
    fail: Block,
) -> Value {
    let tag = builder.ins().band_imm(term, TERM_TAG_MASK);
    let not_boxed = builder.ins().icmp_imm(IntCC::NotEqual, tag, BOXED_TAG);
    branch_to_fail_if(builder, not_boxed, fail);
    let heap = builder.ins().band_imm(term, !TERM_TAG_MASK);
    let header = builder.ins().load(types::I64, MemFlags::trusted(), heap, 0);
    let header_tag = builder.ins().band_imm(header, 0xff);
    let not_float = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, header_tag, BOXED_FLOAT_HEADER_TAG);
    branch_to_fail_if(builder, not_float, fail);
    builder.ins().load(
        types::F64,
        MemFlags::trusted(),
        heap,
        BOXED_FLOAT_PAYLOAD_OFFSET,
    )
}

fn branch_to_fail_if_float_zero(builder: &mut FunctionBuilder<'_>, value: Value, fail: Block) {
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let magnitude = builder.ins().band_imm(bits, 0x7fff_ffff_ffff_ffff_i64);
    let is_zero = builder.ins().icmp_imm(IntCC::Equal, magnitude, 0);
    branch_to_fail_if(builder, is_zero, fail);
}

fn branch_to_fail_if_nan_or_inf(builder: &mut FunctionBuilder<'_>, value: Value, fail: Block) {
    let unordered = builder.ins().fcmp(FloatCC::Unordered, value, value);
    branch_to_fail_if(builder, unordered, fail);

    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let exponent = builder.ins().band_imm(bits, F64_EXPONENT_MASK);
    let exponent_all_ones = builder
        .ins()
        .icmp_imm(IntCC::Equal, exponent, F64_EXPONENT_MASK);
    branch_to_fail_if(builder, exponent_all_ones, fail);
}

fn branch_to_deopt_if_zero(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    deopt: cranelift_codegen::ir::Block,
) {
    let is_zero = builder.ins().icmp_imm(IntCC::Equal, value, 0);
    let continuation = builder.create_block();
    builder.ins().brif(is_zero, deopt, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn float_register_operand(operand: &Operand) -> Result<u32, JitError> {
    match operand {
        Operand::FloatRegister(index) => {
            float_register_index(*index)?;
            Ok(*index)
        }
        other => Err(JitError::UnsupportedOperand {
            operand: format!("expected float register, got {other:?}"),
        }),
    }
}

fn float_register_index(index: u32) -> Result<usize, JitError> {
    let index = usize::try_from(index).map_err(|_| JitError::UnsupportedOperand {
        operand: format!("float register fr{index}"),
    })?;
    if index < FLOAT_REGISTER_COUNT {
        Ok(index)
    } else {
        Err(JitError::UnsupportedOperand {
            operand: format!("float register fr{index}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FloatRegisterMap;
    use cranelift_codegen::ir::{Value, packed_option::ReservedValue};

    #[test]
    fn float_register_map_stores_separate_slots() {
        let default = Value::reserved_value();
        let replacement = Value::from_u32(7);
        let mut map = FloatRegisterMap::new(default);

        map.set(0, replacement).expect("fr0 is valid");

        assert_eq!(map.get(0), Ok(replacement));
        assert_eq!(map.get(1), Ok(default));
    }
}
