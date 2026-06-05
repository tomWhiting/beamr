//! Scheduler — fairness across every core.
//!
//! N OS threads, each with a run queue of ready processes. Work stealing keeps
//! all cores busy. No async runtime in the hot path (per D3) — plain OS threads
//! plus lock-free queues.

pub mod dirty;
pub mod run_queue;
pub mod steal;
mod supervision_integration;
mod timer_integration;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crossbeam_deque::Stealer;
use crossbeam_queue::SegQueue;
use dashmap::DashMap;

use crate::atom::{Atom, AtomTable};
use crate::error::{ExecError, LoadError};
use crate::hook::{Hook, HookDecision};
use crate::interpreter::{self, ExecutionResult};
use crate::io::{IoSink, NullSink};
use crate::loader::{self, Instruction};
use crate::module::PurgeError;
use crate::module::{Module, ModuleRegistry};
use crate::native::{BifRegistryImpl, CodeManagementFacility};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::registry::ProcessTable;
use crate::process::{CodePosition, ExitReason, Process, ProcessStatus};
use crate::supervision::link::LinkSet;
use crate::supervision::monitor::MonitorSet;
use crate::term::Term;
use crate::timer::TimerWheel;

use run_queue::RunQueue;

/// Default number of reductions per scheduler time slice.
pub const DEFAULT_REDUCTION_BUDGET: u32 = crate::process::DEFAULT_REDUCTION_BUDGET;

/// Configuration for the scheduler thread pool.
#[derive(Clone, Debug, Default)]
pub struct SchedulerConfig {
    /// Number of scheduler threads. Defaults to `available_parallelism()`.
    pub thread_count: Option<usize>,
}

pub(super) struct SharedState {
    shutdown: AtomicBool,
    process_table: ProcessTable,
    module_registry: Arc<ModuleRegistry>,
    atom_table: Arc<AtomTable>,
    bif_registry: Arc<BifRegistryImpl>,
    spawn_counter: AtomicUsize,
    thread_count: usize,
    next_pid: AtomicU64,
    wait_set: Mutex<WaitSet>,
    wake_condvar: Condvar,
    process_bodies: DashMap<u64, Mutex<Option<ScheduledProcess>>>,
    exit_tombstones: DashMap<u64, ExitReason>,
    exit_results: DashMap<u64, Term>,
    async_results: DashMap<u64, Term>,
    link_set: Mutex<LinkSet>,
    monitor_set: Mutex<MonitorSet>,
    hook: Hook,
    timers: Arc<Mutex<TimerWheel>>,
    output_sink: Mutex<Arc<dyn IoSink>>,
    #[cfg(test)]
    idle_parks: AtomicUsize,
}

#[derive(Default)]
struct WaitSet {
    waiting: std::collections::HashMap<u64, usize>,
    woken: Vec<(u64, usize)>,
}

struct SpawnRequest {
    pid: u64,
    module: Atom,
    module_version: Arc<Module>,
    instruction_pointer: usize,
    args: Vec<Term>,
}

struct ScheduledProcess(Process);

// SAFETY: Process is not Send at the public API boundary. The scheduler is the
// sole owner of process execution, storing each body behind a mutex-protected
// Option. Workers take exclusive ownership before executing a time slice.
unsafe impl Send for ScheduledProcess {}

/// Work-stealing scheduler with N OS threads.
pub struct Scheduler {
    shared: Arc<SharedState>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    inject_queues: Vec<Arc<SegQueue<SpawnRequest>>>,
    worker_names: Vec<String>,
}

/// Result returned by a successful hot module load.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HotLoadResult {
    pub module_name: Atom,
    pub generation: u64,
    pub had_old_version: bool,
    pub on_load_required: bool,
    pub on_load_succeeded: bool,
}

/// Result returned by safe or forced module purge.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PurgeResult {
    pub module_name: Atom,
    pub processes_killed: usize,
}

