//! Heap allocation opcode lowering for the JIT compiler.

use crate::loader::decode::Operand;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{FuncRef, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{read_operand_term, write_operand_term};

const BOXED_TAG: i64 = 0b100;
const LIST_TAG: i64 = 0b101;
const TUPLE_HEADER_TAG: i64 = 0x10;
const HEADER_TAG_BITS: i64 = 8;
const WORD_BYTES: usize = 8;

pub(crate) struct AllocationHelpers {
    pub(crate) tuple: FuncRef,
    pub(crate) cons: FuncRef,
}

pub(crate) struct LoweringContext {
    pub(crate) register_file: Value,
    pub(crate) process: Value,
    pub(crate) deopt: cranelift_codegen::ir::Block,
}

pub(crate) fn lower_put_list(
    builder: &mut FunctionBuilder<'_>,
    context: LoweringContext,
    cons_helper: FuncRef,
    head: &Operand,
    tail: &Operand,
    destination: &Operand,
) -> Result<(), JitError> {
    let call = builder.ins().call(cons_helper, &[context.process]);
    let heap = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, heap, context.deopt);
    let head_value = read_operand_term(builder, context.register_file, head)?;
    let tail_value = read_operand_term(builder, context.register_file, tail)?;
    builder
        .ins()
        .store(MemFlags::trusted(), head_value, heap, 0);
    builder
        .ins()
        .store(MemFlags::trusted(), tail_value, heap, WORD_BYTES as i32);
    let term = builder.ins().bor_imm(heap, LIST_TAG);
    write_operand_term(builder, context.register_file, destination, term)
}

pub(crate) fn lower_put_tuple2(
    builder: &mut FunctionBuilder<'_>,
    context: LoweringContext,
    tuple_helper: FuncRef,
    destination: &Operand,
    elements: &Operand,
) -> Result<(), JitError> {
    let Operand::List(elements) = elements else {
        return Err(tuple_elements_error(elements));
    };
    let arity = i64::try_from(elements.len()).map_err(|_| JitError::UnsupportedOperand {
        operand: format!("tuple arity {}", elements.len()),
    })?;
    let arity_value = builder.ins().iconst(types::I64, arity);
    let call = builder
        .ins()
        .call(tuple_helper, &[context.process, arity_value]);
    let heap = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, heap, context.deopt);

    let header = (arity << HEADER_TAG_BITS) | TUPLE_HEADER_TAG;
    let header = builder.ins().iconst(types::I64, header);
    builder.ins().store(MemFlags::trusted(), header, heap, 0);
    for (index, element) in elements.iter().enumerate() {
        let value = read_operand_term(builder, context.register_file, element)?;
        let offset =
            i32::try_from((index + 1) * WORD_BYTES).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("tuple element offset {index}"),
            })?;
        builder
            .ins()
            .store(MemFlags::trusted(), value, heap, offset);
    }

    let term = builder.ins().bor_imm(heap, BOXED_TAG);
    write_operand_term(builder, context.register_file, destination, term)
}

pub(crate) fn tuple_root_operands(
    destination: &Operand,
    elements: &Operand,
) -> Result<Vec<Operand>, JitError> {
    let Operand::List(elements) = elements else {
        return Err(tuple_elements_error(elements));
    };
    let mut roots = Vec::with_capacity(elements.len() + 1);
    roots.extend(elements.iter().cloned());
    roots.push(destination.clone());
    Ok(roots)
}

fn branch_to_deopt_if_null(
    builder: &mut FunctionBuilder<'_>,
    pointer: Value,
    deopt: cranelift_codegen::ir::Block,
) {
    let is_null = builder.ins().icmp_imm(IntCC::Equal, pointer, 0);
    let continuation = builder.create_block();
    builder.ins().brif(is_null, deopt, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn tuple_elements_error(elements: &Operand) -> JitError {
    JitError::UnsupportedOperand {
        operand: format!("put_tuple2 elements must be a list, got {elements:?}"),
    }
}
