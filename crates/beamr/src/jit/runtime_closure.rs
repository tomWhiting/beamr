//! Closure runtime helpers callable from JIT-generated code.

use crate::atom::Atom;
use crate::interpreter::opcodes::closures::resolve_closure_target;
use crate::interpreter::{ExecutionResult, run_with_registry};
use crate::jit::NativeCode;
use crate::process::{CodePosition, Exception, ExitReason, JitRuntimeContext, JitStatus, Process};
use crate::term::Term;
use crate::term::boxed::Closure;

use super::ir_common::JIT_DEOPT_SENTINEL;
use super::ir_exceptions::JitReturn;
use super::runtime::{JIT_YIELD_SENTINEL, alloc_words, process_from_abi};

pub(crate) extern "C" fn jit_alloc_closure(process: *mut Process, num_free: u64) -> *mut u64 {
    let Some(process) = process_from_abi(process) else {
        return std::ptr::null_mut();
    };
    let Ok(num_free) = usize::try_from(num_free) else {
        return std::ptr::null_mut();
    };
    let Some(words) = num_free.checked_add(7) else {
        return std::ptr::null_mut();
    };
    alloc_words(process, words)
}
pub(crate) extern "C" fn jit_call_closure(
    process: *mut Process,
    fun: u64,
    arity: u64,
    args: *const u64,
) -> JitReturn {
    let Some(process) = process_from_abi(process) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Some(context) = process.jit_runtime_context() else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Ok(arity) = u8::try_from(arity) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    if args.is_null() && arity != 0 {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    }
    let Some((current_module, registry)) = runtime_module_registry(context) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };

    let fun = Term::from_raw(fun);
    let Some(closure) = Closure::new(fun) else {
        return exception_return(process, Atom::BADFUN);
    };
    if closure.arity() != arity {
        return exception_return(process, Atom::BADARITY);
    }

    for register in 0..arity {
        let raw = if arity == 0 {
            0
        } else {
            // SAFETY: Generated code passes its live register-file pointer as
            // `args`; the helper bounds reads by the call arity validated above.
            unsafe { *args.add(usize::from(register)) }
        };
        process.set_x_reg(u16::from(register), Term::from_raw(raw));
    }
    let num_free = closure.num_free();
    if usize::from(arity)
        .checked_add(num_free)
        .filter(|count| *count <= 256)
        .is_none()
    {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    }
    for index in 0..num_free {
        let Some(value) = closure.free_var(index) else {
            return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
        };
        let Ok(register) = u16::try_from(usize::from(arity) + index) else {
            return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
        };
        process.set_x_reg(register, value);
    }

    let Ok(resolved) = resolve_closure_target(closure, current_module, Some(registry), fun) else {
        return exception_return(process, Atom::BADFUN);
    };
    let Ok(instruction_pointer) = resolved.module.label_ip(resolved.label) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    if let Some(cache) = runtime_cache(context)
        && let Some((function, function_arity)) =
            resolved.module.function_at_ip(instruction_pointer)
        && function_arity == arity
        && let Some(native) = cache.lookup(
            resolved.module.name,
            function,
            arity,
            resolved.module.generation(),
        )
    {
        process.set_code_position(Some(CodePosition {
            module: resolved.module.name,
            instruction_pointer,
        }));
        return call_native_closure(process, resolved.module.as_ref(), registry, native);
    }

    call_interpreted_closure(
        process,
        current_module,
        registry,
        resolved.module.as_ref(),
        instruction_pointer,
    )
}
fn runtime_module_registry(
    context: JitRuntimeContext,
) -> Option<(
    &'static crate::module::Module,
    &'static crate::module::ModuleRegistry,
)> {
    if context.module.is_null() || context.registry.is_null() {
        return None;
    }
    // SAFETY: The interpreter installs pointers to borrowed dispatch state for
    // exactly the duration of the native JIT call. Helpers run synchronously
    // before that context is cleared.
    let module = unsafe { &*context.module };
    // SAFETY: See `module`; the registry pointer has the same lifetime.
    let registry = unsafe { &*context.registry };
    Some((module, registry))
}

