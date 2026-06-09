//! Guard and type-test lowering for the JIT compiler.

use crate::loader::decode::{Operand, TypeTestOp};
use crate::term::Term;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, InstBuilder, MemFlags, Value, types};
use cranelift_frontend::FunctionBuilder;

use super::compiler::JitError;
use super::ir_common::{branch_to_fail_if, read_operand_term};

const TERM_TAG_MASK: i64 = 0b111;
const SMALL_INT_TAG: i64 = 0b000;
const ATOM_TAG: i64 = 0b001;
const PID_TAG: i64 = 0b010;
const BOXED_TAG: i64 = 0b100;
const LIST_TAG: i64 = 0b101;
const HEADER_TAG_MASK: i64 = 0xff;
const HEADER_SIZE_SHIFT: i64 = 8;
const TUPLE_HEADER_TAG: i64 = 0x10;
const BINARY_HEADER_TAG: i64 = 0x16;
const PROC_BIN_HEADER_TAG: i64 = 0x19;
const SUB_BINARY_HEADER_TAG: i64 = 0x1a;
const EXTERNAL_PID_HEADER_TAG: i64 = 0x1c;

pub(crate) fn lower_type_test(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    op: TypeTestOp,
    value: &Operand,
    fail: Block,
    success: Block,
) -> Result<(), JitError> {
    let value = read_operand_term(builder, register_file, value)?;
    let passed = match op {
        TypeTestOp::IsInteger => primary_tag_matches(builder, value, SMALL_INT_TAG),
        TypeTestOp::IsAtom => primary_tag_matches(builder, value, ATOM_TAG),
        TypeTestOp::IsList => {
            let cons = primary_tag_matches(builder, value, LIST_TAG);
            let nil = builder
                .ins()
                .icmp_imm(IntCC::Equal, value, Term::NIL.raw() as i64);
            builder.ins().bor(cons, nil)
        }
        TypeTestOp::IsPid => {
            lower_pid_type_test(builder, value, fail, success);
            return Ok(());
        }
        TypeTestOp::IsBinary => {
            lower_boxed_header_type_test(
                builder,
                value,
                &[
                    BINARY_HEADER_TAG,
                    PROC_BIN_HEADER_TAG,
                    SUB_BINARY_HEADER_TAG,
                ],
                fail,
                success,
            );
            return Ok(());
        }
        TypeTestOp::IsTuple => {
            lower_boxed_header_type_test(builder, value, &[TUPLE_HEADER_TAG], fail, success);
            return Ok(());
        }
        TypeTestOp::IsFloat => {
            return Err(JitError::UnsupportedOpcode {
                opcode: "TypeTest(IsFloat)".to_owned(),
            });
        }
        other => {
            return Err(JitError::UnsupportedOpcode {
                opcode: format!("TypeTest({other:?})"),
            });
        }
    };
    builder.ins().brif(passed, success, &[], fail, &[]);
    Ok(())
}

pub(crate) fn lower_test_arity(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    tuple: &Operand,
    arity: &Operand,
    fail: Block,
    success: Block,
) -> Result<(), JitError> {
    let expected = immediate_usize(arity, "test_arity arity")?;
    let tuple = read_operand_term(builder, register_file, tuple)?;
    let header = checked_tuple_header(builder, tuple, fail);
    let actual = builder.ins().ushr_imm(header, HEADER_SIZE_SHIFT);
    let passed = builder
        .ins()
        .icmp_imm(IntCC::Equal, actual, expected as i64);
    builder.ins().brif(passed, success, &[], fail, &[]);
    Ok(())
}

pub(crate) fn lower_is_tagged_tuple(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    value: &Operand,
    arity: &Operand,
    tag: &Operand,
    fail: Block,
    success: Block,
) -> Result<(), JitError> {
    let expected_arity = immediate_usize(arity, "is_tagged_tuple arity")?;
    if expected_arity == 0 {
        builder.ins().jump(fail, &[]);
        return Ok(());
    }
    let expected_tag = atom_term(tag)?;
    let value = read_operand_term(builder, register_file, value)?;
    let header = checked_tuple_header(builder, value, fail);
    let actual_arity = builder.ins().ushr_imm(header, HEADER_SIZE_SHIFT);
    let arity_mismatch =
        builder
            .ins()
            .icmp_imm(IntCC::NotEqual, actual_arity, expected_arity as i64);
    branch_to_fail_if(builder, arity_mismatch, fail);
    let ptr = untagged_ptr(builder, value);
    let first = builder.ins().load(types::I64, MemFlags::trusted(), ptr, 8);
    let passed = builder
        .ins()
        .icmp_imm(IntCC::Equal, first, expected_tag.raw() as i64);
    builder.ins().brif(passed, success, &[], fail, &[]);
    Ok(())
}

pub(crate) fn lower_select_val(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    value: &Operand,
    fail: Block,
    pairs: &[SelectPair],
) -> Result<(), JitError> {
    let value = read_operand_term(builder, register_file, value)?;
    let mut sorted = pairs.to_vec();
    sorted.sort_by_key(|pair| pair.candidate_raw);
    lower_select_binary_search(builder, value, fail, &sorted)
}

#[derive(Clone, Copy)]
pub(crate) struct SelectPair {
    pub(crate) candidate_raw: u64,
    pub(crate) target: Block,
}

pub(crate) fn parse_select_pairs(list: &Operand) -> Result<Vec<(&Operand, &Operand)>, JitError> {
    let Operand::List(items) = list else {
        return Err(JitError::UnsupportedOperand {
            operand: format!("select_val list {list:?}"),
        });
    };
    if items.len() % 2 != 0 {
        return Err(JitError::UnsupportedOperand {
            operand: format!("select_val odd list {list:?}"),
        });
    }
    Ok(items
        .chunks_exact(2)
        .map(|chunk| (&chunk[0], &chunk[1]))
        .collect())
}

