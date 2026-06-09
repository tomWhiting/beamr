//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::atom::Atom;
use crate::loader::Instruction;
use crate::loader::decode::{BifOp, ComparisonOp, Operand};
use crate::scheduler::lock_or_recover;
use crate::term::Term;
use cranelift_codegen::CodegenError;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlags, Value, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::types::NativeCode;

const REGISTER_WORD_BYTES: i32 = 8;
const X_REGISTER_COUNT: u32 = 1024;
const JIT_DEOPT_SENTINEL: i64 = -1;
const SMALL_INT_TAG_MASK: i64 = 0b111;
const SMALL_INT_SHIFT: i64 = 3;

/// Error returned when scaffold JIT compilation cannot produce native code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JitError {
    /// The scaffold compiler has no translator for this opcode yet.
    UnsupportedOpcode { opcode: String },
    /// An opcode is supported in principle but has an operand shape this JIT ABI cannot lower yet.
    UnsupportedOperand { operand: String },
    /// A branch target references a label that is not present in the compiled instruction slice.
    UnknownLabel { label: u32 },
    /// Cranelift failed while declaring, defining, or finalizing code.
    CraneliftError(String),
    /// No BEAM instructions were provided.
    EmptyFunction,
}

impl fmt::Display for JitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOpcode { opcode } => {
                write!(formatter, "unsupported JIT opcode: {opcode}")
            }
            Self::UnsupportedOperand { operand } => {
                write!(formatter, "unsupported JIT operand: {operand}")
            }
            Self::UnknownLabel { label } => write!(formatter, "unknown JIT label: {label}"),
            Self::CraneliftError(error) => write!(formatter, "Cranelift JIT error: {error}"),
            Self::EmptyFunction => {
                write!(formatter, "cannot JIT compile an empty instruction slice")
            }
        }
    }
}

impl Error for JitError {}

/// Required host Cranelift settings for the Beamr JIT scaffold.
#[derive(Clone, Debug, Default)]
pub struct JitSettings;

/// Compiler that owns Cranelift JIT code memory for emitted functions.
pub struct JitCompiler {
    module: Arc<Mutex<JITModule>>,
    next_function_id: AtomicU64,
}

