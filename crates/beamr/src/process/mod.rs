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
mod types;

pub use types::*;

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::atom::Atom;
use crate::mailbox::Mailbox;
use crate::module::Module;
use crate::namespace::NamespaceId;
use crate::native::NativeContinuation;
use crate::native::native_process::NativeBody;
use crate::process::heap::Heap;
use crate::process::stack::Stack;
#[cfg(feature = "threads")]
use crate::term::boxed::BoxedTag;
use crate::term::{Term, compare};

/// Default number of reductions assigned to a fresh process time slice.
pub const DEFAULT_REDUCTION_BUDGET: u32 = 4000;

/// One pending native higher-order-BIF continuation.
///
/// Pending continuations form a stack so natives can nest (a closure called
/// by `lists:map` may itself call `lists:map`). Each entry records the frame
/// depth just below its trampoline return frame AND the code position the
/// trampoline returns to (the BIF call instruction); the continuation
/// resumes only when the stack is back at that depth at exactly that
/// position — i.e. when its closure call returned. Depth alone is not
/// enough: a process re-entering an await elsewhere at equal stack depth
/// (a tail-called receive inside the closure) would otherwise re-fire the
/// continuation with garbage in x0.
#[derive(Clone, Debug)]
pub struct PendingNativeContinuation {
    /// Saved native state to re-enter.
    pub continuation: NativeContinuation,
    /// Frame count below the trampoline return frame.
    pub resume_depth: usize,
    /// The BIF call instruction the trampoline return frame jumps back to.
    pub resume_position: Option<CodePosition>,
}

/// Why a process is parked beyond a plain receive.
///
/// Plain receives (BEAM `wait`/`wait_timeout`, and select-style native
/// suspends that re-execute on message arrival) carry no suspension record:
/// any message may wake them. The kinds below are *result-gated*: only the
/// matching completion event (identified by the record's call id) may resume
/// the process, because re-executing the parked instruction would repeat its
/// side effect (re-submit a host call, re-enter a dirty native, …).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuspensionKind {
    /// A native requested suspension and a host-side completion will deliver
    /// the result (`Scheduler::wake_with_result*`, file-I/O completions, or a
    /// receive-timeout re-entry).
    HostAwait,
    /// A dirty native call is in flight on a dirty scheduler pool.
    DirtyCall,
    /// The reduction-boundary hook suspended the process; an embedder
    /// `resume_process` call resumes it.
    Hook,
}

/// How execution continues when a host completion is applied into x0 at a
/// parked call instruction (which is then NOT re-executed).
///
/// Body-position native calls resume at the next instruction. Tail-position
/// native calls (`call_ext_only`, `call_ext_last`) have no next instruction
/// — the applied result IS the function's return value, so the process must
/// return to the caller instead of running off the end of the function.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResumeContinuation {
    /// Body-position call: resume at the instruction after the park.
    #[default]
    Advance,
    /// Tail call with no live stack frame at the park (`call_ext_only`, or
    /// a `call_ext_last` whose frame was already popped before the park):
    /// return to the caller.
    Return,
    /// Tail call parked with its y-frame intact (`call_ext_last`, whose
    /// frame pop is deferred across the suspension so wake re-execution is
    /// idempotent): pop the frame, then return to the caller.
    DeallocateAndReturn,
}

