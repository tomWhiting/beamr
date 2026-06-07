//! Process — the unit of life and isolation.
//!
//! Each process owns its heap, stack, mailbox placeholder, reduction counter,
//! link/monitor sets, and status. Processes share no memory. Spawning costs
//! microseconds. A process that crashes takes only itself down — the rest of the
//! system is unaffected.
pub mod gc;
pub mod heap;
pub mod registry;
pub mod stack;

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::atom::Atom;
use crate::mailbox::Mailbox;
use crate::module::Module;
use crate::namespace::NamespaceId;
use crate::native::NativeContinuation;
use crate::process::heap::Heap;
use crate::process::stack::Stack;
use crate::term::{Term, compare};

/// Default number of reductions assigned to a fresh process time slice.
pub const DEFAULT_REDUCTION_BUDGET: u32 = 4000;

/// Per-process monitor metadata.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Monitor {
    reference: u64,
    watcher: u64,
    target: u64,
}

impl Monitor {
    /// Create monitor metadata for `watcher` observing `target`.
    #[must_use]
    pub const fn new(reference: u64, watcher: u64, target: u64) -> Self {
        Self {
            reference,
            watcher,
            target,
        }
    }

    /// Unique monitor reference id.
    #[must_use]
    pub const fn reference(self) -> u64 {
        self.reference
    }

    /// PID that owns the monitor and receives DOWN messages.
    #[must_use]
    pub const fn watcher(self) -> u64 {
        self.watcher
    }

    /// PID being observed by the monitor.
    #[must_use]
    pub const fn target(self) -> u64 {
        self.target
    }
}

/// Current code location for a process.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CodePosition {
    /// Current module.
    pub module: Atom,
    /// Current instruction pointer in `module`.
    pub instruction_pointer: usize,
}

/// A process register addressed by BEAM X/Y register operands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    /// X register index.
    X(u16),
    /// Y register index in the current stack frame.
    Y(u16),
}

/// Kind of exception handler installed on a process.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HandlerKind {
    /// BEAM `try` handler exposing class/reason/stacktrace through `try_case`.
    Try,
    /// BEAM `catch` handler wrapping the raised value in catch-compatible form.
    Catch,
}

/// A try/catch handler installed by BEAM try-family opcodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExceptionHandler {
    /// Whether this handler was installed by `try` or `catch`.
    pub kind: HandlerKind,
    /// Stack depth to restore before transferring control to this handler.
    pub stack_depth: usize,
    /// Label/IP to jump to when an exception is raised.
    pub catch_position: CodePosition,
    /// Destination register supplied by the decoded try/catch instruction.
    pub destination: Register,
}

/// Exception payload propagated through try handlers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Exception {
    /// Exception class, normally atom(error).
    pub class: Term,
    /// Exception reason term.
    pub reason: Term,
    /// Stacktrace term associated with the original raise.
    pub stacktrace: Term,
}

/// Raw stack frame captured at raise time for later stacktrace construction.
#[derive(Clone, Debug)]
pub struct RawStackEntry {
    /// Pinned module version containing the instruction pointer.
    pub module: Arc<Module>,
    /// Instruction pointer within `module`.
    pub ip: usize,
    /// Optional module/function/arity metadata from a preceding `func_info`.
    pub mfa: Option<(Atom, Atom, u8)>,
}

/// Receive timeout state recorded while a process is waiting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReceiveTimeout {
    /// Instruction pointer to resume at if the receive timeout expires.
    pub timeout_position: CodePosition,
    /// Timeout duration in milliseconds.
    pub milliseconds: u64,
}

/// Reason a process exited.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// Normal process completion.
    Normal,
    /// Untrappable kill exit.
    Kill,
    /// Terminal reason reported by a process that received `kill`.
    Killed,
    /// Placeholder error exit until error terms land.
    Error,
}

impl ExitReason {
    /// Atom representation used in EXIT and DOWN messages.
    #[must_use]
    pub const fn as_atom(self) -> Atom {
        match self {
            Self::Normal => Atom::NORMAL,
            Self::Kill => Atom::KILL,
            Self::Killed => Atom::KILLED,
            Self::Error => Atom::ERROR,
        }
    }

    /// Term representation used in EXIT and DOWN messages.
    #[must_use]
    pub const fn as_term(self) -> Term {
        Term::atom(self.as_atom())
    }
}

