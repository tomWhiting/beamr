//! Public Beamr-owned JIT value types.

use cranelift_jit::JITModule;
use std::fmt;
use std::sync::{Arc, Mutex};

/// A GC root location described by a future stack map entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootLocation {
    /// A live root held in a machine register.
    Register(u16),
    /// A live root held in a stack slot relative to the frame layout.
    StackSlot(i32),
}

/// Stack map metadata for one native-code safepoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackMapEntry {
    /// Machine-code offset from the function entry point.
    pub offset_from_entry: u32,
    /// Live roots known at this safepoint.
    pub live_roots: Vec<RootLocation>,
}

/// Immutable native code emitted by the JIT compiler.
#[derive(Clone)]
pub struct NativeCode {
    call_addr: usize,
    stack_maps: Vec<StackMapEntry>,
    _module_owner: Arc<Mutex<JITModule>>,
}

impl NativeCode {
    /// Creates a native-code handle from compiler-owned code memory.
    pub(crate) fn new(
        call_ptr: *const u8,
        stack_maps: Vec<StackMapEntry>,
        module_owner: Arc<Mutex<JITModule>>,
    ) -> Self {
        Self {
            call_addr: call_ptr as usize,
            stack_maps,
            _module_owner: module_owner,
        }
    }

    /// Raw entry pointer for the compiled `extern "C"` function.
    #[must_use]
    pub fn call_ptr(&self) -> *const u8 {
        self.call_addr as *const u8
    }

    /// Stack map entries for GC cooperation.
    #[must_use]
    pub fn stack_maps(&self) -> &[StackMapEntry] {
        &self.stack_maps
    }
}

impl fmt::Debug for NativeCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCode")
            .field("call_ptr", &self.call_ptr())
            .field("stack_maps", &self.stack_maps)
            .finish_non_exhaustive()
    }
}