/// Identity of one result-gated suspension.
///
/// `call_id` is allocated from a per-process monotonic counter at suspend
/// time and never reused, so a completion produced for an earlier suspension
/// can always be recognized as stale and dropped instead of being applied at
/// the wrong park position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SuspensionRecord {
    /// Per-process monotonically increasing suspension identity.
    pub call_id: u64,
    /// What kind of completion may consume this suspension.
    pub kind: SuspensionKind,
    /// Code position at suspend time (the parked call instruction), used to
    /// validate that a result is applied exactly where it was produced.
    pub position: Option<CodePosition>,
    /// True for message-wakeable host awaits (`request_suspend`: select and
    /// marker-style awaits, whose natives are re-entrant and re-execute on
    /// any wake). False for gated awaits (`request_await_suspend`, dirty
    /// calls, hook suspends), which only their own completion may resume.
    pub wake_on_message: bool,
    /// How a host completion applied at this park continues execution.
    pub continuation: ResumeContinuation,
}

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
    capabilities: crate::native::CapabilitySet,
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
    suspension: Option<SuspensionRecord>,
    next_suspension_call_id: u64,
    x_regs: [Term; 1024],
    float_regs: [f64; 16],
    native_continuations: Vec<PendingNativeContinuation>,
    /// Explicit GC roots registered by native code for terms it must keep
    /// alive across allocations. See `ProcessContext::root_term`.
    native_roots: Vec<Term>,
    raw_stacktrace: Vec<RawStackEntry>,
    reduction_counter: u32,
    logical_clock: u64,
    namespace_id: NamespaceId,
    code_position: Option<CodePosition>,
    current_module: Option<Arc<Module>>,
    current_mfa: Option<(Atom, Atom, u8)>,
    #[cfg(feature = "jit")]
    jit_runtime_context: Option<JitRuntimeContext>,
    jit_status: Option<JitStatus>,
    #[cfg(feature = "telemetry")]
    receive_wait_started: Option<crate::telemetry::spans::ReceiveWaitStarted>,
    #[cfg(feature = "telemetry")]
    trace_context: Option<crate::telemetry::spans::ProcessTraceContext>,
    links: Vec<u64>,
    remote_links: Vec<RemotePid>,
    monitors: Vec<Monitor>,
    trap_exit: bool,
    group_leader: Term,
    /// Optional native handler body. `Some` exactly for native processes
    /// (Shape B). Excluded from structural [`Clone`] — a clone is non-native.
    native: Option<NativeBody>,
    not_send_sync: PhantomData<Rc<()>>,
}

impl Clone for Process {
    fn clone(&self) -> Self {
        let mut heap = self.heap.clone();
        heap.rebase_snapshot_terms(&self.heap);
        let mut clone = Self {
            pid: self.pid,
            capabilities: self.capabilities.clone(),
            status: self.status,
            priority: self.priority,
            heap,
            virtual_binary_heap: self.virtual_binary_heap,
            stack: self.stack.clone(),
            mailbox: self.mailbox.clone(),
            handlers: self.handlers.clone(),
            current_exception: self.current_exception,
            dictionary: self.dictionary.clone(),
            receive_timeout: self.receive_timeout,
            receive_timer_ref: self.receive_timer_ref,
            suspension: self.suspension,
            next_suspension_call_id: self.next_suspension_call_id,
            x_regs: self.x_regs,
            float_regs: self.float_regs,
            native_continuations: self.native_continuations.clone(),
            native_roots: self.native_roots.clone(),
            raw_stacktrace: self.raw_stacktrace.clone(),
            reduction_counter: self.reduction_counter,
            logical_clock: self.logical_clock,
            namespace_id: self.namespace_id,
            code_position: self.code_position,
            current_module: self.current_module.clone(),
            current_mfa: self.current_mfa,
            #[cfg(feature = "jit")]
            jit_runtime_context: self.jit_runtime_context,
            jit_status: self.jit_status,
            #[cfg(feature = "telemetry")]
            receive_wait_started: self.receive_wait_started,
            #[cfg(feature = "telemetry")]
            trace_context: self.trace_context.clone(),
            links: self.links.clone(),
            remote_links: self.remote_links.clone(),
            monitors: self.monitors.clone(),
            trap_exit: self.trap_exit,
            group_leader: self.group_leader,
            // A `Box<dyn NativeHandler>` is not `Clone`, so a structural clone
            // is deliberately non-native. See the `Process::clone` audit note
            // in `is_native` — no live scheduler path clones a native process.
            native: None,
            not_send_sync: PhantomData,
        };
        clone.rebase_roots_from(self);
        clone
    }
}

impl Process {
    /// Create a fresh process with `pid` and a heap capacity of `heap_size`
    /// words.
    #[must_use]
    pub fn new(pid: u64, heap_size: usize) -> Self {
        Self::with_capabilities(pid, heap_size, crate::native::CapabilitySet::all())
    }

