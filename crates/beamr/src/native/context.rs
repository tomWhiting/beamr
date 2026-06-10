//! Minimal process-facing context exposed to native code.
//!
//! Native functions deliberately receive this allocation subset instead of the
//! full process so they cannot inspect scheduler, mailbox, or process internals.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::atom::AtomTable;
use crate::distribution::control::DistributionSendFacility;
use crate::distribution::pg::PgFacility;
use crate::distribution::remote_link::DistributionControlFacility;
use crate::io::resource::{FD_RESOURCE_WORDS, FdInner, write_fd_resource};
use crate::io::{
    CompletionRing, IoCompletion, IoError, IoFacility, IoOp, IoSink, NullSink, ResultMode,
};
use crate::native::ets_bifs::EtsFoldlState;
use crate::native::otp_stubs::gleam_stubs::{GleamOptionState, GleamResultState};
use crate::native::stdlib_stubs::{lists_hof_bifs::ListsHofState, maps_bifs::MapsHofState};
use crate::process::{Priority, Process};
use crate::replay::ReplayDriver;
use crate::term::Term;
use crate::term::boxed::{
    write_bigint, write_cons, write_external_pid, write_external_reference, write_float, write_map,
    write_reference, write_tuple,
};
use crate::term::compare;
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};
use crate::timer::{TimerRef, TimerWheel};

use super::code_management_bifs::CodeManagementFacility;
use super::distribution_bifs::GlobalNameFacility;
use super::ets_bifs::EtsFacility;
use super::group_leader::GroupLeaderFacility;
use super::io_message::IoMessageFacility;
use super::links::LinkFacility;
use super::process_info_bifs::ProcessInfoFacility;
use super::registry::RegistryFacility;
use super::select::SelectFacility;
use super::spawn::SpawnFacility;
use super::supervision::SupervisionFacility;
use super::system_info_bifs::SystemInfoFacility;

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
    /// Continuation for lists higher-order BIFs.
    Lists(ListsHofState),
    /// Continuation for ets:foldl/3.
    EtsFoldl(EtsFoldlState),
    /// Continuation for Gleam result.try/2 compatibility.
    GleamResultTry,
    /// Continuation for Gleam option higher-order BIFs.
    GleamOption(GleamOptionState),
    /// Continuation for Gleam result higher-order BIFs.
    GleamResult(GleamResultState),
}

/// File I/O continuation data used when a suspended file BIF resumes.
#[derive(Clone, Debug)]
pub enum FileIoContinuation {
    /// `erlang:open_file/2` completion.
    Open,
    /// `erlang:close_file/1` completion.
    Close { fd: Arc<FdInner> },
    /// `erlang:read_file/2` completion.
    Read { fd: Option<Arc<FdInner>> },
    /// `erlang:write_file/2` completion.
    Write {
        fd: Option<Arc<FdInner>>,
        expected_len: usize,
    },
    /// `erlang:file_seek/3` EOF completion.
    SeekEof { fd: Arc<FdInner>, offset: i64 },
    /// `erlang:file_info/1` completion.
    FileInfo,
    /// `erlang:list_dir/1` completion.
    ListDir,
    /// `erlang:make_dir/1` completion.
    MakeDir,
    /// `erlang:del_file/1` completion.
    DelFile,
    /// `erlang:del_dir/1` completion.
    DelDir,
    /// `erlang:rename/2` completion.
    Rename,
    /// `erlang:tcp_accept/1,2` completion.
    Accept,
    /// `erlang:udp_send/4` completion.
    UdpSend { expected_len: usize },
    /// `erlang:udp_recv/2,3` completion.
    UdpRecv,
    /// Active-mode UDP receive (scheduler-driven, not BIF-resumed).
    UdpActiveRecv { fd: Arc<FdInner> },
    /// Active-mode TCP receive (scheduler-driven, not BIF-resumed).
    TcpActiveRecv { fd: Arc<FdInner> },
    /// `erlang:tcp_connect/3` completion.
    Connect { fd: Arc<FdInner> },
    /// `erlang:tcp_send/2` completion.
    TcpWrite {
        fd: Arc<FdInner>,
        remaining: Vec<u8>,
        bytes_written: usize,
    },
    /// `erlang:tcp_recv/2,3` completion.
    TcpRead {
        fd: Arc<FdInner>,
        requested_len: usize,
        accumulated: Vec<u8>,
        timeout_ms: Option<u64>,
    },
}

