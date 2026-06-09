//! JIT instruction dispatch: function compilation entry point and instruction loop.

use crate::atom::Atom;
use crate::jit::ir_binary::lower_exception_block;
use crate::jit::ir_common::JIT_DEOPT_SENTINEL;
use crate::jit::ir_control::{BlockMap, TranslationPlan};
use crate::jit::ir_exceptions::{
    CompiledFrameInfo, ExceptionLoweringState, JIT_STATUS_DEOPT, JIT_STATUS_NORMAL,
    JIT_STATUS_YIELD, return_status, return_status_raw,
};
use crate::jit::ir_float::FloatRegisterMap;
use crate::jit::runtime::{
    JIT_YIELD_SENTINEL, jit_alloc_cons, jit_alloc_tuple, jit_box_float, jit_call_interpreted,
    jit_charge_reduction,
};
use crate::jit::runtime_binary_build::{
    jit_bs_finish, jit_bs_init, jit_bs_put_binary, jit_bs_put_integer, jit_bs_put_utf8,
    jit_bs_put_utf16, jit_bs_put_utf32,
};
use crate::jit::runtime_binary_match::{
    jit_bs_get_binary, jit_bs_get_integer, jit_bs_get_utf8, jit_bs_get_utf16, jit_bs_get_utf32,
    jit_bs_start_match, jit_bs_test_tail, jit_bs_test_unit,
};
use crate::jit::runtime_closure::{jit_alloc_closure, jit_call_closure};
use crate::jit::runtime_map::{jit_map_get, jit_map_has_key, jit_map_new, jit_map_update};
use crate::jit::runtime_message::{
    jit_receive_accept, jit_receive_next, jit_receive_peek, jit_receive_timeout, jit_receive_wait,
    jit_receive_wait_timeout, jit_send_message,
};
use crate::jit::safepoint::SafepointBuilder;
use crate::jit::type_info::FunctionSignature;
use crate::jit::types::NativeCode;
use crate::scheduler::lock_or_recover;
use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::JITBuilder;
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::jit::ir_exceptions::{
    jit_add_compiled_frame, jit_clear_exception, jit_exception_class, jit_exception_reason,
    jit_exception_trace,
};

use super::dispatch_call::lower_call_instruction;
use super::dispatch_core::lower_core_instruction;
use super::dispatch_data::lower_data_instruction;
use super::dispatch_helpers::{branch_to_yield_if_exhausted, cranelift_error};
use super::ir_helpers::declare_compile_helpers;
use super::ir_typed::TypedRegisterState;
use super::{JitCompiler, JitError, JitSettings, ModuleCompileMetadata};