    /// Create a fresh process with an explicit capability set.
    #[must_use]
    pub fn with_capabilities(
        pid: u64,
        heap_size: usize,
        capabilities: crate::native::CapabilitySet,
    ) -> Self {
        Self {
            pid,
            capabilities,
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
            suspension: None,
            next_suspension_call_id: 0,
            x_regs: [Term::NIL; 1024],
            float_regs: [0.0; 16],
            native_continuations: Vec::new(),
            native_roots: Vec::new(),
            raw_stacktrace: Vec::new(),
            reduction_counter: DEFAULT_REDUCTION_BUDGET,
            logical_clock: 0,
            namespace_id: NamespaceId::DEFAULT,
            code_position: None,
            current_module: None,
            current_mfa: None,
            #[cfg(feature = "jit")]
            jit_runtime_context: None,
            jit_status: None,
            #[cfg(feature = "telemetry")]
            receive_wait_started: None,
            #[cfg(feature = "telemetry")]
            trace_context: None,
            links: Vec::new(),
            remote_links: Vec::new(),
            monitors: Vec::new(),
            trap_exit: false,
            group_leader: Self::initial_group_leader(pid),
            native: None,
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

    /// Capabilities granted to this process.
    #[must_use]
    pub const fn capabilities(&self) -> &crate::native::CapabilitySet {
        &self.capabilities
    }

    /// Replace this process's capability set before it is made runnable.
    pub fn set_capabilities(&mut self, capabilities: crate::native::CapabilitySet) {
        self.capabilities = capabilities;
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

    /// True exactly when this process carries a native handler body (Shape B).
    ///
    /// A native process never sets a code position, x-registers, or stack;
    /// those stay default/empty and its scheduler machinery (status graph,
    /// `ProcessMetadata` swap, park-gap, exit-tombstones) is reused unchanged.
    ///
    /// `Process::clone` AUDIT (NATIVE-001 R2): the structural `Clone` impl
    /// sets `native: None`, so a clone is always non-native. This is safe
    /// because no live scheduler path clones a native process: spawn inserts a
    /// freshly built `Process` (never a clone); the store/take slot swap moves
    /// the `Process` by value through `std::mem::take` / `ProcessSlot`, never
    /// cloning it; and replay does not snapshot process bodies. A native
    /// restart (NATIVE-002) rebuilds the handler via the retained factory,
    /// never by cloning a handler instance — so a clone can never silently
    /// drop a live handler and leave a dead no-op process.
    #[must_use]
    pub fn is_native(&self) -> bool {
        self.native.is_some()
    }

    /// Install a native handler body, making this a native process. Called by
    /// the scheduler's `spawn_native` before the process is made runnable.
    pub(crate) fn set_native_body(&mut self, body: NativeBody) {
        self.native = Some(body);
    }

    /// Mutable access to the native body, for `run_native_slice` to take the
    /// handler out for a slice (and the restart path to reach the factory).
    pub(crate) fn native_body_mut(&mut self) -> Option<&mut NativeBody> {
        self.native.as_mut()
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

    fn rebase_roots_from(&mut self, original: &Self) {
        for root in &mut self.x_regs {
            *root = self.heap.rebase_term_from(*root, &original.heap);
        }
        for root in self.stack.y_regs_mut() {
            *root = self.heap.rebase_term_from(*root, &original.heap);
        }
        for root in self.mailbox.scan_iter_mut() {
            *root = self.heap.rebase_term_from(*root, &original.heap);
        }
        for entry in &mut self.raw_stacktrace {
            entry.location_info = self
                .heap
                .rebase_term_from(entry.location_info, &original.heap);
        }
        if let Some(exception) = &mut self.current_exception {
            exception.class = self.heap.rebase_term_from(exception.class, &original.heap);
            exception.reason = self.heap.rebase_term_from(exception.reason, &original.heap);
            exception.stacktrace = self
                .heap
                .rebase_term_from(exception.stacktrace, &original.heap);
        }
        for (key, value) in &mut self.dictionary {
            *key = self.heap.rebase_term_from(*key, &original.heap);
            *value = self.heap.rebase_term_from(*value, &original.heap);
        }
        self.group_leader = self
            .heap
            .rebase_term_from(self.group_leader, &original.heap);
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
        let mut roots: Vec<Term> = self
            .x_regs
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
            .collect();
        roots.extend(self.native_roots.iter().copied());
        for pending in &self.native_continuations {
            pending
                .continuation
                .for_each_term(&mut |term| roots.push(term));
        }
        roots
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
        index += 1;
        for root in &mut self.native_roots {
            if let Some(value) = roots.get(index).copied() {
                *root = value;
            }
            index += 1;
        }
        for pending in &mut self.native_continuations {
            pending.continuation.for_each_term_mut(&mut |term| {
                if let Some(value) = roots.get(index).copied() {
                    *term = value;
                }
                index += 1;
            });
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

    /// Allocate the next suspension call id from this process's monotonic
    /// counter. Ids start at 1 and are never reused; 0 is reserved as the
    /// embedder wildcard in `Scheduler::resume_process`.
    pub const fn allocate_suspension_call_id(&mut self) -> u64 {
        self.next_suspension_call_id += 1;
        self.next_suspension_call_id
    }

    /// Install the current result-gated suspension record.
    pub const fn set_suspension(&mut self, record: Option<SuspensionRecord>) {
        self.suspension = record;
    }

    /// Current result-gated suspension record, when parked beyond a plain
    /// receive.
    #[must_use]
    pub const fn suspension(&self) -> Option<SuspensionRecord> {
        self.suspension
    }

    /// Consume the current suspension record.
    pub const fn take_suspension(&mut self) -> Option<SuspensionRecord> {
        self.suspension.take()
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn mark_receive_wait_started(&mut self) {
        if self.receive_wait_started.is_none() {
            self.receive_wait_started = Some(crate::telemetry::spans::receive_wait_started_now());
        }
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn take_receive_wait_duration(&mut self) -> Option<std::time::Duration> {
        self.receive_wait_started
            .take()
            .map(|started| started.elapsed())
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn set_trace_context(
        &mut self,
        trace_context: Option<crate::telemetry::spans::ProcessTraceContext>,
    ) {
        self.trace_context = trace_context;
    }

    #[cfg(feature = "telemetry")]
    pub(crate) const fn trace_context(
        &self,
    ) -> Option<&crate::telemetry::spans::ProcessTraceContext> {
        self.trace_context.as_ref()
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

    /// Push native continuation state for closure-return re-entry.
    ///
    /// `resume_depth` is the frame count just below the trampoline return
    /// frame: the continuation resumes once the stack is back at (or below)
    /// that depth, which happens exactly when the closure call returns.
    pub fn push_native_continuation(
        &mut self,
        continuation: NativeContinuation,
        resume_depth: usize,
    ) {
        self.native_continuations.push(PendingNativeContinuation {
            continuation,
            resume_depth,
            resume_position: self.code_position,
        });
    }

    /// Take the innermost native continuation after its closure returns.
    pub fn take_native_continuation(&mut self) -> Option<NativeContinuation> {
        self.native_continuations.pop().map(|p| p.continuation)
    }

    /// True when the innermost pending continuation's closure has returned
    /// (its trampoline return frame has been popped, landing the process
    /// back at the recorded call instruction) and the native must be
    /// re-entered before the next instruction executes.
    ///
    /// The position check prevents a stale re-fire: stack depth alone can
    /// also match at an unrelated await re-entered at equal depth, where
    /// x0 holds garbage rather than the closure's return value.
    #[must_use]
    pub fn native_continuation_ready(&self) -> bool {
        self.native_continuations.last().is_some_and(|pending| {
            self.stack.len() <= pending.resume_depth
                && pending.resume_position == self.code_position
        })
    }

    /// Drop pending continuations whose trampoline return frames were
    /// discarded by an exception-handler stack truncation to `depth`.
    pub fn prune_native_continuations(&mut self, depth: usize) {
        self.native_continuations
            .retain(|pending| pending.resume_depth < depth);
    }

    /// Register `term` as an explicit GC root, returning its stack index.
    ///
    /// The root is traced and forwarded by every collection until removed
    /// with [`Process::truncate_native_roots`].
    pub(crate) fn push_native_root(&mut self, term: Term) -> usize {
        self.native_roots.push(term);
        self.native_roots.len() - 1
    }

    /// Read the current (post-GC) value of the native root at `index`.
    pub(crate) fn native_root(&self, index: usize) -> Option<Term> {
        self.native_roots.get(index).copied()
    }

    /// Overwrite the native root at `index` with a new term.
    pub(crate) fn set_native_root(&mut self, index: usize, term: Term) {
        if let Some(slot) = self.native_roots.get_mut(index) {
            *slot = term;
        }
    }

    /// Clear x registers at and above `live_x`.
    ///
    /// Called by minor GC after reclaiming the nursery: registers outside the
    /// traced live prefix may still point into reclaimed space, and a later
    /// full-register walk (major GC, conservative natives) must never chase
    /// them. The invariant is that every register always holds NIL, an
    /// immediate, or a pointer to a currently-allocated object.
    pub(crate) fn clear_dead_x_regs(&mut self, live_x: usize) {
        for reg in self.x_regs.iter_mut().skip(live_x) {
            *reg = Term::NIL;
        }
    }

    /// Number of registered native roots.
    pub(crate) fn native_root_depth(&self) -> usize {
        self.native_roots.len()
    }

    /// Drop native roots registered after `depth`.
    pub(crate) fn truncate_native_roots(&mut self, depth: usize) {
        self.native_roots.truncate(depth);
    }

    /// Check whether any native continuation is pending.
    #[must_use]
    pub fn has_native_continuation(&self) -> bool {
        !self.native_continuations.is_empty()
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

    /// Current per-process logical clock used by deterministic replay.
    #[must_use]
    pub const fn logical_clock(&self) -> u64 {
        self.logical_clock
    }

    /// Advance the logical clock for a local causal event.
    pub fn tick_logical_clock(&mut self) -> u64 {
        self.logical_clock = self.logical_clock.saturating_add(1);
        self.logical_clock
    }

    /// Merge a sender clock into this process and advance for message delivery.
    pub fn observe_message_clock(&mut self, sender_clock: u64) -> u64 {
        self.logical_clock = self.logical_clock.max(sender_clock).saturating_add(1);
        self.logical_clock
    }

    /// Set the logical clock from a recorded replay delivery.
    pub const fn set_logical_clock(&mut self, clock: u64) {
        self.logical_clock = clock;
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

    /// Runtime context visible to JIT helper calls for the current native invocation.
    #[cfg(feature = "jit")]
    #[must_use]
    pub const fn jit_runtime_context(&self) -> Option<JitRuntimeContext> {
        self.jit_runtime_context
    }

    /// Set the transient JIT runtime context for the duration of a native invocation.
    #[cfg(feature = "jit")]
    pub const fn set_jit_runtime_context(&mut self, context: Option<JitRuntimeContext>) {
        self.jit_runtime_context = context;
    }

    /// Mark the outcome status reported by JIT-generated code.
    pub const fn set_jit_status(&mut self, status: Option<JitStatus>) {
        self.jit_status = status;
    }

    /// Take and clear the current JIT status.
    pub fn take_jit_status(&mut self) -> Option<JitStatus> {
        self.jit_status.take()
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

    /// Linked remote process IDs.
    #[must_use]
    pub fn remote_links(&self) -> &[RemotePid] {
        &self.remote_links
    }

    /// Add a linked remote process id. Returns whether the ordered set changed.
    ///
    /// Link insertion order is preserved separately from local links so remote
    /// exit propagation is deterministic without losing node/serial identity.
    pub fn add_remote_link(&mut self, pid: RemotePid) -> bool {
        if self.remote_links.contains(&pid) {
            return false;
        }
        self.remote_links.push(pid);
        true
    }

    /// Remove a linked remote process id. Returns whether the ordered set changed.
    pub fn remove_remote_link(&mut self, pid: RemotePid) -> bool {
        let before = self.remote_links.len();
        self.remote_links.retain(|linked| *linked != pid);
        before != self.remote_links.len()
    }

    /// Remove all remote links and return the previous set in insertion order.
    pub fn take_remote_links(&mut self) -> Vec<RemotePid> {
        std::mem::take(&mut self.remote_links)
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
        self.close_owned_fd_resources();
        self.status = ProcessStatus::Exited(reason);
        crate::gc::release_all_refcounted_resources(self);
        self.virtual_binary_heap = 0;
        self.heap = Heap::new(1);
        self.stack = Stack::new();
        self.mailbox = Mailbox::new();
        self.handlers.clear();
        self.current_exception = None;
        self.dictionary.clear();
        self.receive_timeout = None;
        self.receive_timer_ref = None;
        self.suspension = None;
        self.x_regs = [Term::NIL; 1024];
        self.float_regs = [0.0; 16];
        self.native_continuations.clear();
        self.native_roots.clear();
        self.reduction_counter = 0;
        self.code_position = None;
        self.current_module = None;
        self.current_mfa = None;
    }

    /// Close FD-backed resources owned by this process. The `io` subsystem only
    /// exists in the threaded (native) build; in the cooperative/wasm build there
    /// are no host file descriptors, so this is a no-op.
    #[cfg(feature = "threads")]
    fn close_owned_fd_resources(&mut self) {
        let owner_pid = self.pid;
        self.heap().visit_boxed_objects(|ptr, tag, _words| {
            if tag == BoxedTag::FdResource {
                crate::io::resource::close_owned_resource_at(ptr, owner_pid);
            }
        });
    }

    #[cfg(not(feature = "threads"))]
    fn close_owned_fd_resources(&mut self) {}
}

#[cfg(test)]
mod tests;
