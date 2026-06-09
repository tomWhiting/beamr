//! Runtime helpers callable from JIT-generated code.

use crate::atom::Atom;
use crate::gc;
use crate::interpreter::opcodes::closures::resolve_closure_target;
use crate::interpreter::{ExecutionResult, run_with_registry};
use crate::jit::NativeCode;
use crate::module::ResolvedImportTarget;
use crate::process::{
    CodePosition, Exception, ExitReason, JitRuntimeContext, JitStatus, Process, ProcessStatus,
    ReceiveTimeout,
};
use crate::term::Term;
use crate::term::binary::packed_word_count;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BoxedHeader, BoxedTag, Closure, ProcBin};
use crate::term::pid_ref::PidRef;
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};
use crate::term::sub_binary::{SUB_BINARY_WORDS, write_sub_binary};

use super::ir_common::JIT_DEOPT_SENTINEL;
use super::ir_exceptions::JitReturn;

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

/// Reserves heap words for a closure and returns the first word to fill.
///
/// The generated code writes the closure header, metadata, and captured free
/// variables after this call. A null return asks compiled code to deopt.
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

const RECEIVE_STATUS_MESSAGE: u8 = 0;
const RECEIVE_STATUS_EMPTY: u8 = 1;
const WAIT_STATUS_NEW_MESSAGE: u8 = 0;
const WAIT_STATUS_TIMEOUT: u8 = 1;
const WAIT_STATUS_WAITING: u8 = 2;

/// Sends `message` to a local PID when this JIT invocation can deliver it.
///
/// The current raw JIT ABI exposes only the caller process, so scheduler-wide
/// process-table delivery is not available here. Self-send is delivered into the
/// caller mailbox; other local PIDs and remote PIDs are silently dropped to keep
/// the BEAM `!` return-value contract without attempting unsupported
/// distribution from compiled code.
pub(crate) extern "C" fn jit_send_message(
    process: *mut Process,
    dest_pid: u64,
    message: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return message;
    };
    let message_term = Term::from_raw(message);
    if let Some(PidRef::Local(pid)) = PidRef::new(Term::from_raw(dest_pid))
        && pid == process.pid()
    {
        process.mailbox_mut().push_owned(message_term);
        if process.status() == ProcessStatus::Waiting {
            let _ = process.transition_to(ProcessStatus::Running);
        }
    }
    message
}

/// Peeks at the current selective-receive save pointer.
///
/// Returns `(status, message)` where status 0 carries a valid raw message term
/// and status 1 means the mailbox scan is exhausted. A two-result ABI avoids
/// colliding with valid raw terms such as small integer zero.
pub(crate) extern "C" fn jit_receive_peek(process: *mut Process) -> JitReturn {
    let Some(process) = process_from_abi(process) else {
        return receive_return(RECEIVE_STATUS_EMPTY, 0);
    };
    match process.mailbox_mut().current_message() {
        Some(message) => receive_return(RECEIVE_STATUS_MESSAGE, message.raw()),
        None => receive_return(RECEIVE_STATUS_EMPTY, 0),
    }
}

const fn receive_return(status: u8, value: u64) -> JitReturn {
    JitReturn {
        status,
        _padding: [0; 7],
        value,
    }
}

/// Advances selective receive scanning past the current message.
pub(crate) extern "C" fn jit_receive_next(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    process.mailbox_mut().advance_save_pointer();
}

/// Removes the current matched message and clears receive timeout state.
pub(crate) extern "C" fn jit_receive_accept(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    let _ = process.mailbox_mut().remove_current_message();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
}

/// Prepares a process to wait for a new mailbox message.
pub(crate) extern "C" fn jit_receive_wait(process: *mut Process) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return WAIT_STATUS_WAITING;
    };
    if process.mailbox_mut().current_message().is_some() {
        return WAIT_STATUS_NEW_MESSAGE;
    }
    transition_process_to_waiting(process);
    process.set_jit_status(Some(JitStatus::Yield));
    WAIT_STATUS_WAITING
}

