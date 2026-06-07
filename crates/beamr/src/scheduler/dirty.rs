//! Dirty scheduler thread pool.
//!
//! A separate pool of OS threads for native functions that take
//! a long time (git push, cargo build). Long-running work goes
//! here so normal scheduler threads stay free and fair.
//! Pool size is configurable independently of the normal
//! scheduler thread count (per D10).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use crate::native::{NativeFn, ProcessContext};
use crate::scheduler::lock_or_recover;
use crate::term::Term;

/// Default maximum number of queued dirty jobs per pool.
pub const DEFAULT_DIRTY_QUEUE_DEPTH: usize = 1024;

/// Default number of IO dirty scheduler threads.
pub const DEFAULT_DIRTY_IO_THREADS: usize = 10;

/// Distinguishes the two BEAM-style dirty scheduler pools.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirtySchedulerKind {
    /// CPU-bound dirty work.
    Cpu,
    /// IO-bound dirty work.
    Io,
}

/// Minimal oneshot result channel used by dirty jobs.
pub mod oneshot {
    use std::sync::mpsc;

    /// Sends a single value to the matching [`Receiver`].
    pub struct Sender<T>(mpsc::SyncSender<T>);

    /// Receives a single value from the matching [`Sender`].
    pub struct Receiver<T>(mpsc::Receiver<T>);

    /// Error returned when the oneshot receiver has been dropped.
    pub struct SendError<T>(pub T);

    /// Error returned when the oneshot sender has been dropped.
    #[derive(Debug, Copy, Clone, Eq, PartialEq)]
    pub struct RecvError;

    /// Creates a single-use channel.
    #[must_use]
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        (Sender(sender), Receiver(receiver))
    }

    impl<T> Sender<T> {
        /// Sends the result to the receiver.
        pub fn send(self, value: T) -> Result<(), SendError<T>> {
            self.0.send(value).map_err(|error| SendError(error.0))
        }
    }

    impl<T> Receiver<T> {
        /// Blocks until the result arrives or the sender is dropped.
        pub fn recv(self) -> Result<T, RecvError> {
            self.0.recv().map_err(|_| RecvError)
        }
    }
}

/// Native function invocation scheduled onto a dirty scheduler thread.
pub struct DirtyJob {
    /// Process id that submitted the dirty job.
    pub pid: u64,
    /// Native function to execute.
    pub function: NativeFn,
    /// Arguments passed to the native function.
    pub args: Vec<Term>,
    /// Native call context for the dirty worker.
    pub context: ProcessContext<'static>,
    /// Channel used to return the native result to the submitter.
    pub result_sender: oneshot::Sender<Result<Term, Term>>,
}

// SAFETY: dirty scheduler jobs are constructed for standalone native calls and
// use `ProcessContext<'static>` so they cannot borrow a scheduler-owned process.
// B-077 does not migrate process bodies to dirty threads; future wiring must keep
// that boundary by submitting only detached contexts.
unsafe impl Send for DirtyJob {}

enum DirtyMessage {
    Run(DirtyJob),
    Shutdown,
}

/// A bounded dirty scheduler pool backed by OS threads.
pub struct DirtyPool {
    name: String,
    thread_count: usize,
    queue_depth: usize,
    sender: Sender<DirtyMessage>,
    shutdown: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
    worker_names: Vec<String>,
}

impl DirtyPool {
    /// Creates a dirty pool with the default bounded queue depth.
    #[must_use]
    pub fn new(name: &str, thread_count: usize) -> Self {
        Self::with_queue_depth(name, thread_count, DEFAULT_DIRTY_QUEUE_DEPTH)
    }

    /// Creates the default CPU dirty pool.
    #[must_use]
    pub fn default_cpu() -> Self {
        Self::new("dirty-cpu", num_cpus::get())
    }

    /// Creates the default IO dirty pool.
    #[must_use]
    pub fn default_io() -> Self {
        Self::new("dirty-io", DEFAULT_DIRTY_IO_THREADS)
    }