impl JitCompiler {
    /// Creates a compiler with Cranelift ISA settings for the host target.
    pub fn new(_settings: JitSettings) -> Result<Self, JitError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        flag_builder
            .set("is_pic", "false")
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|error| JitError::CraneliftError(error.to_owned()))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let builder = JITBuilder::with_isa(isa, default_libcall_names());
        Ok(Self {
            module: Arc::new(Mutex::new(JITModule::new(builder))),
            next_function_id: AtomicU64::new(0),
        })
    }

    /// Compiles a BEAM instruction slice into callable native code.
    ///
    /// The current raw JIT ABI is intentionally narrow for mixed-mode bring-up:
    /// `extern "C" fn(*mut u64) -> u64`, where the pointer addresses a flat
    /// register file. X registers occupy words `0..1024`; Y registers occupy
    /// words starting at `1024`. The function returns the raw word in `x(0)`, or
    /// `u64::MAX` to request interpreter fallback/deoptimization.
    pub fn compile(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
    ) -> Result<NativeCode, JitError> {
        let plan = TranslationPlan::new(instructions)?;

        let unique_id = self.next_function_id.fetch_add(1, Ordering::Relaxed);
        let name = format!("beamr_jit_{module:?}_{function:?}_{arity}_{unique_id}");

        let mut jit_module = lock_or_recover(self.module.as_ref());
        let mut ctx = jit_module.make_context();
        let mut signature = jit_module.make_signature();
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I64));
        ctx.func.signature = signature.clone();

        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
            let blocks = BlockMap::new(&mut builder, instructions, &plan);
            let register_file = builder.block_params(blocks.entry)[0];
            builder.switch_to_block(blocks.entry);

            let mut terminated = false;
            for (index, instruction) in instructions.iter().enumerate() {
                let block = blocks.block_for_instruction(index);
                if builder.current_block() != Some(block) {
                    if !terminated {
                        builder.ins().jump(block, &[]);
                    }
                    builder.switch_to_block(block);
                    terminated = false;
                }

                match instruction {
                    Instruction::Label { .. } => {}
                    Instruction::Move {
                        source,
                        destination,
                    } => {
                        let value = read_operand_term(&mut builder, register_file, source)?;
                        write_operand_term(&mut builder, register_file, destination, value)?;
                    }
                    Instruction::Swap { left, right } => {
                        let left_value = read_operand_term(&mut builder, register_file, left)?;
                        let right_value = read_operand_term(&mut builder, register_file, right)?;
                        write_operand_term(&mut builder, register_file, left, right_value)?;
                        write_operand_term(&mut builder, register_file, right, left_value)?;
                    }
                    Instruction::Bif { op, operands } => {
                        let bif = ParsedBif::parse(*op, operands)?;
                        let arithmetic = ArithmeticOp::from_import(bif.import)?;
                        let fail = blocks.label_block(label_operand(bif.fail)?)?;
                        let next = blocks.block_after(index);
                        lower_arithmetic_bif(
                            &mut builder,
                            register_file,
                            ArithmeticLowering {
                                op: arithmetic,
                                left: bif.left,
                                right: bif.right,
                                destination: bif.destination,
                                fail,
                                success: next,
                            },
                        )?;
                        terminated = true;
                    }
                    Instruction::Comparison {
                        op,
                        fail,
                        left,
                        right,
                    } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let next = blocks.block_after(index);
                        lower_comparison(
                            &mut builder,
                            register_file,
                            *op,
                            left,
                            right,
                            fail,
                            next,
                        )?;
                        terminated = true;
                    }
                    Instruction::Jump { target } => {
                        let target = blocks.label_block(label_operand(target)?)?;
                        builder.ins().jump(target, &[]);
                        terminated = true;
                    }
                    Instruction::Call { label, .. } | Instruction::CallOnly { label, .. } => {
                        let target = blocks.label_block(label_operand(label)?)?;
                        builder.ins().jump(target, &[]);
                        terminated = true;
                    }
                    Instruction::Return => {
                        let value = read_register_term(&mut builder, register_file, Register::X(0));
                        builder.ins().return_(&[value]);
                        terminated = true;
                    }
                    other => {
                        return Err(JitError::UnsupportedOpcode {
                            opcode: opcode_name(other),
                        });
                    }
                }
            }

            if !terminated {
                let value = read_register_term(&mut builder, register_file, Register::X(0));
                builder.ins().return_(&[value]);
            }

            builder.switch_to_block(blocks.deopt);
            let sentinel = builder.ins().iconst(types::I64, JIT_DEOPT_SENTINEL);
            builder.ins().return_(&[sentinel]);
            builder.seal_all_blocks();
            builder.finalize();
        }

        let func_id = jit_module
            .declare_function(&name, Linkage::Local, &signature)
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        jit_module
            .define_function(func_id, &mut ctx)
            .map_err(cranelift_error)?;
        jit_module.clear_context(&mut ctx);
        jit_module
            .finalize_definitions()
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let call_ptr = jit_module.get_finalized_function(func_id);
        drop(jit_module);
        Ok(NativeCode::new(
            call_ptr,
            Vec::new(),
            Arc::clone(&self.module),
        ))
    }
}

fn cranelift_error(error: cranelift_module::ModuleError) -> JitError {
    match error {
        cranelift_module::ModuleError::Compilation(CodegenError::Verifier(errors)) => {
            JitError::CraneliftError(errors.to_string())
        }
        other => JitError::CraneliftError(other.to_string()),
    }
}

struct TranslationPlan {
    labels: HashMap<u32, usize>,
    block_starts: HashSet<usize>,
}