/// Prepares a process to wait with a timeout.
///
/// When interpreted code position metadata is available, records a scheduler
/// timer using that position as the resume target. Compiled code also branches
/// to its in-function timeout label for immediate `after 0`; non-zero timeouts
/// yield out to the scheduler after the waiting state is installed.
pub(crate) extern "C" fn jit_receive_wait_timeout(process: *mut Process, timeout: u64) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return WAIT_STATUS_WAITING;
    };
    if process.mailbox_mut().current_message().is_some() {
        return WAIT_STATUS_NEW_MESSAGE;
    }
    let milliseconds = Term::from_raw(timeout)
        .as_small_int()
        .and_then(|value| u64::try_from(value).ok());
    if milliseconds == Some(0) {
        return WAIT_STATUS_TIMEOUT;
    }
    if let Some(milliseconds) = milliseconds
        && let Some(position) = process.code_position()
    {
        process.set_receive_timeout(Some(ReceiveTimeout {
            timeout_position: position,
            milliseconds,
        }));
        process.set_receive_timer_ref(None);
    }
    transition_process_to_waiting(process);
    process.set_jit_status(Some(JitStatus::Yield));
    WAIT_STATUS_WAITING
}

/// Resets selective receive state after a timeout clause starts executing.
pub(crate) extern "C" fn jit_receive_timeout(process: *mut Process) {
    let Some(process) = process_from_abi(process) else {
        return;
    };
    process.mailbox_mut().reset_save_pointer();
    process.set_receive_timeout(None);
    process.set_receive_timer_ref(None);
}

fn transition_process_to_waiting(process: &mut Process) {
    if process.status() == ProcessStatus::New {
        let _ = process.transition_to(ProcessStatus::Running);
    }
    let _ = process.transition_to(ProcessStatus::Waiting);
}

const BUILDER_META_WORDS: usize = 3;
const MATCH_CONTEXT_WORDS: usize = 4;
const BINARY_HELPER_FAILURE: u64 = u64::MAX;

/// Starts binary matching and returns a heap match-context term, or zero on match failure/deopt.
pub(crate) extern "C" fn jit_bs_start_match(process: *mut Process, binary: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let source = Term::from_raw(binary);
    let Some(binary) = BinaryRef::new(source) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(total_bits) = binary.len().checked_mul(u8::BITS as usize) else {
        return BINARY_HELPER_FAILURE;
    };
    let ptr = alloc_words(process, MATCH_CONTEXT_WORDS);
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `alloc_words` returned a non-null pointer to exactly
    // `MATCH_CONTEXT_WORDS` contiguous process heap words.
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, MATCH_CONTEXT_WORDS) };
    heap[0] = BoxedHeader::new(BoxedTag::MatchContext, MATCH_CONTEXT_WORDS - 1);
    heap[1] = 0;
    heap[2] = total_bits as u64;
    heap[3] = source.raw();
    Term::boxed_ptr(heap.as_ptr()).raw()
}

/// Extracts a byte-aligned integer from a match context and advances it on success.
pub(crate) extern "C" fn jit_bs_get_integer(match_ctx: u64, size_bits: u64, flags: u64) -> u64 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Ok(size_bits) = usize::try_from(size_bits) else {
        return BINARY_HELPER_FAILURE;
    };
    if !size_bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
        || !context.has_bits(size_bits)
    {
        return BINARY_HELPER_FAILURE;
    }
    let Some(bytes) = context.slice(size_bits) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(value) = decode_integer(bytes, SegmentFlags::from_raw(flags)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(term) = Term::try_small_int(value) else {
        return BINARY_HELPER_FAILURE;
    };
    context.set_position_bits(context.position_bits() + size_bits);
    term.raw()
}

