//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::atom::Atom;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use crate::scheduler::lock_or_recover;
use cranelift_codegen::CodegenError;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::ir_allocation::{
    AllocationHelpers, LoweringContext, lower_get_hd, lower_get_list, lower_get_tl,
    lower_get_tuple_element, lower_put_list, lower_put_tuple2, tuple_root_operands,
};
use super::ir_arithmetic::{
    ArithmeticLowering, ArithmeticOp, ParsedBif, lower_arithmetic_bif, lower_comparison,
};
use super::ir_common::{
    JIT_DEOPT_SENTINEL, Register, label_operand, read_operand_term, read_register_term,
    write_operand_term,
};
use super::ir_control::{BlockMap, TranslationPlan, opcode_name};
use super::ir_exceptions::{
    CompiledFrameInfo, ExceptionDispatch, ExceptionHelpers, ExceptionLoweringState,
    JIT_STATUS_DEOPT, JIT_STATUS_NORMAL, JIT_STATUS_YIELD, dispatch_exception_status,
    jit_add_compiled_frame, jit_clear_exception, jit_exception_class, jit_exception_reason,
    jit_exception_trace, return_status, return_status_raw,
};
use super::ir_guards::{
    SelectPair, immediate_raw_term, immediate_usize, lower_is_tagged_tuple, lower_select_val,
    lower_test_arity, lower_type_test, parse_select_pairs,
};
use super::runtime::{
    JIT_YIELD_SENTINEL, jit_alloc_cons, jit_alloc_tuple, jit_call_interpreted, jit_charge_reduction,
};
use super::safepoint::SafepointBuilder;
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
        let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
        builder.symbol("beamr_jit_alloc_tuple", jit_alloc_tuple as *const u8);
        builder.symbol("beamr_jit_alloc_cons", jit_alloc_cons as *const u8);
        builder.symbol(
            "beamr_jit_charge_reduction",
            jit_charge_reduction as *const u8,
        );
        builder.symbol(
            "beamr_jit_call_interpreted",
            jit_call_interpreted as *const u8,
        );
        builder.symbol(
            "beamr_jit_exception_class",
            jit_exception_class as *const u8,
        );
        builder.symbol(
            "beamr_jit_exception_reason",
            jit_exception_reason as *const u8,
        );
        builder.symbol(
            "beamr_jit_exception_trace",
            jit_exception_trace as *const u8,
        );
        builder.symbol(
            "beamr_jit_clear_exception",
            jit_clear_exception as *const u8,
        );
        builder.symbol(
            "beamr_jit_add_compiled_frame",
            jit_add_compiled_frame as *const u8,
        );
        Ok(Self {
            module: Arc::new(Mutex::new(JITModule::new(builder))),
            next_function_id: AtomicU64::new(0),
        })
    }

    /// Compiles a BEAM instruction slice into callable native code.
    ///
    /// The current raw JIT ABI is intentionally narrow for mixed-mode bring-up:
    /// `extern "C" fn(*mut u64, *mut Process) -> JitReturn`, where the first
    /// pointer addresses a flat register file. X registers occupy words
    /// `0..1024`; Y registers occupy words starting at `1024`. Status `0`
    /// returns the raw term value normally; status `1` propagates an exception;
    /// statuses `2` and `3` preserve deopt and yield signalling.
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
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I8));
        signature.returns.push(AbiParam::new(types::I64));
        ctx.func.signature = signature.clone();

        let mut tuple_signature = jit_module.make_signature();
        tuple_signature.params.push(AbiParam::new(types::I64));
        tuple_signature.params.push(AbiParam::new(types::I64));
        tuple_signature.returns.push(AbiParam::new(types::I64));
        let tuple_helper = jit_module
            .declare_function("beamr_jit_alloc_tuple", Linkage::Import, &tuple_signature)
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let tuple_helper = jit_module.declare_func_in_func(tuple_helper, &mut ctx.func);

        let mut cons_signature = jit_module.make_signature();
        cons_signature.params.push(AbiParam::new(types::I64));
        cons_signature.returns.push(AbiParam::new(types::I64));
        let cons_helper = jit_module
            .declare_function("beamr_jit_alloc_cons", Linkage::Import, &cons_signature)
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let cons_helper = jit_module.declare_func_in_func(cons_helper, &mut ctx.func);

        let mut charge_signature = jit_module.make_signature();
        charge_signature.params.push(AbiParam::new(types::I64));
        charge_signature.returns.push(AbiParam::new(types::I64));
        let charge_helper = jit_module
            .declare_function(
                "beamr_jit_charge_reduction",
                Linkage::Import,
                &charge_signature,
            )
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let charge_helper = jit_module.declare_func_in_func(charge_helper, &mut ctx.func);

        let mut call_interpreted_signature = jit_module.make_signature();
        call_interpreted_signature
            .params
            .push(AbiParam::new(types::I64));
        call_interpreted_signature
            .params
            .push(AbiParam::new(types::I64));
        call_interpreted_signature
            .params
            .push(AbiParam::new(types::I64));
        call_interpreted_signature
            .params
            .push(AbiParam::new(types::I64));
        call_interpreted_signature
            .params
            .push(AbiParam::new(types::I64));
        call_interpreted_signature
            .returns
            .push(AbiParam::new(types::I8));
        call_interpreted_signature
            .returns
            .push(AbiParam::new(types::I64));
        let call_interpreted_helper = jit_module
            .declare_function(
                "beamr_jit_call_interpreted",
                Linkage::Import,
                &call_interpreted_signature,
            )
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let call_interpreted_helper =
            jit_module.declare_func_in_func(call_interpreted_helper, &mut ctx.func);

        let exception_class_helper =
            declare_unary_helper(&mut jit_module, &mut ctx.func, "beamr_jit_exception_class")?;
        let exception_reason_helper =
            declare_unary_helper(&mut jit_module, &mut ctx.func, "beamr_jit_exception_reason")?;
        let exception_trace_helper =
            declare_unary_helper(&mut jit_module, &mut ctx.func, "beamr_jit_exception_trace")?;
        let exception_clear_helper = declare_void_unary_helper(&mut jit_module, &mut ctx.func)?;
        let exception_add_frame_helper = declare_add_frame_helper(&mut jit_module, &mut ctx.func)?;

        let allocation_helpers = AllocationHelpers {
            tuple: tuple_helper,
            cons: cons_helper,
        };

        let mut safepoints = SafepointBuilder::new();
        let compiled_frame = CompiledFrameInfo {
            module,
            function,
            arity,
        };
        let exception_helpers = ExceptionHelpers {
            class: exception_class_helper,
            reason: exception_reason_helper,
            trace: exception_trace_helper,
            clear: exception_clear_helper,
            add_frame: exception_add_frame_helper,
        };

        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
            let blocks = BlockMap::new(&mut builder, instructions, &plan);
            let register_file = builder.block_params(blocks.entry)[0];
            let process = builder.block_params(blocks.entry)[1];
            builder.switch_to_block(blocks.entry);

            let exhausted = builder.ins().call(charge_helper, &[process]);
            let exhausted = builder.inst_results(exhausted)[0];
            let first_instruction = blocks.block_for_instruction(0);
            branch_to_yield_if_exhausted(
                &mut builder,
                exhausted,
                blocks.yield_block,
                first_instruction,
            );

            let mut terminated = true;
            let mut exceptions = ExceptionLoweringState::default();
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
                    Instruction::TypeTest { op, fail, value } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let next = blocks.block_after(index);
                        lower_type_test(&mut builder, register_file, *op, value, fail, next)?;
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
                    Instruction::TestArity { fail, tuple, arity } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let next = blocks.block_after(index);
                        lower_test_arity(&mut builder, register_file, tuple, arity, fail, next)?;
                        terminated = true;
                    }
                    Instruction::IsTaggedTuple {
                        fail,
                        value,
                        arity,
                        tag,
                    } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let next = blocks.block_after(index);
                        lower_is_tagged_tuple(
                            &mut builder,
                            register_file,
                            value,
                            arity,
                            tag,
                            fail,
                            next,
                        )?;
                        terminated = true;
                    }
                    Instruction::SelectVal { value, fail, list } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let pairs = parse_select_pairs(list)?
                            .into_iter()
                            .map(|(candidate, target)| {
                                Ok(SelectPair {
                                    candidate_raw: immediate_raw_term(candidate)?,
                                    target: blocks.label_block(label_operand(target)?)?,
                                })
                            })
                            .collect::<Result<Vec<_>, JitError>>()?;
                        lower_select_val(&mut builder, register_file, value, fail, &pairs)?;
                        terminated = true;
                    }
                    Instruction::Jump { target } => {
                        let target = blocks.label_block(label_operand(target)?)?;
                        builder.ins().jump(target, &[]);
                        terminated = true;
                    }
                    Instruction::CallExt { arity: _, import }
                    | Instruction::CallExtOnly { arity: _, import } => {
                        let import_index = import_index(import)?;
                        let call_arity = match instruction {
                            Instruction::CallExt { arity, .. }
                            | Instruction::CallExtOnly { arity, .. } => {
                                immediate_u8(arity, "external call arity")?
                            }
                            _ => 0,
                        };
                        let module_value =
                            builder.ins().iconst(types::I64, i64::from(module.index()));
                        let import_value = builder.ins().iconst(
                            types::I64,
                            i64::try_from(import_index).map_err(|_| {
                                JitError::UnsupportedOperand {
                                    operand: format!("import index out of range: {import_index}"),
                                }
                            })?,
                        );
                        let arity_value = builder.ins().iconst(types::I64, i64::from(call_arity));
                        let returned = builder.ins().call(
                            call_interpreted_helper,
                            &[
                                process,
                                module_value,
                                import_value,
                                arity_value,
                                register_file,
                            ],
                        );
                        let results = builder.inst_results(returned).to_vec();
                        let status = results[0];
                        let returned = results[1];
                        let status_continuation = builder.create_block();
                        branch_to_status_blocks(
                            &mut builder,
                            status,
                            blocks.deopt,
                            blocks.yield_block,
                            status_continuation,
                        );
                        let normal_continuation = builder.create_block();
                        dispatch_exception_status(
                            &mut builder,
                            ExceptionDispatch {
                                helpers: exception_helpers,
                                frame: exceptions.current_frame(),
                                compiled_frame,
                                process,
                                register_file,
                                status,
                                value: returned,
                                continuation: normal_continuation,
                            },
                        );
                        write_operand_term(
                            &mut builder,
                            register_file,
                            &crate::loader::decode::Operand::X(0),
                            returned,
                        )?;
                        return_status(&mut builder, JIT_STATUS_NORMAL, returned);
                        terminated = true;
                    }
                    Instruction::Call { label, .. } | Instruction::CallOnly { label, .. } => {
                        let target = blocks.label_block(label_operand(label)?)?;
                        charge_reduction_or_yield(
                            &mut builder,
                            charge_helper,
                            process,
                            blocks.yield_block,
                        );
                        builder.ins().jump(target, &[]);
                        terminated = true;
                    }
                    Instruction::Try { destination, label } => {
                        let catch_block = blocks.label_block(label_operand(label)?)?;
                        let _frame = exceptions.translate_try(catch_block, destination)?;
                        terminated = false;
                    }
                    Instruction::TryEnd { source } => {
                        let _ = super::ir_common::register_operand(source)?;
                        exceptions.translate_try_end()?;
                        builder.ins().call(exception_clear_helper, &[process]);
                    }
                    Instruction::TryCase { source } => {
                        let caught =
                            exceptions.translate_try_case(&mut builder, register_file, source)?;
                        write_operand_term(
                            &mut builder,
                            register_file,
                            &crate::loader::decode::Operand::X(0),
                            caught.class,
                        )?;
                        write_operand_term(
                            &mut builder,
                            register_file,
                            &crate::loader::decode::Operand::X(1),
                            caught.reason,
                        )?;
                        write_operand_term(
                            &mut builder,
                            register_file,
                            &crate::loader::decode::Operand::X(2),
                            caught.trace,
                        )?;
                    }
                    Instruction::Return => {
                        let value = read_register_term(&mut builder, register_file, Register::X(0));
                        return_status(&mut builder, JIT_STATUS_NORMAL, value);
                        terminated = true;
                    }
                    Instruction::PutList {
                        head,
                        tail,
                        destination,
                    } => {
                        safepoints.record_allocation_site(
                            index,
                            [head.clone(), tail.clone(), destination.clone()],
                        )?;
                        lower_put_list(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            allocation_helpers.cons,
                            head,
                            tail,
                            destination,
                        )?;
                        terminated = false;
                    }
                    Instruction::GetList { source, head, tail } => {
                        lower_get_list(&mut builder, register_file, source, head, tail)?;
                    }
                    Instruction::GetHd {
                        source,
                        destination,
                    } => {
                        lower_get_hd(&mut builder, register_file, source, destination)?;
                    }
                    Instruction::GetTl {
                        source,
                        destination,
                    } => {
                        lower_get_tl(&mut builder, register_file, source, destination)?;
                    }
                    Instruction::PutTuple2 {
                        destination,
                        elements,
                    } => {
                        safepoints.record_allocation_site(
                            index,
                            tuple_root_operands(destination, elements)?,
                        )?;
                        lower_put_tuple2(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            allocation_helpers.tuple,
                            destination,
                            elements,
                        )?;
                        terminated = false;
                    }
                    Instruction::GetTupleElement {
                        source,
                        index,
                        destination,
                    } => {
                        let index = immediate_usize(index, "get_tuple_element index")?;
                        lower_get_tuple_element(
                            &mut builder,
                            register_file,
                            source,
                            index,
                            destination,
                        )?;
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
            return_status(&mut builder, JIT_STATUS_NORMAL, value);

            builder.switch_to_block(blocks.deopt);
            return_status_raw(&mut builder, JIT_STATUS_DEOPT, JIT_DEOPT_SENTINEL);

            builder.switch_to_block(blocks.yield_block);
            return_status_raw(&mut builder, JIT_STATUS_YIELD, JIT_YIELD_SENTINEL);
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
            safepoints.finish(),
            Arc::clone(&self.module),
        ))
    }
}