fn runtime_cache(context: JitRuntimeContext) -> Option<&'static crate::jit::JitCache> {
    if context.jit_cache.is_null() {
        return None;
    }
    // SAFETY: The cache pointer is installed in `JitRuntimeContext` for the
    // synchronous duration of native execution, matching module/registry.
    Some(unsafe { &*context.jit_cache })
}

fn call_native_closure(
    process: &mut Process,
    module: &crate::module::Module,
    registry: &crate::module::ModuleRegistry,
    native: NativeCode,
) -> JitReturn {
    let previous_context = process.jit_runtime_context();
    process.set_jit_runtime_context(Some(JitRuntimeContext::new(
        module as *const crate::module::Module,
        registry as *const crate::module::ModuleRegistry,
        previous_context.map_or(std::ptr::null(), |context| context.jit_cache),
    )));
    process.set_jit_status(None);
    let register_file = process.x_regs_mut().as_mut_ptr().cast::<u64>();
    // SAFETY: `NativeCode` is produced by `JitCompiler` with the raw ABI
    // `extern "C" fn(*mut u64, *mut Process) -> JitReturn`. The code handle
    // keeps the owning JIT module alive, and the register/process pointers are
    // valid for the synchronous duration of this call.
    let raw_fn: extern "C" fn(*mut u64, *mut Process) -> JitReturn =
        unsafe { std::mem::transmute(native.call_ptr()) };
    let returned = raw_fn(register_file, process);
    process.set_jit_runtime_context(previous_context);
    match process.take_jit_status() {
        Some(JitStatus::Yield) => JitReturn::yield_(JIT_YIELD_SENTINEL as u64),
        None => returned,
    }
}

fn call_interpreted_closure(
    process: &mut Process,
    current_module: &crate::module::Module,
    registry: &crate::module::ModuleRegistry,
    target_module: &crate::module::Module,
    instruction_pointer: usize,
) -> JitReturn {
    let saved_module = process.current_module().cloned();
    let saved_position = process.code_position();
    process.set_current_module(std::sync::Arc::new(target_module.clone()));
    process.set_code_position(Some(CodePosition {
        module: target_module.name,
        instruction_pointer,
    }));
    process.decrement_reductions(1);
    if process.reductions_exhausted() {
        process.set_jit_status(Some(JitStatus::Yield));
        return JitReturn::yield_(JIT_YIELD_SENTINEL as u64);
    }

    let result = run_with_registry(process, current_module, registry);
    if let Some(module) = saved_module {
        process.set_current_module(module);
    }
    process.set_code_position(saved_position);
    match result {
        Ok(ExecutionResult::Exited(ExitReason::Normal)) => {
            JitReturn::normal(process.x_reg(0).raw())
        }
        Ok(ExecutionResult::Exited(_)) if process.current_exception().is_some() => {
            let reason = process
                .current_exception()
                .map_or(Term::NIL.raw(), |exception| exception.reason.raw());
            JitReturn::exception(reason)
        }
        Ok(ExecutionResult::Exited(_))
        | Ok(ExecutionResult::Waiting)
        | Ok(ExecutionResult::DirtyCall { .. }) => JitReturn::deopt(JIT_DEOPT_SENTINEL as u64),
        Ok(ExecutionResult::Yielded) => {
            process.set_jit_status(Some(JitStatus::Yield));
            JitReturn::yield_(JIT_YIELD_SENTINEL as u64)
        }
        Err(_error) if process.current_exception().is_some() => {
            let reason = process
                .current_exception()
                .map_or(Term::NIL.raw(), |exception| exception.reason.raw());
            JitReturn::exception(reason)
        }
        Err(_error) => JitReturn::deopt(JIT_DEOPT_SENTINEL as u64),
    }
}

fn exception_return(process: &mut Process, reason: Atom) -> JitReturn {
    let reason = Term::atom(reason);
    process.set_current_exception(Some(Exception {
        class: Term::atom(Atom::ERROR),
        reason,
        stacktrace: Term::NIL,
    }));
    JitReturn::exception(reason.raw())
}