/// Extracts a byte-aligned binary/sub-binary from a match context and advances it on success.
pub(crate) extern "C" fn jit_bs_get_binary(
    process: *mut Process,
    match_ctx: u64,
    size_bits: u64,
) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let bits = if size_bits == u64::MAX {
        context.remaining_bits()
    } else {
        let Ok(bits) = usize::try_from(size_bits) else {
            return BINARY_HELPER_FAILURE;
        };
        bits
    };
    if !bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
        || !context.has_bits(bits)
    {
        return BINARY_HELPER_FAILURE;
    }
    let Some(bytes) = context.slice(bits) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(binary) = allocate_extracted_binary(process, context, bytes, bits) else {
        return 0;
    };
    context.set_position_bits(context.position_bits() + bits);
    binary.raw()
}

pub(crate) extern "C" fn jit_bs_test_tail(match_ctx: u64, expected_bits: u64) -> u8 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return 0;
    };
    let Ok(expected_bits) = usize::try_from(expected_bits) else {
        return 0;
    };
    u8::from(context.remaining_bits() == expected_bits)
}

pub(crate) extern "C" fn jit_bs_test_unit(match_ctx: u64, unit: u64) -> u8 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return 0;
    };
    let Ok(unit) = usize::try_from(unit) else {
        return 0;
    };
    u8::from(unit != 0 && context.remaining_bits().is_multiple_of(unit))
}

pub(crate) extern "C" fn jit_bs_get_utf8(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf8)
}

pub(crate) extern "C" fn jit_bs_get_utf16(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf16)
}

pub(crate) extern "C" fn jit_bs_get_utf32(match_ctx: u64, flags: u64) -> u64 {
    get_utf(match_ctx, flags, decode_utf32)
}

pub(crate) extern "C" fn jit_bs_init(process: *mut Process, size_hint: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Ok(capacity) = usize::try_from(size_hint) else {
        return 0;
    };
    let Some(words) = BUILDER_META_WORDS.checked_add(packed_word_count(capacity)) else {
        return 0;
    };
    let ptr = alloc_words(process, words);
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `alloc_words` returned a non-null pointer to `words` contiguous
    // process heap words for the builder object.
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    heap[0] = BoxedHeader::new(BoxedTag::BinaryBuilder, words - 1);
    heap[1] = 0;
    heap[2] = capacity as u64;
    Term::boxed_ptr(heap.as_ptr()).raw()
}

pub(crate) extern "C" fn jit_bs_put_integer(
    process: *mut Process,
    builder: u64,
    value: u64,
    size_bits: u64,
    flags: u64,
) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(value) = Term::from_raw(value).as_small_int() else {
        set_badarg(process);
        return 1;
    };
    let Ok(size_bits) = usize::try_from(size_bits) else {
        set_badarg(process);
        return 1;
    };
    if size_bits == 0
        || !size_bits.is_multiple_of(u8::BITS as usize)
        || !builder
            .write_position_bits()
            .is_multiple_of(u8::BITS as usize)
        || !builder.can_append(size_bits)
    {
        set_badarg(process);
        return 1;
    }
    let byte_count = size_bits / u8::BITS as usize;
    let Some(bytes) = encode_integer(value, byte_count, Endian::from_raw(flags)) else {
        set_badarg(process);
        return 1;
    };
    let start = builder.write_position_bits();
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