/// Completion facility used by file BIFs to submit ring work and retrieve resume completions.
pub trait FileIoFacility: Send + Sync {
    /// Submit an operation for `pid`, tagged with the BIF continuation metadata.
    fn submit_file_io(&self, pid: u64, op: IoOp, continuation: FileIoContinuation) -> u64;

    /// Associate an already-submitted operation with `pid` and continuation metadata.
    fn track_submitted_file_io(&self, pid: u64, op_id: u64, continuation: FileIoContinuation);

    /// Take a completion that woke `pid`, if any.
    fn take_file_io_completion(&self, pid: u64) -> Option<FileIoCompletion>;

    /// Drop pending operations and future completions for `pid` after a timed wait expires.
    fn cancel_pending_file_io_for_pid(&self, pid: u64);

    /// Completion ring used by `FdInner::explicit_close`.
    fn ring(&self) -> &dyn CompletionRing;
}

/// File I/O completion delivered back to a suspended process.
#[derive(Debug)]
pub struct FileIoCompletion {
    /// Operation id returned by the ring.
    pub op_id: u64,
    /// BIF continuation associated with the operation.
    pub continuation: FileIoContinuation,
    /// Backend completion result.
    pub completion: IoCompletion,
}

/// Active TCP read-loop submission facility used by socket option BIFs.
pub trait TcpIoFacility: Send + Sync {
    /// Start an active TCP read loop for `socket` using `buf_len` read buffers.
    fn submit_active_tcp_read(&self, socket: Arc<FdInner>, buf_len: usize) -> Option<u64>;
}

/// Facility used by node-qualified spawn BIFs to request process creation on a remote node.
pub trait RemoteSpawnFacility: Send + Sync {
    /// Send a SPAWN_REQUEST to `node` and return the SPAWN_REPLY PID components.
    fn remote_spawn(
        &self,
        caller_pid: u64,
        node: crate::atom::Atom,
        module: crate::atom::Atom,
        function: crate::atom::Atom,
        args: Vec<Term>,
        options: super::spawn::SpawnOptions,
    ) -> Result<RemoteSpawnResult, RemoteSpawnError>;
}

/// Successful remote spawn reply, ready to allocate as an external PID.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RemoteSpawnResult {
    /// Remote node that owns the spawned process.
    pub node: crate::atom::Atom,
    /// PID number on the remote node.
    pub pid_number: u64,
    /// PID serial on the remote node.
    pub serial: u64,
    /// Monitor reference when spawn_monitor was requested.
    pub monitor_reference: Option<u64>,
}

/// Error returned by remote spawn facilities.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RemoteSpawnError {
    /// No remote spawn facility is available.
    Unavailable,
    /// The remote spawn request failed.
    Failed,
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
    local_node: Option<crate::distribution::Node>,
    net_kernel: Option<Arc<crate::distribution::NetKernel>>,
    distribution_send: Option<Arc<dyn DistributionSendFacility>>,
    process: Option<&'process mut Process>,
    detached_allocations: Vec<Box<[u64]>>,
    live_x: usize,
    timers: Option<Arc<Mutex<TimerWheel>>>,
    atom_table: Option<Arc<AtomTable>>,
    spawn_facility: Option<Arc<dyn SpawnFacility>>,
    remote_spawn_facility: Option<Arc<dyn RemoteSpawnFacility>>,
    link_facility: Option<Arc<dyn LinkFacility>>,
    distribution_control_facility: Option<Arc<dyn DistributionControlFacility>>,
    global_name_facility: Option<Arc<dyn GlobalNameFacility>>,
    group_leader_facility: Option<Arc<dyn GroupLeaderFacility>>,
    supervision_facility: Option<Arc<dyn SupervisionFacility>>,
    code_management_facility: Option<Arc<dyn CodeManagementFacility>>,
    process_info_facility: Option<Arc<dyn ProcessInfoFacility>>,
    registry_facility: Option<Arc<dyn RegistryFacility>>,
    select_facility: Option<Arc<dyn SelectFacility>>,
    system_info_facility: Option<Arc<dyn SystemInfoFacility>>,
    ets_facility: Option<Arc<dyn EtsFacility>>,
    pg_facility: Option<Arc<dyn PgFacility>>,
    io_facility: Option<Arc<dyn IoFacility>>,
    io_message_facility: Option<Arc<dyn IoMessageFacility>>,
    file_io_facility: Option<Arc<dyn FileIoFacility>>,
    tcp_io_facility: Option<Arc<dyn TcpIoFacility>>,
    io_sink: Arc<dyn IoSink>,
    exception_class: ExceptionClass,
    exception_stacktrace: Term,
    shutdown_requested: bool,
    trampoline: Option<TrampolineRequest>,
    suspend: Option<SuspendRequest>,
    replay_driver: Option<Arc<Mutex<ReplayDriver>>>,
}