/// Lifecycle state for a process.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessStatus {
    /// Allocated but not yet running.
    New,
    /// Currently runnable/running.
    Running,
    /// Yielded after exhausting or giving up a scheduler time slice.
    Yielded,
    /// Waiting for a message or timeout.
    Waiting,
    /// Paused by the scheduler hook; will be requeued or waited on resume.
    Suspended,
    /// Terminal state with exit reason.
    Exited(ExitReason),
}

/// BEAM process scheduling priority.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum Priority {
    /// Low-priority process.
    Low,
    /// Normal process priority.
    #[default]
    Normal,
    /// High-priority process.
    High,
    /// Maximum process priority.
    Max,
}

/// Process operation errors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessError {
    /// The requested status transition is not allowed by the lifecycle graph.
    InvalidStatusTransition {
        /// Current status.
        from: ProcessStatus,
        /// Requested next status.
        to: ProcessStatus,
    },
    /// The requested float register index is outside BEAM's fr0-fr15 range.
    InvalidFloatRegister {
        /// Requested float register index.
        index: u16,
    },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStatusTransition { from, to } => {
                write!(
                    f,
                    "invalid process status transition from {from:?} to {to:?}"
                )
            }
            Self::InvalidFloatRegister { index } => {
                write!(f, "invalid float register index {index}")
            }
        }
    }
}

impl std::error::Error for ProcessError {}

/// One isolated BEAM-style process.
///
/// The `Rc` marker intentionally makes `Process` neither [`Send`] nor [`Sync`]:
/// ownership must remain with one scheduler thread at a time.
///
/// ```compile_fail
/// use beamr::process::Process;
///
/// let process = Process::new(0, 233);
/// std::thread::spawn(move || {
///     process.pid()
/// });
/// ```
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<beamr::process::Process>();
/// ```
#[derive(Debug)]
pub struct Process {
    pid: u64,
    status: ProcessStatus,
    priority: Priority,
    heap: Heap,
    virtual_binary_heap: usize,
    stack: Stack,
    mailbox: Mailbox,
    handlers: Vec<ExceptionHandler>,
    current_exception: Option<Exception>,
    dictionary: Vec<(Term, Term)>,
    receive_timeout: Option<ReceiveTimeout>,
    receive_timer_ref: Option<u64>,
    x_regs: [Term; 1024],
    float_regs: [f64; 16],
    native_continuation: Option<NativeContinuation>,
    raw_stacktrace: Vec<RawStackEntry>,
    reduction_counter: u32,
    namespace_id: NamespaceId,
    code_position: Option<CodePosition>,
    current_module: Option<Arc<Module>>,
    current_mfa: Option<(Atom, Atom, u8)>,
    links: Vec<u64>,
    monitors: Vec<Monitor>,
    trap_exit: bool,
    group_leader: Term,
    not_send_sync: PhantomData<Rc<()>>,
}

impl Process {
    /// Create a fresh process with `pid` and a heap capacity of `heap_size`
    /// words.
    #[must_use]
    pub fn new(pid: u64, heap_size: usize) -> Self {
        Self {
            pid,
            status: ProcessStatus::New,
            priority: Priority::Normal,
            heap: Heap::new(heap_size),
            virtual_binary_heap: 0,
            stack: Stack::new(),
            mailbox: Mailbox::new(),
            handlers: Vec::new(),
            current_exception: None,
            dictionary: Vec::new(),
            receive_timeout: None,
            receive_timer_ref: None,
            x_regs: [Term::NIL; 1024],
            float_regs: [0.0; 16],
            native_continuation: None,
            raw_stacktrace: Vec::new(),
            reduction_counter: DEFAULT_REDUCTION_BUDGET,
            namespace_id: NamespaceId::DEFAULT,
            code_position: None,
            current_module: None,
            current_mfa: None,
            links: Vec::new(),
            monitors: Vec::new(),
            trap_exit: false,
            group_leader: Self::initial_group_leader(pid),
            not_send_sync: PhantomData,
        }
    }

    /// Process identifier.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }

    const fn initial_group_leader(pid: u64) -> Term {
        match Term::try_pid(pid) {
            Some(pid_term) => pid_term,
            None => Term::NIL,
        }
    }

    /// Current lifecycle status.
    #[must_use]
    pub const fn status(&self) -> ProcessStatus {
        self.status
    }

    /// Current scheduling priority.
    #[must_use]
    pub const fn priority(&self) -> Priority {
        self.priority
    }

    /// Set this process's scheduling priority.
    pub const fn set_priority(&mut self, priority: Priority) {
        self.priority = priority;
    }

