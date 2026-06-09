//! Closure opcode lowering for the JIT compiler.

use crate::atom::Atom;
use crate::loader::decode::Operand;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{FuncRef, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_allocation::LoweringContext;
use super::ir_common::{read_operand_term, write_operand_term};

const TERM_TAG_MASK: i64 = 0b111;
const BOXED_TAG: i64 = 0b100;
const CLOSURE_HEADER_TAG: i64 = 0x13;
const HEADER_TAG_BITS: i64 = 8;
const HEADER_TAG_MASK: i64 = (1 << HEADER_TAG_BITS) - 1;
const WORD_BYTES: usize = 8;
const CLOSURE_FIXED_PAYLOAD_WORDS: i64 = 6;
const CLOSURE_MODULE_OFFSET: i32 = WORD_BYTES as i32;
const CLOSURE_FUNCTION_INDEX_OFFSET: i32 = (2 * WORD_BYTES) as i32;
const CLOSURE_ARITY_OFFSET: i32 = (3 * WORD_BYTES) as i32;
const CLOSURE_NUM_FREE_OFFSET: i32 = (4 * WORD_BYTES) as i32;
const CLOSURE_GENERATION_OFFSET: i32 = (5 * WORD_BYTES) as i32;
const CLOSURE_UNIQUE_ID_OFFSET: i32 = (6 * WORD_BYTES) as i32;
const CLOSURE_FREE_VAR_BASE_OFFSET: usize = 7 * WORD_BYTES;

pub(crate) struct ClosureHelpers {
    pub(crate) alloc: FuncRef,
    pub(crate) dispatch: FuncRef,
}

pub(crate) struct ClosureMetadata {
    pub(crate) module: Atom,
    pub(crate) function_index: u64,
    pub(crate) arity: u8,
    pub(crate) generation: u64,
    pub(crate) unique_id: u64,
}

pub(crate) struct ClosureCall<'a> {
    pub(crate) fun: &'a Operand,
    pub(crate) arity: u8,
}

pub(crate) fn lower_make_fun2(
    builder: &mut FunctionBuilder<'_>,
    context: LoweringContext,
    alloc_helper: FuncRef,
    metadata: ClosureMetadata,
    free_vars: &[Operand],
    destination: &Operand,
) -> Result<(), JitError> {
    let num_free = i64::try_from(free_vars.len()).map_err(|_| JitError::UnsupportedOperand {
        operand: format!("closure free variable count {}", free_vars.len()),
    })?;
    let num_free_value = builder.ins().iconst(types::I64, num_free);
    let call = builder
        .ins()
        .call(alloc_helper, &[context.process, num_free_value]);
    let heap = builder.inst_results(call)[0];
    branch_to_deopt_if_null(builder, heap, context.deopt);

    let header = ((CLOSURE_FIXED_PAYLOAD_WORDS + num_free) << HEADER_TAG_BITS) | CLOSURE_HEADER_TAG;
    let header = builder.ins().iconst(types::I64, header);
    builder.ins().store(MemFlags::trusted(), header, heap, 0);

    let module = builder.ins().iconst(
        types::I64,
        crate::term::Term::atom(metadata.module).raw() as i64,
    );
    builder
        .ins()
        .store(MemFlags::trusted(), module, heap, CLOSURE_MODULE_OFFSET);
    let function_index =
        i64::try_from(metadata.function_index).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("closure function index {}", metadata.function_index),
        })?;
    let function_index = builder.ins().iconst(types::I64, function_index);
    builder.ins().store(
        MemFlags::trusted(),
        function_index,
        heap,
        CLOSURE_FUNCTION_INDEX_OFFSET,
    );
    let arity = builder.ins().iconst(types::I64, i64::from(metadata.arity));
    builder
        .ins()
        .store(MemFlags::trusted(), arity, heap, CLOSURE_ARITY_OFFSET);
    builder.ins().store(
        MemFlags::trusted(),
        num_free_value,
        heap,
        CLOSURE_NUM_FREE_OFFSET,
    );
    let generation =
        i64::try_from(metadata.generation).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("closure generation {}", metadata.generation),
        })?;
    let generation = builder.ins().iconst(types::I64, generation);
    builder.ins().store(
        MemFlags::trusted(),
        generation,
        heap,
        CLOSURE_GENERATION_OFFSET,
    );
    let unique_id =
        i64::try_from(metadata.unique_id).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("closure unique id {}", metadata.unique_id),
        })?;
    let unique_id = builder.ins().iconst(types::I64, unique_id);
    builder.ins().store(
        MemFlags::trusted(),
        unique_id,
        heap,
        CLOSURE_UNIQUE_ID_OFFSET,
    );

    for (index, free_var) in free_vars.iter().enumerate() {
        let value = read_operand_term(builder, context.register_file, free_var)?;
        let offset = closure_free_var_offset(index)?;
        builder
            .ins()
            .store(MemFlags::trusted(), value, heap, offset);
    }

    let term = builder.ins().bor_imm(heap, BOXED_TAG);
    write_operand_term(builder, context.register_file, destination, term)
}

