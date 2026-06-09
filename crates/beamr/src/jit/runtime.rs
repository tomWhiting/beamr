//! Runtime helpers callable from JIT-generated code.

use crate::gc;
use crate::process::Process;

/// Reserves heap words for a tuple and returns the first word to fill.
///
/// The generated code writes the tuple header and payload after this call. A
/// null return asks compiled code to deopt when allocation or GC cannot provide
/// enough space.
#[allow(improper_ctypes_definitions)]
pub(crate) extern "C" fn jit_alloc_tuple(process: &mut Process, arity: u64) -> *mut u64 {
    let Ok(arity) = usize::try_from(arity) else {
        return std::ptr::null_mut();
    };
    alloc_words(process, arity.saturating_add(1))
}

/// Reserves heap words for one cons cell and returns the first word to fill.
///
/// The generated code writes the head/tail words and tags the returned pointer
/// as a list term. A null return asks compiled code to deopt.
#[allow(improper_ctypes_definitions)]
pub(crate) extern "C" fn jit_alloc_cons(process: &mut Process) -> *mut u64 {
    alloc_words(process, 2)
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