pub(crate) extern "C" fn jit_bs_put_binary(process: *mut Process, builder: u64, source: u64) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(binary) = BinaryRef::new(Term::from_raw(source)) else {
        set_badarg(process);
        return 1;
    };
    let bytes = binary.as_bytes();
    let size_bits = bytes.len() * u8::BITS as usize;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        set_badarg(process);
        return 1;
    }
    builder.write_bytes(start / u8::BITS as usize, bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

pub(crate) extern "C" fn jit_bs_put_utf8(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
) -> u8 {
    put_utf(process, builder, codepoint, |codepoint, out| {
        encode_utf8(codepoint, out)
    })
}

pub(crate) extern "C" fn jit_bs_put_utf16(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    flags: u64,
) -> u8 {
    put_utf(process, builder, codepoint, |codepoint, out| {
        encode_utf16(codepoint, Endian::from_raw(flags), out)
    })
}

pub(crate) extern "C" fn jit_bs_put_utf32(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    flags: u64,
) -> u8 {
    put_utf(process, builder, codepoint, |codepoint, out| {
        encode_utf32(codepoint, Endian::from_raw(flags), out)
    })
}

pub(crate) extern "C" fn jit_bs_finish(process: *mut Process, builder: u64) -> u64 {
    let Some(process) = process_from_abi(process) else {
        return 0;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 0;
    };
    if !builder
        .write_position_bits()
        .is_multiple_of(u8::BITS as usize)
    {
        set_badarg(process);
        return 0;
    }
    let byte_len = builder.write_position_bits() / u8::BITS as usize;
    let Some(bytes) = builder.bytes(byte_len) else {
        set_badarg(process);
        return 0;
    };
    let Some(binary) = allocate_binary(process, bytes) else {
        return 0;
    };
    binary.raw()
}

/// Calls an interpreted external function from compiled code.
///
/// `module`, `function`, and `arity` identify the import MFA and `args` points
/// to the compiled register file containing the call arguments in x registers.
/// The helper returns `(status, value)`, where status `1` propagates an
/// exception left in the process exception state.
pub(crate) extern "C" fn jit_call_interpreted(
    process: *mut Process,
    module: u64,
    function: u64,
    arity: u64,
    args: *const u64,
) -> JitReturn {
    let Some(process) = process_from_abi(process) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Some(context) = process.jit_runtime_context() else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    if context.module.is_null() || context.registry.is_null() {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    }
    let Ok(module_index) = u32::try_from(module) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Ok(import_index) = usize::try_from(function) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Ok(arity) = u8::try_from(arity) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    if args.is_null() && arity != 0 {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
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
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    }
    let Some(resolved) = current_module.resolved_imports.get(import_index) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    if resolved.arity != arity {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    }
    let (target_module_atom, target_function, target_arity) = match resolved.target {
        ResolvedImportTarget::Code { .. } | ResolvedImportTarget::Deferred { .. } => {
            (resolved.module, resolved.function, resolved.arity)
        }
        ResolvedImportTarget::Unresolved { .. } | ResolvedImportTarget::Native(_) => {
            return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
        }
    };
    let Some(target_module) = registry.lookup(target_module_atom) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let Ok(instruction_pointer) = target_module.export_ip(target_function, target_arity) else {
        return JitReturn::deopt(JIT_DEOPT_SENTINEL as u64);
    };
    let saved_module = process.current_module().cloned();
    let saved_position = process.code_position();
    process.set_current_module(target_module);
    process.set_code_position(Some(CodePosition {
        module: target_module_atom,
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

/// Dispatches a closure call from compiled code through the same mixed-mode
/// target resolution used by interpreted `call_fun`.
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

pub(crate) fn process_from_abi(process: *mut Process) -> Option<&'static mut Process> {
    if process.is_null() {
        return None;
    }

    // SAFETY: The JIT raw entry ABI passes the live `Process` pointer that owns
    // the heap for this invocation. The helper uses it only for the duration of
    // the call and rejects null pointers before constructing the reference.
    Some(unsafe { &mut *process })
}

#[derive(Copy, Clone)]
struct JitMatchContext {
    ptr: *mut u64,
}

impl JitMatchContext {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::MatchContext)).then_some(Self { ptr })
    }

    fn position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }

    fn set_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }

    fn total_bits(self) -> usize {
        read_word(self.ptr, 2) as usize
    }

    fn source_term(self) -> Term {
        Term::from_raw(read_word(self.ptr, 3))
    }

    fn source(self) -> Option<BinaryRef> {
        BinaryRef::new(self.source_term())
    }

    fn remaining_bits(self) -> usize {
        self.total_bits().saturating_sub(self.position_bits())
    }

    fn has_bits(self, bits: usize) -> bool {
        self.position_bits()
            .checked_add(bits)
            .is_some_and(|end| end <= self.total_bits())
    }

    fn slice(self, bits: usize) -> Option<&'static [u8]> {
        if !bits.is_multiple_of(u8::BITS as usize)
            || !self.position_bits().is_multiple_of(u8::BITS as usize)
        {
            return None;
        }
        let start = self.position_bits() / u8::BITS as usize;
        let len = bits / u8::BITS as usize;
        let end = start.checked_add(len)?;
        self.source()?.as_bytes().get(start..end)
    }
}