pub(crate) fn immediate_raw_term(operand: &Operand) -> Result<u64, JitError> {
    match operand {
        Operand::Integer(value) => Term::try_small_int(*value)
            .map(|term| term.raw())
            .ok_or_else(|| JitError::UnsupportedOperand {
                operand: format!("small integer literal {value}"),
            }),
        Operand::Unsigned(value) => i64::try_from(*value)
            .ok()
            .and_then(Term::try_small_int)
            .map(|term| term.raw())
            .ok_or_else(|| JitError::UnsupportedOperand {
                operand: format!("unsigned literal {value}"),
            }),
        Operand::Atom(Some(atom)) => Ok(Term::atom(*atom).raw()),
        Operand::Atom(None) => Ok(Term::NIL.raw()),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("select_val candidate {other:?}"),
        }),
    }
}

pub(crate) fn immediate_usize(operand: &Operand, context: &'static str) -> Result<usize, JitError> {
    match operand {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} {operand:?}"),
            })
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} {operand:?}"),
            })
        }
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{context} {other:?}"),
        }),
    }
}

pub(crate) fn validate_tag_atom(operand: &Operand) -> Result<(), JitError> {
    atom_term(operand).map(|_| ())
}

fn lower_select_binary_search(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    fail: Block,
    pairs: &[SelectPair],
) -> Result<(), JitError> {
    let rest = match pairs.split_first() {
        Some((_, rest)) => rest,
        None => {
            builder.ins().jump(fail, &[]);
            return Ok(());
        }
    };
    let mid = pairs.len() / 2;
    let pivot = pairs[mid];
    let left = &pairs[..mid];
    let right = &pairs[mid + 1..];
    let matched = builder
        .ins()
        .icmp_imm(IntCC::Equal, value, pivot.candidate_raw as i64);
    let compare_block = if rest.is_empty() {
        fail
    } else {
        builder.create_block()
    };
    builder
        .ins()
        .brif(matched, pivot.target, &[], compare_block, &[]);
    if !rest.is_empty() {
        builder.switch_to_block(compare_block);
        let less =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedLessThan, value, pivot.candidate_raw as i64);
        let left_block = builder.create_block();
        let right_block = builder.create_block();
        builder.ins().brif(less, left_block, &[], right_block, &[]);
        builder.switch_to_block(left_block);
        lower_select_binary_search(builder, value, fail, left)?;
        builder.switch_to_block(right_block);
        lower_select_binary_search(builder, value, fail, right)?;
    }
    Ok(())
}

fn primary_tag_matches(builder: &mut FunctionBuilder<'_>, value: Value, tag: i64) -> Value {
    let term_tag = builder.ins().band_imm(value, TERM_TAG_MASK);
    builder.ins().icmp_imm(IntCC::Equal, term_tag, tag)
}

fn primary_tag_mismatches(builder: &mut FunctionBuilder<'_>, value: Value, tag: i64) -> Value {
    let term_tag = builder.ins().band_imm(value, TERM_TAG_MASK);
    builder.ins().icmp_imm(IntCC::NotEqual, term_tag, tag)
}

fn lower_pid_type_test(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    fail: Block,
    success: Block,
) {
    let local = primary_tag_matches(builder, value, PID_TAG);
    let boxed_check = builder.create_block();
    builder.ins().brif(local, success, &[], boxed_check, &[]);
    builder.switch_to_block(boxed_check);
    lower_boxed_header_type_test(builder, value, &[EXTERNAL_PID_HEADER_TAG], fail, success);
}

fn lower_boxed_header_type_test(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    tags: &[i64],
    fail: Block,
    success: Block,
) {
    let not_boxed = primary_tag_mismatches(builder, value, BOXED_TAG);
    branch_to_fail_if(builder, not_boxed, fail);
    let ptr = untagged_ptr(builder, value);
    let header = builder.ins().load(types::I64, MemFlags::trusted(), ptr, 0);
    let header_tag = builder.ins().band_imm(header, HEADER_TAG_MASK);
    let mut matched = builder.ins().icmp_imm(IntCC::Equal, header_tag, tags[0]);
    for tag in &tags[1..] {
        let next = builder.ins().icmp_imm(IntCC::Equal, header_tag, *tag);
        matched = builder.ins().bor(matched, next);
    }
    builder.ins().brif(matched, success, &[], fail, &[]);
}

fn checked_tuple_header(builder: &mut FunctionBuilder<'_>, value: Value, fail: Block) -> Value {
    let not_boxed = primary_tag_mismatches(builder, value, BOXED_TAG);
    branch_to_fail_if(builder, not_boxed, fail);
    let ptr = untagged_ptr(builder, value);
    let header = builder.ins().load(types::I64, MemFlags::trusted(), ptr, 0);
    let header_tag = builder.ins().band_imm(header, HEADER_TAG_MASK);
    let not_tuple = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, header_tag, TUPLE_HEADER_TAG);
    branch_to_fail_if(builder, not_tuple, fail);
    header
}

fn untagged_ptr(builder: &mut FunctionBuilder<'_>, value: Value) -> Value {
    builder.ins().band_imm(value, !TERM_TAG_MASK)
}

fn atom_term(operand: &Operand) -> Result<Term, JitError> {
    match operand {
        Operand::Atom(Some(atom)) => Ok(Term::atom(*atom)),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("expected atom, got {other:?}"),
        }),
    }
}
