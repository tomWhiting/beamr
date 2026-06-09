//! Dirty-CPU JIT compilation job wiring.

use crate::loader::Instruction;
use crate::scheduler::dirty::{DirtyPool, DirtySubmitError, DirtyTask};
use std::sync::Arc;

use super::cache::{JitCache, JitCacheKey};
use super::compiler::{JitCompiler, JitError};
use super::profiler::JitProfiler;

/// Owned request to compile one BEAM function on a dirty CPU worker.
pub struct CompilationJob {
    key: JitCacheKey,
    instructions: Vec<Instruction>,
    compiler: Arc<JitCompiler>,
    profiler: Arc<JitProfiler>,
    cache: Arc<JitCache>,
}

impl CompilationJob {
    /// Creates a compilation job for an MFA and its current instruction slice.
    #[must_use]
    pub fn new(
        key: JitCacheKey,
        instructions: Vec<Instruction>,
        compiler: Arc<JitCompiler>,
        profiler: Arc<JitProfiler>,
        cache: Arc<JitCache>,
    ) -> Self {
        Self {
            key,
            instructions,
            compiler,
            profiler,
            cache,
        }
    }

    fn run(self) {
        match self.compiler.compile(
            &self.instructions,
            self.key.module,
            self.key.function,
            self.key.arity,
        ) {
            Ok(native_code) => {
                self.cache.insert(self.key, native_code);
                self.profiler
                    .mark_compiled(self.key.module, self.key.function, self.key.arity);
            }
            Err(
                JitError::UnsupportedOpcode { .. }
                | JitError::UnsupportedOperand { .. }
                | JitError::UnknownLabel { .. },
            ) => {
                self.profiler
                    .mark_unsupported(self.key.module, self.key.function, self.key.arity);
            }
            Err(JitError::CraneliftError(_) | JitError::EmptyFunction) => {
                self.profiler
                    .reset_counter(self.key.module, self.key.function, self.key.arity);
            }
        }
    }
}

/// Submits JIT compilation to the dirty CPU pool without blocking the caller.
pub fn submit_jit_compilation(
    dirty_cpu: &DirtyPool,
    job: CompilationJob,
) -> Result<(), DirtySubmitError> {
    dirty_cpu.submit_task(DirtyTask::new(move || job.run()))
}

#[cfg(test)]
mod tests {
    use super::{CompilationJob, submit_jit_compilation};
    use crate::atom::Atom;
    use crate::jit::cache::JitCache;
    use crate::jit::compiler::{JitCompiler, JitSettings};
    use crate::jit::profiler::{JitProfiler, RecordResult};
    use crate::loader::Instruction;
    use crate::scheduler::dirty::DirtyPool;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn wait_until(mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn empty_return_function_marks_compiled() {
        let pool = DirtyPool::with_queue_depth("jit-compile-success", 1, 4);
        let compiler = Arc::new(JitCompiler::new(JitSettings).unwrap());
        let profiler = Arc::new(JitProfiler::new(1));
        let cache = Arc::new(JitCache::new());
        assert_eq!(
            profiler.record_call(Atom::MODULE, Atom::OK, 0),
            RecordResult::CompileNow
        );

        let job = CompilationJob::new(
            crate::jit::cache::JitCacheKey::new(Atom::MODULE, Atom::OK, 0, 1),
            vec![Instruction::Return],
            compiler,
            Arc::clone(&profiler),
            Arc::clone(&cache),
        );
        assert_eq!(submit_jit_compilation(&pool, job), Ok(()));

        assert!(wait_until(|| profiler.is_compiled(
            Atom::MODULE,
            Atom::OK,
            0
        ) && cache
            .lookup(Atom::MODULE, Atom::OK, 0, 1)
            .is_some()));
        pool.shutdown();
    }

    #[test]
    fn unsupported_function_marks_unsupported() {
        let pool = DirtyPool::with_queue_depth("jit-compile-unsupported", 1, 4);
        let compiler = Arc::new(JitCompiler::new(JitSettings).unwrap());
        let profiler = Arc::new(JitProfiler::new(1));
        let cache = Arc::new(JitCache::new());
        assert_eq!(
            profiler.record_call(Atom::MODULE, Atom::ERROR, 0),
            RecordResult::CompileNow
        );

        let job = CompilationJob::new(
            crate::jit::cache::JitCacheKey::new(Atom::MODULE, Atom::ERROR, 0, 1),
            vec![Instruction::Generic {
                opcode: 255,
                name: "unknown",
                operands: Vec::new(),
            }],
            compiler,
            Arc::clone(&profiler),
            Arc::clone(&cache),
        );
        assert_eq!(submit_jit_compilation(&pool, job), Ok(()));

        assert!(wait_until(|| profiler.is_unsupported(
            Atom::MODULE,
            Atom::ERROR,
            0
        )));
        assert!(cache.lookup(Atom::MODULE, Atom::ERROR, 0, 1).is_none());
        for _ in 0..10 {
            assert_eq!(
                profiler.record_call(Atom::MODULE, Atom::ERROR, 0),
                RecordResult::Continue
            );
        }
        pool.shutdown();
    }
}
