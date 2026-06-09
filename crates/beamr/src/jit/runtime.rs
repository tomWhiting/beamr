//! Runtime helpers callable from JIT-generated code.

use crate::atom::Atom;
use crate::gc;
use crate::interpreter::{ExecutionResult, run_with_registry};
use crate::module::ResolvedImportTarget;
use crate::process::{CodePosition, JitStatus, Process};
use crate::term::Term;

use super::ir_common::JIT_DEOPT_SENTINEL;

pub(crate) const JIT_YIELD_SENTINEL: i64 = -2;

/// Reserves heap words for a tuple and returns the first word to fill.
///
/// The generated code writes the tuple header and payload after this call. A
/// null return asks compiled code to deopt when allocation or GC cannot provide
/// enough space.
pub(crate) extern "C" fn jit_alloc_tuple(process: *mut Process, arity: u64) -> *mut u64 {
    let Some(process) = process_from_abi(process) else {
        return std::ptr::null_mut();
    };
    let Ok(arity) = usize::try_from(arity) else {
        return std::ptr::null_mut();
    };
    let Some(words) = arity.checked_add(1) else {
        return std::ptr::null_mut();
    };
    alloc_words(process, words)
}

/// Reserves heap words for one cons cell and returns the first word to fill.
///
/// The generated code writes the head/tail words and tags the returned pointer
/// as a list term. A null return asks compiled code to deopt.
pub(crate) extern "C" fn jit_alloc_cons(process: *mut Process) -> *mut u64 {
    let Some(process) = process_from_abi(process) else {
        return std::ptr::null_mut();
    };
    alloc_words(process, 2)
}

/// Charges one reduction at compiled function entry.
///
/// Returns `0` when compiled execution can continue and `1` when the native
/// wrapper should yield back to the scheduler.
pub(crate) extern "C" fn jit_charge_reduction(process: *mut Process) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    process.decrement_reductions(1);
    u64::from(process.reductions_exhausted())
}

/// Calls an interpreted external function from compiled code.
///
/// `module`, `function`, and `arity` identify the import MFA and `args` points
/// to the compiled register file containing the call arguments in x registers.
/// The helper returns the raw x(0) word, or a JIT sentinel when fallback cannot
/// complete synchronously.
pub(crate) extern "C" fn jit_call_interpreted(
    process: *mut Process,
    module: u64,
    function: u64,
    arity: u64,
    args: *const u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    let Some(context) = process.jit_runtime_context() else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    if context.module.is_null() || context.registry.is_null() {
        return JIT_DEOPT_SENTINEL as u64;
    }
    let Ok(module_index) = u32::try_from(module) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    let Ok(import_index) = usize::try_from(function) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    let Ok(arity) = u8::try_from(arity) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    if args.is_null() && arity != 0 {
        return JIT_DEOPT_SENTINEL as u64;
    }

    let module_atom = Atom::new(module_index);

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

    // SAFETY: The interpreter installs pointers to the current borrowed module
    // and registry for exactly the duration of the native JIT call. Helpers run
    // synchronously before that context is cleared.
    let current_module = unsafe { &*context.module };
    // SAFETY: See `current_module`; the registry pointer has the same lifetime.
    let registry = unsafe { &*context.registry };
    if current_module.name != module_atom {
        return JIT_DEOPT_SENTINEL as u64;
    }
    let Some(resolved) = current_module.resolved_imports.get(import_index) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    if resolved.arity != arity {
        return JIT_DEOPT_SENTINEL as u64;
    }
    let (target_module_atom, target_function, target_arity) = match resolved.target {
        ResolvedImportTarget::Code { .. } | ResolvedImportTarget::Deferred { .. } => {
            (resolved.module, resolved.function, resolved.arity)
        }
        ResolvedImportTarget::Unresolved { .. } | ResolvedImportTarget::Native(_) => {
            return JIT_DEOPT_SENTINEL as u64;
        }
    };
    let Some(target_module) = registry.lookup(target_module_atom) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    let Ok(instruction_pointer) = target_module.export_ip(target_function, target_arity) else {
        return JIT_DEOPT_SENTINEL as u64;
    };
    process.set_current_module(target_module);
    process.set_code_position(Some(CodePosition {
        module: target_module_atom,
        instruction_pointer,
    }));

    match run_with_registry(process, current_module, registry) {
        Ok(ExecutionResult::Exited(_))
        | Ok(ExecutionResult::Waiting)
        | Ok(ExecutionResult::DirtyCall { .. }) => JIT_DEOPT_SENTINEL as u64,
        Ok(ExecutionResult::Yielded) => {
            process.set_jit_status(Some(JitStatus::Yield));
            JIT_YIELD_SENTINEL as u64
        }
        Err(_error) => JIT_DEOPT_SENTINEL as u64,
    }
}

fn process_from_abi(process: *mut Process) -> Option<&'static mut Process> {
    if process.is_null() {
        return None;
    }

    // SAFETY: The JIT raw entry ABI passes the live `Process` pointer that owns
    // the heap for this invocation. The helper uses it only for the duration of
    // the call and rejects null pointers before constructing the reference.
    Some(unsafe { &mut *process })
}

fn alloc_words(process: &mut Process, words: usize) -> *mut u64 {
    if words == 0 {
        return std::ptr::null_mut();
    }

    if gc::ensure_space(process, words, 256).is_err() {
        return std::ptr::null_mut();
    }

    match process.heap_mut().alloc(words) {
        Ok(ptr) => ptr,
        Err(_heap_full) => std::ptr::null_mut(),
    }
}
