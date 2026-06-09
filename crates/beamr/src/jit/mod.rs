//! JIT compilation infrastructure.

pub mod aot;
pub(crate) mod aot_format;
pub mod cache;
pub mod compile_job;
pub mod compiler;
pub(crate) mod ir_allocation;
pub(crate) mod ir_arithmetic;
pub(crate) mod ir_binary;
pub(crate) mod ir_binary_lowering;
pub(crate) mod ir_common;
pub(crate) mod ir_control;
pub(crate) mod ir_exceptions;
pub(crate) mod ir_guards;
pub mod profiler;
pub(crate) mod runtime;
pub mod safepoint;
pub mod type_info;
pub mod types;

pub use aot::{
    AotCompiler, AotError, AotResult, NativeCodeBundle, NativeEntries, NativeModuleEntries,
    TypedIrTranslator,
};
pub use cache::{JitCache, JitCacheKey};
pub use compile_job::{CompilationJob, CompilationRequest, submit_jit_compilation};
pub use compiler::{JitCompiler, JitError, JitSettings};
pub use profiler::{DEFAULT_JIT_THRESHOLD, JitProfiler, MfaKey, RecordResult};
pub use type_info::GleamTypeReader;
pub use types::{NativeCode, RootLocation, StackMapEntry};