fn branch_to_yield_if_exhausted(
    builder: &mut FunctionBuilder<'_>,
    exhausted: cranelift_codegen::ir::Value,
    yield_block: cranelift_codegen::ir::Block,
    continuation: cranelift_codegen::ir::Block,
) {
    let should_yield = builder.ins().icmp_imm(IntCC::NotEqual, exhausted, 0);
    builder
        .ins()
        .brif(should_yield, yield_block, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn charge_reduction_or_yield(
    builder: &mut FunctionBuilder<'_>,
    charge_helper: cranelift_codegen::ir::FuncRef,
    process: cranelift_codegen::ir::Value,
    yield_block: cranelift_codegen::ir::Block,
) {
    let exhausted = builder.ins().call(charge_helper, &[process]);
    let exhausted = builder.inst_results(exhausted)[0];
    let continuation = builder.create_block();
    branch_to_yield_if_exhausted(builder, exhausted, yield_block, continuation);
}

fn branch_to_status_blocks(
    builder: &mut FunctionBuilder<'_>,
    status: cranelift_codegen::ir::Value,
    deopt_block: cranelift_codegen::ir::Block,
    yield_block: cranelift_codegen::ir::Block,
    continuation: cranelift_codegen::ir::Block,
) {
    let is_deopt = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(JIT_STATUS_DEOPT));
    let check_yield = builder.create_block();
    builder
        .ins()
        .brif(is_deopt, deopt_block, &[], check_yield, &[]);
    builder.switch_to_block(check_yield);
    let is_yield = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(JIT_STATUS_YIELD));
    builder
        .ins()
        .brif(is_yield, yield_block, &[], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn declare_unary_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I64));
    let helper = module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    Ok(module.declare_func_in_func(helper, func))
}

