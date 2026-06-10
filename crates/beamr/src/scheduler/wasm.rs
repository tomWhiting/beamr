//! Cooperative single-threaded scheduler for `wasm32-unknown-unknown` hosts.
//!
//! The host owns the event loop and repeatedly calls [`WasmScheduler::run_until_idle`]
//! from `requestAnimationFrame`, a microtask, or an equivalent callback. No OS
//! threads, blocking I/O, dirty pools, or distribution services are started here.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::ets::copy::OwnedTerm;
use crate::interpreter::{ExecutionResult, NativeServices, run_with_native_services};
use crate::module::ModuleRegistry;
use crate::namespace::NamespaceId;
use crate::native::{BifRegistryImpl, CapabilitySet, WasmAsyncNifFacility};
use crate::process::heap::DEFAULT_HEAP_SIZE;
use crate::process::{CodePosition, ExitReason, Priority, Process, ProcessStatus};
use crate::term::Term;

/// Receive timer scheduled by the WASM scheduler and awaiting a host timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WasmScheduledTimer {
    /// Process that should be resumed when the timer expires.
    pub pid: u64,
    /// Opaque timer id stored on the process and mirrored by the host.
    pub timer_id: u64,
    /// Delay requested by the `receive after` instruction.
    pub milliseconds: u64,
}

/// Outcome returned when host async work completes.
#[derive(Debug)]
pub enum WasmAsyncCompletion {
    /// Promise fulfilled; inject the value into x(0) and advance past the NIF.
    Ok(OwnedTerm),
    /// Promise rejected; terminate the process with a NIF error mapping.
    Error(OwnedTerm),
}

/// Summary returned from one cooperative scheduler turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WasmRunSummary {
    /// Number of processes that received one reduction-bounded slice.
    pub executed: usize,
    /// PIDs that yielded and were requeued for a later host tick.
    pub yielded: Vec<u64>,
    /// PIDs that blocked waiting for a message or explicit wake.
    pub waiting: Vec<u64>,
    /// PIDs that exited during this turn.
    pub exited: Vec<u64>,
    /// PIDs that faulted with an interpreter error during this turn.
    pub errored: Vec<u64>,
}

/// Single-threaded cooperative scheduler for WASM.
pub struct WasmScheduler {
    atom_table: Arc<AtomTable>,
    module_registry: Arc<ModuleRegistry>,
    bif_registry: Arc<BifRegistryImpl>,
    next_pid: u64,
    pub(super) processes: BTreeMap<u64, Process>,
    pub(super) ready: ReadyQueues,
    pub(super) waiting: BTreeSet<u64>,
    exit_reasons: BTreeMap<u64, ExitReason>,
    exit_results: BTreeMap<u64, OwnedTerm>,
    exit_errors: BTreeMap<u64, ExecError>,
    next_timer_id: u64,
    pending_timer_schedules: Vec<WasmScheduledTimer>,
    pending_timer_cancellations: Vec<u64>,
    pub(super) async_results: BTreeMap<u64, WasmAsyncCompletion>,
    wasm_async_nif_facility: Option<Rc<dyn WasmAsyncNifFacility>>,
}

impl WasmScheduler {
    /// Create a scheduler around the VM-global registries used by module loading
    /// and native import resolution.
    #[must_use]
    pub fn new(
        atom_table: Arc<AtomTable>,
        module_registry: Arc<ModuleRegistry>,
        bif_registry: Arc<BifRegistryImpl>,
    ) -> Self {
        Self {
            atom_table,
            module_registry,
            bif_registry,
            next_pid: 1,
            processes: BTreeMap::new(),
            ready: ReadyQueues::default(),
            waiting: BTreeSet::new(),
            exit_reasons: BTreeMap::new(),
            exit_results: BTreeMap::new(),
            exit_errors: BTreeMap::new(),
            next_timer_id: 1,
            pending_timer_schedules: Vec::new(),
            pending_timer_cancellations: Vec::new(),
            async_results: BTreeMap::new(),
            wasm_async_nif_facility: None,
        }
    }

    /// Access the atom table backing this scheduler.
    #[must_use]
    pub fn atom_table(&self) -> &Arc<AtomTable> {
        &self.atom_table
    }

