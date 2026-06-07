//! Minimal process-facing context exposed to native code.
//!
//! Native functions deliberately receive this allocation subset instead of the
//! full process so they cannot inspect scheduler, mailbox, or process internals.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::atom::AtomTable;
use crate::io::{IoSink, NullSink};
use crate::native::stdlib_stubs::{lists_bifs::ListsMapState, maps_bifs::MapsHofState};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::boxed::{write_bigint, write_cons, write_float, write_map, write_tuple};
use crate::timer::{TimerRef, TimerWheel};

use super::code_management_bifs::CodeManagementFacility;
use super::links::LinkFacility;
use super::registry::RegistryFacility;
use super::select::SelectFacility;
use super::spawn::SpawnFacility;
use super::supervision::SupervisionFacility;

/// Minimal process-facing context exposed to native code.
///
/// Native functions deliberately receive this allocation subset instead of the
/// full process so they cannot inspect scheduler, mailbox, or process internals.
/// Trampoline request from a BIF that needs interpreter re-entry.
///
/// When a BIF returns normally but needs the interpreter to call a BEAM
/// closure and use the closure's return value as the BIF's result, it stores
/// a `TrampolineRequest` in the process context. The interpreter checks for
/// this after each BIF call.
#[derive(Clone, Debug)]
pub struct TrampolineRequest {
    /// The closure (fun) term to invoke.
    pub fun: Term,
    /// Arguments to pass to the closure.
    pub args: Vec<Term>,
    /// Optional native continuation to resume after the closure returns.
    pub continuation: Option<NativeContinuation>,
}

/// Native continuation state for collection BIFs that call closures repeatedly.
#[derive(Clone, Debug)]
pub enum NativeContinuation {
    /// Continuation for maps higher-order BIFs.
    Maps(MapsHofState),
    /// Continuation for lists:map/2.
    ListsMap(ListsMapState),
    /// Continuation for Gleam result.try/2 compatibility.
    GleamResultTry,
}

/// Suspend request from a BIF that wants the process to wait.
///
/// Used by `select` when no mailbox message matches any handler.
#[derive(Copy, Clone, Debug)]
pub struct SuspendRequest {
    /// Optional timeout in milliseconds. `None` means wait indefinitely.
    pub timeout_ms: Option<u64>,
}

/// Exception classes that BIFs can request when returning `Err(reason)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExceptionClass {
    /// Ordinary error exception class.
    Error,
    /// Non-local throw exception class.
    Throw,
    /// Process exit exception class.
    Exit,
}

pub struct ProcessContext<'process> {
    pid: Option<u64>,
    process: Option<&'process mut Process>,
    live_x: usize,
    timers: Option<Arc<Mutex<TimerWheel>>>,
    atom_table: Option<Arc<AtomTable>>,
    spawn_facility: Option<Arc<dyn SpawnFacility>>,
    link_facility: Option<Arc<dyn LinkFacility>>,
    supervision_facility: Option<Arc<dyn SupervisionFacility>>,
    code_management_facility: Option<Arc<dyn CodeManagementFacility>>,
    registry_facility: Option<Arc<dyn RegistryFacility>>,
    select_facility: Option<Arc<dyn SelectFacility>>,
    io_sink: Arc<dyn IoSink>,
    exception_class: ExceptionClass,
    exception_stacktrace: Term,
    shutdown_requested: bool,
    trampoline: Option<TrampolineRequest>,
    suspend: Option<SuspendRequest>,
}

impl fmt::Debug for ProcessContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessContext")
            .field("pid", &self.pid)
            .field("process_heap", &self.process.as_ref().map(|_| ".."))
            .field("live_x", &self.live_x)
            .field("timers", &self.timers)
            .field("atom_table", &self.atom_table.as_ref().map(|_| ".."))
            .field(
                "spawn_facility",
                &self.spawn_facility.as_ref().map(|_| ".."),
            )
            .field("link_facility", &self.link_facility.as_ref().map(|_| ".."))
            .field(
                "supervision_facility",
                &self.supervision_facility.as_ref().map(|_| ".."),
            )
            .field(
                "code_management_facility",
                &self.code_management_facility.as_ref().map(|_| ".."),
            )
            .field(
                "registry_facility",
                &self.registry_facility.as_ref().map(|_| ".."),
            )
            .field(
                "select_facility",
                &self.select_facility.as_ref().map(|_| ".."),
            )
            .field("io_sink", &"..")
            .field("exception_class", &self.exception_class)
            .field("shutdown_requested", &self.shutdown_requested)
            .field("trampoline", &self.trampoline)
            .field("suspend", &self.suspend)
            .field("exception_class", &self.exception_class)
            .field("exception_stacktrace", &self.exception_stacktrace)
            .finish()
    }
}

