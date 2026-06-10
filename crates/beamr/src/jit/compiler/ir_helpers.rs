use cranelift_codegen::ir::{AbiParam, FuncRef, types};
use cranelift_jit::JITModule;
use cranelift_module::{Linkage, Module};

use crate::jit::ir_allocation::AllocationHelpers;
use crate::jit::ir_binary::BinaryHelpers;
use crate::jit::ir_closure::ClosureHelpers;
use crate::jit::ir_exceptions::ExceptionHelpers;
use crate::jit::ir_map::MapHelpers;
use crate::jit::ir_message::MessageHelpers;

use super::JitError;

/// All declared Cranelift helper function references needed during instruction lowering.
#[derive(Clone, Copy)]
pub(super) struct CompileHelpers {
    pub(super) allocation: AllocationHelpers,
    pub(super) closure: ClosureHelpers,
    pub(super) exception: ExceptionHelpers,
    pub(super) binary: BinaryHelpers,
    pub(super) map: MapHelpers,
    pub(super) message: MessageHelpers,
    pub(super) charge: FuncRef,
    pub(super) call_interpreted: FuncRef,
    pub(super) box_float: FuncRef,
}

/// Declare all Cranelift helper function references used by the instruction dispatch loop.
pub(super) fn declare_compile_helpers(
    jit_module: &mut JITModule,
    ctx: &mut cranelift_codegen::ir::Function,
) -> Result<CompileHelpers, JitError> {
    let mut tuple_signature = jit_module.make_signature();
    tuple_signature.params.push(AbiParam::new(types::I64));
    tuple_signature.params.push(AbiParam::new(types::I64));
    tuple_signature.returns.push(AbiParam::new(types::I64));
    let tuple_helper = jit_module
        .declare_function("beamr_jit_alloc_tuple", Linkage::Import, &tuple_signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    let tuple_helper = jit_module.declare_func_in_func(tuple_helper, ctx);

    let mut cons_signature = jit_module.make_signature();
    cons_signature.params.push(AbiParam::new(types::I64));
    cons_signature.returns.push(AbiParam::new(types::I64));
    let cons_helper = jit_module
        .declare_function("beamr_jit_alloc_cons", Linkage::Import, &cons_signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    let cons_helper = jit_module.declare_func_in_func(cons_helper, ctx);

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
    let closure_alloc_helper = jit_module.declare_func_in_func(closure_alloc_helper, ctx);

    let mut box_float_signature = jit_module.make_signature();
    box_float_signature.params.push(AbiParam::new(types::I64));
    box_float_signature.params.push(AbiParam::new(types::F64));
    box_float_signature.returns.push(AbiParam::new(types::I64));
    let box_float_helper = jit_module
        .declare_function("beamr_jit_box_float", Linkage::Import, &box_float_signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    let box_float_helper = jit_module.declare_func_in_func(box_float_helper, ctx);

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
    let closure_call_helper = jit_module.declare_func_in_func(closure_call_helper, ctx);

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
    let charge_helper = jit_module.declare_func_in_func(charge_helper, ctx);

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
    let call_interpreted_helper = jit_module.declare_func_in_func(call_interpreted_helper, ctx);

    let exception_class_helper =
        declare_unary_helper(jit_module, ctx, "beamr_jit_exception_class")?;
    let exception_reason_helper =
        declare_unary_helper(jit_module, ctx, "beamr_jit_exception_reason")?;
    let exception_trace_helper =
        declare_unary_helper(jit_module, ctx, "beamr_jit_exception_trace")?;
    let exception_clear_helper = declare_void_unary_helper(jit_module, ctx)?;
    let exception_add_frame_helper = declare_add_frame_helper(jit_module, ctx)?;

    let binary_helpers = declare_binary_helpers(jit_module, ctx)?;
    let map_helpers = declare_map_helpers(jit_module, ctx)?;
    let message_helpers = declare_message_helpers(jit_module, ctx)?;

    Ok(CompileHelpers {
        allocation: AllocationHelpers {
            tuple: tuple_helper,
            cons: cons_helper,
        },
        closure: ClosureHelpers {
            alloc: closure_alloc_helper,
            dispatch: closure_call_helper,
        },
        exception: ExceptionHelpers {
            class: exception_class_helper,
            reason: exception_reason_helper,
            trace: exception_trace_helper,
            clear: exception_clear_helper,
            add_frame: exception_add_frame_helper,
        },
        binary: binary_helpers,
        map: map_helpers,
        message: message_helpers,
        charge: charge_helper,
        call_interpreted: call_interpreted_helper,
        box_float: box_float_helper,
    })
}

fn declare_message_helpers(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<MessageHelpers, JitError> {
    Ok(MessageHelpers {
        send: declare_helper(
            module,
            func,
            "beamr_jit_send_message",
            &[types::I64, types::I64, types::I64],
            types::I64,
        )?,
        receive_peek: declare_multi_return_helper(
            module,
            func,
            "beamr_jit_receive_peek",
            &[types::I64],
            &[types::I8, types::I64],
        )?,
        receive_next: declare_void_helper(module, func, "beamr_jit_receive_next", &[types::I64])?,
        receive_accept: declare_void_helper(
            module,
            func,
            "beamr_jit_receive_accept",
            &[types::I64],
        )?,
        receive_wait: declare_helper(
            module,
            func,
            "beamr_jit_receive_wait",
            &[types::I64],
            types::I8,
        )?,
        receive_wait_timeout: declare_helper(
            module,
            func,
            "beamr_jit_receive_wait_timeout",
            &[types::I64, types::I64],
            types::I8,
        )?,
        receive_timeout: declare_void_helper(
            module,
            func,
            "beamr_jit_receive_timeout",
            &[types::I64],
        )?,
    })
}

fn declare_map_helpers(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<MapHelpers, JitError> {
    Ok(MapHelpers {
        new: declare_helper(
            module,
            func,
            "beamr_jit_map_new",
            &[types::I64, types::I64, types::I64, types::I64],
            types::I64,
        )?,
        update: declare_helper(
            module,
            func,
            "beamr_jit_map_update",
            &[types::I64, types::I64, types::I64, types::I64],
            types::I64,
        )?,
        get: declare_multi_return_helper(
            module,
            func,
            "beamr_jit_map_get",
            &[types::I64, types::I64],
            &[types::I8, types::I64],
        )?,
        has_key: declare_helper(
            module,
            func,
            "beamr_jit_map_has_key",
            &[types::I64, types::I64],
            types::I8,
        )?,
    })
}

fn declare_binary_helpers(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<BinaryHelpers, JitError> {
    Ok(BinaryHelpers {
        start_match: declare_helper(
            module,
            func,
            "beamr_jit_bs_start_match",
            &[types::I64, types::I64],
            types::I64,
        )?,
        get_integer: declare_helper(
            module,
            func,
            "beamr_jit_bs_get_integer",
            &[types::I64, types::I64, types::I64],
            types::I64,
        )?,
        get_binary: declare_helper(
            module,
            func,
            "beamr_jit_bs_get_binary",
            &[types::I64, types::I64, types::I64],
            types::I64,
        )?,
        test_tail: declare_helper(
            module,
            func,
            "beamr_jit_bs_test_tail",
            &[types::I64, types::I64],
            types::I8,
        )?,
        test_unit: declare_helper(
            module,
            func,
            "beamr_jit_bs_test_unit",
            &[types::I64, types::I64],
            types::I8,
        )?,
        get_utf8: declare_helper(
            module,
            func,
            "beamr_jit_bs_get_utf8",
            &[types::I64, types::I64],
            types::I64,
        )?,
        get_utf16: declare_helper(
            module,
            func,
            "beamr_jit_bs_get_utf16",
            &[types::I64, types::I64],
            types::I64,
        )?,
        get_utf32: declare_helper(
            module,
            func,
            "beamr_jit_bs_get_utf32",
            &[types::I64, types::I64],
            types::I64,
        )?,
        init: declare_helper(
            module,
            func,
            "beamr_jit_bs_init",
            &[types::I64, types::I64],
            types::I64,
        )?,
        put_integer: declare_helper(
            module,
            func,
            "beamr_jit_bs_put_integer",
            &[types::I64, types::I64, types::I64, types::I64, types::I64],
            types::I8,
        )?,
        put_binary: declare_helper(
            module,
            func,
            "beamr_jit_bs_put_binary",
            &[types::I64, types::I64, types::I64],
            types::I8,
        )?,
        put_utf8: declare_helper(
            module,
            func,
            "beamr_jit_bs_put_utf8",
            &[types::I64, types::I64, types::I64],
            types::I8,
        )?,
        put_utf16: declare_helper(
            module,
            func,
            "beamr_jit_bs_put_utf16",
            &[types::I64, types::I64, types::I64, types::I64],
            types::I8,
        )?,
        put_utf32: declare_helper(
            module,
            func,
            "beamr_jit_bs_put_utf32",
            &[types::I64, types::I64, types::I64, types::I64],
            types::I8,
        )?,
        finish: declare_helper(
            module,
            func,
            "beamr_jit_bs_finish",
            &[types::I64, types::I64],
            types::I64,
        )?,
    })
}

fn declare_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
    return_type: cranelift_codegen::ir::Type,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    declare_multi_return_helper(module, func, name, params, &[return_type])
}

fn declare_void_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    declare_multi_return_helper(module, func, name, params, &[])
}

fn declare_multi_return_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
    return_types: &[cranelift_codegen::ir::Type],
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    let mut signature = module.make_signature();
    for param in params {
        signature.params.push(AbiParam::new(*param));
    }
    for return_type in return_types {
        signature.returns.push(AbiParam::new(*return_type));
    }
    let helper = module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| JitError::CraneliftError(error.to_string()))?;
    Ok(module.declare_func_in_func(helper, func))
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
    declare_void_helper(module, func, "beamr_jit_clear_exception", &[types::I64])
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