    /// Transition this process to `next` if the lifecycle graph allows it.
    pub fn transition_to(&mut self, next: ProcessStatus) -> Result<(), ProcessError> {
        if Self::can_transition(self.status, next) {
            self.status = next;
            Ok(())
        } else {
            Err(ProcessError::InvalidStatusTransition {
                from: self.status,
                to: next,
            })
        }
    }

    const fn can_transition(from: ProcessStatus, to: ProcessStatus) -> bool {
        matches!(
            (from, to),
            (ProcessStatus::New, ProcessStatus::Running)
                | (ProcessStatus::Running, ProcessStatus::Yielded)
                | (ProcessStatus::Running, ProcessStatus::Waiting)
                | (ProcessStatus::Running, ProcessStatus::Suspended)
                | (ProcessStatus::Running, ProcessStatus::Exited(_))
                | (ProcessStatus::Yielded, ProcessStatus::Running)
                | (ProcessStatus::Yielded, ProcessStatus::Suspended)
                | (ProcessStatus::Waiting, ProcessStatus::Running)
                | (ProcessStatus::Waiting, ProcessStatus::Suspended)
                | (ProcessStatus::Suspended, ProcessStatus::Yielded)
                | (ProcessStatus::Suspended, ProcessStatus::Waiting)
        )
    }

    /// Immutable access to this process heap.
    #[must_use]
    pub const fn heap(&self) -> &Heap {
        &self.heap
    }

    /// Mutable access to this process heap.
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// Bytes of off-heap binary data currently referenced by ProcBins on this heap.
    #[must_use]
    pub const fn virtual_binary_heap(&self) -> usize {
        self.virtual_binary_heap
    }

    /// Record a newly allocated heap ProcBin's off-heap byte ownership.
    pub fn increase_virtual_binary_heap(&mut self, bytes: usize) {
        self.virtual_binary_heap = self.virtual_binary_heap.saturating_add(bytes);
    }

    /// Record removal of a heap ProcBin's off-heap byte ownership.
    pub(crate) fn decrease_virtual_binary_heap(&mut self, bytes: usize) {
        self.virtual_binary_heap = self.virtual_binary_heap.saturating_sub(bytes);
    }

    /// Immutable access to this process stack.
    #[must_use]
    pub const fn stack(&self) -> &Stack {
        &self.stack
    }

    /// Mutable access to this process stack.
    pub fn stack_mut(&mut self) -> &mut Stack {
        &mut self.stack
    }

    /// Immutable placeholder mailbox access for future receive support.
    #[must_use]
    pub const fn mailbox(&self) -> &Mailbox {
        &self.mailbox
    }

    /// Mutable placeholder mailbox access for message enqueue/receive support.
    pub fn mailbox_mut(&mut self) -> &mut Mailbox {
        &mut self.mailbox
    }

    /// Store `value` under `key` in the process dictionary.
    ///
    /// Existing keys are matched with Erlang exact equality (`=:=`). Returns the
    /// previous value, or `undefined` when the key was not present.
    pub fn dict_put(&mut self, key: Term, value: Term) -> Term {
        for (existing_key, existing_value) in &mut self.dictionary {
            if compare::exact_eq(*existing_key, key) {
                let old_value = *existing_value;
                *existing_value = value;
                return old_value;
            }
        }

        self.dictionary.push((key, value));
        Term::atom(Atom::UNDEFINED)
    }

    /// Fetch a value from the process dictionary by exact-equality key match.
    #[must_use]
    pub fn dict_get(&self, key: Term) -> Term {
        self.dictionary
            .iter()
            .find_map(|(existing_key, value)| {
                compare::exact_eq(*existing_key, key).then_some(*value)
            })
            .unwrap_or_else(|| Term::atom(Atom::UNDEFINED))
    }

    /// Borrow all process dictionary entries in current vector order.
    #[must_use]
    pub fn dict_get_all(&self) -> &[(Term, Term)] {
        &self.dictionary
    }

    /// Remove a dictionary entry by exact-equality key match.
    ///
    /// Uses `swap_remove`, so entry order may change after deletion.
    pub fn dict_erase(&mut self, key: Term) -> Term {
        let Some(index) = self
            .dictionary
            .iter()
            .position(|(existing_key, _)| compare::exact_eq(*existing_key, key))
        else {
            return Term::atom(Atom::UNDEFINED);
        };

        let (_key, value) = self.dictionary.swap_remove(index);
        value
    }