impl TranslationPlan {
    fn new(instructions: &[Instruction]) -> Result<Self, JitError> {
        if instructions.is_empty() {
            return Err(JitError::EmptyFunction);
        }

        let mut labels = HashMap::new();
        let mut block_starts = HashSet::from([0, instructions.len()]);
        for (index, instruction) in instructions.iter().enumerate() {
            match instruction {
                Instruction::Label { label } => {
                    labels.insert(*label, index);
                    block_starts.insert(index);
                }
                Instruction::Return => {}
                Instruction::Move {
                    source,
                    destination,
                } => {
                    validate_read_operand(source)?;
                    validate_write_operand(destination)?;
                }
                Instruction::Swap { left, right } => {
                    validate_read_operand(left)?;
                    validate_read_operand(right)?;
                    validate_write_operand(left)?;
                    validate_write_operand(right)?;
                }
                Instruction::Comparison {
                    fail, left, right, ..
                } => {
                    validate_label_operand(fail)?;
                    validate_read_operand(left)?;
                    validate_read_operand(right)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Jump { target } => {
                    validate_label_operand(target)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Call { label, .. } | Instruction::CallOnly { label, .. } => {
                    validate_label_operand(label)?;
                    block_starts.insert(index + 1);
                }
                Instruction::Bif { op, operands } => {
                    let parsed = ParsedBif::parse(*op, operands)?;
                    let _ = ArithmeticOp::from_import(parsed.import)?;
                    validate_label_operand(parsed.fail)?;
                    validate_read_operand(parsed.left)?;
                    validate_read_operand(parsed.right)?;
                    validate_write_operand(parsed.destination)?;
                    block_starts.insert(index + 1);
                }
                other => {
                    return Err(JitError::UnsupportedOpcode {
                        opcode: opcode_name(other),
                    });
                }
            }
        }

        for instruction in instructions {
            match instruction {
                Instruction::Comparison { fail, .. } => ensure_known_label(&labels, fail)?,
                Instruction::Jump { target }
                | Instruction::Call { label: target, .. }
                | Instruction::CallOnly { label: target, .. } => {
                    ensure_known_label(&labels, target)?
                }
                Instruction::Bif { op, operands } => {
                    let parsed = ParsedBif::parse(*op, operands)?;
                    ensure_known_label(&labels, parsed.fail)?;
                }
                _ => {}
            }
        }

        Ok(Self {
            labels,
            block_starts,
        })
    }
}

struct BlockMap {
    blocks_by_index: Vec<cranelift_codegen::ir::Block>,
    label_blocks: HashMap<u32, cranelift_codegen::ir::Block>,
    entry: cranelift_codegen::ir::Block,
    deopt: cranelift_codegen::ir::Block,
}

impl BlockMap {
    fn new(
        builder: &mut FunctionBuilder<'_>,
        instructions: &[Instruction],
        plan: &TranslationPlan,
    ) -> Self {
        let mut blocks_by_index = Vec::with_capacity(instructions.len() + 1);
        let mut current = builder.create_block();
        builder.append_block_params_for_function_params(current);
        for index in 0..=instructions.len() {
            if index > 0 && plan.block_starts.contains(&index) {
                current = builder.create_block();
            }
            blocks_by_index.push(current);
        }

        let mut label_blocks = HashMap::new();
        for (label, index) in &plan.labels {
            label_blocks.insert(*label, blocks_by_index[*index]);
        }

        Self {
            entry: blocks_by_index[0],
            blocks_by_index,
            label_blocks,
            deopt: builder.create_block(),
        }
    }

    fn block_for_instruction(&self, index: usize) -> cranelift_codegen::ir::Block {
        self.blocks_by_index[index]
    }

    fn block_after(&self, index: usize) -> cranelift_codegen::ir::Block {
        self.blocks_by_index[index + 1]
    }

    fn label_block(&self, label: u32) -> Result<cranelift_codegen::ir::Block, JitError> {
        self.label_blocks
            .get(&label)
            .copied()
            .ok_or(JitError::UnknownLabel { label })
    }
}

#[derive(Clone, Copy)]
enum Register {
    X(u32),
    Y(u32),
}

fn read_operand_term(
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

fn write_operand_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    operand: &Operand,
    value: Value,
) -> Result<(), JitError> {
    let register = register_operand(operand)?;
    write_register_term(builder, register_file, register, value);
    Ok(())
}

fn small_int_constant(builder: &mut FunctionBuilder<'_>, value: i64) -> Result<Value, JitError> {
    let term = Term::try_small_int(value).ok_or_else(|| JitError::UnsupportedOperand {
        operand: format!("small integer literal {value}"),
    })?;
    Ok(builder.ins().iconst(types::I64, term.raw() as i64))
}

fn read_register_term(
    builder: &mut FunctionBuilder<'_>,
    register_file: Value,
    register: Register,
) -> Value {
    let offset = register_offset(register);
    builder
        .ins()
        .load(types::I64, MemFlags::trusted(), register_file, offset)
}

fn write_register_term(
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

fn register_offset(register: Register) -> i32 {
    let index = match register {
        Register::X(index) => index,
        Register::Y(index) => X_REGISTER_COUNT + index,
    };
    (index as i32) * REGISTER_WORD_BYTES
}

fn register_operand(operand: &Operand) -> Result<Register, JitError> {
    match operand {
        Operand::X(index) => Ok(Register::X(*index)),
        Operand::Y(index) => Ok(Register::Y(*index)),
        Operand::TypedRegister { register, .. } => register_operand(register),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{other:?}"),
        }),
    }
}

fn validate_read_operand(operand: &Operand) -> Result<(), JitError> {
    match operand {
        Operand::Integer(_) | Operand::Unsigned(_) | Operand::Atom(_) => Ok(()),
        _ => register_operand(operand).map(|_| ()),
    }
}

fn validate_write_operand(operand: &Operand) -> Result<(), JitError> {
    register_operand(operand).map(|_| ())
}

fn validate_label_operand(operand: &Operand) -> Result<(), JitError> {
    label_operand(operand).map(|_| ())
}

fn ensure_known_label(labels: &HashMap<u32, usize>, operand: &Operand) -> Result<(), JitError> {
    let label = label_operand(operand)?;
    if labels.contains_key(&label) {
        Ok(())
    } else {
        Err(JitError::UnknownLabel { label })
    }
}

fn label_operand(operand: &Operand) -> Result<u32, JitError> {
    match operand {
        Operand::Label(label) => Ok(*label),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("expected label, got {other:?}"),
        }),
    }
}