pub(crate) fn lower_call_fun(
    builder: &mut FunctionBuilder<'_>,
    context: LoweringContext,
    dispatch_helper: FuncRef,
    call: ClosureCall<'_>,
) -> Result<(Value, Value), JitError> {
    let fun_term = read_operand_term(builder, context.register_file, call.fun)?;
    let closure = validate_closure_or_deopt(builder, fun_term, context.deopt);
    let expected_arity = builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        closure,
        CLOSURE_ARITY_OFFSET,
    );
    let arity_matches = builder
        .ins()
        .icmp_imm(IntCC::Equal, expected_arity, i64::from(call.arity));
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(arity_matches, continuation, &[], context.deopt, &[]);
    builder.switch_to_block(continuation);

    let arity_value = builder.ins().iconst(types::I64, i64::from(call.arity));
    let returned = builder.ins().call(
        dispatch_helper,
        &[
            context.process,
            fun_term,
            arity_value,
            context.register_file,
        ],
    );
    let results = builder.inst_results(returned).to_vec();
    Ok((results[0], results[1]))
}

pub(crate) fn make_fun_free_var_roots(
    destination: &Operand,
    num_free: usize,
) -> Result<Vec<Operand>, JitError> {
    let mut roots = Vec::with_capacity(num_free + 1);
    for register in 0..num_free {
        let register = u32::try_from(register).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("closure free variable register {register}"),
        })?;
        roots.push(Operand::X(register));
    }
    roots.push(destination.clone());
    Ok(roots)
}

pub(crate) fn make_fun_free_var_operands(num_free: usize) -> Result<Vec<Operand>, JitError> {
    (0..num_free)
        .map(|register| {
            u32::try_from(register)
                .map(Operand::X)
                .map_err(|_| JitError::UnsupportedOperand {
                    operand: format!("closure free variable register {register}"),
                })
        })
        .collect()
}

fn validate_closure_or_deopt(
    builder: &mut FunctionBuilder<'_>,
    term: Value,
    deopt: cranelift_codegen::ir::Block,
) -> Value {
    let tag = builder.ins().band_imm(term, TERM_TAG_MASK);
    let is_boxed = builder.ins().icmp_imm(IntCC::Equal, tag, BOXED_TAG);
    branch_to_deopt_if_false(builder, is_boxed, deopt);
    let pointer = builder.ins().band_imm(term, !TERM_TAG_MASK);
    let header = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), pointer, 0);
    let header_tag = builder.ins().band_imm(header, HEADER_TAG_MASK);
    let is_closure = builder
        .ins()
        .icmp_imm(IntCC::Equal, header_tag, CLOSURE_HEADER_TAG);
    branch_to_deopt_if_false(builder, is_closure, deopt);
    pointer
}

fn branch_to_deopt_if_false(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    deopt: cranelift_codegen::ir::Block,
) {
    let continuation = builder.create_block();
    builder.ins().brif(condition, continuation, &[], deopt, &[]);
    builder.switch_to_block(continuation);
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

fn closure_free_var_offset(index: usize) -> Result<i32, JitError> {
    let byte_offset =
        CLOSURE_FREE_VAR_BASE_OFFSET
            .checked_add(index.checked_mul(WORD_BYTES).ok_or_else(|| {
                JitError::UnsupportedOperand {
                    operand: format!("closure free variable offset {index}"),
                }
            })?)
            .ok_or_else(|| JitError::UnsupportedOperand {
                operand: format!("closure free variable offset {index}"),
            })?;
    i32::try_from(byte_offset).map_err(|_| JitError::UnsupportedOperand {
        operand: format!("closure free variable offset {index}"),
    })
}