fn declare_void_unary_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::I64));
    let helper = module
        .declare_function("beamr_jit_clear_exception", Linkage::Import, &signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    Ok(module.declare_func_in_func(helper, func))
}

fn declare_add_frame_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I64));
    let helper = module
        .declare_function("beamr_jit_add_compiled_frame", Linkage::Import, &signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    Ok(module.declare_func_in_func(helper, func))
}

fn import_index(import: &Operand) -> Result<usize, JitError> {
    match import {
        Operand::Unsigned(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            })
        }
        Operand::Integer(value) => {
            usize::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("import index out of range: {value}"),
            })
        }
        other => Err(JitError::UnsupportedOperand {
            operand: format!("external call import must be an index, got {other:?}"),
        }),
    }
}

fn immediate_u8(operand: &Operand, context: &'static str) -> Result<u8, JitError> {
    match operand {
        Operand::Unsigned(value) => {
            u8::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
                operand: format!("{context} out of range: {value}"),
            })
        }
        Operand::Integer(value) => u8::try_from(*value).map_err(|_| JitError::UnsupportedOperand {
            operand: format!("{context} out of range: {value}"),
        }),
        other => Err(JitError::UnsupportedOperand {
            operand: format!("{context} must be an integer, got {other:?}"),
        }),
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
mod compiler_tests;
