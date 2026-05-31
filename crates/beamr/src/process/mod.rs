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

use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::atom::Atom;
use crate::mailbox::Mailbox;
use crate::native::NativeContinuation;
use crate::process::heap::Heap;
use crate::process::stack::Stack;
use crate::term::Term;

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
    X(u8),
    /// Y register index in the current stack frame.
    Y(u16),
}

/// A try/catch handler installed by BEAM try-family opcodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExceptionHandler {
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
    heap: Heap,
    stack: Stack,
    mailbox: Mailbox,
    handlers: Vec<ExceptionHandler>,
    current_exception: Option<Exception>,
    receive_timeout: Option<ReceiveTimeout>,
    receive_timer_ref: Option<u64>,
    x_regs: [Term; 256],
    native_continuation: Option<NativeContinuation>,
    reduction_counter: u32,
    code_position: Option<CodePosition>,
    current_mfa: Option<(Atom, Atom, u8)>,
    links: HashSet<u64>,
    monitors: Vec<Monitor>,
    trap_exit: bool,
    group_leader: Option<u64>,
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
            heap: Heap::new(heap_size),
            stack: Stack::new(),
            mailbox: Mailbox::new(),
            handlers: Vec::new(),
            current_exception: None,
            receive_timeout: None,
            receive_timer_ref: None,
            x_regs: [Term::NIL; 256],
            native_continuation: None,
            reduction_counter: DEFAULT_REDUCTION_BUDGET,
            code_position: None,
            current_mfa: None,
            links: HashSet::new(),
            monitors: Vec::new(),
            trap_exit: false,
            group_leader: None,
            not_send_sync: PhantomData,
        }
    }

    /// Process identifier.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }

    /// Current lifecycle status.
    #[must_use]
    pub const fn status(&self) -> ProcessStatus {
        self.status
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
    pub fn x_reg(&self, n: u8) -> Term {
        self.x_regs[usize::from(n)]
    }

    /// Write X register `n`.
    pub fn set_x_reg(&mut self, n: u8, value: Term) {
        self.x_regs[usize::from(n)] = value;
    }

    /// Read all X registers.
    #[must_use]
    pub const fn x_regs(&self) -> &[Term; 256] {
        &self.x_regs
    }

    /// Mutable access to all X registers.
    pub fn x_regs_mut(&mut self) -> &mut [Term; 256] {
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

    /// Current code position, if one has been assigned.
    #[must_use]
    pub const fn code_position(&self) -> Option<CodePosition> {
        self.code_position
    }

    /// Set the current code position.
    pub const fn set_code_position(&mut self, code_position: Option<CodePosition>) {
        self.code_position = code_position;
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
    pub const fn links(&self) -> &HashSet<u64> {
        &self.links
    }

    /// Add a linked process id. Returns whether the set changed.
    pub fn add_link(&mut self, pid: u64) -> bool {
        pid != self.pid && self.links.insert(pid)
    }

    /// Remove a linked process id. Returns whether the set changed.
    pub fn remove_link(&mut self, pid: u64) -> bool {
        self.links.remove(&pid)
    }

    /// Remove all links and return the previous link set.
    pub fn take_links(&mut self) -> HashSet<u64> {
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

    /// Group leader placeholder PID, when assigned.
    #[must_use]
    pub const fn group_leader(&self) -> Option<u64> {
        self.group_leader
    }

    /// Set group leader placeholder PID.
    pub const fn set_group_leader(&mut self, group_leader: Option<u64>) {
        self.group_leader = group_leader;
    }

    /// Mark the process exited and release owned runtime state that can keep
    /// heap terms alive after process death.
    pub fn terminate(&mut self, reason: ExitReason) {
        self.status = ProcessStatus::Exited(reason);
        self.heap = Heap::new(1);
        self.stack = Stack::new();
        self.mailbox = Mailbox::new();
        self.handlers.clear();
        self.current_exception = None;
        self.receive_timeout = None;
        self.receive_timer_ref = None;
        self.x_regs = [Term::NIL; 256];
        self.reduction_counter = 0;
        self.code_position = None;
        self.current_mfa = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_REDUCTION_BUDGET, ExitReason, Process, ProcessError, ProcessStatus};
    use crate::term::Term;

    #[test]
    fn fresh_process_has_expected_state() {
        let process = Process::new(7, 233);

        assert_eq!(process.pid(), 7);
        assert_eq!(process.status(), ProcessStatus::New);
        assert_eq!(process.heap().capacity(), 233);
        assert!(process.stack().is_empty());
        assert!(process.mailbox().is_empty());
        assert_eq!(process.reduction_counter(), DEFAULT_REDUCTION_BUDGET);
        assert_eq!(process.code_position(), None);
        assert!(process.links().is_empty());
        assert!(process.monitors().is_empty());
        assert!(!process.trap_exit());
        assert_eq!(process.group_leader(), None);
    }

    #[test]
    fn all_x_registers_start_as_nil() {
        let process = Process::new(0, 233);

        for register in u8::MIN..=u8::MAX {
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
