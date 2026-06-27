//! Process-supporting value types — exit reasons, exceptions, monitors,
//! scheduling metadata, and JIT runtime state.

use std::fmt;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::module::Module;
use crate::term::{
    Term,
    boxed::{Cons, Tuple},
    format::format_term,
};

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

impl Exception {
    /// Format exception details for user-facing diagnostics using atom-name
    /// resolution from `atom_table`.
    #[must_use]
    pub fn format_with_atoms(&self, atom_table: &AtomTable) -> String {
        let mut output = format!(
            "{}: {}",
            format_term(self.class, atom_table),
            format_term(self.reason, atom_table)
        );

        if !self.stacktrace.is_nil() {
            append_stacktrace(&mut output, self.stacktrace, atom_table);
        }

        output
    }
}

fn append_stacktrace(output: &mut String, stacktrace: Term, atom_table: &AtomTable) {
    let mut current = stacktrace;
    let mut appended_frame = false;

    loop {
        if current.is_nil() {
            return;
        }

        let Some(cons) = Cons::new(current) else {
            if !appended_frame {
                output.push_str("\n  stacktrace: ");
                output.push_str(&format_term(stacktrace, atom_table));
            } else {
                output.push_str("\n  at ");
                output.push_str(&format_term(current, atom_table));
            }
            return;
        };

        output.push_str("\n  at ");
        output.push_str(&format_stacktrace_frame(cons.head(), atom_table));
        appended_frame = true;
        current = cons.tail();
    }
}

fn format_stacktrace_frame(frame: Term, atom_table: &AtomTable) -> String {
    let Some(tuple) = Tuple::new(frame) else {
        return format_term(frame, atom_table);
    };

    if tuple.arity() != 4 {
        return format_term(frame, atom_table);
    }

    let module = tuple
        .get(0)
        .map(|term| format_term(term, atom_table))
        .unwrap_or_else(|| "#<missing module>".to_owned());
    let function = tuple
        .get(1)
        .map(|term| format_term(term, atom_table))
        .unwrap_or_else(|| "#<missing function>".to_owned());
    let arity = tuple
        .get(2)
        .and_then(Term::as_small_int)
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            tuple
                .get(2)
                .map(|term| format_term(term, atom_table))
                .unwrap_or_else(|| "#<missing arity>".to_owned())
        });

    let mut formatted = format!("{module}:{function}/{arity}");
    if let Some(info) = tuple.get(3)
        && let Some(line) = stacktrace_line(info)
    {
        formatted.push(':');
        formatted.push_str(&line.to_string());
    }
    formatted
}

fn stacktrace_line(info: Term) -> Option<i64> {
    let mut current = info;
    loop {
        if current.is_nil() {
            return None;
        }
        let cons = Cons::new(current)?;
        let tuple = Tuple::new(cons.head())?;
        if tuple.arity() == 2 && tuple.get(0).and_then(Term::as_atom) == Some(Atom::LINE) {
            return tuple.get(1).and_then(Term::as_small_int);
        }
        current = cons.tail();
    }
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
    /// Precomputed source-location info for frames that do not map to an interpreted IP.
    pub location_info: Term,
    /// True when this entry represents a compiled frame rather than an interpreted IP.
    pub compiled: bool,
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
    /// Distribution connection to a linked or monitored remote process was lost.
    NoConnection,
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
            Self::NoConnection => Atom::NOCONNECTION,
        }
    }

    /// Term representation used in EXIT and DOWN messages.
    #[must_use]
    pub const fn as_term(self) -> Term {
        Term::atom(self.as_atom())
    }
}

/// Transient runtime context installed while the interpreter is inside native JIT code.
#[cfg(feature = "jit")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct JitRuntimeContext {
    /// Current module for import-table resolution.
    pub module: *const Module,
    /// Registry used by mixed-mode fallback calls.
    pub registry: *const crate::module::ModuleRegistry,
    /// Optional native-code cache used by helper-backed dynamic dispatch.
    pub jit_cache: *const crate::jit::JitCache,
}

#[cfg(feature = "jit")]
impl JitRuntimeContext {
    /// Creates a runtime context from borrowed interpreter dispatch state.
    #[must_use]
    pub const fn new(
        module: *const Module,
        registry: *const crate::module::ModuleRegistry,
        jit_cache: *const crate::jit::JitCache,
    ) -> Self {
        Self {
            module,
            registry,
            jit_cache,
        }
    }
}

/// Out-of-band status set by native JIT helpers when a raw return word is not a normal term.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JitStatus {
    /// The compiled function consumed the current reduction budget and yielded.
    Yield,
}

/// Stable identity for a PID hosted by another distribution node.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RemotePid {
    /// Remote node atom.
    pub node: Atom,
    /// Remote process id number.
    pub pid_number: u64,
    /// Remote pid serial.
    pub serial: u64,
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