    /// Remove and return all process dictionary entries.
    pub fn dict_erase_all(&mut self) -> Vec<(Term, Term)> {
        std::mem::take(&mut self.dictionary)
    }

    /// Return all keys whose values exactly match `value`.
    #[must_use]
    pub fn dict_get_keys(&self, value: Term) -> Vec<Term> {
        self.dictionary
            .iter()
            .filter_map(|(key, existing_value)| {
                compare::exact_eq(*existing_value, value).then_some(*key)
            })
            .collect()
    }

    /// Snapshot every GC root owned by this process, treating all X registers as live.
    pub(crate) fn roots(&mut self) -> Vec<Term> {
        self.roots_with_live_x(256)
    }

    /// Snapshot every live GC root owned by this process.
    pub(crate) fn roots_with_live_x(&mut self, live_x: usize) -> Vec<Term> {
        self.mailbox.drain_arrival();
        let live_x = live_x.min(self.x_regs.len());
        let exception_roots = self
            .current_exception
            .into_iter()
            .flat_map(|exception| [exception.reason, exception.stacktrace]);
        self.x_regs
            .iter()
            .take(live_x)
            .chain(self.stack.y_regs())
            .chain(self.mailbox.scan_iter())
            .copied()
            .chain(exception_roots)
            .chain(
                self.dictionary
                    .iter()
                    .flat_map(|(key, value)| [*key, *value]),
            )
            .chain(std::iter::once(self.group_leader))
            .collect()
    }

    /// Replace every GC root with the next term yielded by `roots`, in the same
    /// order as [`Process::roots`]. Extra yielded terms are ignored.
    pub(crate) fn replace_roots(&mut self, roots: &[Term]) {
        self.replace_roots_with_live_x(256, roots);
    }

    /// Replace every live GC root with the next term yielded by `roots`, in the
    /// same order as [`Process::roots_with_live_x`]. Extra yielded terms are ignored.
    pub(crate) fn replace_roots_with_live_x(&mut self, live_x: usize, roots: &[Term]) {
        let mut index = 0;
        let live_x = live_x.min(self.x_regs.len());
        for root in self.x_regs.iter_mut().take(live_x) {
            if let Some(value) = roots.get(index).copied() {
                *root = value;
            }
            index += 1;
        }
        for root in self.stack.y_regs_mut() {
            if let Some(value) = roots.get(index).copied() {
                *root = value;
            }
            index += 1;
        }
        for root in self.mailbox.scan_iter_mut() {
            if let Some(value) = roots.get(index).copied() {
                *root = value;
            }
            index += 1;
        }
        if let Some(exception) = &mut self.current_exception {
            if let Some(value) = roots.get(index).copied() {
                exception.reason = value;
            }
            index += 1;
            if let Some(value) = roots.get(index).copied() {
                exception.stacktrace = value;
            }
            index += 1;
        }
        for (key, value) in &mut self.dictionary {
            if let Some(root) = roots.get(index).copied() {
                *key = root;
            }
            index += 1;
            if let Some(root) = roots.get(index).copied() {
                *value = root;
            }
            index += 1;
        }
        if let Some(root) = roots.get(index).copied() {
            self.group_leader = root;
        }
    }

    /// Install an exception handler.
    pub fn push_exception_handler(&mut self, handler: ExceptionHandler) {
        self.handlers.push(handler);
    }

    /// Remove the most recently installed exception handler.
    pub fn pop_exception_handler(&mut self) -> Option<ExceptionHandler> {
        self.handlers.pop()
    }

    /// Number of installed exception handlers.
    #[must_use]
    pub fn exception_handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Store the raw stacktrace captured when an exception is raised.
    pub fn set_raw_stacktrace(&mut self, raw_stacktrace: Vec<RawStackEntry>) {
        self.raw_stacktrace = raw_stacktrace;
    }

    /// Clear any raw stacktrace associated with a handled exception.
    pub fn clear_raw_stacktrace(&mut self) {
        self.raw_stacktrace.clear();
    }

    /// Raw stacktrace entries captured at the most recent raise.
    #[must_use]
    pub fn raw_stacktrace(&self) -> &[RawStackEntry] {
        &self.raw_stacktrace
    }

    /// Store the current caught exception.
    pub const fn set_current_exception(&mut self, exception: Option<Exception>) {
        self.current_exception = exception;
    }

    /// Current caught exception, when present.
    #[must_use]
    pub const fn current_exception(&self) -> Option<Exception> {
        self.current_exception
    }