#[derive(Copy, Clone)]
struct JitBinaryBuilder {
    ptr: *mut u64,
}

impl JitBinaryBuilder {
    fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::BinaryBuilder)).then_some(Self { ptr })
    }

    fn write_position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }

    fn set_write_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }

    fn capacity_bytes(self) -> usize {
        read_word(self.ptr, 2) as usize
    }

    fn can_append(self, bits: usize) -> bool {
        self.write_position_bits()
            .checked_add(bits)
            .is_some_and(|end| end <= self.capacity_bytes() * u8::BITS as usize)
    }

    fn write_bytes(self, start: usize, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().copied().enumerate() {
            let index = start + offset;
            let word_offset = BUILDER_META_WORDS + index / std::mem::size_of::<u64>();
            let shift = (index % std::mem::size_of::<u64>()) * u8::BITS as usize;
            let mut word = read_word(self.ptr, word_offset);
            word &= !(0xff_u64 << shift);
            word |= u64::from(byte) << shift;
            write_word(self.ptr, word_offset, word);
        }
    }

    fn bytes(self, len: usize) -> Option<&'static [u8]> {
        if len > self.capacity_bytes() {
            return None;
        }
        // SAFETY: The builder payload starts at `BUILDER_META_WORDS` and remains
        // live for the process heap lifetime; `len` is capped to builder capacity.
        Some(unsafe {
            std::slice::from_raw_parts(self.ptr.add(BUILDER_META_WORDS).cast::<u8>(), len)
        })
    }
}

#[derive(Copy, Clone)]
enum Endian {
    Big,
    Little,
}

impl Endian {
    fn from_raw(flags: u64) -> Self {
        if flags & 0x02 != 0 || flags & 0x01 != 0 {
            Self::Little
        } else {
            Self::Big
        }
    }
}

#[derive(Copy, Clone)]
struct SegmentFlags {
    endian: Endian,
    signed: bool,
}

impl SegmentFlags {
    fn from_raw(flags: u64) -> Self {
        Self {
            endian: Endian::from_raw(flags),
            signed: flags & 0x04 != 0,
        }
    }
}

fn boxed_tag(ptr: *const u64) -> Option<BoxedTag> {
    BoxedHeader::tag(read_word(ptr.cast_mut(), 0))
}

fn read_word(ptr: *mut u64, offset: usize) -> u64 {
    // SAFETY: Callers only pass pointers to live boxed terms or builder/match
    // contexts and offsets within their fixed layouts.
    unsafe { *ptr.add(offset) }
}

fn write_word(ptr: *mut u64, offset: usize, value: u64) {
    // SAFETY: Callers only write offsets within heap objects allocated for the
    // matching fixed binary-builder/match-context layouts.
    unsafe { *ptr.add(offset) = value }
}

fn decode_integer(bytes: &[u8], flags: SegmentFlags) -> Option<i64> {
    if bytes.len() > std::mem::size_of::<i64>() {
        return None;
    }
    let msb = match flags.endian {
        Endian::Big => bytes.first(),
        Endian::Little => bytes.last(),
    };
    let negative = flags.signed && msb.is_some_and(|byte| byte & 0x80 != 0);
    let fill = if negative { 0xff_u8 } else { 0x00_u8 };
    let mut full = [fill; 8];
    match flags.endian {
        Endian::Big => full[8 - bytes.len()..].copy_from_slice(bytes),
        Endian::Little => full[..bytes.len()].copy_from_slice(bytes),
    }
    Some(match flags.endian {
        Endian::Big => u64::from_be_bytes(full) as i64,
        Endian::Little => u64::from_le_bytes(full) as i64,
    })
}