impl fmt::Debug for ProcessContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessContext")
            .field("pid", &self.pid)
            .field("local_node", &self.local_node)
            .field("net_kernel", &self.net_kernel.as_ref().map(|_| ".."))
            .field(
                "distribution_send",
                &self.distribution_send.as_ref().map(|_| ".."),
            )
            .field("process_heap", &self.process.as_ref().map(|_| ".."))
            .field("live_x", &self.live_x)
            .field("timers", &self.timers)
            .field("atom_table", &self.atom_table.as_ref().map(|_| ".."))
            .field(
                "spawn_facility",
                &self.spawn_facility.as_ref().map(|_| ".."),
            )
            .field(
                "remote_spawn_facility",
                &self.remote_spawn_facility.as_ref().map(|_| ".."),
            )
            .field("link_facility", &self.link_facility.as_ref().map(|_| ".."))
            .field(
                "distribution_control_facility",
                &self.distribution_control_facility.as_ref().map(|_| ".."),
            )
            .field(
                "global_name_facility",
                &self.global_name_facility.as_ref().map(|_| ".."),
            )
            .field(
                "group_leader_facility",
                &self.group_leader_facility.as_ref().map(|_| ".."),
            )
            .field(
                "supervision_facility",
                &self.supervision_facility.as_ref().map(|_| ".."),
            )
            .field(
                "code_management_facility",
                &self.code_management_facility.as_ref().map(|_| ".."),
            )
            .field(
                "process_info_facility",
                &self.process_info_facility.as_ref().map(|_| ".."),
            )
            .field(
                "registry_facility",
                &self.registry_facility.as_ref().map(|_| ".."),
            )
            .field(
                "select_facility",
                &self.select_facility.as_ref().map(|_| ".."),
            )
            .field(
                "system_info_facility",
                &self.system_info_facility.as_ref().map(|_| ".."),
            )
            .field("ets_facility", &self.ets_facility.as_ref().map(|_| ".."))
            .field("pg_facility", &self.pg_facility.as_ref().map(|_| ".."))
            .field("io_facility", &self.io_facility.as_ref().map(|_| ".."))
            .field(
                "io_message_facility",
                &self.io_message_facility.as_ref().map(|_| ".."),
            )
            .field(
                "file_io_facility",
                &self.file_io_facility.as_ref().map(|_| ".."),
            )
            .field(
                "tcp_io_facility",
                &self.tcp_io_facility.as_ref().map(|_| ".."),
            )
            .field("io_sink", &"..")
            .field("exception_class", &self.exception_class)
            .field("shutdown_requested", &self.shutdown_requested)
            .field("trampoline", &self.trampoline)
            .field("suspend", &self.suspend)
            .field("exception_stacktrace", &self.exception_stacktrace)
            .field("replay_driver", &self.replay_driver.as_ref().map(|_| ".."))
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
            local_node: None,
            net_kernel: None,
            distribution_send: None,
            process: None,
            detached_allocations: Vec::new(),
            live_x: 256,
            timers: None,
            atom_table: None,
            spawn_facility: None,
            remote_spawn_facility: None,
            link_facility: None,
            distribution_control_facility: None,
            global_name_facility: None,
            group_leader_facility: None,
            supervision_facility: None,
            code_management_facility: None,
            process_info_facility: None,
            registry_facility: None,
            select_facility: None,
            system_info_facility: None,
            ets_facility: None,
            pg_facility: None,
            io_facility: None,
            io_message_facility: None,
            file_io_facility: None,
            tcp_io_facility: None,
            io_sink: Arc::new(NullSink),
            exception_class: ExceptionClass::Error,
            exception_stacktrace: Term::NIL,
            trampoline: None,
            suspend: None,
            shutdown_requested: false,
            replay_driver: None,
        }
    }

    /// Creates a context with timer services for asynchronous timer BIFs.
    #[must_use]
    pub fn with_timer_services(pid: u64, timers: Arc<Mutex<TimerWheel>>) -> Self {
        Self {
            pid: Some(pid),
            local_node: None,
            net_kernel: None,
            distribution_send: None,
            process: None,
            detached_allocations: Vec::new(),
            live_x: 256,
            timers: Some(timers),
            atom_table: None,
            spawn_facility: None,
            remote_spawn_facility: None,
            link_facility: None,
            distribution_control_facility: None,
            global_name_facility: None,
            group_leader_facility: None,
            supervision_facility: None,
            code_management_facility: None,
            process_info_facility: None,
            registry_facility: None,
            select_facility: None,
            system_info_facility: None,
            ets_facility: None,
            pg_facility: None,
            io_facility: None,
            io_message_facility: None,
            file_io_facility: None,
            tcp_io_facility: None,
            io_sink: Arc::new(NullSink),
            exception_class: ExceptionClass::Error,
            exception_stacktrace: Term::NIL,
            trampoline: None,
            suspend: None,
            shutdown_requested: false,
            replay_driver: None,
        }
    }

    /// Return the replay driver when running under deterministic replay.
    #[must_use]
    pub fn replay_driver(&self) -> Option<&Arc<Mutex<ReplayDriver>>> {
        self.replay_driver.as_ref()
    }

    /// Set the replay driver for native BIFs that consume recorded decisions.
    pub fn set_replay_driver(&mut self, driver: Option<Arc<Mutex<ReplayDriver>>>) {
        self.replay_driver = driver;
    }

    /// Return the calling process id when provided by the runtime.
    #[must_use]
    pub fn pid(&self) -> Option<u64> {
        self.pid
    }

    /// Return the immutable local node identity when provided by the runtime.
    #[must_use]
    pub fn local_node(&self) -> Option<crate::distribution::Node> {
        self.local_node
    }

    /// Set the immutable local node identity for node-aware BIFs.
    pub fn set_local_node(&mut self, node: Option<crate::distribution::Node>) {
        self.local_node = node;
    }

    /// Return the net-kernel distribution facade, if one has been configured.
    #[must_use]
    pub fn net_kernel(&self) -> Option<&crate::distribution::NetKernel> {
        self.net_kernel.as_deref()
    }

    /// Set the net-kernel distribution facade for distribution BIFs.
    pub fn set_net_kernel(&mut self, net_kernel: Option<Arc<crate::distribution::NetKernel>>) {
        self.net_kernel = net_kernel;
    }

    /// Return the distribution send facility, if one has been configured.
    #[must_use]
    pub fn distribution_send_facility(&self) -> Option<&dyn DistributionSendFacility> {
        self.distribution_send.as_deref()
    }

    /// Set the distribution send facility for remote PID messaging.
    pub fn set_distribution_send_facility(
        &mut self,
        facility: Option<Arc<dyn DistributionSendFacility>>,
    ) {
        self.distribution_send = facility;
    }

    /// Returns true when the attached process is re-entering a timed suspend after expiry.
    #[must_use]
    pub fn receive_timeout_expired(&self) -> bool {
        self.process
            .as_ref()
            .is_some_and(|process| process.receive_timeout().is_some())
    }

    /// Clear timed-suspend metadata after a native timed wait has resolved.
    pub fn clear_receive_timeout(&mut self) {
        if let Some(process) = self.process.as_deref_mut() {
            process.set_receive_timeout(None);
            process.set_receive_timer_ref(None);
        }
    }

    /// Cancel any file I/O operation tracked for the attached process.
    pub fn cancel_pending_file_io_for_current_process(&self) {
        if let (Some(pid), Some(facility)) = (self.pid, self.file_io_facility.as_ref()) {
            facility.cancel_pending_file_io_for_pid(pid);
        }
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

    /// Return the attached calling process for native operations that must use process APIs.
    pub fn process_mut(&mut self) -> Option<&mut Process> {
        self.process.as_deref_mut()
    }

    /// Enqueue a message to the attached calling process when `target` is its pid.
    pub fn send_to_attached_self(&mut self, target: u64, message: Term) -> bool {
        let Some(process) = self.process.as_deref_mut() else {
            return false;
        };
        if process.pid() != target {
            return false;
        }
        process.mailbox_mut().push_owned(message);
        true
    }

    /// Ensure the calling process has at least `words` nursery words available.
    pub fn ensure_heap_space(&mut self, words: usize) -> Result<(), Term> {
        let Some(process) = self.process.as_deref_mut() else {
            let _ = words;
            return Ok(());
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

    /// Return the remote spawn facility, if one has been configured.
    #[must_use]
    pub fn remote_spawn_facility(&self) -> Option<&dyn RemoteSpawnFacility> {
        self.remote_spawn_facility.as_deref()
    }

    /// Set the remote spawn facility for node-qualified spawn BIFs.
    pub fn set_remote_spawn_facility(&mut self, facility: Option<Arc<dyn RemoteSpawnFacility>>) {
        self.remote_spawn_facility = facility;
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

    /// Return the distribution control facility, if one has been configured.
    #[must_use]
    pub fn distribution_control_facility(&self) -> Option<&dyn DistributionControlFacility> {
        self.distribution_control_facility.as_deref()
    }

    /// Set the distribution control facility for remote link lifecycle BIFs.
    pub fn set_distribution_control_facility(
        &mut self,
        facility: Option<Arc<dyn DistributionControlFacility>>,
    ) {
        self.distribution_control_facility = facility;
    }

    /// Return the global name facility, if one has been configured.
    #[must_use]
    pub fn global_name_facility(&self) -> Option<&dyn GlobalNameFacility> {
        self.global_name_facility.as_deref()
    }

    /// Set the global name facility for `global:*_name` BIFs.
    pub fn set_global_name_facility(&mut self, facility: Option<Arc<dyn GlobalNameFacility>>) {
        self.global_name_facility = facility;
    }

    /// Return the group-leader facility, if one has been configured.
    #[must_use]
    pub fn group_leader_facility(&self) -> Option<&dyn GroupLeaderFacility> {
        self.group_leader_facility.as_deref()
    }

    /// Set the group-leader facility for process metadata BIFs.
    pub fn set_group_leader_facility(&mut self, facility: Option<Arc<dyn GroupLeaderFacility>>) {
        self.group_leader_facility = facility;
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

    /// Return the process-info facility, if one has been configured.
    #[must_use]
    pub fn process_info_facility(&self) -> Option<&dyn ProcessInfoFacility> {
        self.process_info_facility.as_deref()
    }

    /// Set the process-info facility for process introspection BIFs.
    pub fn set_process_info_facility(&mut self, facility: Option<Arc<dyn ProcessInfoFacility>>) {
        self.process_info_facility = facility;
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

    // --- System info facility ---

    /// Return the system-info facility, if one has been configured.
    #[must_use]
    pub fn system_info_facility(&self) -> Option<&dyn SystemInfoFacility> {
        self.system_info_facility.as_deref()
    }

    /// Set the system-info facility for VM introspection BIFs.
    pub fn set_system_info_facility(&mut self, facility: Option<Arc<dyn SystemInfoFacility>>) {
        self.system_info_facility = facility;
    }

    // --- ETS facility ---

    /// Return the ETS facility, if one has been configured.
    #[must_use]
    pub fn ets_facility(&self) -> Option<&dyn EtsFacility> {
        self.ets_facility.as_deref()
    }

    /// Set the ETS facility for `ets` module BIFs.
    pub fn set_ets_facility(&mut self, facility: Option<Arc<dyn EtsFacility>>) {
        self.ets_facility = facility;
    }

    // --- PG facility ---

    /// Return the pg facility, if one has been configured.
    #[must_use]
    pub fn pg_facility(&self) -> Option<&dyn PgFacility> {
        self.pg_facility.as_deref()
    }

    /// Set the pg facility for process group BIFs.
    pub fn set_pg_facility(&mut self, facility: Option<Arc<dyn PgFacility>>) {
        self.pg_facility = facility;
    }

    // --- I/O facility ---

    /// Return the async I/O facility, if one has been configured.
    #[must_use]
    pub fn io_facility(&self) -> Option<&dyn IoFacility> {
        self.io_facility.as_deref()
    }

    /// Set the async I/O facility for I/O BIFs.
    pub fn set_io_facility(&mut self, facility: Option<Arc<dyn IoFacility>>) {
        self.io_facility = facility;
    }

    // --- IO message facility ---

    /// Return the IO message facility, if one has been configured.
    #[must_use]
    pub fn io_message_facility(&self) -> Option<&dyn IoMessageFacility> {
        self.io_message_facility.as_deref()
    }

    /// Set the IO message facility for group-leader protocol BIFs.
    pub fn set_io_message_facility(&mut self, facility: Option<Arc<dyn IoMessageFacility>>) {
        self.io_message_facility = facility;
    }

    /// Submit an I/O operation for the attached pid and request suspension.
    pub fn submit_io_and_suspend(&mut self, op: IoOp, mode: ResultMode) -> Result<(), IoError> {
        let pid = self.pid.ok_or(IoError::MissingPid)?;
        let Some(facility) = self.io_facility.as_ref() else {
            return Err(IoError::Unavailable);
        };
        facility.submit_and_suspend_for_pid(pid, op, mode)?;
        self.request_suspend(None);
        Ok(())
    }

    // --- File I/O facility ---

    /// Return the file I/O facility, if one has been configured.
    #[must_use]
    pub fn file_io_facility(&self) -> Option<&dyn FileIoFacility> {
        self.file_io_facility.as_deref()
    }

    /// Set the file I/O facility for completion-ring backed file BIFs.
    pub fn set_file_io_facility(&mut self, facility: Option<Arc<dyn FileIoFacility>>) {
        self.file_io_facility = facility;
    }

    // --- TCP I/O facility ---

    /// Return the TCP I/O facility, if one has been configured.
    #[must_use]
    pub fn tcp_io_facility(&self) -> Option<&dyn TcpIoFacility> {
        self.tcp_io_facility.as_deref()
    }

    /// Set the TCP I/O facility for active-mode socket BIFs.
    pub fn set_tcp_io_facility(&mut self, facility: Option<Arc<dyn TcpIoFacility>>) {
        self.tcp_io_facility = facility;
    }

    /// Submit a file I/O operation and suspend the calling process until completion.
    pub fn submit_file_io(
        &mut self,
        op: IoOp,
        continuation: FileIoContinuation,
    ) -> Result<u64, Term> {
        self.submit_file_io_with_timeout(op, continuation, None)
    }

    /// Submit a file I/O operation and suspend the calling process until completion or timeout.
    pub fn submit_file_io_with_timeout(
        &mut self,
        op: IoOp,
        continuation: FileIoContinuation,
        timeout_ms: Option<u64>,
    ) -> Result<u64, Term> {
        let pid = self
            .pid
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        let facility = self
            .file_io_facility
            .as_ref()
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        let op_id = facility.submit_file_io(pid, op, continuation);
        self.request_suspend(timeout_ms);
        Ok(op_id)
    }

    /// Associate an already-submitted file I/O operation with this process.
    pub fn track_submitted_file_io(
        &mut self,
        op_id: u64,
        continuation: FileIoContinuation,
    ) -> Result<(), Term> {
        let pid = self
            .pid
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        let facility = self
            .file_io_facility
            .as_ref()
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        facility.track_submitted_file_io(pid, op_id, continuation);
        Ok(())
    }

    /// Take the completion used to resume the currently executing file BIF.
    pub fn take_file_io_completion(&self) -> Option<FileIoCompletion> {
        let pid = self.pid?;
        self.file_io_facility.as_ref()?.take_file_io_completion(pid)
    }

    /// Return the completion ring backing file I/O resources.
    #[must_use]
    pub fn file_completion_ring(&self) -> Option<&dyn CompletionRing> {
        self.file_io_facility
            .as_ref()
            .map(|facility| facility.ring())
    }

    /// Store a value in the attached process dictionary.
    pub fn dict_put(&mut self, key: Term, value: Term) -> Result<Term, Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_put(key, value))
    }

    /// Return the attached process group leader.
    pub fn group_leader(&self) -> Result<Term, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.group_leader())
    }

    /// Return the attached process scheduling priority.
    pub fn priority(&self) -> Result<Priority, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.priority())
    }

    /// Set the attached process scheduling priority and return its old value.
    pub fn set_priority(&mut self, priority: Priority) -> Result<Priority, Term> {
        let Some(process) = self.process.as_deref_mut() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        let old_priority = process.priority();
        process.set_priority(priority);
        Ok(old_priority)
    }

    /// Set the attached process group leader when it matches `pid`.
    pub fn set_attached_group_leader(&mut self, pid: u64, group_leader: Term) -> bool {
        let Some(process) = self.process.as_deref_mut() else {
            return false;
        };
        if process.pid() != pid {
            return false;
        }
        process.set_group_leader(group_leader);
        true
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

    /// Count attached process dictionary entries without copying their terms.
    pub fn dict_len(&self) -> Result<usize, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process.dict_get_all().len())
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

    /// Count dictionary keys whose values exactly match `value` without copying terms.
    pub fn dict_count_keys_for_value(&self, value: Term) -> Result<usize, Term> {
        let Some(process) = self.process.as_ref() else {
            return Err(Term::atom(crate::atom::Atom::BADARG));
        };
        Ok(process
            .dict_get_all()
            .iter()
            .filter(|(_, existing_value)| compare::exact_eq(*existing_value, value))
            .count())
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
        self.alloc_words_prereserved(words)
    }

    /// Keep detached allocations alive by moving them into an owned term.
    ///
    /// Dirty native calls run without an attached process heap. Terms allocated
    /// in that detached context point into `detached_allocations`, so the dirty
    /// completion path must preserve those allocations until it can copy the
    /// returned term onto the resuming process heap.
    pub fn take_detached_result(&mut self, root: Term) -> Option<crate::ets::OwnedTerm> {
        if self.detached_allocations.is_empty() {
            None
        } else {
            Some(crate::ets::OwnedTerm::from_allocations(
                root,
                std::mem::take(&mut self.detached_allocations),
            ))
        }
    }

    /// Allocate heap words WITHOUT triggering GC. Caller must have already
    /// called `ensure_heap_space` for the total allocation budget. Panics
    /// (via alloc_slice error) if insufficient space remains.
    fn alloc_words_prereserved(&mut self, words: usize) -> Result<&mut [u64], Term> {
        if let Some(process) = self.process.as_deref_mut() {
            return process
                .heap_mut()
                .alloc_slice(words)
                .map_err(|_| Term::atom(crate::atom::Atom::BADARG));
        }

        self.detached_allocations
            .push(vec![0; words].into_boxed_slice());
        self.detached_allocations
            .last_mut()
            .map(|words| words.as_mut())
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a tuple on the calling process heap.
    pub fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let words = 1 + elements.len();
        let heap = self.alloc_words(words)?;
        write_tuple(heap, elements).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a tuple using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_tuple_prereserved(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let words = 1 + elements.len();
        let heap = self.alloc_words_prereserved(words)?;
        write_tuple(heap, elements).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a reference on the calling process heap.
    pub fn alloc_reference(&mut self, id: u64) -> Result<Term, Term> {
        let heap = self.alloc_words(2)?;
        write_reference(heap, id).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a reference using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_reference_prereserved(&mut self, id: u64) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(2)?;
        write_reference(heap, id).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote PID on the calling process heap.
    pub fn alloc_external_pid(
        &mut self,
        node: crate::atom::Atom,
        pid_number: u64,
        serial: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words(4)?;
        write_external_pid(heap, node, pid_number, serial)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote PID using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_external_pid_prereserved(
        &mut self,
        node: crate::atom::Atom,
        pid_number: u64,
        serial: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(4)?;
        write_external_pid(heap, node, pid_number, serial)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote reference on the calling process heap.
    pub fn alloc_external_reference(
        &mut self,
        node: crate::atom::Atom,
        id: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words(3)?;
        write_external_reference(heap, node, id)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a remote reference using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_external_reference_prereserved(
        &mut self,
        node: crate::atom::Atom,
        id: u64,
    ) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(3)?;
        write_external_reference(heap, node, id)
            .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
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

    /// Allocate a binary on the calling process heap, promoting large binaries to ProcBin.
    pub fn alloc_binary(&mut self, bytes: &[u8]) -> Result<Term, Term> {
        let words = alloc_binary_word_count(bytes.len());
        let heap = self.alloc_words(words)?;
        alloc_binary(heap, bytes).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate an FdResource on the calling process heap.
    pub fn alloc_fd_resource(&mut self, fd_inner: Arc<FdInner>) -> Result<Term, Term> {
        let heap = self.alloc_words(FD_RESOURCE_WORDS)?;
        write_fd_resource(heap, fd_inner).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
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
    ///
    /// SAFETY NOTE: `ensure_heap_space` at the start may trigger GC which
    /// moves heap objects. If `elements` contains boxed Terms (heap pointers),
    /// they become stale after GC. Callers with boxed Terms MUST save them to
    /// x-registers before calling this method and re-read after GC. For
    /// immediate-only Terms (atoms, small ints, pids), this is safe as-is.
    pub fn alloc_list_with_tail(
        &mut self,
        elements: &[Term],
        mut tail: Term,
    ) -> Result<Term, Term> {
        self.ensure_heap_space(elements.len() * 2)?;
        for element in elements.iter().rev().copied() {
            let heap = self.alloc_words_prereserved(2)?;
            tail = write_cons(heap, element, tail)
                .ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))?;
        }
        Ok(tail)
    }

    /// Allocate a cons cell using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_cons_prereserved(&mut self, head: Term, tail: Term) -> Result<Term, Term> {
        let heap = self.alloc_words_prereserved(2)?;
        write_cons(heap, head, tail).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a flatmap on the calling process heap.
    pub fn alloc_map(&mut self, keys: &[Term], values: &[Term]) -> Result<Term, Term> {
        let words = 2 + keys.len() + values.len();
        let heap = self.alloc_words(words)?;
        write_map(heap, keys, values).ok_or_else(|| Term::atom(crate::atom::Atom::BADARG))
    }

    /// Allocate a flatmap using pre-reserved heap space (no GC trigger).
    /// Caller must have called `ensure_heap_space` for the total budget.
    pub fn alloc_map_prereserved(&mut self, keys: &[Term], values: &[Term]) -> Result<Term, Term> {
        let words = 2 + keys.len() + values.len();
        let heap = self.alloc_words_prereserved(words)?;
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
    fn detached_context_allocations_are_owned_until_taken() {
        let mut context = ProcessContext::new();
        let tuple = context
            .alloc_tuple(&[Term::atom(Atom::OK)])
            .expect("detached tuple allocation");
        assert_eq!(Tuple::new(tuple).expect("tuple accessor").arity(), 1);

        let owned = context
            .take_detached_result(tuple)
            .expect("detached allocation ownership");
        assert_eq!(owned.allocation_count(), 1);
        assert!(context.take_detached_result(Term::NIL).is_none());
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