    /// Record receive timeout state for scheduler/timer integration.
    pub const fn set_receive_timeout(&mut self, timeout: Option<ReceiveTimeout>) {
        self.receive_timeout = timeout;
    }

    /// Receive timeout state, when waiting with a deadline.
    #[must_use]
    pub const fn receive_timeout(&self) -> Option<ReceiveTimeout> {
        self.receive_timeout
    }

    /// Store the timer reference for the active receive timeout, used by the
    /// scheduler to cancel the timer when a message arrives first.
    pub const fn set_receive_timer_ref(&mut self, timer_ref: Option<u64>) {
        self.receive_timer_ref = timer_ref;
    }

    /// Active receive timer reference, when a timeout timer is outstanding.
    #[must_use]
    pub const fn receive_timer_ref(&self) -> Option<u64> {
        self.receive_timer_ref
    }

    /// Read X register `n`.
    #[must_use]
    pub fn x_reg(&self, n: u16) -> Term {
        self.x_regs[usize::from(n)]
    }

    /// Write X register `n`.
    pub fn set_x_reg(&mut self, n: u16, value: Term) {
        self.x_regs[usize::from(n)] = value;
    }

    /// Read float register `index`.
    pub fn get_float_reg(&self, index: u16) -> Result<f64, ProcessError> {
        self.float_regs
            .get(usize::from(index))
            .copied()
            .ok_or(ProcessError::InvalidFloatRegister { index })
    }

    /// Write float register `index`.
    pub fn set_float_reg(&mut self, index: u16, value: f64) -> Result<(), ProcessError> {
        let register = self
            .float_regs
            .get_mut(usize::from(index))
            .ok_or(ProcessError::InvalidFloatRegister { index })?;
        *register = value;
        Ok(())
    }

    /// Read all X registers.
    #[must_use]
    pub const fn x_regs(&self) -> &[Term; 1024] {
        &self.x_regs
    }

    /// Mutable access to all X registers.
    pub fn x_regs_mut(&mut self) -> &mut [Term; 1024] {
        &mut self.x_regs
    }

    /// Store native continuation state for closure-return re-entry.
    pub fn set_native_continuation(&mut self, continuation: Option<NativeContinuation>) {
        self.native_continuation = continuation;
    }

    /// Take native continuation state after a closure returns.
    pub fn take_native_continuation(&mut self) -> Option<NativeContinuation> {
        self.native_continuation.take()
    }

    /// Check whether a native continuation is pending.
    #[must_use]
    pub fn has_native_continuation(&self) -> bool {
        self.native_continuation.is_some()
    }

    /// Current reduction budget remainder.
    #[must_use]
    pub const fn reduction_counter(&self) -> u32 {
        self.reduction_counter
    }

    /// Subtract reductions from the current budget, saturating at zero.
    pub fn decrement_reductions(&mut self, n: u32) {
        self.reduction_counter = self.reduction_counter.saturating_sub(n);
    }

    /// Returns true when no reductions remain in this time slice.
    #[must_use]
    pub const fn reductions_exhausted(&self) -> bool {
        self.reduction_counter == 0
    }

    /// Reset the reduction counter for a new scheduler time slice.
    pub const fn reset_reductions(&mut self, budget: u32) {
        self.reduction_counter = budget;
    }

    /// Namespace whose module registry this process executes against.
    #[must_use]
    pub const fn namespace_id(&self) -> NamespaceId {
        self.namespace_id
    }

    /// Set the namespace whose module registry this process executes against.
    pub const fn set_namespace_id(&mut self, namespace_id: NamespaceId) {
        self.namespace_id = namespace_id;
    }

    /// Current code position, if one has been assigned.
    #[must_use]
    pub const fn code_position(&self) -> Option<CodePosition> {
        self.code_position
    }

    /// Set the current code position.
    pub const fn set_code_position(&mut self, code_position: Option<CodePosition>) {
        self.code_position = code_position;
    }

    /// Current pinned module version, if one has been assigned.
    #[must_use]
    pub fn current_module(&self) -> Option<&Arc<Module>> {
        self.current_module.as_ref()
    }