impl JitCompiler {
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
        builder.symbol("beamr_jit_box_float", jit_box_float as *const u8);
        builder.symbol("beamr_jit_call_closure", jit_call_closure as *const u8);
        builder.symbol(
            "beamr_jit_charge_reduction",
            jit_charge_reduction as *const u8,
        );
        builder.symbol(
            "beamr_jit_call_interpreted",
            jit_call_interpreted as *const u8,
        );
        builder.symbol("beamr_jit_bs_start_match", jit_bs_start_match as *const u8);
        builder.symbol("beamr_jit_bs_get_integer", jit_bs_get_integer as *const u8);
        builder.symbol("beamr_jit_bs_get_binary", jit_bs_get_binary as *const u8);
        builder.symbol("beamr_jit_bs_test_tail", jit_bs_test_tail as *const u8);
        builder.symbol("beamr_jit_bs_test_unit", jit_bs_test_unit as *const u8);
        builder.symbol("beamr_jit_bs_get_utf8", jit_bs_get_utf8 as *const u8);
        builder.symbol("beamr_jit_bs_get_utf16", jit_bs_get_utf16 as *const u8);
        builder.symbol("beamr_jit_bs_get_utf32", jit_bs_get_utf32 as *const u8);
        builder.symbol("beamr_jit_bs_init", jit_bs_init as *const u8);
        builder.symbol("beamr_jit_bs_put_integer", jit_bs_put_integer as *const u8);
        builder.symbol("beamr_jit_bs_put_binary", jit_bs_put_binary as *const u8);
        builder.symbol("beamr_jit_bs_put_utf8", jit_bs_put_utf8 as *const u8);
        builder.symbol("beamr_jit_bs_put_utf16", jit_bs_put_utf16 as *const u8);
        builder.symbol("beamr_jit_bs_put_utf32", jit_bs_put_utf32 as *const u8);
        builder.symbol("beamr_jit_bs_finish", jit_bs_finish as *const u8);
        builder.symbol("beamr_jit_map_new", jit_map_new as *const u8);
        builder.symbol("beamr_jit_map_update", jit_map_update as *const u8);
        builder.symbol("beamr_jit_map_get", jit_map_get as *const u8);
        builder.symbol("beamr_jit_map_has_key", jit_map_has_key as *const u8);
        builder.symbol("beamr_jit_send_message", jit_send_message as *const u8);
        builder.symbol("beamr_jit_receive_peek", jit_receive_peek as *const u8);
        builder.symbol("beamr_jit_receive_next", jit_receive_next as *const u8);
        builder.symbol("beamr_jit_receive_accept", jit_receive_accept as *const u8);
        builder.symbol("beamr_jit_receive_wait", jit_receive_wait as *const u8);
        builder.symbol(
            "beamr_jit_receive_wait_timeout",
            jit_receive_wait_timeout as *const u8,
        );
        builder.symbol(
            "beamr_jit_receive_timeout",
            jit_receive_timeout as *const u8,
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
            module: Arc::new(Mutex::new(cranelift_jit::JITModule::new(builder))),
            next_function_id: AtomicU64::new(0),
        })
    }

    pub fn compile(
        &self,
        instructions: &[crate::loader::Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_signature(instructions, module, function, arity, None)
    }

    pub fn compile_typed(
        &self,
        instructions: &[crate::loader::Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        signature: FunctionSignature,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_signature(instructions, module, function, arity, Some(signature))
    }

    pub fn compile_module_function(
        &self,
        instructions: &[crate::loader::Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        metadata: ModuleCompileMetadata<'_>,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_module_metadata(instructions, module, function, arity, None, metadata)
    }

    pub fn compile_typed_module_function(
        &self,
        instructions: &[crate::loader::Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        signature: FunctionSignature,
        metadata: ModuleCompileMetadata<'_>,
    ) -> Result<NativeCode, JitError> {
        self.compile_with_module_metadata(
            instructions,
            module,
            function,
            arity,
            Some(signature),
            metadata,
        )
    }

    fn compile_with_signature(
        &self,
        instructions: &[crate::loader::Instruction],
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
            ModuleCompileMetadata::EMPTY,
        )
    }

    fn compile_with_module_metadata(
        &self,
        instructions: &[crate::loader::Instruction],
        module: Atom,
        function: Atom,
        arity: u8,
        typed_signature: Option<FunctionSignature>,
        metadata: ModuleCompileMetadata<'_>,
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

        let helpers = declare_compile_helpers(&mut jit_module, &mut ctx.func)?;
        let mut safepoints = SafepointBuilder::new();
        let compiled_frame = CompiledFrameInfo {
            module,
            function,
            arity,
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
            let zero_float = builder.ins().f64const(0.0);
            let mut float_registers = FloatRegisterMap::new(zero_float);

            let exhausted = builder.ins().call(helpers.charge, &[process]);
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

                // Try each lowering pass in order.
                if let Some(t) = lower_core_instruction(
                    &mut builder,
                    register_file,
                    process,
                    &blocks,
                    &mut typed_state,
                    &mut safepoints,
                    &mut exceptions,
                    helpers,
                    index,
                    instruction,
                    instructions,
                )? {
                    terminated = t;
                    continue;
                }

                if let Some(t) = lower_call_instruction(
                    &mut builder,
                    register_file,
                    process,
                    &blocks,
                    &mut typed_state,
                    &mut safepoints,
                    &mut exceptions,
                    helpers,
                    compiled_frame,
                    module,
                    metadata,
                    index,
                    instruction,
                )? {
                    terminated = t;
                    continue;
                }

                terminated = lower_data_instruction(
                    &mut builder,
                    register_file,
                    process,
                    &blocks,
                    &mut typed_state,
                    &mut float_registers,
                    &mut safepoints,
                    helpers,
                    index,
                    instruction,
                )?;
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
            builder.switch_to_block(blocks.exception_block);
            lower_exception_block(&mut builder);
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