impl Scheduler {
    /// Create and start a scheduler with the supplied module registry.
    pub fn new(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
    ) -> Result<Self, String> {
        Self::with_code_server(
            config,
            module_registry,
            Arc::new(AtomTable::with_common_atoms()),
            Arc::new(BifRegistryImpl::new()),
        )
    }

    /// Create and start a scheduler with explicit code-server state.
    pub fn with_code_server(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        atom_table: Arc<AtomTable>,
        bif_registry: Arc<BifRegistryImpl>,
    ) -> Result<Self, String> {
        let thread_count = configured_thread_count(config.thread_count);
        let shared = Arc::new(SharedState {
            shutdown: AtomicBool::new(false),
            process_table: ProcessTable::new(),
            module_registry,
            atom_table,
            bif_registry,
            spawn_counter: AtomicUsize::new(0),
            thread_count,
            next_pid: AtomicU64::new(0),
            wait_set: Mutex::new(WaitSet::default()),
            wake_condvar: Condvar::new(),
            process_bodies: DashMap::new(),
            exit_tombstones: DashMap::new(),
            exit_results: DashMap::new(),
            async_results: DashMap::new(),
            link_set: Mutex::new(LinkSet::new()),
            monitor_set: Mutex::new(MonitorSet::new()),
            hook: Hook::new(),
            timers: Arc::new(Mutex::new(TimerWheel::new())),
            output_sink: Mutex::new(Arc::new(NullSink)),
            #[cfg(test)]
            idle_parks: AtomicUsize::new(0),
        });
        let inject_queues: Vec<_> = (0..thread_count)
            .map(|_| Arc::new(SegQueue::new()))
            .collect();
        let barrier = Arc::new(std::sync::Barrier::new(thread_count + 1));
        let stealers_ready: Arc<Mutex<Option<Vec<Stealer<u64>>>>> = Arc::new(Mutex::new(None));
        let mut stealer_receivers = Vec::with_capacity(thread_count);
        let mut threads = Vec::with_capacity(thread_count);
        let mut worker_names = Vec::with_capacity(thread_count);
        for (index, inject_queue) in inject_queues.iter().enumerate() {
            let (tx, rx) = std::sync::mpsc::channel();
            stealer_receivers.push(rx);
            let shared_for_thread = Arc::clone(&shared);
            let barrier_for_thread = Arc::clone(&barrier);
            let ready_for_thread = Arc::clone(&stealers_ready);
            let inject = Arc::clone(inject_queue);
            let thread_name = format!("beamr-sched-{index}");
            worker_names.push(thread_name.clone());
            let handle = std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || {
                    let queue = RunQueue::new();
                    if tx.send(queue.stealer()).is_err() {
                        return;
                    }
                    barrier_for_thread.wait();
                    let stealers = {
                        let guard = lock_or_recover(&ready_for_thread);
                        guard.as_ref().cloned().unwrap_or_default()
                    };
                    scheduler_loop(&shared_for_thread, &queue, index, &stealers, &inject);
                })
                .map_err(|error| format!("failed to spawn {thread_name}: {error}"))?;
            threads.push(handle);
        }
        let mut stealers = Vec::with_capacity(thread_count);
        for rx in stealer_receivers {
            let stealer = rx
                .recv()
                .map_err(|error| format!("failed to receive scheduler stealer: {error}"))?;
            stealers.push(stealer);
        }
        {
            let mut guard = lock_or_recover(&stealers_ready);
            *guard = Some(stealers);
        }
        barrier.wait();
        Ok(Self {
            shared,
            threads: Mutex::new(threads),
            inject_queues,
            worker_names,
        })
    }

    /// Hot-load a BEAM module, running on_load before committing when required.
    pub fn hot_load_module(&self, bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
        hot_load_module_shared(&self.shared, bytes)
    }

    /// Safely purge retained old code when no process still references it.
    pub fn purge_module(&self, name: Atom) -> Result<PurgeResult, PurgeError> {
        purge_module_shared(&self.shared, name)
    }

    /// Kill processes pinned to old code, then purge the retained old version.
    pub fn force_purge_module(&self, name: Atom) -> Result<PurgeResult, PurgeError> {
        force_purge_module_shared(&self.shared, name)
    }

    /// Remove every version of a module from the registry.
    pub fn delete_module(&self, name: Atom) -> bool {
        self.shared.module_registry.delete_module(name)
    }

    /// Return true when an old module version is retained.
    pub fn check_old_code(&self, name: Atom) -> bool {
        self.shared.module_registry.has_old_code(name)
    }

    /// Return true when a process is currently running or pinned to old code.
    pub fn check_process_code(&self, pid: u64, name: Atom) -> bool {
        let Some(old) = self.shared.module_registry.lookup_old(name) else {
            return false;
        };
        process_references_old_code(&self.shared, pid, &old)
    }

    /// Spawn a process at an exported module/function/arity entrypoint.
    pub fn spawn(
        &self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let entry = self
            .shared
            .module_registry
            .lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;
        Ok(self.enqueue_spawn(entry.module, instruction_pointer, args))
    }

    /// Spawn a process at the beginning of a module.
    pub fn spawn_process(&self, module: &Arc<Module>) -> u64 {
        self.enqueue_spawn(Arc::clone(module), 0, Vec::new())
    }

    /// Spawn a process with trap-exit set before it is made runnable.
    pub fn spawn_trap_exit(
        &self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let entry = self
            .shared
            .module_registry
            .lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;
        Ok(self.enqueue_spawn_with_trap_exit(entry.module, instruction_pointer, args, true))
    }

    /// Set a live process's trap-exit flag, returning the previous value.
    pub fn set_trap_exit(
        &self,
        pid: u64,
        value: bool,
    ) -> Result<bool, crate::native::links::LinkError> {
        let facility = supervision_integration::SchedulerLinkFacility {
            shared: Arc::clone(&self.shared),
        };
        crate::native::LinkFacility::set_trap_exit(&facility, pid, value)
    }

    /// Return true when a live process currently traps exits.
    #[must_use]
    pub fn trap_exit(&self, pid: u64) -> Option<bool> {
        self.with_process(pid, |process| process.trap_exit())
    }

    /// Return true when `left` and `right` have a bidirectional live-process link.
    #[must_use]
    pub fn is_linked(&self, left: u64, right: u64) -> bool {
        self.with_process(left, |process| process.links().contains(&right))
            .unwrap_or(false)
            && self
                .with_process(right, |process| process.links().contains(&left))
                .unwrap_or(false)
    }

    /// Spawn a process and link it to `parent_pid`.
    pub fn spawn_link(
        &self,
        parent_pid: u64,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let facility = supervision_integration::SchedulerSpawnFacility {
            shared: Arc::clone(&self.shared),
        };
        crate::native::SpawnFacility::spawn(
            &facility,
            entry_module,
            entry_function,
            args,
            Some(parent_pid),
        )
        .map_err(|_| ExecError::Badarg)
    }

    /// Spawn a linked process eligible for the dirty scheduler pool.
    ///
    /// The dirty pool integration is scaffolded; this uses normal linked-spawn
    /// until the pool is wired in.
    pub fn spawn_link_dirty(
        &self,
        parent_pid: u64,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        self.spawn_link(parent_pid, entry_module, entry_function, args)
    }

    /// Enqueue an immediate atom message into a live process mailbox.
    #[must_use]
    pub fn enqueue_atom_message(&self, target_pid: u64, atom: Atom) -> bool {
        let delivered = self
            .with_process(target_pid, |process| {
                process.mailbox_mut().push_owned(Term::atom(atom));
                true
            })
            .unwrap_or(false);
        if delivered {
            self.wake_process(target_pid);
        }
        delivered
    }

    fn enqueue_spawn(
        &self,
        module_version: Arc<Module>,
        instruction_pointer: usize,
        args: Vec<Term>,
    ) -> u64 {
        self.enqueue_spawn_with_trap_exit(module_version, instruction_pointer, args, false)
    }

    fn enqueue_spawn_with_trap_exit(
        &self,
        module_version: Arc<Module>,
        instruction_pointer: usize,
        args: Vec<Term>,
        trap_exit: bool,
    ) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let index =
            self.shared.spawn_counter.fetch_add(1, Ordering::Relaxed) % self.shared.thread_count;
        if trap_exit {
            let mut process = build_process(SpawnRequest {
                pid,
                module: module_version.name,
                module_version,
                instruction_pointer,
                args,
            });
            process.set_trap_exit(true);
            self.shared
                .process_bodies
                .insert(pid, Mutex::new(Some(ScheduledProcess(process))));
            let mut wait_set = lock_or_recover(&self.shared.wait_set);
            wait_set.woken.push((pid, index));
            self.shared.wake_condvar.notify_all();
            return pid;
        }
        self.inject_queues[index].push(SpawnRequest {
            pid,
            module: module_version.name,
            module_version,
            instruction_pointer,
            args,
        });
        self.shared.wake_condvar.notify_all();
        pid
    }

    fn with_process<T>(&self, pid: u64, f: impl FnOnce(&mut Process) -> T) -> Option<T> {
        let entry = self.shared.process_bodies.get(&pid)?;
        let mut slot = lock_or_recover(&entry);
        let scheduled = slot.as_mut()?;
        Some(f(&mut scheduled.0))
    }

    /// Spawn an inert process without module code for host-side policy tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn spawn_test_process(&self, trap_exit: bool) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let mut process = Process::new(pid, DEFAULT_HEAP_SIZE);
        process.set_trap_exit(trap_exit);
        self.shared
            .process_bodies
            .insert(pid, Mutex::new(Some(ScheduledProcess(process))));
        pid
    }

    /// Spawn an inert process linked to a live parent for host-side policy tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn spawn_linked_test_process(
        &self,
        parent_pid: u64,
    ) -> Result<u64, crate::native::links::LinkError> {
        let Some(parent_entry) = self.shared.process_bodies.get(&parent_pid) else {
            return Err(crate::native::links::LinkError::NoProc);
        };
        let mut parent_slot = lock_or_recover(&parent_entry);
        let Some(ScheduledProcess(parent)) = parent_slot.as_mut() else {
            return Err(crate::native::links::LinkError::NoProc);
        };
        let child_pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(child_pid);
        let mut child = Process::new(child_pid, DEFAULT_HEAP_SIZE);
        child.add_link(parent_pid);
        parent.add_link(child_pid);
        self.shared
            .process_bodies
            .insert(child_pid, Mutex::new(Some(ScheduledProcess(child))));
        Ok(child_pid)
    }

    /// Return true when a trapped EXIT message from `source_pid` is queued.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn has_trapped_exit_message(&self, target_pid: u64, source_pid: u64) -> Option<bool> {
        self.with_process(target_pid, |process| {
            process.mailbox_mut().drain_arrival();
            process.mailbox().scan_iter().any(|message| {
                let Some(tuple) = crate::term::boxed::Tuple::new(*message) else {
                    return false;
                };
                tuple.arity() == 3
                    && tuple.get(0) == Some(Term::atom(Atom::EXIT))
                    && tuple.get(1) == Some(Term::pid(source_pid))
            })
        })
    }

    /// Return a callback suitable for mailbox senders to wake `pid`.
    pub fn wake_notifier(&self, pid: u64) -> impl Fn() + Send + Sync + 'static {
        let shared = Arc::clone(&self.shared);
        move || wake_process(&shared, pid)
    }

    /// Wake a process that is in the Waiting state after message arrival.
    pub fn wake_process(&self, pid: u64) {
        wake_process(&self.shared, pid);
    }

    /// Resume a suspended process, returning true if the process was found in
    /// the wait set and re-enqueued.
    pub fn resume_process(&self, pid: u64) -> bool {
        timer_integration::resume_suspended(&self.shared, pid)
    }

    /// Shut down all worker threads after their current time slice.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_condvar.notify_all();
        let mut threads = lock_or_recover(&self.threads);
        for handle in threads.drain(..) {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Block until the given process exits, returning its exit reason and
    /// the value in x(0) at the time of exit.
    pub fn run_until_exit(&self, pid: u64) -> (ExitReason, Term) {
        loop {
            if let Some(entry) = self.shared.exit_tombstones.get(&pid) {
                let reason = *entry;
                let result = self
                    .shared
                    .exit_results
                    .remove(&pid)
                    .map(|(_, term)| term)
                    .unwrap_or(Term::NIL);
                return (reason, result);
            }
            let guard = lock_or_recover(&self.shared.wait_set);
            let timeout = std::time::Duration::from_millis(10);
            let _ = self.shared.wake_condvar.wait_timeout(guard, timeout);
        }
    }

    /// Wake a suspended process with a result term.
    ///
    /// The process will resume execution with `result` in x(0) and the
    /// instruction pointer advanced past the suspending CallExt. This does
    /// NOT replay the suspending instruction.
    pub fn wake_with_result(&self, pid: u64, result: Term) {
        self.shared.async_results.insert(pid, result);
        wake_process(&self.shared, pid);
    }

    /// Access the live process table.
    #[must_use]
    pub fn process_table(&self) -> &ProcessTable {
        &self.shared.process_table
    }

    /// Terminate a process externally, writing an exit tombstone so that
    /// `run_until_exit` returns with the given reason.
    ///
    /// This is the host-side kill mechanism for timeout and cancellation.
    /// If the process has already exited, this is a no-op.
    pub fn terminate_process(&self, pid: u64, reason: ExitReason) {
        if self.shared.exit_tombstones.contains_key(&pid) {
            return;
        }
        cleanup_exited_process(&self.shared, pid, reason);
    }
    /// Number of scheduler worker threads.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.shared.thread_count
    }
    /// Names assigned to scheduler worker threads.
    #[must_use]
    pub fn worker_names(&self) -> &[String] {
        &self.worker_names
    }
    /// Access the reduction-boundary hook registration slot.
    #[must_use]
    pub fn hook(&self) -> &Hook {
        &self.shared.hook
    }
    /// Access the shared timer wheel for BIF integration.
    #[must_use]
    pub fn timers(&self) -> &Arc<Mutex<TimerWheel>> {
        &self.shared.timers
    }

    /// Configure the output sink used by `io` module BIFs.
    pub fn set_output_sink(&self, sink: Arc<dyn IoSink>) {
        *lock_or_recover(&self.shared.output_sink) = sink;
    }

    #[cfg(test)]
    fn idle_park_count(&self) -> usize {
        self.shared.idle_parks.load(Ordering::Acquire)
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(super) struct SchedulerCodeManagementFacility {
    pub(super) shared: Arc<SharedState>,
}

impl CodeManagementFacility for SchedulerCodeManagementFacility {
    fn load_module(&self, bytes: &[u8]) -> Result<HotLoadResult, LoadError> {
        hot_load_module_shared(&self.shared, bytes)
    }

    fn purge_module(&self, module: Atom) -> Result<PurgeResult, PurgeError> {
        purge_module_shared(&self.shared, module)
    }

    fn delete_module(&self, module: Atom) -> bool {
        self.shared.module_registry.delete_module(module)
    }

    fn check_old_code(&self, module: Atom) -> bool {
        self.shared.module_registry.has_old_code(module)
    }

    fn check_process_code(&self, pid: u64, module: Atom) -> bool {
        let Some(old) = self.shared.module_registry.lookup_old(module) else {
            return false;
        };
        process_references_old_code(&self.shared, pid, &old)
    }
}

fn configured_thread_count(override_count: Option<usize>) -> usize {
    override_count
        .filter(|count| *count > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        })
}