fn encode_integer(value: i64, byte_count: usize, endian: Endian) -> Option<Vec<u8>> {
    if byte_count > std::mem::size_of::<i64>() {
        return None;
    }
    let bits = byte_count * u8::BITS as usize;
    if bits < i64::BITS as usize && (value < 0 || (value as u64) >= (1_u64 << bits)) {
        return None;
    }
    Some(match endian {
        Endian::Big => value.to_be_bytes()[std::mem::size_of::<i64>() - byte_count..].to_vec(),
        Endian::Little => value.to_le_bytes()[..byte_count].to_vec(),
    })
}

fn allocate_binary(process: &mut Process, bytes: &[u8]) -> Option<Term> {
    let words = alloc_binary_word_count(bytes.len());
    let ptr = alloc_words(process, words);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `alloc_words` returned `words` contiguous process heap words for
    // the threshold-aware binary writer.
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    alloc_binary(heap, bytes)
}

fn allocate_extracted_binary(
    process: &mut Process,
    context: JitMatchContext,
    bytes: &[u8],
    bits: usize,
) -> Option<Term> {
    let source = context.source_term();
    if ProcBin::new(source).is_some() {
        let start = context.position_bits() / u8::BITS as usize;
        let length = bits / u8::BITS as usize;
        let ptr = alloc_words(process, SUB_BINARY_WORDS);
        if ptr.is_null() {
            return None;
        }
        // SAFETY: `alloc_words` returned exactly `SUB_BINARY_WORDS` contiguous
        // process heap words for the sub-binary writer.
        let heap = unsafe { std::slice::from_raw_parts_mut(ptr, SUB_BINARY_WORDS) };
        return write_sub_binary(heap, source, start, length);
    }
    allocate_binary(process, bytes)
}

fn get_utf(
    match_ctx: u64,
    flags: u64,
    decoder: fn(JitMatchContext, Endian) -> Option<(u32, usize)>,
) -> u64 {
    let Some(context) = JitMatchContext::new(Term::from_raw(match_ctx)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some((codepoint, bits)) = decoder(context, Endian::from_raw(flags)) else {
        return BINARY_HELPER_FAILURE;
    };
    let Some(term) = Term::try_small_int(i64::from(codepoint)) else {
        return BINARY_HELPER_FAILURE;
    };
    context.set_position_bits(context.position_bits() + bits);
    term.raw()
}

fn decode_utf8(context: JitMatchContext, _endian: Endian) -> Option<(u32, usize)> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return None;
    }
    let bytes = context.slice(context.remaining_bits())?;
    let first = bytes.first().copied()?;
    let (needed, mut codepoint, min) = if first <= 0x7f {
        (1, u32::from(first), 0)
    } else if (0xc2..=0xdf).contains(&first) {
        (2, u32::from(first & 0x1f), 0x80)
    } else if (0xe0..=0xef).contains(&first) {
        (3, u32::from(first & 0x0f), 0x800)
    } else if (0xf0..=0xf4).contains(&first) {
        (4, u32::from(first & 0x07), 0x10000)
    } else {
        return None;
    };
    if bytes.len() < needed {
        return None;
    }
    for byte in &bytes[1..needed] {
        if byte & 0xc0 != 0x80 {
            return None;
        }
        codepoint = (codepoint << 6) | u32::from(byte & 0x3f);
    }
    (codepoint >= min && valid_codepoint(codepoint))
        .then_some((codepoint, needed * u8::BITS as usize))
}