    /// Access the module registry backing this scheduler.
    #[must_use]
    pub fn module_registry(&self) -> &Arc<ModuleRegistry> {
        &self.module_registry
    }

    /// Access the BIF registry backing this scheduler.
    #[must_use]
    pub fn bif_registry(&self) -> &Arc<BifRegistryImpl> {
        &self.bif_registry
    }

    /// Install the single-threaded host bridge used by WASM async NIF stubs.
    pub fn set_wasm_async_nif_facility(&mut self, facility: Option<Rc<dyn WasmAsyncNifFacility>>) {
        self.wasm_async_nif_facility = facility;
    }

    /// Drain receive timers that the host must schedule with `setTimeout`.
    pub fn take_pending_timer_schedules(&mut self) -> Vec<WasmScheduledTimer> {
        std::mem::take(&mut self.pending_timer_schedules)
    }

    /// Drain receive timers that the host must cancel with `clearTimeout`.
    pub fn take_pending_timer_cancellations(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_timer_cancellations)
    }

    /// Expire a host timer callback if it still matches the waiting process.
    pub fn timer_fired(&mut self, pid: u64, timer_id: u64) -> bool {
        let Some(process) = self.processes.get_mut(&pid) else {
            return false;
        };
        if process.receive_timer_ref() != Some(timer_id) {
            return false;
        }
        process.set_receive_timer_ref(None);
        if let Some(position) = process
            .receive_timeout()
            .map(|timeout| timeout.timeout_position)
        {
            process.set_code_position(Some(position));
        }
        self.wake(pid)
    }

    /// Record an async NIF completion and wake the suspended process.
    pub fn complete_async(&mut self, pid: u64, completion: WasmAsyncCompletion) -> bool {
        self.async_results.insert(pid, completion);
        self.wake(pid)
    }

    /// Spawn a process at an exported module/function/arity entrypoint.
    pub fn spawn(
        &mut self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        self.spawn_in(NamespaceId::DEFAULT, entry_module, entry_function, args)
    }

    /// Spawn a process with arguments held in owned detached storage.
    pub fn spawn_owned(
        &mut self,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<OwnedTerm>,
    ) -> Result<u64, ExecError> {
        self.spawn_in_owned(NamespaceId::DEFAULT, entry_module, entry_function, args)
    }

    /// Spawn a process in a namespace. WASM is single-node and currently only
    /// supports the default namespace.
    pub fn spawn_in(
        &mut self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<Term>,
    ) -> Result<u64, ExecError> {
        if namespace != NamespaceId::DEFAULT {
            return Err(ExecError::Badarg);
        }
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let entry = self
            .module_registry
            .lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;

        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);

        let mut process = Process::with_capabilities(pid, DEFAULT_HEAP_SIZE, CapabilitySet::all());
        process.set_group_leader(Term::pid(pid));
        process.set_priority(Priority::Normal);
        process.set_namespace_id(namespace);
        process.set_code_position(Some(CodePosition {
            module: entry_module,
            instruction_pointer,
        }));
        process.set_current_module(entry.module);
        for (index, arg) in args.into_iter().enumerate().take(1024) {
            if let Ok(register) = u16::try_from(index) {
                process.set_x_reg(register, arg);
            }
        }
        self.ready.push(pid, process.priority());
        self.processes.insert(pid, process);
        Ok(pid)
    }

    /// Spawn a process in a namespace with arguments copied from owned storage
    /// into the new process heap.
    pub fn spawn_in_owned(
        &mut self,
        namespace: NamespaceId,
        entry_module: Atom,
        entry_function: Atom,
        args: Vec<OwnedTerm>,
    ) -> Result<u64, ExecError> {
        if namespace != NamespaceId::DEFAULT {
            return Err(ExecError::Badarg);
        }
        let arity = u8::try_from(args.len()).map_err(|_| ExecError::Badarg)?;
        let entry = self
            .module_registry
            .lookup_mfa(entry_module, entry_function, arity)?;
        let instruction_pointer = entry.module.label_ip(entry.label)?;

        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);

        let mut process = Process::with_capabilities(pid, DEFAULT_HEAP_SIZE, CapabilitySet::all());
        process.set_group_leader(Term::pid(pid));
        process.set_priority(Priority::Normal);
        process.set_namespace_id(namespace);
        process.set_code_position(Some(CodePosition {
            module: entry_module,
            instruction_pointer,
        }));
        process.set_current_module(entry.module);
        for (index, arg) in args.iter().enumerate().take(1024) {
            if let Ok(register) = u16::try_from(index) {
                let copied = arg
                    .copy_to_heap(process.heap_mut())
                    .map_err(|_| ExecError::Badarg)?;
                process.set_x_reg(register, copied);
            }
        }
        self.ready.push(pid, process.priority());
        self.processes.insert(pid, process);
        Ok(pid)
    }

    /// Wake a previously waiting process so it can be run by a later host tick.
    pub fn wake(&mut self, pid: u64) -> bool {
        if !self.waiting.remove(&pid) {
            return false;
        }
        let Some(process) = self.processes.get_mut(&pid) else {
            return false;
        };
        if process.transition_to(ProcessStatus::Running).is_err() {
            return false;
        }
        self.ready.push(pid, process.priority());
        true
    }

    /// Deliver a message to a local process and wake it if it was blocked.
    pub fn send(&mut self, pid: u64, message: Term) -> bool {
        let Some(process) = self.processes.get_mut(&pid) else {
            return false;
        };
        process.mailbox_mut().push_owned(message);
        if let Some(timer_id) = process.receive_timer_ref() {
            process.set_receive_timer_ref(None);
            self.pending_timer_cancellations.push(timer_id);
        }
        if self.waiting.contains(&pid) {
            return self.wake(pid);
        }
        true
    }

    /// Execute at most one ready-queue snapshot. Processes that yield are
    /// requeued for the next host-driven turn, preserving cooperative fairness.
    pub fn run_until_idle(&mut self) -> WasmRunSummary {
        let mut summary = WasmRunSummary::default();
        let budget = self.ready.len();
        let mut yielded_next_tick = Vec::new();

        for _ in 0..budget {
            let Some(pid) = self.ready.pop() else {
                break;
            };
            if self.waiting.contains(&pid) {
                continue;
            }
            let Some(mut process) = self.processes.remove(&pid) else {
                continue;
            };
            let priority = process.priority();
            if !matches!(process.status(), ProcessStatus::Running) {
                let _transition = process.transition_to(ProcessStatus::Running);
            }
            if let Some(reason) = self.apply_async_completion(&mut process) {
                let x0 = process.x_reg(0);
                let _transition = process.transition_to(ProcessStatus::Exited(reason));
                self.exit_reasons.insert(pid, reason);
                self.exit_results
                    .insert(pid, super::exit_capture::capture_term(x0));
                summary.exited.push(pid);
                continue;
            }
            process.reset_reductions(crate::scheduler::DEFAULT_REDUCTION_BUDGET);

            let Some(module) = process.current_module().cloned() else {
                self.exit_errors
                    .insert(pid, ExecError::InvalidOperand("current module"));
                summary.errored.push(pid);
                continue;
            };

            let services = self.native_services();
            let result = run_with_native_services(
                &mut process,
                module.as_ref(),
                self.module_registry.as_ref(),
                &services,
            );
            summary.executed += 1;

            match result {
                Ok(ExecutionResult::Yielded) => {
                    let _transition = process.transition_to(ProcessStatus::Yielded);
                    self.processes.insert(pid, process);
                    yielded_next_tick.push((pid, priority));
                    summary.yielded.push(pid);
                }
                Ok(ExecutionResult::Waiting) => {
                    let _transition = process.transition_to(ProcessStatus::Waiting);
                    self.register_receive_timer(&mut process);
                    self.processes.insert(pid, process);
                    self.waiting.insert(pid);
                    summary.waiting.push(pid);
                }
                Ok(ExecutionResult::Exited(reason)) => {
                    let x0 = process.x_reg(0);
                    let _transition = process.transition_to(ProcessStatus::Exited(reason));
                    self.exit_reasons.insert(pid, reason);
                    // Deep-copy while the process heap is still alive; the
                    // process is dropped at the end of this scope.
                    self.exit_results
                        .insert(pid, super::exit_capture::capture_term(x0));
                    summary.exited.push(pid);
                }
                Ok(ExecutionResult::DirtyCall { .. }) => {
                    self.exit_errors.insert(
                        pid,
                        ExecError::UnsupportedOpcode {
                            name: "dirty native call on wasm",
                        },
                    );
                    summary.errored.push(pid);
                }
                Err(error) => {
                    self.exit_errors.insert(pid, error);
                    summary.errored.push(pid);
                }
            }
        }

        for (pid, priority) in yielded_next_tick {
            self.ready.push(pid, priority);
        }
        summary
    }

    /// Return a process exit result captured from x(0), if available.
    ///
    /// The result is an owning deep copy that outlives the exited process.
    #[must_use]
    pub fn take_exit_result(&mut self, pid: u64) -> Option<OwnedTerm> {
        self.exit_results.remove(&pid)
    }

    /// Return all currently recorded exit results without consuming them.
    ///
    /// The returned terms borrow storage owned by this scheduler; they stay
    /// valid until the corresponding entry is removed via `take_exit_result`.
    #[must_use]
    pub fn exit_results(&self) -> Vec<(u64, Term)> {
        self.exit_results
            .iter()
            .map(|(pid, owned)| (*pid, owned.root()))
            .collect()
    }

    fn native_services(&self) -> NativeServices {
        NativeServices {
            atom_table: Some(Arc::clone(&self.atom_table)),
            wasm_async_nif_facility: self.wasm_async_nif_facility.clone(),
            ..NativeServices::default()
        }
    }

    pub(super) fn register_receive_timer(&mut self, process: &mut Process) {
        let Some(timeout) = process.receive_timeout() else {
            return;
        };
        if process.receive_timer_ref().is_some() {
            return;
        }
        let timer_id = self.next_timer_id;
        self.next_timer_id = self.next_timer_id.saturating_add(1);
        process.set_receive_timer_ref(Some(timer_id));
        self.pending_timer_schedules.push(WasmScheduledTimer {
            pid: process.pid(),
            timer_id,
            milliseconds: timeout.milliseconds,
        });
    }

    pub(super) fn apply_async_completion(&mut self, process: &mut Process) -> Option<ExitReason> {
        let completion = self.async_results.remove(&process.pid())?;
        match completion {
            WasmAsyncCompletion::Ok(term) => {
                let result = term
                    .copy_to_heap(process.heap_mut())
                    .unwrap_or_else(|_| Term::atom(Atom::BADARG));
                process.set_x_reg(0, result);
                advance_past_current_instruction(process);
                None
            }
            WasmAsyncCompletion::Error(term) => {
                let result = term
                    .copy_to_heap(process.heap_mut())
                    .unwrap_or_else(|_| Term::atom(Atom::BADARG));
                process.set_x_reg(0, result);
                Some(ExitReason::Error)
            }
        }
    }
}

fn advance_past_current_instruction(process: &mut Process) {
    if let Some(position) = process.code_position() {
        process.set_code_position(Some(CodePosition {
            module: position.module,
            instruction_pointer: position.instruction_pointer.saturating_add(1),
        }));
    }
}

#[derive(Default)]
pub(super) struct ReadyQueues {
    max: VecDeque<u64>,
    high: VecDeque<u64>,
    normal: VecDeque<u64>,
    low: VecDeque<u64>,
}

impl ReadyQueues {
    fn push(&mut self, pid: u64, priority: Priority) {
        match priority {
            Priority::Max => self.max.push_back(pid),
            Priority::High => self.high.push_back(pid),
            Priority::Normal => self.normal.push_back(pid),
            Priority::Low => self.low.push_back(pid),
        }
    }

    pub(super) fn pop(&mut self) -> Option<u64> {
        self.max
            .pop_front()
            .or_else(|| self.high.pop_front())
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.low.pop_front())
    }

    fn len(&self) -> usize {
        self.max.len() + self.high.len() + self.normal.len() + self.low.len()
    }
}

#[cfg(test)]
#[path = "wasm_tests.rs"]
mod tests;