fn scheduler_loop(
    shared: &Arc<SharedState>,
    queue: &RunQueue,
    my_index: usize,
    stealers: &[Stealer<u64>],
    inject: &SegQueue<SpawnRequest>,
) {
    let mut last_victim = my_index;
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        drain_injected(shared, queue, inject);
        if my_index == 0 {
            timer_integration::tick_timers(shared);
        }
        drain_woken(shared, queue, my_index);
        let pid = match queue.pop() {
            Some(pid) => pid,
            None => {
                let (result, next_victim) =
                    steal::try_steal(queue, my_index, stealers, last_victim);
                last_victim = next_victim;
                match result {
                    steal::StealResult::Stolen { .. } => match queue.pop() {
                        Some(pid) => pid,
                        None => {
                            park_thread(shared);
                            continue;
                        }
                    },
                    steal::StealResult::Empty => {
                        park_thread(shared);
                        continue;
                    }
                }
            }
        };
        run_process(shared, queue, pid, my_index);
    }
}

fn drain_injected(shared: &SharedState, queue: &RunQueue, inject: &SegQueue<SpawnRequest>) {
    while let Some(request) = inject.pop() {
        let pid = request.pid;
        let process = build_process(request);
        shared
            .process_bodies
            .insert(pid, Mutex::new(Some(ScheduledProcess(process))));
        queue.push(pid);
    }
}