    /// Returns true when the current module or any stack frame pins `module`.
    #[must_use]
    pub fn references_module(&self, module: &Arc<Module>) -> bool {
        self.current_module
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, module))
            || self
                .stack
                .pinned_modules()
                .any(|pinned| Arc::ptr_eq(pinned, module))
    }

    /// Set the currently executing module version.
    pub fn set_current_module(&mut self, module: Arc<Module>) {
        self.current_module = Some(module);
    }

    /// Clear the currently executing module version.
    pub fn clear_current_module(&mut self) {
        self.current_module = None;
    }

    /// Current module/function/arity metadata from the most recent func_info.
    #[must_use]
    pub const fn current_mfa(&self) -> Option<(Atom, Atom, u8)> {
        self.current_mfa
    }

    /// Store module/function/arity metadata for later error reporting.
    pub const fn set_current_mfa(&mut self, current_mfa: Option<(Atom, Atom, u8)>) {
        self.current_mfa = current_mfa;
    }

    /// Linked process IDs.
    #[must_use]
    pub fn links(&self) -> &[u64] {
        &self.links
    }

    /// Add a linked process id. Returns whether the ordered set changed.
    ///
    /// Link insertion order is preserved for deterministic exit propagation.
    /// Self-links and duplicate links are ignored.
    pub fn add_link(&mut self, pid: u64) -> bool {
        if pid == self.pid || self.links.contains(&pid) {
            return false;
        }
        self.links.push(pid);
        true
    }

    /// Remove a linked process id. Returns whether the ordered set changed.
    pub fn remove_link(&mut self, pid: u64) -> bool {
        let before = self.links.len();
        self.links.retain(|linked| *linked != pid);
        before != self.links.len()
    }

    /// Remove all links and return the previous link set in insertion order.
    pub fn take_links(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.links)
    }

    /// Monitor metadata attached to this process.
    #[must_use]
    pub const fn monitors(&self) -> &Vec<Monitor> {
        &self.monitors
    }

    /// Add monitor metadata owned by or targeting this process.
    pub fn add_monitor(&mut self, monitor: Monitor) {
        self.monitors.push(monitor);
    }

    /// Remove monitor metadata by reference. Returns removed metadata.
    pub fn remove_monitor(&mut self, reference: u64) -> Option<Monitor> {
        let index = self
            .monitors
            .iter()
            .position(|monitor| monitor.reference() == reference)?;
        Some(self.monitors.remove(index))
    }

    /// Whether this process traps exits.
    #[must_use]
    pub const fn trap_exit(&self) -> bool {
        self.trap_exit
    }

    /// Set whether this process traps exits.
    pub const fn set_trap_exit(&mut self, trap_exit: bool) {
        self.trap_exit = trap_exit;
    }

    /// Group leader PID term.
    #[must_use]
    pub const fn group_leader(&self) -> Term {
        self.group_leader
    }

    /// Set group leader PID term.
    pub const fn set_group_leader(&mut self, group_leader: Term) {
        self.group_leader = group_leader;
    }

    /// Mark the process exited and release owned runtime state that can keep
    /// heap terms alive after process death.
    pub fn terminate(&mut self, reason: ExitReason) {
        self.status = ProcessStatus::Exited(reason);
        crate::gc::release_all_proc_bins(self);
        self.virtual_binary_heap = 0;
        self.heap = Heap::new(1);
        self.stack = Stack::new();
        self.mailbox = Mailbox::new();
        self.handlers.clear();
        self.current_exception = None;
        self.dictionary.clear();
        self.receive_timeout = None;
        self.receive_timer_ref = None;
        self.x_regs = [Term::NIL; 1024];
        self.float_regs = [0.0; 16];
        self.reduction_counter = 0;
        self.code_position = None;
        self.current_module = None;
        self.current_mfa = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CodePosition, DEFAULT_REDUCTION_BUDGET, ExitReason, Priority, Process, ProcessError,
        ProcessStatus,
    };
    use crate::atom::Atom;
    use crate::gc::tests::{alloc_proc_bin, module_pin};
    use crate::namespace::NamespaceId;
    use crate::term::{Term, shared_binary::SharedBinary};

    #[test]
    fn fresh_process_has_expected_state() {
        let process = Process::new(7, 233);

        assert_eq!(process.pid(), 7);
        assert_eq!(process.status(), ProcessStatus::New);
        assert_eq!(process.priority(), Priority::Normal);
        assert_eq!(process.heap().capacity(), 233);
        assert!(process.stack().is_empty());
        assert!(process.mailbox().is_empty());
        assert!(process.dict_get_all().is_empty());
        assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
        assert_eq!(process.namespace_id(), NamespaceId::DEFAULT);
        assert_eq!(process.code_position(), None);
        assert!(process.current_module().is_none());
        assert!(process.links().is_empty());
        assert!(process.monitors().is_empty());
        assert!(!process.trap_exit());
        assert_eq!(process.group_leader(), Term::pid(7));
    }

    #[test]
    fn dictionary_put_get_round_trip() {
        let mut process = Process::new(7, 233);
        let key = Term::atom(Atom::OK);
        let value = Term::small_int(42);

        assert_eq!(process.dict_put(key, value), Term::atom(Atom::UNDEFINED));
        assert_eq!(process.dict_get(key), value);
        assert_eq!(process.dict_get_all(), &[(key, value)]);
    }

    #[test]
    fn dictionary_put_replaces_existing_entry_and_returns_old_value() {
        let mut process = Process::new(7, 233);
        let key = Term::atom(Atom::OK);
        let old_value = Term::small_int(1);
        let new_value = Term::small_int(2);

        assert_eq!(
            process.dict_put(key, old_value),
            Term::atom(Atom::UNDEFINED)
        );
        assert_eq!(process.dict_put(key, new_value), old_value);
        assert_eq!(process.dict_get(key), new_value);
        assert_eq!(process.dict_get_all(), &[(key, new_value)]);
    }

    #[test]
    fn dictionary_get_and_erase_missing_return_undefined() {
        let mut process = Process::new(7, 233);
        let key = Term::atom(Atom::OK);

        assert_eq!(process.dict_get(key), Term::atom(Atom::UNDEFINED));
        assert_eq!(process.dict_erase(key), Term::atom(Atom::UNDEFINED));
    }

    #[test]
    fn dictionary_erase_removes_entry_with_swap_remove() {
        let mut process = Process::new(7, 233);
        let key = Term::atom(Atom::OK);
        let value = Term::small_int(42);
        process.dict_put(key, value);

        assert_eq!(process.dict_erase(key), value);
        assert_eq!(process.dict_get(key), Term::atom(Atom::UNDEFINED));
        assert!(process.dict_get_all().is_empty());
    }

    #[test]
    fn dictionary_erase_all_drains_entries() {
        let mut process = Process::new(7, 233);
        process.dict_put(Term::atom(Atom::OK), Term::small_int(1));
        process.dict_put(Term::atom(Atom::ERROR), Term::small_int(2));

        assert_eq!(
            process.dict_erase_all(),
            vec![
                (Term::atom(Atom::OK), Term::small_int(1)),
                (Term::atom(Atom::ERROR), Term::small_int(2)),
            ]
        );
        assert!(process.dict_get_all().is_empty());
    }

    #[test]
    fn dictionary_get_keys_returns_exact_value_matches() {
        let mut process = Process::new(7, 233);
        process.dict_put(Term::atom(Atom::OK), Term::small_int(1));
        process.dict_put(Term::atom(Atom::ERROR), Term::small_int(1));
        process.dict_put(Term::atom(Atom::UNDEFINED), Term::small_int(2));

        assert_eq!(
            process.dict_get_keys(Term::small_int(1)),
            vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)]
        );
    }

    #[test]
    fn links_preserve_insertion_order_and_deduplicate() {
        let mut process = Process::new(7, 233);

        assert!(process.add_link(11));
        assert!(process.add_link(13));
        assert!(process.add_link(17));
        assert!(!process.add_link(13));
        assert!(!process.add_link(7));

        assert_eq!(process.links(), &[11, 13, 17]);
    }

    #[test]
    fn remove_link_preserves_remaining_order() {
        let mut process = Process::new(7, 233);
        process.add_link(11);
        process.add_link(13);
        process.add_link(17);
        process.add_link(19);

        assert!(process.remove_link(13));
        assert!(!process.remove_link(23));

        assert_eq!(process.links(), &[11, 17, 19]);
    }

    #[test]
    fn take_links_returns_ordered_links_and_clears_storage() {
        let mut process = Process::new(7, 233);
        process.add_link(11);
        process.add_link(13);
        process.add_link(17);

        assert_eq!(process.take_links(), vec![11, 13, 17]);
        assert!(process.links().is_empty());
    }

    #[test]
    fn terminate_clears_current_module_pin() {
        let mut process = Process::new(0, 233);
        process.set_code_position(Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        }));
        process.set_current_module(module_pin(Atom::OK));

        process.terminate(ExitReason::Normal);

        assert!(process.current_module().is_none());
        assert_eq!(process.code_position(), None);
    }

    #[test]
    fn terminate_releases_heap_proc_bins_and_resets_virtual_binary_heap() {
        let shared = SharedBinary::new(vec![0xAB; 256 * 1024]);
        let mut process = Process::new(0, 233);
        let proc_bin = alloc_proc_bin(&mut process, &shared);
        process.set_x_reg(0, proc_bin);
        assert_eq!(shared.ref_count(), 2);
        assert_eq!(process.virtual_binary_heap(), 256 * 1024);

        process.terminate(ExitReason::Normal);

        assert_eq!(shared.ref_count(), 1);
        assert_eq!(process.virtual_binary_heap(), 0);
        assert_eq!(process.heap().total_used(), 0);
        assert_eq!(process.x_reg(0), Term::NIL);
    }

    #[test]
    fn all_x_registers_start_as_nil() {
        let process = Process::new(0, 233);

        for register in u16::MIN..=u8::MAX as u16 {
            assert_eq!(process.x_reg(register), Term::NIL);
        }
    }

    #[test]
    fn x_registers_are_independently_addressable() {
        let mut process = Process::new(0, 233);

        process.set_x_reg(0, Term::small_int(10));
        process.set_x_reg(255, Term::small_int(20));

        assert_eq!(process.x_reg(0), Term::small_int(10));
        assert_eq!(process.x_reg(255), Term::small_int(20));
        assert_eq!(process.x_reg(1), Term::NIL);
    }

    #[test]
    fn float_registers_start_at_zero_and_are_independent() {
        let mut process = Process::new(0, 233);

        assert_eq!(process.get_float_reg(0), Ok(0.0));
        assert_eq!(process.get_float_reg(15), Ok(0.0));
        process.set_x_reg(0, Term::small_int(314));
        assert_eq!(process.set_float_reg(0, 3.14), Ok(()));

        assert_eq!(process.get_float_reg(0), Ok(3.14));
        assert_eq!(process.get_float_reg(1), Ok(0.0));
        assert_eq!(process.x_reg(0), Term::small_int(314));
    }

    #[test]
    fn float_register_bounds_return_errors() {
        let mut process = Process::new(0, 233);

        assert_eq!(
            process.get_float_reg(16),
            Err(ProcessError::InvalidFloatRegister { index: 16 })
        );
        assert_eq!(
            process.set_float_reg(16, 1.0),
            Err(ProcessError::InvalidFloatRegister { index: 16 })
        );
    }

    #[test]
    fn terminate_clears_float_registers() {
        let mut process = Process::new(0, 233);
        assert_eq!(process.set_float_reg(0, 3.14), Ok(()));

        process.terminate(ExitReason::Normal);

        assert_eq!(process.get_float_reg(0), Ok(0.0));
    }

    #[test]
    fn valid_status_transitions_succeed() {
        let mut process = Process::new(0, 233);

        assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
        assert_eq!(process.transition_to(ProcessStatus::Yielded), Ok(()));
        assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
        assert_eq!(process.transition_to(ProcessStatus::Waiting), Ok(()));
        assert_eq!(process.transition_to(ProcessStatus::Running), Ok(()));
        assert_eq!(
            process.transition_to(ProcessStatus::Exited(ExitReason::Normal)),
            Ok(())
        );
    }

    #[test]
    fn new_to_exited_transition_fails() {
        let mut process = Process::new(0, 233);

        assert_eq!(
            process.transition_to(ProcessStatus::Exited(ExitReason::Error)),
            Err(ProcessError::InvalidStatusTransition {
                from: ProcessStatus::New,
                to: ProcessStatus::Exited(ExitReason::Error),
            })
        );
        assert_eq!(process.status(), ProcessStatus::New);
    }

    #[test]
    fn exited_state_is_terminal() {
        let mut process = Process::new(0, 233);

        process
            .transition_to(ProcessStatus::Running)
            .expect("new process can start running");
        process
            .transition_to(ProcessStatus::Exited(ExitReason::Kill))
            .expect("running process can exit");

        assert_eq!(
            process.transition_to(ProcessStatus::Running),
            Err(ProcessError::InvalidStatusTransition {
                from: ProcessStatus::Exited(ExitReason::Kill),
                to: ProcessStatus::Running,
            })
        );
    }

    #[test]
    fn reductions_decrement_saturate_and_reset() {
        let mut process = Process::new(0, 233);

        assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
        process.decrement_reductions(1);
        assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET - 1);
        process.decrement_reductions(DEFAULT_REDUCTION_BUDGET);
        assert_eq!(process.reduction_counter(), 0);
        assert!(process.reductions_exhausted());
        process.reset_reductions(DEFAULT_REDUCTION_BUDGET);
        assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
        assert!(!process.reductions_exhausted());
    }
}