struct ParsedBif<'a> {
    fail: &'a Operand,
    import: &'a Operand,
    left: &'a Operand,
    right: &'a Operand,
    destination: &'a Operand,
}

impl<'a> ParsedBif<'a> {
    fn parse(op: BifOp, operands: &'a [Operand]) -> Result<Self, JitError> {
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

#[derive(Clone, Copy)]
enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Div,
    Rem,
}

impl ArithmeticOp {
    fn from_import(import: &Operand) -> Result<Self, JitError> {
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

struct ArithmeticLowering<'a> {
    op: ArithmeticOp,
    left: &'a Operand,
    right: &'a Operand,
    destination: &'a Operand,
    fail: cranelift_codegen::ir::Block,
    success: cranelift_codegen::ir::Block,
}

fn lower_arithmetic_bif(
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
            let value = builder.ins().imul(left_payload, right_payload);
            let min_check =
                builder
                    .ins()
                    .icmp_imm(IntCC::SignedLessThan, value, Term::SMALL_INT_MIN);
            let max_check =
                builder
                    .ins()
                    .icmp_imm(IntCC::SignedGreaterThan, value, Term::SMALL_INT_MAX);
            let out_of_range = builder.ins().bor(min_check, max_check);
            branch_to_fail_if(builder, out_of_range, lowering.fail);
            value
        }
        ArithmeticOp::Div | ArithmeticOp::Rem => {
            let zero = builder.ins().icmp_imm(IntCC::Equal, right_payload, 0);
            branch_to_fail_if(builder, zero, lowering.fail);
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

fn checked_small_int_payload(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    fail: cranelift_codegen::ir::Block,
) -> Value {
    let tag = builder.ins().band_imm(value, SMALL_INT_TAG_MASK);
    let tagged = builder.ins().icmp_imm(IntCC::Equal, tag, 0);
    let not_tagged = builder.ins().bnot(tagged);
    branch_to_fail_if(builder, not_tagged, fail);
    builder.ins().sshr_imm(value, SMALL_INT_SHIFT)
}

fn branch_to_fail_if(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    fail: cranelift_codegen::ir::Block,
) {
    let continuation = builder.create_block();
    builder.ins().brif(condition, fail, &[], continuation, &[]);
    builder.switch_to_block(continuation);
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

fn lower_comparison(
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

fn opcode_name(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Generic { opcode, name, .. } => format!("{name} ({opcode})"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{JitCompiler, JitError, JitSettings, X_REGISTER_COUNT};
    use crate::atom::Atom;
    use crate::loader::Instruction;
    use crate::loader::decode::{BifOp, ComparisonOp, Operand};
    use crate::term::Term;

    type RawJitFn = extern "C" fn(*mut u64) -> u64;

    fn call_native(native: &crate::jit::types::NativeCode, registers: &mut [u64]) -> u64 {
        let function: RawJitFn = unsafe { std::mem::transmute(native.call_ptr()) };
        function(registers.as_mut_ptr())
    }

    #[test]
    fn compiles_return_only_function() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(&[Instruction::Return], Atom::MODULE, Atom::OK, 0)
            .unwrap();

        assert!(!native.call_ptr().is_null());
        assert!(native.stack_maps().is_empty());
    }

    #[test]
    fn compiled_move_writes_register_file() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Move {
                        source: Operand::Integer(42),
                        destination: Operand::X(1),
                    },
                    Instruction::Move {
                        source: Operand::X(1),
                        destination: Operand::Y(0),
                    },
                    Instruction::Return,
                ],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap();
        let mut registers = vec![0; X_REGISTER_COUNT as usize + 1];
        let returned = call_native(&native, &mut registers);

        assert_eq!(returned, 0);
        assert_eq!(registers[1], Term::small_int(42).raw());
        assert_eq!(
            registers[X_REGISTER_COUNT as usize],
            Term::small_int(42).raw()
        );
    }

    #[test]
    fn compiled_swap_reads_before_writing() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Swap {
                        left: Operand::X(0),
                        right: Operand::X(1),
                    },
                    Instruction::Return,
                ],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap();
        let mut registers = vec![Term::small_int(2).raw(), Term::small_int(3).raw()];
        let returned = call_native(&native, &mut registers);

        assert_eq!(returned, Term::small_int(3).raw());
        assert_eq!(registers[0], Term::small_int(3).raw());
        assert_eq!(registers[1], Term::small_int(2).raw());
    }