fn build_process(request: SpawnRequest) -> Process {
    let mut process = Process::new(request.pid, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: request.module,
        instruction_pointer: request.instruction_pointer,
    }));
    process.set_current_module(request.module_version);
    for (index, arg) in request.args.into_iter().enumerate().take(256) {
        if let Ok(register) = u8::try_from(index) {
            process.set_x_reg(register, arg);
        }
    }
    process
}

enum SliceOutcome {
    Requeue(Process),
    Wait(Process),
    Suspended(Process),
    Exited(ExitReason, Term),
}

fn run_process(shared: &Arc<SharedState>, queue: &RunQueue, pid: u64, my_index: usize) {
    if shared.process_table.get(pid).is_none() {
        return;
    }
    let Some(mut process) = take_runnable_process(shared, pid) else {
        return;
    };
    let outcome = execute_slice(shared, &mut process);
    if shared.exit_tombstones.contains_key(&pid) {
        return;
    }
    match outcome {
        SliceOutcome::Requeue(process) => {
            store_runnable_process(shared, process);
            queue.push(pid);
        }
        SliceOutcome::Wait(mut process) => {
            timer_integration::register_receive_timer(shared, &mut process);
            store_runnable_process(shared, process);
            let mut ws = lock_or_recover(&shared.wait_set);
            ws.waiting.insert(pid, my_index);
        }
        SliceOutcome::Suspended(process) => {
            store_runnable_process(shared, process);
            let mut ws = lock_or_recover(&shared.wait_set);
            ws.waiting.insert(pid, my_index);
        }
        SliceOutcome::Exited(reason, result) => {
            shared.exit_results.insert(pid, result);
            cleanup_exited_process(shared, pid, reason);
        }
    }
}

