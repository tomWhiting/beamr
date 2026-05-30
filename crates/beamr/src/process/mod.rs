//! Process — the unit of life and isolation.
//!
//! Each process owns its heap, stack, mailbox placeholder, reduction counter,
//! link/monitor sets, and status. Processes share no memory. Spawning costs
//! microseconds. A process that crashes takes only itself down — the rest of the
//! system is unaffected.
pub mod heap;
pub mod registry;
pub mod stack;

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::atom::Atom;
use crate::process::heap::Heap;
use crate::process::stack::Stack;
use crate::term::Term;

/// Default number of reductions assigned to a fresh process time slice.
pub const DEFAULT_REDUCTION_BUDGET: u32 = 4000;

/// Placeholder for process monitors; supervision semantics are implemented in a
/// later brief.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Monitor;

/// Current code location for a process.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CodePosition {
    /// Current module.
    pub module: Atom,
    /// Current instruction pointer in `module`.
    pub instruction_pointer: usize,
}

/// Reason a process exited.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// Normal process completion.
    Normal,
    /// Untrappable kill exit.
    Kill,
    /// Placeholder error exit until error terms land.
    Error,
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
    mailbox: VecDeque<Term>,
    x_regs: [Term; 256],
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
            mailbox: VecDeque::new(),
            x_regs: [Term::NIL; 256],
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
                | (ProcessStatus::Running, ProcessStatus::Exited(_))
                | (ProcessStatus::Yielded, ProcessStatus::Running)
                | (ProcessStatus::Waiting, ProcessStatus::Running)
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
    pub const fn mailbox(&self) -> &VecDeque<Term> {
        &self.mailbox
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

    /// Linked process IDs. Link propagation is handled by a later brief.
    #[must_use]
    pub const fn links(&self) -> &HashSet<u64> {
        &self.links
    }

    /// Monitor placeholders. Monitor behavior is handled by a later brief.
    #[must_use]
    pub const fn monitors(&self) -> &Vec<Monitor> {
        &self.monitors
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
