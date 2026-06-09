use cranelift_codegen::ir::{AbiParam, types};
use cranelift_jit::JITModule;
use cranelift_module::{Linkage, Module};

use crate::jit::ir_binary::BinaryHelpers;
use crate::jit::ir_map::MapHelpers;
use crate::jit::ir_message::MessageHelpers;

use super::JitError;

pub(super) fn declare_message_helpers(
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

pub(super) fn declare_map_helpers(
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

pub(super) fn declare_binary_helpers(
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

pub(super) fn declare_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
    return_type: cranelift_codegen::ir::Type,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    declare_multi_return_helper(module, func, name, params, &[return_type])
}

pub(super) fn declare_void_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
    name: &str,
    params: &[cranelift_codegen::ir::Type],
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    declare_multi_return_helper(module, func, name, params, &[])
}

pub(super) fn declare_multi_return_helper(
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

pub(super) fn declare_unary_helper(
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

pub(super) fn declare_void_unary_helper(
    module: &mut JITModule,
    func: &mut cranelift_codegen::ir::Function,
) -> Result<cranelift_codegen::ir::FuncRef, JitError> {
    declare_void_helper(module, func, "beamr_jit_clear_exception", &[types::I64])
}

pub(super) fn declare_add_frame_helper(
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
