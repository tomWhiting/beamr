//! Process spawn entrypoints and spawn-request materialization.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crossbeam_queue::SegQueue;

use crate::atom::Atom;
use crate::error::ExecError;
use crate::module::Module;
use crate::namespace::NamespaceId;
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, Process};
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
    pub(in crate::scheduler) namespace_id: NamespaceId,
    pub(in crate::scheduler) group_leader: Term,
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
        self.enqueue_spawn(Arc::clone(module), 0, Vec::new())
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
        Ok(self.enqueue_spawn_with_trap_exit(
            entry.module,
            instruction_pointer,
            args,
            false,
            namespace,
        ))
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
        Ok(self.enqueue_spawn_with_trap_exit(
            entry.module,
            instruction_pointer,
            args,
            true,
            namespace,
        ))
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
    ) -> u64 {
        self.enqueue_spawn_with_trap_exit(
            module_version,
            instruction_pointer,
            args,
            false,
            NamespaceId::DEFAULT,
        )
    }

    fn enqueue_spawn_with_trap_exit(
        &self,
        module_version: Arc<Module>,
        instruction_pointer: usize,
        args: Vec<Term>,
        trap_exit: bool,
        namespace_id: NamespaceId,
    ) -> u64 {
        let pid = self.shared.next_pid.fetch_add(1, Ordering::Relaxed);
        self.shared.process_table.spawn_with_pid(pid);
        let index =
            self.shared.spawn_counter.fetch_add(1, Ordering::Relaxed) % self.shared.thread_count;
        let request = SpawnRequest {
            pid,
            module: module_version.name,
            module_version,
            instruction_pointer,
            namespace_id,
            group_leader: Term::pid(pid),
            args,
        };
        if trap_exit {
            let mut process = build_process(request);
            process.set_trap_exit(true);
            self.shared.process_bodies.insert(
                pid,
                Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
            );
            let mut wait_set = lock_or_recover(&self.shared.wait_set);
            wait_set.woken.push((pid, index));
            self.shared.wake_condvar.notify_all();
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
    let process = build_process(request);
    shared.process_bodies.insert(
        pid,
        Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
    );
    pid
}

pub(in crate::scheduler) fn build_process(request: SpawnRequest) -> Process {
    let mut process = Process::new(request.pid, DEFAULT_HEAP_SIZE);
    process.set_group_leader(request.group_leader);
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
    process
}