fn take_runnable_process(shared: &SharedState, pid: u64) -> Option<Process> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    slot.take().map(|scheduled| scheduled.0)
}

fn store_runnable_process(shared: &SharedState, process: Process) {
    let pid = process.pid();
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        *slot = Some(ScheduledProcess(process));
    } else {
        shared
            .process_bodies
            .insert(pid, Mutex::new(Some(ScheduledProcess(process))));
    }
}

fn execute_slice(shared: &Arc<SharedState>, process: &mut Process) -> SliceOutcome {
    if !matches!(
        process.status(),
        ProcessStatus::New
            | ProcessStatus::Yielded
            | ProcessStatus::Waiting
            | ProcessStatus::Suspended
    ) {
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    if process.transition_to(ProcessStatus::Running).is_err() {
        return SliceOutcome::Exited(exit_reason_from_status(process.status()), process.x_reg(0));
    }
    if let Some((_, result_term)) = shared.async_results.remove(&process.pid()) {
        process.set_x_reg(0, result_term);
        if let Some(pos) = process.code_position() {
            process.set_code_position(Some(CodePosition {
                module: pos.module,
                instruction_pointer: pos.instruction_pointer.saturating_add(1),
            }));
        }
    }

    process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
    let module_atom = match process.code_position() {
        Some(position) => position.module,
        None => return exit_process(process, ExitReason::Normal),
    };
    let module = if let Some(current) = process.current_module()
        && current.name == module_atom
        && current
            .code
            .get(
                process
                    .code_position()
                    .map_or(usize::MAX, |pos| pos.instruction_pointer),
            )
            .is_some()
    {
        Arc::clone(current)
    } else {
        let Some(module) = shared.module_registry.lookup(module_atom) else {
            return exit_process(process, ExitReason::Error);
        };
        process.set_current_module(Arc::clone(&module));
        module
    };
    let services = supervision_integration::build_native_services(shared);
    let result =
        interpreter::run_with_native_services(process, &module, &shared.module_registry, &services);
    let reductions = DEFAULT_REDUCTION_BUDGET.saturating_sub(process.reduction_counter());
    if matches!(
        result,
        Ok(ExecutionResult::Yielded) | Ok(ExecutionResult::Waiting)
    ) && timer_integration::invoke_hook(shared, process, reductions) == HookDecision::Suspend
    {
        let _t = process.transition_to(ProcessStatus::Suspended);
        return SliceOutcome::Suspended(take_process(process));
    }
    match result {
        Ok(ExecutionResult::Yielded) => {
            let _t = process.transition_to(ProcessStatus::Yielded);
            process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
            SliceOutcome::Requeue(take_process(process))
        }
        Ok(ExecutionResult::Waiting) => {
            let _t = process.transition_to(ProcessStatus::Waiting);
            SliceOutcome::Wait(take_process(process))
        }
        Ok(ExecutionResult::Exited(reason)) => exit_process(process, reason),
        Err(_error) => exit_process(process, ExitReason::Error),
    }
}

fn exit_process(process: &mut Process, reason: ExitReason) -> SliceOutcome {
    let result = process.x_reg(0);
    process.terminate(reason);
    SliceOutcome::Exited(reason, result)
}

fn exit_reason_from_status(status: ProcessStatus) -> ExitReason {
    match status {
        ProcessStatus::Exited(reason) => reason,
        _ => ExitReason::Error,
    }
}

fn hot_load_module_shared(
    shared: &Arc<SharedState>,
    bytes: &[u8],
) -> Result<HotLoadResult, LoadError> {
    let (staged, _report) = loader::prepare_module(
        bytes,
        &shared.atom_table,
        &shared.module_registry,
        shared.bif_registry.as_ref(),
    )?;
    let module_name = staged.name;
    if shared.module_registry.lookup_old(module_name).is_some() {
        return Err(LoadError::OldCodeStillRunning);
    }
    let had_old_version = shared.module_registry.lookup(module_name).is_some();
    let on_load_ip = find_on_load_ip(&staged);
    if let Some(ip) = on_load_ip {
        let outcome = run_on_load(shared, &staged, ip);
        if outcome != ExitReason::Normal {
            return Ok(HotLoadResult {
                module_name,
                generation: staged.generation,
                had_old_version,
                on_load_required: true,
                on_load_succeeded: false,
            });
        }
    }
    let committed = shared.module_registry.insert(staged);
    Ok(HotLoadResult {
        module_name,
        generation: committed.generation(),
        had_old_version,
        on_load_required: on_load_ip.is_some(),
        on_load_succeeded: on_load_ip.is_some(),
    })
}

fn find_on_load_ip(module: &Module) -> Option<usize> {
    module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::OnLoad))
}