    /// Creates a dirty pool with a configurable bounded queue depth.
    #[must_use]
    pub fn with_queue_depth(name: &str, thread_count: usize, queue_depth: usize) -> Self {
        let pool_thread_count = thread_count.max(1);
        let pool_queue_depth = queue_depth.max(1);
        let (sender, receiver) = crossbeam_channel::bounded(pool_queue_depth);
        let mut threads = Vec::with_capacity(pool_thread_count);
        let mut worker_names = Vec::with_capacity(pool_thread_count);

        for index in 0..pool_thread_count {
            let thread_name = format!("{name}-{index}");
            let receiver_for_thread = receiver.clone();
            match std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || worker_loop(receiver_for_thread))
            {
                Ok(handle) => {
                    worker_names.push(thread_name);
                    threads.push(handle);
                }
                Err(error) => {
                    eprintln!("failed to spawn {thread_name}: {error}");
                    break;
                }
            }
        }

        Self {
            name: name.to_owned(),
            thread_count: worker_names.len(),
            queue_depth: pool_queue_depth,
            sender,
            shutdown: AtomicBool::new(false),
            threads: Mutex::new(threads),
            worker_names,
        }
    }

    /// Enqueues a dirty job, blocking while the bounded queue is full.
    pub fn submit(&self, job: DirtyJob) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        let _ = self.sender.send(DirtyMessage::Run(job));
    }

    /// Signals all dirty workers to stop and joins them.
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }

        let mut threads = lock_or_recover(&self.threads);
        for _ in 0..threads.len() {
            let _ = self.sender.send(DirtyMessage::Shutdown);
        }
        for handle in threads.drain(..) {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Number of worker threads successfully started for this pool.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.thread_count
    }

    /// Configured bounded queue depth.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.queue_depth
    }

    /// Pool base name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Names of worker OS threads in this pool.
    #[must_use]
    pub fn worker_names(&self) -> &[String] {
        &self.worker_names
    }

    /// Whether shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

impl Drop for DirtyPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(receiver: Receiver<DirtyMessage>) {
    while let Ok(message) = receiver.recv() {
        match message {
            DirtyMessage::Run(mut job) => {
                let _pid = job.pid;
                let result = (job.function)(&job.args, &mut job.context);
                let _ = job.result_sender.send(result);
            }
            DirtyMessage::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirtyJob, DirtyPool, DirtySchedulerKind, oneshot};
    use crate::native::ProcessContext;
    use crate::term::Term;

    fn forty_two(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        Ok(Term::small_int(42))
    }

    #[test]
    fn dirty_pool_starts_named_threads_and_shuts_down_cleanly() {
        let pool = DirtyPool::new("dirty-test", 4);

        assert_eq!(pool.thread_count(), 4);
        assert_eq!(pool.worker_names().len(), 4);
        assert_eq!(
            pool.worker_names(),
            &[
                "dirty-test-0".to_owned(),
                "dirty-test-1".to_owned(),
                "dirty-test-2".to_owned(),
                "dirty-test-3".to_owned(),
            ]
        );

        pool.shutdown();
        assert!(pool.is_shutdown());
        pool.shutdown();
    }

    #[test]
    fn dirty_pool_executes_submitted_job_and_returns_result() {
        let pool = DirtyPool::with_queue_depth("dirty-test-job", 1, 1);
        let (result_sender, result_receiver) = oneshot::channel();

        pool.submit(DirtyJob {
            pid: 7,
            function: forty_two,
            args: Vec::new(),
            context: ProcessContext::new(),
            result_sender,
        });

        assert_eq!(result_receiver.recv(), Ok(Ok(Term::small_int(42))));
        pool.shutdown();
    }

    #[test]
    fn dirty_scheduler_kind_distinguishes_cpu_and_io() {
        assert_eq!(DirtySchedulerKind::Cpu, DirtySchedulerKind::Cpu);
        assert_ne!(DirtySchedulerKind::Cpu, DirtySchedulerKind::Io);
    }
}
