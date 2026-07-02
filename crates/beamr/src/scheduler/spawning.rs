//! Process spawn entrypoints and spawn-request materialization.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crossbeam_queue::SegQueue;

use crate::atom::Atom;
use crate::error::ExecError;
use crate::module::Module;
use crate::namespace::NamespaceId;
use crate::native::CapabilitySet;
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, Priority, Process};
use crate::term::Term;

use super::{
    ProcessSlot, ScheduledProcess, Scheduler, SharedState, lock_or_recover, namespace_registry,
    supervision_integration,
};

pub(in crate::scheduler) struct SpawnRequest {
    pub(in crate::scheduler) pid: u64,
    pub(in crate::scheduler) module: Atom,
    pub(in crate::scheduler) module_version: Arc<Module>,
    pub(in crate::scheduler) instruction_pointer: usize,
    pub(in crate::scheduler) args: Vec<Term>,
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    pub(in crate::scheduler) parent_pid: u64,
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    pub(in crate::scheduler) function: Atom,
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    pub(in crate::scheduler) arity: u8,
    pub(in crate::scheduler) capabilities: CapabilitySet,
    pub(in crate::scheduler) namespace_id: NamespaceId,
    pub(in crate::scheduler) group_leader: Term,
    pub(in crate::scheduler) priority: Priority,
    pub(in crate::scheduler) heap_size: usize,
    #[cfg(feature = "telemetry")]
    pub(in crate::scheduler) trace_context: Option<crate::telemetry::spans::TraceCarrier>,
}

struct EnqueueSpawnRequest {
    module_version: Arc<Module>,
    instruction_pointer: usize,
    args: Vec<Term>,
    trap_exit: bool,
    namespace_id: NamespaceId,
    parent_pid: u64,
    function: Atom,
    arity: u8,
    #[cfg(feature = "telemetry")]
    trace_context: Option<crate::telemetry::spans::TraceCarrier>,
}

impl Scheduler {
    /// Spawn a process at an exported module/function/arity entrypoint.
    pub fn spawn(
        &self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        self.spawn_in(NamespaceId::DEFAULT, entry_module, entry_function, args)
    }

    /// Spawn a process at the beginning of a module.
    pub fn spawn_process(&self, module: &Arc<Module>) -> u64 {
        self.enqueue_spawn(Arc::clone(module), 0, Vec::new(), Atom::NIL, 0)
    }

    /// Spawn a process at the beginning of a module under the supplied OpenTelemetry context.
    #[cfg(feature = "telemetry")]
    pub fn spawn_process_with_trace_context(
        &self,
        module: &Arc<Module>,
        context: &opentelemetry::Context,
    ) -> u64 {
        self.enqueue_spawn_with_context(
            Arc::clone(module),
            0,
            Vec::new(),
            Atom::NIL,
            0,
            Some(crate::telemetry::spans::inject_context(context)),
        )
    }

    /// Spawn a process with trap-exit set before it is made runnable.
    pub fn spawn_trap_exit(
        &self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        self.spawn_in_trap_exit(NamespaceId::DEFAULT, entry_module, entry_function, args)
    }

    /// Spawn a process in a namespace at an exported module/function/arity entrypoint.
    pub fn spawn_in(
        &self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let registry = namespace_registry(&self.shared, namespace).ok_or(ExecError::Undef {
            module: entry_module,
            function: entry_function,
            arity,
        })?;
        let entry = registry.lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;
        Ok(self.enqueue_spawn_with_trap_exit(EnqueueSpawnRequest {
            module_version: entry.module,
            instruction_pointer,
            args,
            trap_exit: false,
            namespace_id: namespace,
            parent_pid: 0,
            function: entry_function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        }))
    }

    /// Spawn a process in a namespace under the supplied OpenTelemetry context.
    #[cfg(feature = "telemetry")]
    pub fn spawn_in_with_trace_context(
        &self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
        context: &opentelemetry::Context,
    ) -> Result<u64, ExecError> {
        self.spawn_in_with_optional_trace_context(
            namespace,
            entry_module,
            entry_function,
            args,
            Some(crate::telemetry::spans::inject_context(context)),
        )
    }

    #[cfg(feature = "telemetry")]
    fn spawn_in_with_optional_trace_context(
        &self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
        trace_context: Option<crate::telemetry::spans::TraceCarrier>,
    ) -> Result<u64, ExecError> {
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let registry = namespace_registry(&self.shared, namespace).ok_or(ExecError::Undef {
            module: entry_module,
            function: entry_function,
            arity,
        })?;
        let entry = registry.lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;
        Ok(self.enqueue_spawn_with_trap_exit(EnqueueSpawnRequest {
            module_version: entry.module,
            instruction_pointer,
            args,
            trap_exit: false,
            namespace_id: namespace,
            parent_pid: 0,
            function: entry_function,
            arity,
            trace_context,
        }))
    }