fn run_on_load(shared: &Arc<SharedState>, module: &Module, ip: usize) -> ExitReason {
    let Some(entry_ip) = ip
        .checked_add(1)
        .filter(|entry_ip| *entry_ip < module.code.len())
    else {
        return ExitReason::Error;
    };
    let mut process = Process::new(u64::MAX, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));
    process.set_current_module(Arc::new(module.clone()));
    loop {
        process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
        let services = supervision_integration::build_native_services(shared);
        match interpreter::run_with_native_services(
            &mut process,
            module,
            &shared.module_registry,
            &services,
        ) {
            Ok(ExecutionResult::Exited(reason)) => return reason,
            Ok(ExecutionResult::Yielded) => continue,
            Ok(ExecutionResult::Waiting) | Err(_) => return ExitReason::Error,
        }
    }
}

fn purge_module_shared(shared: &Arc<SharedState>, name: Atom) -> Result<PurgeResult, PurgeError> {
    if let Some(old) = shared.module_registry.lookup_old(name) {
        let references = process_references_to_module(shared, &old);
        if references != 0 {
            return Err(PurgeError::StillReferenced {
                module: name,
                ref_count: references,
            });
        }
    }
    shared.module_registry.purge_old(name)?;
    Ok(PurgeResult {
        module_name: name,
        processes_killed: 0,
    })
}