fn decode_utf16(context: JitMatchContext, endian: Endian) -> Option<(u32, usize)> {
    let first = read_u16(context, 0, endian)?;
    if (0xd800..=0xdbff).contains(&first) {
        let second = read_u16(context, 2, endian)?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return None;
        }
        let high = u32::from(first) - 0xd800;
        let low = u32::from(second) - 0xdc00;
        let codepoint = 0x10000 + ((high << 10) | low);
        valid_codepoint(codepoint).then_some((codepoint, 32))
    } else if (0xdc00..=0xdfff).contains(&first) {
        None
    } else {
        Some((u32::from(first), 16))
    }
}

fn decode_utf32(context: JitMatchContext, endian: Endian) -> Option<(u32, usize)> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) || !context.has_bits(32) {
        return None;
    }
    let bytes = context.slice(32)?;
    let codepoint = match endian {
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    };
    valid_codepoint(codepoint).then_some((codepoint, 32))
}

fn read_u16(context: JitMatchContext, byte_offset: usize, endian: Endian) -> Option<u16> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return None;
    }
    let bits = (byte_offset + 2) * u8::BITS as usize;
    if !context.has_bits(bits) {
        return None;
    }
    let bytes = context.slice(bits)?;
    let pair = [bytes[byte_offset], bytes[byte_offset + 1]];
    Some(match endian {
        Endian::Big => u16::from_be_bytes(pair),
        Endian::Little => u16::from_le_bytes(pair),
    })
}

fn put_utf(
    process: *mut Process,
    builder: u64,
    codepoint: u64,
    encoder: impl FnOnce(u32, &mut Vec<u8>) -> bool,
) -> u8 {
    let Some(process) = process_from_abi(process) else {
        return 1;
    };
    let Some(builder) = JitBinaryBuilder::new(Term::from_raw(builder)) else {
        set_badarg(process);
        return 1;
    };
    let Some(codepoint) = Term::from_raw(codepoint).as_small_int() else {
        set_badarg(process);
        return 1;
    };
    let Ok(codepoint) = u32::try_from(codepoint) else {
        set_badarg(process);
        return 1;
    };
    if !valid_codepoint(codepoint) {
        set_badarg(process);
        return 1;
    }
    let mut bytes = Vec::with_capacity(4);
    if !encoder(codepoint, &mut bytes) {
        set_badarg(process);
        return 1;
    }
    let size_bits = bytes.len() * u8::BITS as usize;
    let start = builder.write_position_bits();
    if !start.is_multiple_of(u8::BITS as usize) || !builder.can_append(size_bits) {
        set_badarg(process);
        return 1;
    }
    builder.write_bytes(start / u8::BITS as usize, &bytes);
    builder.set_write_position_bits(start + size_bits);
    0
}

fn encode_utf8(codepoint: u32, out: &mut Vec<u8>) -> bool {
    let Some(character) = char::from_u32(codepoint) else {
        return false;
    };
    let mut buffer = [0_u8; 4];
    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    true
}

fn encode_utf16(codepoint: u32, endian: Endian, out: &mut Vec<u8>) -> bool {
    let Some(character) = char::from_u32(codepoint) else {
        return false;
    };
    let mut units = [0_u16; 2];
    for unit in character.encode_utf16(&mut units) {
        let bytes = match endian {
            Endian::Big => unit.to_be_bytes(),
            Endian::Little => unit.to_le_bytes(),
        };
        out.extend_from_slice(&bytes);
    }
    true
}

fn encode_utf32(codepoint: u32, endian: Endian, out: &mut Vec<u8>) -> bool {
    if !valid_codepoint(codepoint) {
        return false;
    }
    let bytes = match endian {
        Endian::Big => codepoint.to_be_bytes(),
        Endian::Little => codepoint.to_le_bytes(),
    };
    out.extend_from_slice(&bytes);
    true
}

fn valid_codepoint(codepoint: u32) -> bool {
    codepoint <= 0x10ffff && !(0xd800..=0xdfff).contains(&codepoint)
}

fn set_badarg(process: &mut Process) {
    process.set_current_exception(Some(Exception {
        class: Term::atom(Atom::ERROR),
        reason: Term::atom(Atom::BADARG),
        stacktrace: Term::NIL,
    }));
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
