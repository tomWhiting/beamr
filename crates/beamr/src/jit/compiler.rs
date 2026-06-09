//! Cranelift-backed BEAM JIT compiler scaffold.

use crate::atom::Atom;
use crate::jit::type_info::{FunctionSignature, TypeDescriptor};
use crate::loader::Instruction;
use crate::loader::decode::TypeTestOp;
use crate::loader::decode::chunks::LambdaEntry;
use crate::loader::decode::compact::Operand;
use crate::scheduler::lock_or_recover;
use crate::term::Term;
use cranelift_codegen::CodegenError;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::collections::HashMap;
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
use super::ir_closure::{
    ClosureCall, ClosureHelpers, ClosureMetadata, lower_call_fun, lower_make_fun2,
    make_fun_free_var_operands, make_fun_free_var_roots,
};
use super::ir_common::{
    JIT_DEOPT_SENTINEL, Register, SMALL_INT_SHIFT, label_operand, read_operand_term,
    read_register_term, register_operand, write_operand_term, write_register_term,
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
    JIT_YIELD_SENTINEL, jit_alloc_closure, jit_alloc_cons, jit_alloc_tuple, jit_call_closure,
    jit_call_interpreted, jit_charge_reduction,
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

fn lower_typed_int_arithmetic(
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

fn lower_typed_type_test(
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

fn lower_typed_test_arity(
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
        builder.symbol("beamr_jit_alloc_closure", jit_alloc_closure as *const u8);
        builder.symbol("beamr_jit_call_closure", jit_call_closure as *const u8);
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
        self.compile_with_signature(instructions, module, function, arity, None)
    }

    /// Compiles a BEAM instruction slice with statically-known Gleam types.
    pub fn compile_typed(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        signature: FunctionSignature,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_signature(instructions, module, function, arity, Some(signature))
    }

    /// Compiles with module-level closure metadata available to `make_fun` lowering.
    pub fn compile_module_function(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        lambdas: &[LambdaEntry],
        generation: u64,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_module_metadata(
            instructions,
            module,
            function,
            arity,
            None,
            lambdas,
            generation,
        )
    }

    /// Compiles typed code with module-level closure metadata available to `make_fun` lowering.
    pub fn compile_typed_module_function(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        signature: FunctionSignature,
        lambdas: &[LambdaEntry],
        generation: u64,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_module_metadata(
            instructions,
            module,
            function,
            arity,
            Some(signature),
            lambdas,
            generation,
        )
    }

    fn compile_with_signature(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        typed_signature: Option<FunctionSignature>,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_module_metadata(
            instructions,
            module,
            function,
            arity,
            typed_signature,
            &[],
            0,
        )
    }

    fn compile_with_module_metadata(
        &self,
        instructions: &[Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        typed_signature: Option<FunctionSignature>,
        lambdas: &[LambdaEntry],
        generation: u64,
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

        let mut closure_alloc_signature = jit_module.make_signature();
        closure_alloc_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_alloc_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_alloc_signature
            .returns
            .push(AbiParam::new(types::I64));
        let closure_alloc_helper = jit_module
            .declare_function(
                "beamr_jit_alloc_closure",
                Linkage::Import,
                &closure_alloc_signature,
            )
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let closure_alloc_helper =
            jit_module.declare_func_in_func(closure_alloc_helper, &mut ctx.func);

        let mut closure_call_signature = jit_module.make_signature();
        closure_call_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_call_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_call_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_call_signature
            .params
            .push(AbiParam::new(types::I64));
        closure_call_signature
            .returns
            .push(AbiParam::new(types::I8));
        closure_call_signature
            .returns
            .push(AbiParam::new(types::I64));
        let closure_call_helper = jit_module
            .declare_function(
                "beamr_jit_call_closure",
                Linkage::Import,
                &closure_call_signature,
            )
            .map_err(|error| JitError::CraneliftError(error.to_string()))?;
        let closure_call_helper =
            jit_module.declare_func_in_func(closure_call_helper, &mut ctx.func);

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
        let closure_helpers = ClosureHelpers {
            alloc: closure_alloc_helper,
            dispatch: closure_call_helper,
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
            let mut typed_state = TypedRegisterState::new(typed_signature.as_ref());
            typed_state.initialize_entry_values(&mut builder, register_file);

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
                        let value =
                            typed_state.read_operand_value(&mut builder, register_file, source)?;
                        write_operand_term(&mut builder, register_file, destination, value)?;
                        typed_state.copy(source, destination);
                    }
                    Instruction::Swap { left, right } => {
                        let left_value = read_operand_term(&mut builder, register_file, left)?;
                        let right_value = read_operand_term(&mut builder, register_file, right)?;
                        write_operand_term(&mut builder, register_file, left, right_value)?;
                        write_operand_term(&mut builder, register_file, right, left_value)?;
                        typed_state.swap(left, right);
                    }
                    Instruction::Bif { op, operands } => {
                        let bif = ParsedBif::parse(*op, operands)?;
                        let arithmetic = ArithmeticOp::from_import(bif.import)?;
                        let fail = blocks.label_block(label_operand(bif.fail)?)?;
                        let next = blocks.block_after(index);
                        let lowering = ArithmeticLowering {
                            op: arithmetic,
                            left: bif.left,
                            right: bif.right,
                            destination: bif.destination,
                            fail,
                            success: next,
                        };
                        if typed_state.operands_are_int(bif.left, bif.right)
                            && typed_state.can_write_typed(bif.destination)
                        {
                            lower_typed_int_arithmetic(
                                &mut builder,
                                register_file,
                                lowering,
                                blocks.deopt,
                            )?;
                            typed_state.set_operand_type(bif.destination, TypeDescriptor::Int);
                        } else {
                            typed_state.materialize_operands_for_untyped_lowering(
                                &mut builder,
                                register_file,
                                [bif.left, bif.right],
                            );
                            lower_arithmetic_bif(&mut builder, register_file, lowering)?;
                            typed_state.clear_operand(bif.destination);
                        }
                        terminated = true;
                    }
                    Instruction::TypeTest { op, fail, value } => {
                        let fail = blocks.label_block(label_operand(fail)?)?;
                        let next = blocks.block_after(index);
                        if !lower_typed_type_test(
                            &mut builder,
                            &typed_state,
                            *op,
                            value,
                            fail,
                            next,
                        )? {
                            lower_type_test(&mut builder, register_file, *op, value, fail, next)?;
                        }
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
                        typed_state.materialize_operands_for_untyped_lowering(
                            &mut builder,
                            register_file,
                            [left, right],
                        );
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
                        if !lower_typed_test_arity(
                            &mut builder,
                            &typed_state,
                            tuple,
                            arity,
                            fail,
                            next,
                        )? {
                            lower_test_arity(
                                &mut builder,
                                register_file,
                                tuple,
                                arity,
                                fail,
                                next,
                            )?;
                        }
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
                        typed_state.materialize_operands_for_untyped_lowering(
                            &mut builder,
                            register_file,
                            [value],
                        );
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
                        typed_state.materialize_all_for_untyped_call(&mut builder, register_file);
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
                        handle_helper_return(
                            &mut builder,
                            results[0],
                            results[1],
                            blocks.deopt,
                            blocks.yield_block,
                            exception_helpers,
                            exceptions.current_frame(),
                            compiled_frame,
                            process,
                            register_file,
                        )?;
                        terminated = true;
                    }
                    Instruction::MakeFun { operands } => {
                        let lambda_index = make_fun_lambda_index(operands)?;
                        let lambda = lambdas.get(lambda_index).ok_or_else(|| {
                            JitError::UnsupportedOperand {
                                operand: format!("make_fun lambda index {lambda_index}"),
                            }
                        })?;
                        let num_free = usize::try_from(lambda.num_free).map_err(|_| {
                            JitError::UnsupportedOperand {
                                operand: format!("make_fun num_free {}", lambda.num_free),
                            }
                        })?;
                        safepoints.record_allocation_site(
                            index,
                            make_fun_free_var_roots(&Operand::X(0), num_free)?,
                        )?;
                        let free_vars = make_fun_free_var_operands(num_free)?;
                        typed_state.materialize_operands_for_untyped_lowering(
                            &mut builder,
                            register_file,
                            free_vars.iter(),
                        );
                        lower_make_fun2(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            closure_helpers.alloc,
                            ClosureMetadata {
                                module,
                                function_index: u64::try_from(lambda_index).map_err(|_| {
                                    JitError::UnsupportedOperand {
                                        operand: format!("make_fun lambda index {lambda_index}"),
                                    }
                                })?,
                                arity: lambda.arity,
                                generation,
                                unique_id: lambda.unique_id,
                            },
                            &free_vars,
                            &Operand::X(0),
                        )?;
                        typed_state.clear_operand(&Operand::X(0));
                        terminated = false;
                    }
                    Instruction::CallFun { arity } => {
                        typed_state.materialize_all_for_untyped_call(&mut builder, register_file);
                        let call_arity = immediate_u8(arity, "call_fun arity")?;
                        let fun = Operand::X(u32::from(call_arity));
                        let (status, returned) = lower_call_fun(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            closure_helpers.dispatch,
                            ClosureCall {
                                fun: &fun,
                                arity: call_arity,
                            },
                        )?;
                        handle_helper_return(
                            &mut builder,
                            status,
                            returned,
                            blocks.deopt,
                            blocks.yield_block,
                            exception_helpers,
                            exceptions.current_frame(),
                            compiled_frame,
                            process,
                            register_file,
                        )?;
                        terminated = true;
                    }
                    Instruction::CallFun2 {
                        function: fun,
                        arity,
                        destination,
                    } => {
                        typed_state.materialize_all_for_untyped_call(&mut builder, register_file);
                        let call_arity = immediate_u8(arity, "call_fun2 arity")?;
                        let (status, returned) = lower_call_fun(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            closure_helpers.dispatch,
                            ClosureCall {
                                fun,
                                arity: call_arity,
                            },
                        )?;
                        handle_helper_return(
                            &mut builder,
                            status,
                            returned,
                            blocks.deopt,
                            blocks.yield_block,
                            exception_helpers,
                            exceptions.current_frame(),
                            compiled_frame,
                            process,
                            register_file,
                        )?;
                        typed_state.clear_operand(destination);
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
                        let value = typed_state.read_return_value(&mut builder, register_file);
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
                        let destination_type = typed_state.list_type_from_head(head);
                        typed_state.materialize_operands_for_untyped_lowering(
                            &mut builder,
                            register_file,
                            [head, tail],
                        );
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
                        typed_state.set_optional_operand_type(destination, destination_type);
                        terminated = false;
                    }
                    Instruction::GetList { source, head, tail } => {
                        let head_type = typed_state.list_head_type(source);
                        let tail_type = typed_state.list_tail_type(source);
                        lower_get_list(&mut builder, register_file, source, head, tail)?;
                        typed_state.mark_loaded_operand_type(
                            &mut builder,
                            register_file,
                            head,
                            head_type,
                        );
                        typed_state.mark_loaded_operand_type(
                            &mut builder,
                            register_file,
                            tail,
                            tail_type,
                        );
                    }
                    Instruction::GetHd {
                        source,
                        destination,
                    } => {
                        let destination_type = typed_state.list_head_type(source);
                        lower_get_hd(&mut builder, register_file, source, destination)?;
                        typed_state.mark_loaded_operand_type(
                            &mut builder,
                            register_file,
                            destination,
                            destination_type,
                        );
                    }
                    Instruction::GetTl {
                        source,
                        destination,
                    } => {
                        let destination_type = typed_state.list_tail_type(source);
                        lower_get_tl(&mut builder, register_file, source, destination)?;
                        typed_state.mark_loaded_operand_type(
                            &mut builder,
                            register_file,
                            destination,
                            destination_type,
                        );
                    }
                    Instruction::PutTuple2 {
                        destination,
                        elements,
                    } => {
                        safepoints.record_allocation_site(
                            index,
                            tuple_root_operands(destination, elements)?,
                        )?;
                        let destination_type = typed_state.tuple_type_from_elements(elements);
                        typed_state.materialize_tuple_elements_for_untyped_lowering(
                            &mut builder,
                            register_file,
                            elements,
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
                        typed_state.set_optional_operand_type(destination, destination_type);
                        terminated = false;
                    }
                    Instruction::GetTupleElement {
                        source,
                        index,
                        destination,
                    } => {
                        let index = immediate_usize(index, "get_tuple_element index")?;
                        let destination_type = typed_state.tuple_element_type(source, index);
                        lower_get_tuple_element(
                            &mut builder,
                            register_file,
                            source,
                            index,
                            destination,
                        )?;
                        typed_state.mark_loaded_operand_type(
                            &mut builder,
                            register_file,
                            destination,
                            destination_type,
                        );
                    }
                    Instruction::Apply { arity } => {
                        typed_state.materialize_all_for_untyped_call(&mut builder, register_file);
                        let call_arity = immediate_u8(arity, "closure apply arity")?;
                        let fun = Operand::X(u32::from(call_arity));
                        let (status, returned) = lower_call_fun(
                            &mut builder,
                            LoweringContext {
                                register_file,
                                process,
                                deopt: blocks.deopt,
                            },
                            closure_helpers.dispatch,
                            ClosureCall {
                                fun: &fun,
                                arity: call_arity,
                            },
                        )?;
                        handle_helper_return(
                            &mut builder,
                            status,
                            returned,
                            blocks.deopt,
                            blocks.yield_block,
                            exception_helpers,
                            exceptions.current_frame(),
                            compiled_frame,
                            process,
                            register_file,
                        )?;
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
            let value = typed_state.read_return_value(&mut builder, register_file);
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

struct TypedRegisterState {
    registers: HashMap<Register, TypeDescriptor>,
}

impl TypedRegisterState {
    fn new(signature: Option<&FunctionSignature>) -> Self {
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

    fn initialize_entry_values(
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

    fn read_return_value(
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

    fn materialize_all_for_untyped_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        register_file: cranelift_codegen::ir::Value,
    ) {
        let registers = self.registers.keys().copied().collect::<Vec<_>>();
        self.materialize_registers(builder, register_file, registers);
    }

    fn materialize_operands_for_untyped_lowering<'a>(
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

    fn materialize_tuple_elements_for_untyped_lowering(
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

    fn materialize_registers(
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

    fn read_operand_value(
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

    fn operand_type(&self, operand: &Operand) -> Option<&TypeDescriptor> {
        let register = register_operand(operand).ok()?;
        self.registers.get(&register)
    }

    fn set_operand_type(&mut self, operand: &Operand, type_: TypeDescriptor) {
        self.set_optional_operand_type(operand, Some(type_));
    }

    fn set_optional_operand_type(&mut self, operand: &Operand, type_: Option<TypeDescriptor>) {
        if let Ok(register) = register_operand(operand) {
            if let Some(type_) = type_.as_ref().and_then(supported_type) {
                self.registers.insert(register, type_);
            } else {
                self.registers.remove(&register);
            }
        }
    }

    fn clear_operand(&mut self, operand: &Operand) {
        if let Ok(register) = register_operand(operand) {
            self.registers.remove(&register);
        }
    }

    fn copy(&mut self, source: &Operand, destination: &Operand) {
        if let Some(type_) = self.operand_type(source).cloned() {
            self.set_operand_type(destination, type_);
        } else {
            self.clear_operand(destination);
        }
    }

    fn swap(&mut self, left: &Operand, right: &Operand) {
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

    fn operands_are_int(&self, left: &Operand, right: &Operand) -> bool {
        matches!(self.operand_type(left), Some(TypeDescriptor::Int))
            && matches!(self.operand_type(right), Some(TypeDescriptor::Int))
    }

    fn can_write_typed(&self, operand: &Operand) -> bool {
        register_operand(operand).is_ok()
    }

    fn list_type_from_head(&self, head: &Operand) -> Option<TypeDescriptor> {
        self.operand_type(head)
            .cloned()
            .map(|head_type| TypeDescriptor::List(Box::new(head_type)))
    }

    fn list_head_type(&self, source: &Operand) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::List(inner)) = self.operand_type(source) {
            Some(inner.as_ref().clone())
        } else {
            None
        }
    }

    fn list_tail_type(&self, source: &Operand) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::List(inner)) = self.operand_type(source) {
            Some(TypeDescriptor::List(inner.clone()))
        } else {
            None
        }
    }

    fn tuple_type_from_elements(&self, elements: &Operand) -> Option<TypeDescriptor> {
        let Operand::List(elements) = elements else {
            return None;
        };
        elements
            .iter()
            .map(|element| self.operand_type(element).cloned())
            .collect::<Option<Vec<_>>>()
            .map(TypeDescriptor::Tuple)
    }

    fn tuple_element_type(&self, source: &Operand, index: usize) -> Option<TypeDescriptor> {
        if let Some(TypeDescriptor::Tuple(elements)) = self.operand_type(source) {
            elements.get(index).cloned()
        } else {
            None
        }
    }

    fn mark_loaded_operand_type(
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

fn supported_type(type_: &TypeDescriptor) -> Option<TypeDescriptor> {
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

#[allow(clippy::too_many_arguments)]
fn handle_helper_return(
    builder: &mut FunctionBuilder<'_>,
    status: cranelift_codegen::ir::Value,
    returned: cranelift_codegen::ir::Value,
    deopt_block: cranelift_codegen::ir::Block,
    yield_block: cranelift_codegen::ir::Block,
    exception_helpers: ExceptionHelpers,
    frame: Option<super::ir_exceptions::TryCatchFrame>,
    compiled_frame: CompiledFrameInfo,
    process: cranelift_codegen::ir::Value,
    register_file: cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let status_continuation = builder.create_block();
    branch_to_status_blocks(
        builder,
        status,
        deopt_block,
        yield_block,
        status_continuation,
    );
    let normal_continuation = builder.create_block();
    dispatch_exception_status(
        builder,
        ExceptionDispatch {
            helpers: exception_helpers,
            frame,
            compiled_frame,
            process,
            register_file,
            status,
            value: returned,
            continuation: normal_continuation,
        },
    );
    write_operand_term(
        builder,
        register_file,
        &crate::loader::decode::Operand::X(0),
        returned,
    )?;
    return_status(builder, JIT_STATUS_NORMAL, returned);
    Ok(())
}

fn make_fun_lambda_index(operands: &[Operand]) -> Result<usize, JitError> {
    match operands {
        [index] => immediate_usize(index, "make_fun lambda index"),
        [index, _uniq, _old_index] => immediate_usize(index, "make_fun lambda index"),
        _ => Err(JitError::UnsupportedOperand {
            operand: format!("make_fun operands {operands:?}"),
        }),
    }
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