fn force_purge_module_shared(
    shared: &Arc<SharedState>,
    name: Atom,
) -> Result<PurgeResult, PurgeError> {
    let old = shared
        .module_registry
        .lookup_old(name)
        .ok_or(PurgeError::NoOldVersion { module: name })?;
    let victims = old_code_pids(shared, &old);
    let processes_killed = victims.len();
    for pid in victims {
        cleanup_exited_process(shared, pid, ExitReason::Killed);
    }
    shared.module_registry.force_remove_old(name)?;
    Ok(PurgeResult {
        module_name: name,
        processes_killed,
    })
}

fn process_references_to_module(shared: &SharedState, module: &Arc<Module>) -> usize {
    old_code_pids(shared, module).len()
}

fn old_code_pids(shared: &SharedState, module: &Arc<Module>) -> Vec<u64> {
    shared
        .process_bodies
        .iter()
        .filter_map(|entry| {
            let pid = *entry.key();
            process_references_old_code(shared, pid, module).then_some(pid)
        })
        .collect()
}

fn process_references_old_code(shared: &SharedState, pid: u64, module: &Arc<Module>) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    let slot = lock_or_recover(&entry);
    slot.as_ref()
        .is_some_and(|scheduled| scheduled.0.references_module(module))
}

fn cleanup_exited_process(shared: &SharedState, pid: u64, reason: ExitReason) {
    shared.exit_tombstones.insert(pid, reason);
    supervision_integration::propagate_exit(shared, pid, reason);
    let _removed = shared.process_table.remove(pid);
    let _removed_body = shared.process_bodies.remove(&pid);
    let mut wait_set = lock_or_recover(&shared.wait_set);
    wait_set.waiting.remove(&pid);
    wait_set.woken.retain(|(woken_pid, _)| *woken_pid != pid);
}