impl Default for ProcessContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'process> ProcessContext<'process> {
    /// Creates an empty process context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pid: None,
            process: None,
            live_x: 256,
            timers: None,
            atom_table: None,
            spawn_facility: None,
            link_facility: None,
            supervision_facility: None,
            code_management_facility: None,
            registry_facility: None,
            select_facility: None,
            io_sink: Arc::new(NullSink),
            exception_class: ExceptionClass::Error,
            exception_stacktrace: Term::NIL,
            trampoline: None,
            suspend: None,
            shutdown_requested: false,
        }
    }

    /// Creates a context with timer services for asynchronous timer BIFs.
    #[must_use]
    pub fn with_timer_services(pid: u64, timers: Arc<Mutex<TimerWheel>>) -> Self {
        Self {
            pid: Some(pid),
            process: None,
            live_x: 256,
            timers: Some(timers),
            atom_table: None,
            spawn_facility: None,
            link_facility: None,
            supervision_facility: None,
            code_management_facility: None,
            registry_facility: None,
            select_facility: None,
            io_sink: Arc::new(NullSink),
            exception_class: ExceptionClass::Error,
            exception_stacktrace: Term::NIL,
            trampoline: None,
            suspend: None,
            shutdown_requested: false,
        }
    }

    /// Return the calling process id when provided by the runtime.
    #[must_use]
    pub fn pid(&self) -> Option<u64> {
        self.pid
    }

    /// Set the calling process id.
    pub fn set_pid(&mut self, pid: Option<u64>) {
        self.pid = pid;
    }

    /// Attach the calling process for process-heap native result allocation.
    pub fn attach_process(&mut self, process: &'process mut Process, live_x: usize) {
        self.pid = Some(process.pid());
        self.process = Some(process);
        self.live_x = live_x;
    }

    /// Detach the calling process before the interpreter resumes using it directly.
    pub fn detach_process(&mut self) {
        self.process = None;
    }

    /// Return the calling process heap, when this context is heap-backed.
    #[must_use]
    pub fn process_heap(&self) -> Option<&crate::process::heap::Heap> {
        self.process.as_ref().map(|process| process.heap())
    }

    /// Ensure the calling process has at least `words` nursery words available.
    pub fn ensure_heap_space(&mut self, words: usize) -> Result<(), Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        crate::gc::ensure_space(process, words, self.live_x)
            .map_err(|_| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Return the spawn facility, if one has been configured.
    #[must_use]
    pub fn spawn_facility(&self) -> Option<&dyn SpawnFacility> {
        self.spawn_facility.as_deref()
    }

    /// Set the spawn facility for process creation BIFs.
    pub fn set_spawn_facility(&mut self, facility: Option<Arc<dyn SpawnFacility>>) {
        self.spawn_facility = facility;
    }

    /// Return the link facility, if one has been configured.
    #[must_use]
    pub fn link_facility(&self) -> Option<&dyn LinkFacility> {
        self.link_facility.as_deref()
    }

    /// Set the link facility for link management BIFs.
    pub fn set_link_facility(&mut self, facility: Option<Arc<dyn LinkFacility>>) {
        self.link_facility = facility;
    }

    /// Return the supervision facility, if one has been configured.
    #[must_use]
    pub fn supervision_facility(&self) -> Option<&dyn SupervisionFacility> {
        self.supervision_facility.as_deref()
    }

    /// Set the supervision facility for monitor/demonitor/exit BIFs.
    pub fn set_supervision_facility(&mut self, facility: Option<Arc<dyn SupervisionFacility>>) {
        self.supervision_facility = facility;
    }

    /// Return the code-management facility, if one has been configured.
    #[must_use]
    pub fn code_management_facility(&self) -> Option<&dyn CodeManagementFacility> {
        self.code_management_facility.as_deref()
    }

    /// Set the code-management facility for hot-code BIFs.
    pub fn set_code_management_facility(
        &mut self,
        facility: Option<Arc<dyn CodeManagementFacility>>,
    ) {
        self.code_management_facility = facility;
    }

    /// Return the atom table, if one has been configured.
    #[must_use]
    pub fn atom_table(&self) -> Option<&AtomTable> {
        self.atom_table.as_deref()
    }

    /// Return a shared atom table handle, if one has been configured.
    #[must_use]
    pub fn atom_table_arc(&self) -> Option<Arc<AtomTable>> {
        self.atom_table.clone()
    }

    /// Set the atom table for type conversion BIFs.
    pub fn set_atom_table(&mut self, table: Option<Arc<AtomTable>>) {
        self.atom_table = table;
    }

    /// Return the registry facility, if one has been configured.
    #[must_use]
    pub fn registry_facility(&self) -> Option<&dyn RegistryFacility> {
        self.registry_facility.as_deref()
    }

    /// Set the registry facility for process name registry BIFs.
    pub fn set_registry_facility(&mut self, facility: Option<Arc<dyn RegistryFacility>>) {
        self.registry_facility = facility;
    }

    /// Schedule a timer via the runtime timer wheel.
    pub fn schedule_timer(
        &mut self,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> Option<TimerRef> {
        let timers = self.timers.as_ref()?;
        Some(
            timers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .schedule(delay, target_pid, message),
        )
    }

    /// Reserve a timer reference and schedule with a message derived from it.
    pub fn schedule_timer_with_reference<F>(
        &mut self,
        delay: Duration,
        target_pid: u64,
        message: F,
    ) -> Option<TimerRef>
    where
        F: FnOnce(TimerRef) -> Term,
    {
        let timers = self.timers.as_ref()?;
        let mut timers = timers.lock().unwrap_or_else(|error| error.into_inner());
        let reference = timers.reserve_reference();
        timers.schedule_reserved(reference, delay, target_pid, message(reference))
    }

    /// Reserve a timer reference without scheduling it yet.
    pub fn reserve_timer_reference(&mut self) -> Option<TimerRef> {
        let timers = self.timers.as_ref()?;
        Some(
            timers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .reserve_reference(),
        )
    }

    /// Schedule a message using an already reserved timer reference.
    pub fn schedule_reserved_timer(
        &mut self,
        reference: TimerRef,
        delay: Duration,
        target_pid: u64,
        message: Term,
    ) -> Option<TimerRef> {
        let timers = self.timers.as_ref()?;
        timers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .schedule_reserved(reference, delay, target_pid, message)
    }

    /// Cancel a timer via the runtime timer wheel.
    pub fn cancel_timer(&mut self, reference: TimerRef) -> Option<Duration> {
        let timers = self.timers.as_ref()?;
        timers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel(reference)
    }

    /// Allocates a term on the calling process heap.
    ///
    /// Gate 1 only has immediate terms, so this currently returns the term
    /// unchanged. Boxed values can later route through the process heap without
    /// changing the native calling convention.
    pub const fn allocate_term(&mut self, term: Term) -> Term {
        term
    }

    // --- Select facility ---

    /// Return the select facility, if one has been configured.
    #[must_use]
    pub fn select_facility(&self) -> Option<&dyn SelectFacility> {
        self.select_facility.as_deref()
    }

    /// Set the select facility for mailbox scanning BIFs.
    pub fn set_select_facility(&mut self, facility: Option<Arc<dyn SelectFacility>>) {
        self.select_facility = facility;
    }

    /// Store a value in the attached process dictionary.
    pub fn dict_put(&mut self, key: Term, value: Term) -> Result<Term, Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_put(key, value))
    }

    /// Fetch a value from the attached process dictionary.
    pub fn dict_get(&self, key: Term) -> Result<Term, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_get(key))
    }

    /// Copy all attached process dictionary entries in current vector order.
    pub fn dict_get_all(&self) -> Result<Vec<(Term, Term)>, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_get_all().to_vec())
    }

    /// Remove a value from the attached process dictionary.
    pub fn dict_erase(&mut self, key: Term) -> Result<Term, Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_erase(key))
    }

    /// Remove and return all attached process dictionary entries.
    pub fn dict_erase_all(&mut self) -> Result<Vec<(Term, Term)>, Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_erase_all())
    }

    /// Copy all dictionary keys whose values exactly match `value`.
    pub fn dict_get_keys(&self, value: Term) -> Result<Vec<Term>, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_get_keys(value))
    }

    /// Return the configured output sink for `io` module BIFs.
    #[must_use]
    pub fn io_sink(&self) -> &dyn IoSink {
        self.io_sink.as_ref()
    }

    /// Set the output sink for `io` module BIFs.
    pub fn set_io_sink(&mut self, sink: Arc<dyn IoSink>) {
        self.io_sink = sink;
    }

    /// Request runtime shutdown after the current BIF returns.
    pub fn request_shutdown(&mut self) {
        self.shutdown_requested = true;
    }

    /// Take and clear the shutdown request flag.
    pub fn take_shutdown_request(&mut self) -> bool {
        let requested = self.shutdown_requested;
        self.shutdown_requested = false;
        requested
    }

    /// Set the exception class to use if this BIF returns `Err(reason)`.
    pub fn set_exception_class(&mut self, class: ExceptionClass) {
        self.exception_class = class;
    }

    /// Take the requested exception class, resetting subsequent errors to `error`.
    pub fn take_exception_class(&mut self) -> ExceptionClass {
        let class = self.exception_class;
        self.exception_class = ExceptionClass::Error;
        class
    }

    // --- Trampoline ---

    /// Store a trampoline request for the interpreter to execute.
    ///
    /// The interpreter checks for a trampoline after each BIF call. When
    /// present, it sets up the closure call and uses the closure's return
    /// value as the BIF's return value.
    pub fn set_trampoline(&mut self, fun: Term, args: Vec<Term>) {
        self.trampoline = Some(TrampolineRequest {
            fun,
            args,
            continuation: None,
        });
    }

    /// Store a trampoline request with native continuation state.
    pub fn set_continuation_trampoline(
        &mut self,
        fun: Term,
        args: Vec<Term>,
        continuation: NativeContinuation,
    ) {
        self.trampoline = Some(TrampolineRequest {
            fun,
            args,
            continuation: Some(continuation),
        });
    }

    /// Take the trampoline request, clearing it from the context.
    ///
    /// Returns `None` if no trampoline was requested.
    pub fn take_trampoline(&mut self) -> Option<TrampolineRequest> {
        self.trampoline.take()
    }

    /// Check whether a trampoline request is pending.
    #[must_use]
    pub fn has_trampoline(&self) -> bool {
        self.trampoline.is_some()
    }

    // --- Suspend ---

    /// Request that the process be suspended (waiting for messages).
    ///
    /// Called by `select` when no mailbox message matches any handler.
    pub fn request_suspend(&mut self, timeout_ms: Option<u64>) {
        self.suspend = Some(SuspendRequest { timeout_ms });
    }

    /// Take the suspend request, clearing it from the context.
    pub fn take_suspend(&mut self) -> Option<SuspendRequest> {
        self.suspend.take()
    }

    // --- Exception metadata ---

    /// Set the stacktrace to use if the current BIF returns `Err(reason)`.
    pub fn set_exception_stacktrace(&mut self, trace: Term) {
        self.exception_stacktrace = trace;
    }

    /// Take the pending exception stacktrace, resetting subsequent BIF errors to `[]`.
    pub fn take_exception_stacktrace(&mut self) -> Term {
        let stacktrace = self.exception_stacktrace;
        self.exception_stacktrace = Term::NIL;
        stacktrace
    }

    // --- Heap allocation helpers ---

    fn alloc_words(&mut self, words: usize) -> Result<&mut [u64], Term> {
        self.ensure_heap_space(words)?;
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        process
            .heap_mut()
            .alloc_slice(words)
            .map_err(|_| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a tuple on the calling process heap.
    pub fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let words = 1 + elements.len();
        let heap = self.alloc_words(words)?;
        write_tuple(heap, elements).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a cons cell on the calling process heap.
    pub fn alloc_cons(&mut self, head: Term, tail: Term) -> Result<Term, Term> {
        let heap = self.alloc_words(2)?;
        write_cons(heap, head, tail).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a float on the calling process heap.
    pub fn alloc_float(&mut self, value: f64) -> Result<Term, Term> {
        let heap = self.alloc_words(2)?;
        write_float(heap, value).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate an inline binary on the calling process heap.
    pub fn alloc_binary(&mut self, bytes: &[u8]) -> Result<Term, Term> {
        let words = 2 + packed_word_count(bytes.len());
        let heap = self.alloc_words(words)?;
        write_binary(heap, bytes).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a big integer on the calling process heap.
    pub fn alloc_bigint(&mut self, negative: bool, limbs: &[u64]) -> Result<Term, Term> {
        let words = 3 + limbs.len();
        let heap = self.alloc_words(words)?;
        write_bigint(heap, negative, limbs).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a proper list on the calling process heap.
    pub fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term> {
        self.alloc_list_with_tail(elements, Term::NIL)
    }

    /// Allocate list cells for `elements`, ending in `tail`.
    pub fn alloc_list_with_tail(
        &mut self,
        elements: &[Term],
        mut tail: Term,
    ) -> Result<Term, Term> {
        self.ensure_heap_space(elements.len() * 2)?;
        for element in elements.iter().rev().copied() {
            let heap = self.alloc_words(2)?;
            tail = write_cons(heap, element, tail)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        }
        Ok(tail)
    }

    /// Allocate a flatmap on the calling process heap.
    pub fn alloc_map(&mut self, keys: &[Term], values: &[Term]) -> Result<Term, Term> {
        let words = 2 + keys.len() + values.len();
        let heap = self.alloc_words(words)?;
        write_map(heap, keys, values).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::process::Process;
    use crate::term::binary::Binary;
    use crate::term::boxed::{Cons, Float, Map, Tuple};

    fn heap_context(process: &mut Process) -> ProcessContext<'_> {
        let mut context = ProcessContext::new();
        context.attach_process(process, 0);
        context
    }

    fn assert_on_heap(heap: &crate::process::heap::Heap, term: Term) {
        let ptr = term.heap_ptr().expect("boxed/list term has heap pointer");
        assert!(heap.contains(ptr));
    }

    #[test]
    fn allocation_helpers_write_valid_terms_on_process_heap() {
        let mut process = Process::new(1, 32);
        let tuple = {
            let mut context = heap_context(&mut process);
            let float = context.alloc_float(1.5).expect("float allocation");
            let binary = context.alloc_binary(b"beamr").expect("binary allocation");
            let list = context
                .alloc_list(&[Term::small_int(1), Term::small_int(2)])
                .expect("list allocation");
            let map = context
                .alloc_map(&[Term::atom(Atom::OK)], &[binary])
                .expect("map allocation");
            let bigint = context
                .alloc_bigint(false, &[u64::MAX])
                .expect("bigint allocation");
            let tuple = context
                .alloc_tuple(&[float, binary, list, map, bigint])
                .expect("tuple allocation");

            for term in [float, binary, list, map, bigint, tuple] {
                assert_on_heap(context.process_heap().expect("process heap"), term);
            }

            assert_eq!(Float::new(float).expect("float accessor").value(), 1.5);
            assert_eq!(
                Binary::new(binary).expect("binary accessor").as_bytes(),
                b"beamr"
            );
            let cons = Cons::new(list).expect("list accessor");
            assert_eq!(cons.head(), Term::small_int(1));
            assert_eq!(
                Map::new(map)
                    .expect("map accessor")
                    .get(Term::atom(Atom::OK)),
                Some(binary)
            );
            assert_eq!(Tuple::new(tuple).expect("tuple accessor").arity(), 5);
            tuple
        };
        assert_on_heap(process.heap(), tuple);
    }

    #[test]
    fn helpers_fail_without_attached_process() {
        let mut context = ProcessContext::new();
        assert_eq!(
            context.alloc_tuple(&[Term::atom(Atom::OK)]),
            Err(Term::atom(Atom::BADARG))
        );
    }

    #[test]
    fn exception_class_defaults_sets_and_resets_to_error() {
        let mut context = ProcessContext::new();
        assert_eq!(context.take_exception_class(), ExceptionClass::Error);

        context.set_exception_class(ExceptionClass::Throw);
        assert_eq!(context.take_exception_class(), ExceptionClass::Throw);
        assert_eq!(context.take_exception_class(), ExceptionClass::Error);
    }
}