    /// Spawn a process in a namespace with trap-exit set before it is made runnable.
    pub fn spawn_in_trap_exit(
        &self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let registry = namespace_registry(&self.shared, namespace).ok_or(ExecError::Undef {
            module: entry_module,
            function: entry_function,
            arity,
        })?;
        let entry = registry.lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;
        Ok(self.enqueue_spawn_with_trap_exit(EnqueueSpawnRequest {
            module_version: entry.module,
            instruction_pointer,
            args,
            trap_exit: true,
            namespace_id: namespace,
            parent_pid: 0,
            function: entry_function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        }))
    }

    /// Spawn a native process whose body is the handler produced by `factory`.
    ///
    /// The handler runs as a first-class, scheduler-supervised process (real
    /// pid, mailbox, send/receive) through the same machinery as a bytecode
    /// process — it is dispatched to `run_native_slice` instead of the
    /// interpreter. Returns the new pid.
    pub fn spawn_native(
        &self,
        factory: crate::native::native_process::NativeHandlerFactory,
    ) -> Result<u64, ExecError> {
        let facility = supervision_integration::SchedulerSpawnFacility {
            shared: Arc::clone(&self.shared),
            namespace_id: NamespaceId::DEFAULT,
        };
        crate::native::SpawnFacility::spawn_native(&facility, 0, factory, None)
            .map_err(|_| ExecError::Badarg)
    }

    /// Spawn a native process linked to `parent_pid`.
    pub fn spawn_native_link(
        &self,
        parent_pid: u64,
        factory: crate::native::native_process::NativeHandlerFactory,
    ) -> Result<u64, ExecError> {
        let parent_namespace = self
            .process_namespace(parent_pid)
            .unwrap_or(NamespaceId::DEFAULT);
        let facility = supervision_integration::SchedulerSpawnFacility {
            shared: Arc::clone(&self.shared),
            namespace_id: parent_namespace,
        };
        crate::native::SpawnFacility::spawn_native(&facility, parent_pid, factory, Some(parent_pid))
            .map_err(|_| ExecError::Badarg)
    }

    /// Spawn a process and link it to `parent_pid`.
    pub fn spawn_link(
        &self,
        parent_pid: u64,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        let parent_namespace = self
            .process_namespace(parent_pid)
            .ok_or(ExecError::Badarg)?;
        let facility = supervision_integration::SchedulerSpawnFacility {
            shared: Arc::clone(&self.shared),
            namespace_id: parent_namespace,
        };
        crate::native::SpawnFacility::spawn(
            &facility,
            parent_pid,
            entry_module,
            entry_function,
            args,
            Some(parent_pid),
        )
        .map_err(|_| ExecError::Badarg)
    }