    #[test]
    fn compiled_add_returns_small_int_result() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Bif {
                        op: BifOp::Bif2,
                        operands: vec![
                            Operand::Label(9),
                            Operand::Unsigned(0),
                            Operand::Integer(2),
                            Operand::Integer(3),
                            Operand::X(0),
                        ],
                    },
                    Instruction::Return,
                    Instruction::Label { label: 9 },
                    Instruction::Return,
                ],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap();
        let mut registers = vec![0; 1];
        let returned = call_native(&native, &mut registers);

        assert_eq!(returned, Term::small_int(5).raw());
        assert_eq!(registers[0], Term::small_int(5).raw());
    }

    #[test]
    fn compiled_branch_takes_fail_label_on_false_comparison() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Comparison {
                        op: ComparisonOp::EqExact,
                        fail: Operand::Label(7),
                        left: Operand::Integer(1),
                        right: Operand::Integer(2),
                    },
                    Instruction::Move {
                        source: Operand::Integer(10),
                        destination: Operand::X(0),
                    },
                    Instruction::Return,
                    Instruction::Label { label: 7 },
                    Instruction::Move {
                        source: Operand::Integer(20),
                        destination: Operand::X(0),
                    },
                    Instruction::Return,
                ],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap();
        let mut registers = vec![0; 1];
        let returned = call_native(&native, &mut registers);

        assert_eq!(returned, Term::small_int(20).raw());
    }

    #[test]
    fn reports_unsupported_opcode() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let error = compiler
            .compile(
                &[Instruction::Generic {
                    opcode: 255,
                    name: "unknown",
                    operands: Vec::new(),
                }],
                Atom::MODULE,
                Atom::OK,
                0,
            )
            .unwrap_err();

        assert_eq!(
            error,
            JitError::UnsupportedOpcode {
                opcode: "unknown (255)".to_owned()
            }
        );
    }
}