fn take_process(process: &mut Process) -> Process {
    std::mem::replace(process, Process::new(u64::MAX, DEFAULT_HEAP_SIZE))
}

fn wake_process(shared: &SharedState, pid: u64) {
    timer_integration::cancel_receive_timer(shared, pid);
    let mut wait_set = lock_or_recover(&shared.wait_set);
    if let Some(scheduler_index) = wait_set.waiting.remove(&pid) {
        wait_set.woken.push((pid, scheduler_index));
        shared.wake_condvar.notify_all();
    }
}

fn drain_woken(shared: &SharedState, queue: &RunQueue, my_index: usize) {
    let woken = {
        let mut wait_set = lock_or_recover(&shared.wait_set);
        let mut mine = Vec::new();
        wait_set.woken.retain(|(pid, sched_idx)| {
            if *sched_idx == my_index {
                mine.push(*pid);
                false
            } else {
                true
            }
        });
        mine
    };
    for pid in woken {
        if shared.process_table.get(pid).is_some() {
            queue.push(pid);
        }
    }
}

fn park_thread(shared: &SharedState) {
    #[cfg(test)]
    shared.idle_parks.fetch_add(1, Ordering::Relaxed);
    if shared.shutdown.load(Ordering::Acquire) {
        return;
    }
    let guard = lock_or_recover(&shared.wait_set);
    let timeout = std::time::Duration::from_millis(5);
    match shared.wake_condvar.wait_timeout(guard, timeout) {
        Ok(_) => {}
        Err(error) => {
            let _recovered = error.into_inner();
        }
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod supervision_tests;
#[cfg(test)]
mod tests;