    /// Spawn a linked child process that runs a zero-arity closure (thunk).
    ///
    /// The closure's environment (its free variables) is DEEP-COPIED into the
    /// child's own heap through the mailbox copy machinery before the child
    /// becomes runnable, so the child never aliases the caller's heap: the
    /// caller may GC, mutate, or exit immediately after this returns. This is
    /// deliberately different from the `args: Vec<Term>` spawn entrypoints,
    /// whose argument terms are NOT heap-copied and require the caller to keep
    /// any backing heap alive.
    ///
    /// The closure must be a zero-arity fun. Its target resolves through the
    /// parent's namespace registry exactly as `call_fun` resolves it
    /// (generation match with unique-id validation, unique-id fallback across
    /// generations, old-generation fallback); export funs (`fun m:f/0`)
    /// resolve through the module export table. Funs whose target is a native
    /// (BIF/NIF) entry are not spawnable — they have no bytecode entry IP.
    ///
    /// The child is linked to `parent_pid` atomically at spawn (no unlinked
    /// window) and does not trap exits: an abnormal parent exit kills the
    /// child, and an abnormal child exit signals the parent.
    ///
    /// # Errors
    ///
    /// - [`ExecError::Badarg`] when `parent_pid` is not live.
    /// - [`ExecError::Badfun`] when the term is not a closure or its module
    ///   cannot be resolved.
    /// - [`ExecError::Badarity`] when the closure's arity is not zero.
    /// - [`ExecError::HeapFull`] when the environment exceeds the child-heap
    ///   doubling cap.
    pub fn spawn_link_closure(
        &self,
        parent_pid: u64,
        closure_term: Term,
    ) -> Result<u64, ExecError> {
        let parent_namespace = self
            .process_namespace(parent_pid)
            .ok_or(ExecError::Badarg)?;
        let facility = supervision_integration::SchedulerSpawnFacility {
            shared: Arc::clone(&self.shared),
            namespace_id: parent_namespace,
        };
        facility.spawn_closure_linked(parent_pid, closure_term)
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

    fn enqueue_spawn(
        &self,
        module_version: Arc<Module>,
        instruction_pointer: usize,
        args: Vec<Term>,
        function: Atom,
        arity: u8,
    ) -> u64 {
        self.enqueue_spawn_with_trap_exit(EnqueueSpawnRequest {
            module_version,
            instruction_pointer,
            args,
            trap_exit: false,
            namespace_id: NamespaceId::DEFAULT,
            parent_pid: 0,
            function,
            arity,
            #[cfg(feature = "telemetry")]
            trace_context: None,
        })
    }

    #[cfg(feature = "telemetry")]
    fn enqueue_spawn_with_context(
        &self,
        module_version: Arc<Module>,
        instruction_pointer: usize,
        args: Vec<Term>,
        function: Atom,
        arity: u8,
        trace_context: Option<crate::telemetry::spans::TraceCarrier>,
    ) -> u64 {
        self.enqueue_spawn_with_trap_exit(EnqueueSpawnRequest {
            module_version,
            instruction_pointer,
            args,
            trap_exit: false,
            namespace_id: NamespaceId::DEFAULT,
            parent_pid: 0,
            function,
            arity,
            trace_context,
        })
    }

    fn enqueue_spawn_with_trap_exit(&self, enqueue: EnqueueSpawnRequest) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let index =
            self.shared.spawn_counter.fetch_add(1, Ordering::Relaxed) % self.shared.thread_count;
        let module = enqueue.module_version.name;
        let parent_pid = enqueue.parent_pid;
        let function = enqueue.function;
        let arity = enqueue.arity;
        let request = SpawnRequest {
            pid,
            module,
            module_version: enqueue.module_version,
            instruction_pointer: enqueue.instruction_pointer,
            capabilities: CapabilitySet::all(),
            namespace_id: enqueue.namespace_id,
            group_leader: Term::pid(pid),
            priority: Priority::Normal,
            heap_size: DEFAULT_HEAP_SIZE,
            parent_pid,
            function,
            arity,
            args: enqueue.args,
            #[cfg(feature = "telemetry")]
            trace_context: enqueue.trace_context,
        };
        if enqueue.trap_exit {
            let mut process = build_process(request);
            process.set_trap_exit(true);
            self.shared.process_bodies.insert(
                pid,
                Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
            );
            #[cfg(feature = "telemetry")]
            self.shared
                .record_scheduler_executing(std::time::Duration::ZERO);
            let mut wait_set = lock_or_recover(&self.shared.wait_set);
            wait_set.woken.push((pid, index));
            self.shared.wake_condvar.notify_all();
            #[cfg(feature = "telemetry")]
            crate::telemetry::lifecycle::record_process_spawned(
                &self.shared.atom_table,
                pid,
                parent_pid,
                module,
                function,
                arity,
            );
        } else {
            self.inject_queues[index].push(request);
            self.shared.wake_condvar.notify_all();
        }
        pid
    }
}

pub(in crate::scheduler) fn drain_pending_spawns(
    shared: &SharedState,
    inject_queues: &[Arc<SegQueue<SpawnRequest>>],
) {
    let mut woken = Vec::new();
    for (index, inject) in inject_queues.iter().enumerate() {
        while let Some(request) = inject.pop() {
            let pid = materialize_spawn_request(shared, request);
            woken.push((pid, index));
        }
    }
    if !woken.is_empty() {
        let mut wait_set = lock_or_recover(&shared.wait_set);
        wait_set.woken.extend(woken);
        shared.wake_condvar.notify_all();
    }
}

pub(super) fn materialize_spawn_request(shared: &SharedState, request: SpawnRequest) -> u64 {
    let pid = request.pid;
    #[cfg(feature = "telemetry")]
    let parent_pid = request.parent_pid;
    #[cfg(feature = "telemetry")]
    let module = request.module;
    #[cfg(feature = "telemetry")]
    let function = request.function;
    #[cfg(feature = "telemetry")]
    let arity = request.arity;
    let process = build_process(request);
    shared.process_bodies.insert(
        pid,
        Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
    );
    #[cfg(feature = "telemetry")]
    crate::telemetry::lifecycle::record_process_spawned(
        &shared.atom_table,
        pid,
        parent_pid,
        module,
        function,
        arity,
    );
    #[cfg(feature = "telemetry")]
    shared.record_scheduler_executing(std::time::Duration::ZERO);
    pid
}

pub(in crate::scheduler) fn build_process(request: SpawnRequest) -> Process {
    let mut process =
        Process::with_capabilities(request.pid, request.heap_size, request.capabilities);
    process.set_group_leader(request.group_leader);
    process.set_priority(request.priority);
    process.set_namespace_id(request.namespace_id);
    process.set_code_position(Some(CodePosition {
        module: request.module,
        instruction_pointer: request.instruction_pointer,
    }));
    process.set_current_module(request.module_version);
    for (index, arg) in request.args.into_iter().enumerate().take(1024) {
        if let Ok(register) = u16::try_from(index) {
            process.set_x_reg(register, arg);
        }
    }
    #[cfg(feature = "telemetry")]
    if let Some(carrier) = request.trace_context.as_ref() {
        let parent = crate::telemetry::spans::extract_context(carrier);
        let trace_context =
            crate::telemetry::spans::start_process_trace_context(&parent, request.pid);
        process.set_trace_context(Some(trace_context));
    }
    process
}
