//! JIT compilation infrastructure.

pub mod cache;
pub mod compile_job;
pub mod compiler;
pub(crate) mod ir_arithmetic;
pub(crate) mod ir_common;
pub(crate) mod ir_control;
pub mod profiler;
pub mod types;

pub use cache::{JitCache, JitCacheKey};
pub use compile_job::{CompilationJob, submit_jit_compilation};
pub use compiler::{JitCompiler, JitError, JitSettings};
pub use profiler::{JitProfiler, MfaKey, RecordResult};
pub use types::{NativeCode, RootLocation, StackMapEntry};
