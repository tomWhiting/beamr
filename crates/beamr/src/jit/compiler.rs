//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::atom::Atom;
use crate::loader::Instruction;
use crate::scheduler::lock_or_recover;
use cranelift_codegen::CodegenError;
use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::ir_arithmetic::{
    ArithmeticLowering, ArithmeticOp, ParsedBif, lower_arithmetic_bif, lower_comparison,
};
use super::ir_common::{
    JIT_DEOPT_SENTINEL, Register, label_operand, read_operand_term, read_register_term,
    write_operand_term,
};
use super::ir_control::{BlockMap, TranslationPlan, opcode_name};
use super::types::NativeCode;

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
                        let value =
                            read_register_term(&mut builder, register_file, Register::X(0));
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

            let exit = blocks.exit_block();
            if !terminated {
                builder.ins().jump(exit, &[]);
            }
            builder.switch_to_block(exit);
            let value = read_register_term(&mut builder, register_file, Register::X(0));
            builder.ins().return_(&[value]);

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

#[cfg(test)]
mod tests {
    use super::{JitCompiler, JitError, JitSettings};
    use crate::atom::Atom;
    use crate::jit::ir_common::X_REGISTER_COUNT;
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
    fn compiled_add_at_end_falls_through_to_return_x0() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Label { label: 1 },
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
    }

    #[test]
    fn compiled_multiply_overflow_takes_fail_label() {
        let compiler = JitCompiler::new(JitSettings).unwrap();
        let native = compiler
            .compile(
                &[
                    Instruction::Bif {
                        op: BifOp::Bif2,
                        operands: vec![
                            Operand::Label(9),
                            Operand::Unsigned(2),
                            Operand::Integer(Term::SMALL_INT_MAX),
                            Operand::Integer(Term::SMALL_INT_MAX),
                            Operand::X(0),
                        ],
                    },
                    Instruction::Return,
                    Instruction::Label { label: 9 },
                    Instruction::Move {
                        source: Operand::Integer(99),
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

        assert_eq!(returned, Term::small_int(99).raw());
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
