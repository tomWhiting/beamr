// Cooperative-runtime modules. The single-threaded `WasmScheduler` and its
// native-slice driver do not depend on the threaded scheduler, so they (and the
// shared `exit_capture` term helper they reuse) build under `cooperative`.
pub mod exit_capture;
pub mod wasm;
mod wasm_native;
pub use exit_capture::OwnedException;
pub use wasm::{WasmAsyncCompletion, WasmRunSummary, WasmScheduledTimer, WasmScheduler};

/// Default preemption budget for a process slice, shared by both the threaded
/// and the cooperative scheduler.
pub const DEFAULT_REDUCTION_BUDGET: u32 = crate::process::DEFAULT_REDUCTION_BUDGET;

/// Distinguishes the two BEAM-style dirty scheduler pools.
///
/// This is pure call-classification metadata carried on every `NativeEntry`, so
/// it must exist in every build (the cooperative build has no dirty *pool*, but
/// the registry types that reference this enum are platform-neutral).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirtySchedulerKind {
    /// CPU-bound dirty work.
    Cpu,
    /// IO-bound dirty work.
    Io,
}

/// Result returned by a successful hot module load.
///
/// Plain metadata returned by the threaded code server. It is named in the
/// platform-neutral `CodeManagementFacility` trait, so it is defined here (always
/// available) even though only the threaded scheduler can produce one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HotLoadResult {
    pub module_name: crate::atom::Atom,
    pub generation: u64,
    pub had_old_version: bool,
    pub on_load_required: bool,
    pub on_load_succeeded: bool,
}

/// Result returned by safe or forced module purge.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PurgeResult {
    pub module_name: crate::atom::Atom,
    pub processes_killed: usize,
}

// ---------------------------------------------------------------------------
// Threaded (work-stealing, OS-thread) scheduler. Everything below requires the
// `threads` feature: it pulls in crossbeam, the io/jit/replay/distribution
// subsystems, and `std::thread`/`Condvar`, none of which exist on wasm32.
// ---------------------------------------------------------------------------
#[cfg(feature = "threads")]
pub mod dirty;
#[cfg(feature = "threads")]
mod execution;
#[cfg(feature = "threads")]
mod exit_tombstones;
#[cfg(feature = "threads")]
mod module_management;
#[cfg(feature = "threads")]
mod pg_propagation;
#[cfg(feature = "threads")]
mod process_slot;
#[cfg(feature = "threads")]
pub mod run_queue;
#[cfg(feature = "threads")]
mod spawning;
#[cfg(feature = "threads")]
pub mod steal;
#[cfg(feature = "threads")]
mod supervision_integration;
#[cfg(feature = "threads")]
mod suspension;
#[cfg(feature = "threads")]
mod test_helpers;
#[cfg(feature = "threads")]
mod timer_integration;
#[cfg(feature = "threads")]
use self::dirty::DirtyPool;
#[cfg(feature = "threads")]
use self::execution::scheduler_loop;
#[cfg(feature = "threads")]
use self::spawning::SpawnRequest;
#[cfg(feature = "threads")]
use crate::atom::AtomTable;
#[cfg(feature = "threads")]
use crate::distribution::DistributionConfig;
#[cfg(feature = "threads")]
use crate::distribution::connection::{ConnectionManager, HeartbeatConfig};
#[cfg(feature = "threads")]
use crate::distribution::pg::PgRegistry;
#[cfg(feature = "threads")]
use crate::distribution::remote_link::ControlRouter;
#[cfg(feature = "threads")]
use crate::distribution::sender::DistSender;
#[cfg(feature = "threads")]
use crate::distribution::{DEFAULT_NODE_NAME, NetKernel, Node};

#[cfg(feature = "threads")]
use crate::error::ExecError;
#[cfg(feature = "threads")]
use crate::ets::copy::OwnedTerm;
#[cfg(feature = "threads")]
use crate::ets::{EtsRegistry, EtsTable, EtsTableId, EtsTableMetadata};
#[cfg(feature = "threads")]
use crate::hook::Hook;
#[cfg(feature = "threads")]
use crate::io::{
    CompletionRing, CompletionRingIoFacility, IoCompletion, IoCompletionBridge, IoFacility, IoOp,
    IoSink, IoWakeTarget, NullSink, PendingIoRegistry, RingConfig, StandardIoServer, create_ring,
};
#[cfg(feature = "threads")]
use crate::jit::{DEFAULT_JIT_THRESHOLD, JitCache, JitProfiler};
#[cfg(feature = "threads")]
use crate::module::ModuleRegistry;
#[cfg(feature = "threads")]
use crate::namespace::NamespaceId;
#[cfg(feature = "threads")]
use crate::native::{
    AllCapabilitiesPolicy, BifRegistryImpl, CapabilityPolicy, FileIoCompletion, FileIoContinuation,
    ProcessInfoItem, ProcessInfoStatus, ProcessInfoValue, ProcessMonitorInfo,
};
#[cfg(feature = "threads")]
use crate::process::registry::ProcessTable;
#[cfg(feature = "threads")]
use crate::process::{ExitReason, Process, ProcessStatus};
#[cfg(feature = "threads")]
use crate::replay::{ReplayDriver, ReplayLog};
#[cfg(feature = "threads")]
use crate::supervision::link::LinkSet;
#[cfg(feature = "threads")]
use crate::supervision::monitor::MonitorSet;
#[cfg(feature = "threads")]
use crate::term::Term;
#[cfg(feature = "threads")]
use crate::timer::TimerWheel;
#[cfg(feature = "threads")]
use crossbeam_queue::SegQueue;
#[cfg(feature = "threads")]
use dashmap::{DashMap, DashSet};
#[cfg(feature = "threads")]
use process_slot::{ProcessMetadata, ProcessSlot};
#[cfg(feature = "threads")]
use run_queue::{PriorityStealers, RunQueue};
#[cfg(feature = "threads")]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "threads")]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(feature = "threads")]
use std::thread::JoinHandle;
#[cfg(feature = "threads")]
use std::time::Duration;
#[cfg(all(feature = "threads", feature = "telemetry"))]
use std::time::Instant;

#[cfg(feature = "threads")]
enum ReplayMode {
    Live,
    Replay(ReplayLog),
}

#[cfg(feature = "threads")]
#[derive(Default)]
struct ReplayDisabledRing {
    next_op_id: AtomicU64,
}

#[cfg(feature = "threads")]
impl CompletionRing for ReplayDisabledRing {
    fn submit(&self, _op: IoOp) -> u64 {
        self.next_op_id.fetch_add(1, Ordering::Relaxed)
    }

    fn poll_completions(&self, _timeout: Duration) -> Vec<IoCompletion> {
        Vec::new()
    }

    fn pending_count(&self) -> usize {
        0
    }

    fn shutdown(&self) {}
}

#[cfg(feature = "threads")]
#[derive(Clone, Default)]
pub struct SchedulerConfig {
    pub thread_count: Option<usize>,
    pub dirty_cpu_threads: Option<usize>,
    pub dirty_io_threads: Option<usize>,
    pub dirty_queue_depth: Option<usize>,
    pub io: Option<RingConfig>,
    pub node_name: Option<String>,
    pub creation: Option<u32>,
    pub distribution: Option<DistributionConfig>,
    pub jit_threshold: Option<u32>,
    /// Minimum interval between per-process telemetry samples at scheduler slice boundaries.
    pub telemetry_sample_interval: Option<Duration>,
    /// Embedder-supplied private data handed to every native call via
    /// [`crate::native::ProcessContext::nif_private_data`] — the ERTS
    /// `enif_priv_data` equivalent, scoped to this scheduler instance so
    /// embedders hosting several runtimes in one OS process never need
    /// process-wide globals.
    pub nif_private_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

#[cfg(feature = "threads")]
impl std::fmt::Debug for SchedulerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerConfig")
            .field("thread_count", &self.thread_count)
            .field("dirty_cpu_threads", &self.dirty_cpu_threads)
            .field("dirty_io_threads", &self.dirty_io_threads)
            .field("dirty_queue_depth", &self.dirty_queue_depth)
            .field("io", &self.io)
            .field("node_name", &self.node_name)
            .field("creation", &self.creation)
            .field("distribution", &self.distribution)
            .field("jit_threshold", &self.jit_threshold)
            .field("telemetry_sample_interval", &self.telemetry_sample_interval)
            .field(
                "nif_private_data",
                &self.nif_private_data.as_ref().map(|_| ".."),
            )
            .finish()
    }
}
#[cfg(feature = "threads")]
pub(super) struct SharedState {
    shutdown: AtomicBool,
    process_table: ProcessTable,
    module_registry: Arc<ModuleRegistry>,
    namespace_store: DashMap<NamespaceId, Arc<ModuleRegistry>>,
    next_namespace_id: AtomicU64,
    atom_table: Arc<AtomTable>,
    local_node: Node,
    net_kernel: Arc<NetKernel>,
    ets_registry: Arc<EtsRegistry>,
    pg_registry: Arc<PgRegistry>,
    bif_registry: Arc<BifRegistryImpl>,
    capability_policy: Arc<dyn CapabilityPolicy>,
    spawn_counter: AtomicUsize,
    thread_count: usize,
    pub(super) dirty_cpu: DirtyPool,
    pub(super) dirty_io: DirtyPool,
    jit_profiler: Arc<JitProfiler>,
    jit_cache: Arc<JitCache>,
    next_pid: AtomicU64,
    wait_set: Mutex<WaitSet>,
    wake_condvar: Condvar,
    process_bodies: DashMap<u64, Mutex<ProcessSlot>>,
    exit_tombstones: exit_tombstones::BoundedTombstones,
    exit_results: DashMap<u64, OwnedTerm>,
    exit_errors: DashMap<u64, ExecError>,
    exit_exceptions: DashMap<u64, OwnedException>,
    /// pid → current result-gated suspension identity (call id + kind).
    /// Owning-thread written; read by completion publishers and the wake
    /// gate. See `suspension.rs`.
    suspensions: DashMap<u64, suspension::SuspensionMirror>,
    /// pid → completion published for a specific suspension call id. The
    /// owning thread applies it at slice start only when the id matches the
    /// process's current suspension record.
    suspension_results: DashMap<u64, suspension::PendingSuspensionResult>,
    /// pid → sticky embedder resume for a hook suspension (call id, or
    /// `RESUME_ANY_HOOK` when the resume raced the suspension's creation).
    pending_resumes: DashMap<u64, u64>,
    file_io_ring: Arc<dyn CompletionRing>,
    file_io_pending: DashMap<u64, (u64, FileIoContinuation)>,
    file_io_orphans: DashMap<u64, IoCompletion>,
    file_io_results: DashMap<u64, FileIoCompletion>,
    file_io_canceled: DashSet<u64>,
    link_set: Mutex<LinkSet>,
    monitor_set: Mutex<MonitorSet>,
    hook: Hook,
    distribution: DistributionConfig,
    distribution_connections: ConnectionManager,
    /// Async outbound distribution sender. `None` under replay (no runtime).
    /// Holds the `ConnectionManager`, never `Arc<SharedState>`, so it does not
    /// form a reference cycle with the scheduler.
    dist_sender: Option<DistSender>,
    control_router: ControlRouter,
    process_registry: DashMap<crate::atom::Atom, u64>,
    timers: Arc<Mutex<TimerWheel>>,
    /// Receive timers that fired but could not be applied in place: pid →
    /// fired timer ids. `expire_timers` only marks and wakes; the woken
    /// process applies the timeout jump itself at the start of its next
    /// slice (and drops stale ids whose receive completed first). This keeps
    /// the timeout-label jump on the owning thread, so it can never race a
    /// slot that is `Executing` or a park gap.
    expired_receive_timers: DashMap<u64, Vec<u64>>,
    output_sink: Mutex<Arc<dyn IoSink>>,
    io_ring: Option<Arc<dyn CompletionRing>>,
    io_registry: Option<Arc<PendingIoRegistry>>,
    io_bridge: Mutex<Option<IoCompletionBridge>>,
    io_facility: Option<Arc<dyn IoFacility>>,
    standard_io_pid: u64,
    replay_driver: Option<Arc<Mutex<ReplayDriver>>>,
    replay_mode: bool,
    pub(super) nif_private_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
    #[cfg(feature = "telemetry")]
    telemetry_metrics: TelemetryMetricState,

    // Kept for ownership: dropping SharedState must also stop the backing standard I/O server.
    #[allow(dead_code)]
    _standard_io_server: StandardIoServer,

    #[cfg(test)]
    idle_parks: AtomicUsize,

    #[cfg(test)]
    park_gap_hook: Mutex<Option<ParkGapHook>>,
}

#[cfg(feature = "threads")]
#[cfg(feature = "telemetry")]
pub(super) struct TelemetryMetricState {
    sample_interval: Duration,
    last_process_samples: Mutex<std::collections::HashMap<u64, Instant>>,
    scheduler_executing_nanos: AtomicU64,
    scheduler_idle_nanos: AtomicU64,
}

#[cfg(feature = "threads")]
#[cfg(feature = "telemetry")]
impl TelemetryMetricState {
    fn new(sample_interval: Duration) -> Self {
        Self {
            sample_interval,
            last_process_samples: Mutex::new(std::collections::HashMap::new()),
            scheduler_executing_nanos: AtomicU64::new(0),
            scheduler_idle_nanos: AtomicU64::new(0),
        }
    }
}

#[cfg(feature = "threads")]
impl SharedState {
    /// Insert an exit tombstone for `pid`, evicting the oldest tombstone (and
    /// its paired satellite entries) if the bounded store is over capacity.
    ///
    /// This is the single write path for tombstones. Eviction removes the
    /// evicted pid's `exit_results` / `exit_errors` / `exit_exceptions` along
    /// with its tombstone, so a satellite can never outlive the tombstone it
    /// pairs with and the "tombstone observed ⇒ paired result already present"
    /// invariant the readers rely on is preserved.
    pub(super) fn insert_exit_tombstone(&self, pid: u64, reason: ExitReason) {
        if let Some(evicted) = self.exit_tombstones.insert(pid, reason) {
            self.exit_results.remove(&evicted);
            self.exit_errors.remove(&evicted);
            self.exit_exceptions.remove(&evicted);
        }
    }

    pub(super) fn create_table(&self, metadata: EtsTableMetadata) -> EtsTableId {
        self.ets_registry.create_table(metadata)
    }

    pub(super) fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.ets_registry.lookup_table(id)
    }

    pub(super) fn lookup_table_by_name(&self, name: crate::atom::Atom) -> Option<EtsTableId> {
        self.ets_registry.lookup_table_by_name(name)
    }

    pub(super) fn delete_table(&self, id: EtsTableId) -> bool {
        self.ets_registry.delete_table(id)
    }

    pub(super) fn transfer_or_delete_tables_owned_by(&self, owner: u64) -> usize {
        let before = self.ets_registry.table_count();
        let owned_ids = self.ets_registry.table_ids_owned_by(owner);
        for table_id in owned_ids {
            let Some(table) = self.ets_registry.lookup_table(table_id) else {
                continue;
            };
            let Some(heir) = &table.metadata().heir else {
                let _deleted = self.ets_registry.delete_table(table_id);
                continue;
            };
            if self.process_table.get(heir.pid).is_some()
                && supervision_integration::deliver_ets_transfer(
                    self,
                    heir.pid,
                    table_id,
                    owner,
                    heir.data.root(),
                    &self.atom_table,
                )
                && self.ets_registry.transfer_table_owner(table_id, heir.pid)
            {
                continue;
            }
            let _deleted = self.ets_registry.delete_table(table_id);
        }
        before.saturating_sub(self.ets_registry.table_count())
    }

    /// Return the number of alive processes tracked by the scheduler.
    #[must_use]
    pub(super) fn process_count(&self) -> usize {
        self.process_table.len()
    }

    /// Return the configured number of normal scheduler threads.
    #[must_use]
    pub(super) fn scheduler_count(&self) -> usize {
        self.thread_count
    }

    /// Return the current number of interned atoms.
    #[must_use]
    pub(super) fn atom_count(&self) -> usize {
        self.atom_table.len()
    }

    /// Return an approximate memory summary for OTP compatibility probes.
    #[must_use]
    pub(super) fn memory_summary(&self) -> crate::native::system_info_bifs::MemorySummary {
        let (process_heap_words, binary) = self.process_heap_and_binary_words();

        let processes =
            process_heap_words.saturating_mul(crate::native::system_info_bifs::WORDSIZE_BYTES);
        let atom = self
            .atom_count()
            .saturating_mul(crate::native::system_info_bifs::WORDSIZE_BYTES);
        crate::native::system_info_bifs::MemorySummary::from_components(processes, atom, binary)
    }

    /// Return approximate process heap and virtual binary memory words.
    #[must_use]
    pub(super) fn process_heap_and_binary_words(&self) -> (usize, usize) {
        let mut process_heap_words = 0usize;
        let mut binary = 0usize;

        for entry in &self.process_bodies {
            match &*lock_or_recover(&entry) {
                ProcessSlot::Present(scheduled) => {
                    if matches!(scheduled.0.status(), ProcessStatus::Exited(_)) {
                        continue;
                    }
                    process_heap_words =
                        process_heap_words.saturating_add(scheduled.0.heap().total_used());
                    binary = binary.saturating_add(scheduled.0.virtual_binary_heap());
                }
                ProcessSlot::Executing(metadata) => {
                    process_heap_words = process_heap_words.saturating_add(metadata.heap_size);
                    binary = binary.saturating_add(metadata.binary_heap_size);
                }
                ProcessSlot::Absent => {}
            }
        }

        (process_heap_words, binary)
    }

    #[cfg(feature = "telemetry")]
    pub(super) fn record_scheduler_executing(&self, duration: Duration) {
        self.add_scheduler_duration(&self.telemetry_metrics.scheduler_executing_nanos, duration);
        self.record_vm_health_metrics();
    }

    #[cfg(feature = "telemetry")]
    pub(super) fn record_scheduler_idle(&self, duration: Duration) {
        self.add_scheduler_duration(&self.telemetry_metrics.scheduler_idle_nanos, duration);
        self.record_vm_health_metrics();
    }

    #[cfg(feature = "telemetry")]
    pub(super) fn record_process_slice_metrics(&self, process: &Process, reductions_consumed: u32) {
        let now = Instant::now();
        {
            let mut last_samples = lock_or_recover(&self.telemetry_metrics.last_process_samples);
            if let Some(last_sample) = last_samples.get(&process.pid())
                && now.duration_since(*last_sample) < self.telemetry_metrics.sample_interval
            {
                return;
            }
            last_samples.insert(process.pid(), now);
        }
        crate::telemetry::metrics::record_process_slice(
            process.pid(),
            reductions_consumed,
            process.mailbox().message_count(),
        );
    }

    #[cfg(feature = "telemetry")]
    pub(super) fn remove_process_metric_state(&self, pid: u64) {
        lock_or_recover(&self.telemetry_metrics.last_process_samples).remove(&pid);
    }

    #[cfg(feature = "telemetry")]
    fn record_vm_health_metrics(&self) {
        let (heap_words, _) = self.process_heap_and_binary_words();
        crate::telemetry::metrics::record_vm_health(
            self.process_count(),
            heap_words,
            self.scheduler_utilization(),
        );
    }

    #[cfg(feature = "telemetry")]
    fn scheduler_utilization(&self) -> f64 {
        let executing = self
            .telemetry_metrics
            .scheduler_executing_nanos
            .load(Ordering::Relaxed);
        let idle = self
            .telemetry_metrics
            .scheduler_idle_nanos
            .load(Ordering::Relaxed);
        let total = executing.saturating_add(idle);
        if total == 0 {
            0.0
        } else {
            executing as f64 / total as f64
        }
    }

    #[cfg(feature = "telemetry")]
    fn add_scheduler_duration(&self, counter: &AtomicU64, duration: Duration) {
        let nanos = match u64::try_from(duration.as_nanos()) {
            Ok(value) => value,
            Err(_) => u64::MAX,
        };
        let _previous = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(nanos))
        });
    }
}

#[cfg(feature = "threads")]
#[derive(Default)]
struct WaitSet {
    waiting: std::collections::HashMap<u64, usize>,
    woken: Vec<(u64, usize)>,
}

/// Test-only injection points inside the park sequences of `run_process`,
/// used to drive deliver/resume interleavings deterministically.
#[cfg(feature = "threads")]
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ParkGap {
    /// Wait arm: after the store-back, before wait-set registration.
    WaitStored,
    /// Wait arm: after wait-set registration, before the mailbox recheck.
    WaitRegistered,
    /// Suspended arm: after the store-back, before wait-set registration.
    SuspendStored,
}

#[cfg(feature = "threads")]
#[cfg(test)]
type ParkGapHook = Box<dyn Fn(&SharedState, ParkGap, u64) + Send + Sync>;
#[cfg(feature = "threads")]
pub(super) struct ScheduledProcess(Process);
// SAFETY: Process is not Send at the public API boundary. The scheduler is the
// sole owner of process execution, storing each body behind a mutex-protected
// ProcessSlot. Workers take exclusive ownership before executing a time slice.
#[cfg(feature = "threads")]
unsafe impl Send for ScheduledProcess {}
#[cfg(feature = "threads")]
pub struct Scheduler {
    shared: Arc<SharedState>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    inject_queues: Vec<Arc<SegQueue<SpawnRequest>>>,
    worker_names: Vec<String>,
}
#[cfg(feature = "threads")]
impl Scheduler {
    /// Allocate and register an ETS table owned by a process.
    ///
    /// The provided metadata's `id` field is overwritten with the allocated,
    /// monotonically increasing table ID before the table is inserted.
    pub fn create_ets_table(&self, metadata: EtsTableMetadata) -> EtsTableId {
        self.shared.create_table(metadata)
    }

    /// Look up a registered ETS table by ID.
    pub fn lookup_ets_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.shared.lookup_table(id)
    }

    /// Look up a named ETS table by atom.
    pub fn lookup_ets_table_by_name(&self, name: crate::atom::Atom) -> Option<EtsTableId> {
        self.shared.lookup_table_by_name(name)
    }

    /// Delete a registered ETS table by ID.
    pub fn delete_ets_table(&self, id: EtsTableId) -> bool {
        self.shared.delete_table(id)
    }

    pub fn new(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
    ) -> Result<Self, String> {
        Self::with_code_server(
            config,
            module_registry,
            Arc::new(AtomTable::with_common_atoms()),
            Arc::new(BifRegistryImpl::new()),
        )
    }

    /// Create a scheduler in deterministic replay mode over `log`.
    pub fn new_replay(config: SchedulerConfig, log: ReplayLog) -> Result<Self, String> {
        Self::new_replay_with_registry(config, Arc::new(ModuleRegistry::new()), log)
    }

    /// Create a replay scheduler using an explicit module registry.
    pub fn new_replay_with_registry(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        log: ReplayLog,
    ) -> Result<Self, String> {
        Self::construct(config, module_registry, ReplayMode::Replay(log))
    }

    pub fn with_code_server(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        atom_table: Arc<AtomTable>,
        bif_registry: Arc<BifRegistryImpl>,
    ) -> Result<Self, String> {
        Self::with_code_server_and_policy(
            config,
            module_registry,
            atom_table,
            bif_registry,
            Arc::new(AllCapabilitiesPolicy),
        )
    }
    pub fn with_code_server_and_policy(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        atom_table: Arc<AtomTable>,
        bif_registry: Arc<BifRegistryImpl>,
        capability_policy: Arc<dyn CapabilityPolicy>,
    ) -> Result<Self, String> {
        Self::construct_with_services(
            config,
            module_registry,
            atom_table,
            bif_registry,
            capability_policy,
            ReplayMode::Live,
        )
    }

    fn construct(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        replay_mode: ReplayMode,
    ) -> Result<Self, String> {
        Self::construct_with_services(
            config,
            module_registry,
            Arc::new(AtomTable::with_common_atoms()),
            Arc::new(BifRegistryImpl::new()),
            Arc::new(AllCapabilitiesPolicy),
            replay_mode,
        )
    }

    fn construct_with_services(
        config: SchedulerConfig,
        module_registry: Arc<ModuleRegistry>,
        atom_table: Arc<AtomTable>,
        bif_registry: Arc<BifRegistryImpl>,
        capability_policy: Arc<dyn CapabilityPolicy>,
        replay_mode: ReplayMode,
    ) -> Result<Self, String> {
        let replay_driver = match replay_mode {
            ReplayMode::Live => None,
            ReplayMode::Replay(log) => Some(Arc::new(Mutex::new(ReplayDriver::new(log)))),
        };
        let replay_enabled = replay_driver.is_some();
        let thread_count = if replay_enabled {
            1
        } else {
            configured_thread_count(config.thread_count)
        };
        let dirty_queue_depth = config
            .dirty_queue_depth
            .unwrap_or(dirty::DEFAULT_DIRTY_QUEUE_DEPTH);
        let dirty_cpu = DirtyPool::with_queue_depth(
            "dirty-cpu",
            config.dirty_cpu_threads.unwrap_or_else(num_cpus::get),
            dirty_queue_depth,
        );
        let dirty_io = DirtyPool::with_queue_depth(
            "dirty-io",
            config
                .dirty_io_threads
                .unwrap_or(dirty::DEFAULT_DIRTY_IO_THREADS),
            dirty_queue_depth,
        );
        let jit_profiler = Arc::new(JitProfiler::new(
            config.jit_threshold.unwrap_or(DEFAULT_JIT_THRESHOLD),
        ));
        #[cfg(feature = "telemetry")]
        let telemetry_sample_interval = config
            .telemetry_sample_interval
            .unwrap_or_else(|| Duration::from_millis(100));
        #[cfg(not(feature = "telemetry"))]
        let _telemetry_sample_interval = config.telemetry_sample_interval;
        let jit_cache = Arc::new(JitCache::new());
        let io_runtime = if replay_enabled {
            None
        } else {
            config.io.map(|ring_config| {
                let ring: Arc<dyn CompletionRing> = Arc::from(create_ring(ring_config));
                let registry = Arc::new(PendingIoRegistry::default());
                let facility: Arc<dyn IoFacility> = Arc::new(CompletionRingIoFacility::new(
                    Arc::clone(&ring),
                    Arc::clone(&registry),
                ));
                (ring, registry, facility)
            })
        };
        let (io_ring, io_registry, io_facility) = match io_runtime {
            Some((ring, registry, facility)) => (Some(ring), Some(registry), Some(facility)),
            None => (None, None, None),
        };
        let distribution = config.distribution.unwrap_or_default();
        let dist_local_node_name = config
            .node_name
            .as_deref()
            .unwrap_or(DEFAULT_NODE_NAME)
            .to_owned();
        let dist_local_creation = config.creation.unwrap_or(0);
        // Enable the proactive net-tick with production defaults so an idle link
        // to a silently-partitioned peer (no FIN/RST) is detected within the
        // liveness deadline and the existing connection-down / pg-purge /
        // monitor-DOWN machinery fires, instead of the link hanging "up" forever.
        let distribution_connections = ConnectionManager::new(
            Arc::clone(&atom_table),
            Arc::clone(&distribution.resolver),
            distribution.cookie.clone(),
            dist_local_node_name,
            dist_local_creation,
        )
        .with_heartbeat(HeartbeatConfig::with_defaults());
        // Build the async outbound distribution sender (skipped under replay,
        // which has no runtime). Bind its owned runtime handle to the connection
        // manager so the read/accept tasks are driven in production, where there
        // is no ambient tokio runtime.
        let dist_sender = if replay_enabled {
            None
        } else {
            DistSender::new(distribution_connections.clone())
        };
        if let Some(sender) = &dist_sender {
            distribution_connections.set_runtime_handle(sender.handle());
        }
        let namespace_store = DashMap::new();
        namespace_store.insert(NamespaceId::DEFAULT, Arc::clone(&module_registry));
        let file_io_ring: Arc<dyn CompletionRing> = if replay_enabled {
            Arc::new(ReplayDisabledRing::default())
        } else {
            Arc::from(crate::io::create_ring(RingConfig::default()))
        };
        let standard_io_ring: Arc<dyn CompletionRing> = if replay_enabled {
            Arc::new(ReplayDisabledRing::default())
        } else {
            Arc::from(crate::io::create_ring(RingConfig::default()))
        };
        let standard_io_pid = 0u64;
        let local_node_name = config.node_name.as_deref().unwrap_or(DEFAULT_NODE_NAME);
        let local_node = Node::new(
            atom_table.intern(local_node_name),
            config.creation.unwrap_or(0),
        );
        let connection_manager = ConnectionManager::new(
            Arc::clone(&atom_table),
            distribution.resolver.clone(),
            distribution.cookie.clone(),
            local_node_name.to_owned(),
            config.creation.unwrap_or(0),
        );
        let net_kernel = Arc::new(NetKernel::new(connection_manager));
        let pg_registry = Arc::new(PgRegistry::new(atom_table.as_ref()));
        let standard_io_server =
            StandardIoServer::new(standard_io_pid, standard_io_ring, atom_table.as_ref());
        let shared = Arc::new(SharedState {
            shutdown: AtomicBool::new(false),
            process_table: ProcessTable::new(),
            module_registry,
            namespace_store,
            next_namespace_id: AtomicU64::new(1),
            atom_table,
            local_node,
            net_kernel,
            ets_registry: Arc::new(EtsRegistry::new()),
            pg_registry,
            bif_registry,
            capability_policy,
            spawn_counter: AtomicUsize::new(0),
            thread_count,
            dirty_cpu,
            dirty_io,
            jit_profiler,
            jit_cache,
            next_pid: AtomicU64::new(1),
            wait_set: Mutex::new(WaitSet::default()),
            wake_condvar: Condvar::new(),
            process_bodies: DashMap::new(),
            exit_tombstones: exit_tombstones::BoundedTombstones::new(),
            exit_results: DashMap::new(),
            exit_errors: DashMap::new(),
            exit_exceptions: DashMap::new(),
            suspensions: DashMap::new(),
            suspension_results: DashMap::new(),
            pending_resumes: DashMap::new(),
            file_io_ring,
            file_io_pending: DashMap::new(),
            file_io_orphans: DashMap::new(),
            file_io_results: DashMap::new(),
            file_io_canceled: DashSet::new(),
            link_set: Mutex::new(LinkSet::new()),
            monitor_set: Mutex::new(MonitorSet::new()),
            hook: Hook::new(),
            distribution,
            distribution_connections,
            dist_sender,
            control_router: ControlRouter::new(),
            process_registry: DashMap::new(),
            timers: Arc::new(Mutex::new(TimerWheel::new())),
            expired_receive_timers: DashMap::new(),
            output_sink: Mutex::new(Arc::new(NullSink)),
            io_ring,
            io_registry,
            io_bridge: Mutex::new(None),
            io_facility,
            standard_io_pid,
            replay_driver,
            replay_mode: replay_enabled,
            nif_private_data: config.nif_private_data,
            #[cfg(feature = "telemetry")]
            telemetry_metrics: TelemetryMetricState::new(telemetry_sample_interval),
            _standard_io_server: standard_io_server,
            #[cfg(test)]
            idle_parks: AtomicUsize::new(0),
            #[cfg(test)]
            park_gap_hook: Mutex::new(None),
        });
        if !shared.replay_mode {
            let standard_io_pid = shared._standard_io_server.pid();
            shared.process_table.spawn_with_pid(standard_io_pid);
            shared.process_bodies.insert(
                standard_io_pid,
                Mutex::new(ProcessSlot::Present(ScheduledProcess(
                    StandardIoServer::process(standard_io_pid),
                ))),
            );
        }
        #[cfg(feature = "telemetry")]
        shared.record_vm_health_metrics();
        supervision_integration::register_distribution_control_handler(&shared);
        // Install the real cross-node pg propagation now that `shared` exists.
        // Both the propagation backend and the connection-down hook hold a
        // `Weak<SharedState>` (not `Arc`) so they never keep the scheduler alive:
        // `SharedState` owns `pg_registry`, which owns the propagation, which would
        // otherwise own `SharedState` back and form a leak-forever cycle.
        shared
            .pg_registry
            .set_propagation(Arc::new(pg_propagation::SchedulerPgPropagation {
                shared: Arc::downgrade(&shared),
            }));
        // On node failure, drop every remote pg member that belonged to the lost
        // node so group membership reflects the surviving cluster.
        let pg_down_weak = Arc::downgrade(&shared);
        shared
            .distribution_connections
            .register_connection_down(move |event| {
                if let Some(shared) = pg_down_weak.upgrade() {
                    shared.pg_registry.purge_remote_node(event.node);
                }
            });
        if !shared.replay_mode
            && let (Some(ring), Some(registry)) = (&shared.io_ring, &shared.io_registry)
        {
            let target: Arc<dyn IoWakeTarget> = shared.clone();
            let bridge = IoCompletionBridge::start(Arc::clone(ring), Arc::clone(registry), target)
                .map_err(|error| format!("failed to spawn beamr-io-completion thread: {error}"))?;
            *lock_or_recover(&shared.io_bridge) = Some(bridge);
        }
        let inject_queues: Vec<_> = (0..thread_count)
            .map(|_| Arc::new(SegQueue::new()))
            .collect();
        let barrier = Arc::new(std::sync::Barrier::new(thread_count + 1));
        let stealers_ready: Arc<Mutex<Option<Vec<PriorityStealers>>>> = Arc::new(Mutex::new(None));
        let mut stealer_receivers = Vec::with_capacity(thread_count);
        let mut threads = Vec::with_capacity(thread_count);
        let mut worker_names = Vec::with_capacity(thread_count);
        for (index, inject_queue) in inject_queues.iter().enumerate() {
            let (tx, rx) = std::sync::mpsc::channel();
            stealer_receivers.push(rx);
            let shared_for_thread = Arc::clone(&shared);
            let barrier_for_thread = Arc::clone(&barrier);
            let ready_for_thread = Arc::clone(&stealers_ready);
            let inject = Arc::clone(inject_queue);
            let thread_name = format!("beamr-sched-{index}");
            worker_names.push(thread_name.clone());
            let handle = std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || {
                    let queue = RunQueue::new();
                    if tx.send(queue.stealer()).is_err() {
                        return;
                    }
                    barrier_for_thread.wait();
                    let stealers = {
                        let guard = lock_or_recover(&ready_for_thread);
                        guard.as_ref().cloned().unwrap_or_default()
                    };
                    scheduler_loop(&shared_for_thread, &queue, index, &stealers, &inject);
                })
                .map_err(|error| format!("failed to spawn {thread_name}: {error}"))?;
            threads.push(handle);
        }
        let mut stealers = Vec::with_capacity(thread_count);
        for rx in stealer_receivers {
            let stealer = rx
                .recv()
                .map_err(|error| format!("failed to receive scheduler stealer: {error}"))?;
            stealers.push(stealer);
        }
        {
            let mut guard = lock_or_recover(&stealers_ready);
            *guard = Some(stealers);
        }
        barrier.wait();
        Ok(Self {
            shared,
            threads: Mutex::new(threads),
            inject_queues,
            worker_names,
        })
    }
    #[must_use]
    pub fn create_namespace(&self) -> NamespaceId {
        let id = NamespaceId(
            self.shared
                .next_namespace_id
                .fetch_add(1, Ordering::Relaxed),
        );
        debug_assert_ne!(id, NamespaceId::DEFAULT);
        self.shared
            .namespace_store
            .insert(id, Arc::new(ModuleRegistry::new()));
        id
    }
    pub fn set_trap_exit(
        &self,
        pid: u64,
        value: bool,
    ) -> Result<bool, crate::native::links::LinkError> {
        let facility = supervision_integration::SchedulerLinkFacility {
            shared: Arc::clone(&self.shared),
        };
        crate::native::LinkFacility::set_trap_exit(&facility, pid, value)
    }
    #[must_use]
    pub fn trap_exit(&self, pid: u64) -> Option<bool> {
        process_trap_exit(&self.shared, pid)
    }
    #[must_use]
    pub fn is_linked(&self, left: u64, right: u64) -> bool {
        process_links_contain(&self.shared, left, right)
            && process_links_contain(&self.shared, right, left)
    }
    /// Whether the process is native (carries a Rust handler).
    ///
    /// `Some(true)` for a parked native process, `Some(false)` for a parked
    /// bytecode process, and `None` when the process is absent (its body has
    /// been removed by `cleanup_exited_process`) or currently mid-slice (its
    /// `Process` is checked out, so native-ness is not observable from the
    /// metadata shadow). A `None` after an expected exit therefore confirms
    /// the body was removed from `process_bodies`.
    #[must_use]
    pub fn is_native(&self, pid: u64) -> Option<bool> {
        process_is_native(&self.shared, pid)
    }
    /// Establish a unidirectional monitor from `watcher_pid` to `target_pid`,
    /// returning the monitor reference.
    ///
    /// Delegates to the existing pid-keyed [`SupervisionFacility`] used by the
    /// `monitor/2` BIF, so a `{'DOWN', ref, process, pid, reason}` message is
    /// delivered to the watcher via the same `deliver_down_messages` path when
    /// the target exits — there is no native-specific monitor handling. Works
    /// uniformly for bytecode and native targets.
    pub fn monitor(
        &self,
        watcher_pid: u64,
        target_pid: u64,
    ) -> Result<u64, crate::native::supervision::SupervisionError> {
        let facility = supervision_integration::SchedulerSupervisionFacility {
            shared: Arc::clone(&self.shared),
        };
        crate::native::supervision::SupervisionFacility::monitor(&facility, watcher_pid, target_pid)
            .map(|result| result.reference)
    }
    /// Send an exit signal to `target_pid` with `reason`, the embedding-side
    /// equivalent of `erlang:exit/2`.
    ///
    /// Delegates to the existing pid-keyed [`SupervisionFacility`]: an abnormal
    /// reason terminates a non-trapping target through `cleanup_exited_process`
    /// (propagating to its links and monitors) or is delivered as an
    /// `{'EXIT', from, reason}` message to a trapping target. No native-specific
    /// path is involved.
    pub fn exit_signal(
        &self,
        from_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), crate::native::supervision::SupervisionError> {
        let facility = supervision_integration::SchedulerSupervisionFacility {
            shared: Arc::clone(&self.shared),
        };
        crate::native::supervision::SupervisionFacility::exit_signal(
            &facility, from_pid, target_pid, reason,
        )
    }
    #[must_use]
    pub fn process_namespace(&self, pid: u64) -> Option<NamespaceId> {
        process_namespace(&self.shared, pid)
    }
    #[must_use]
    pub fn process_table(&self) -> &ProcessTable {
        &self.shared.process_table
    }
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.shared.scheduler_count()
    }
    #[must_use]
    pub fn process_count(&self) -> usize {
        self.shared.process_count()
    }
    #[must_use]
    pub fn scheduler_count(&self) -> usize {
        self.shared.scheduler_count()
    }
    #[must_use]
    pub fn atom_count(&self) -> usize {
        self.shared.atom_count()
    }
    #[must_use]
    pub fn atom_limit(&self) -> usize {
        self.shared.atom_table.limit()
    }
    #[must_use]
    pub fn local_node(&self) -> Node {
        self.shared.local_node
    }
    #[must_use]
    pub fn worker_names(&self) -> &[String] {
        &self.worker_names
    }
    #[must_use]
    pub fn dirty_cpu_pool(&self) -> &DirtyPool {
        &self.shared.dirty_cpu
    }
    #[must_use]
    pub fn dirty_io_pool(&self) -> &DirtyPool {
        &self.shared.dirty_io
    }
    #[must_use]
    pub fn jit_profiler(&self) -> &Arc<JitProfiler> {
        &self.shared.jit_profiler
    }
    #[must_use]
    pub fn jit_cache(&self) -> &Arc<JitCache> {
        &self.shared.jit_cache
    }
    #[must_use]
    pub fn hook(&self) -> &Hook {
        &self.shared.hook
    }
    #[must_use]
    pub fn timers(&self) -> &Arc<Mutex<TimerWheel>> {
        &self.shared.timers
    }
    #[must_use]
    pub fn distribution_config(&self) -> &DistributionConfig {
        &self.shared.distribution
    }
    #[must_use]
    pub fn distribution_connections(&self) -> ConnectionManager {
        self.shared.distribution_connections.clone()
    }
    /// Start accepting inbound distribution connections on `addr`.
    ///
    /// Accepted peers run the OTP handshake (authenticated by the configured
    /// cookie) before being registered under their advertised node name. The
    /// returned [`AcceptHandle`](crate::distribution::connection::AcceptHandle)
    /// owns the accept loop: the caller must keep it alive, as dropping it aborts
    /// the loop and stops accepting new connections.
    pub async fn start_distribution_listener(
        &self,
        addr: std::net::SocketAddr,
    ) -> std::io::Result<crate::distribution::connection::AcceptHandle> {
        self.shared.distribution_connections.listen(addr).await
    }
    #[must_use]
    pub fn pg_registry(&self) -> Arc<PgRegistry> {
        Arc::clone(&self.shared.pg_registry)
    }
    /// The scheduler's shared atom table.
    ///
    /// Distribution-facing embedders need this to intern names into the SAME
    /// atoms the scheduler uses internally: pg group/scope atoms and the node
    /// atoms returned by [`ConnectionManager::connected_nodes`] are indices into
    /// this table, so a separately-constructed table would not match. Mirrors the
    /// accessor [`WasmScheduler::atom_table`](crate::scheduler::WasmScheduler::atom_table)
    /// already exposes.
    #[must_use]
    pub fn atom_table(&self) -> &Arc<AtomTable> {
        &self.shared.atom_table
    }
    pub fn set_output_sink(&self, sink: Arc<dyn IoSink>) {
        *lock_or_recover(&self.shared.output_sink) = sink;
    }
    #[cfg(test)]
    fn idle_park_count(&self) -> usize {
        self.shared.idle_parks.load(Ordering::Acquire)
    }
}
#[cfg(feature = "threads")]
impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[cfg(feature = "threads")]
fn configured_thread_count(override_count: Option<usize>) -> usize {
    override_count
        .filter(|count| *count > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        })
}
#[cfg(feature = "threads")]
fn process_namespace(shared: &SharedState, pid: u64) -> Option<NamespaceId> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => Some(scheduled.0.namespace_id()),
        ProcessSlot::Executing(metadata) => Some(metadata.namespace_id),
        ProcessSlot::Absent => None,
    }
}
#[cfg(feature = "threads")]
fn process_trap_exit(shared: &SharedState, pid: u64) -> Option<bool> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => Some(scheduled.0.trap_exit()),
        ProcessSlot::Executing(metadata) => Some(metadata.trap_exit),
        ProcessSlot::Absent => None,
    }
}
#[cfg(feature = "threads")]
fn process_is_native(shared: &SharedState, pid: u64) -> Option<bool> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => Some(scheduled.0.is_native()),
        // Mid-slice the `Process` is checked out; native-ness lives on it, not
        // on the metadata shadow. Absent means the body is being swapped.
        ProcessSlot::Executing(_) | ProcessSlot::Absent => None,
    }
}
#[cfg(feature = "threads")]
fn process_links_contain(shared: &SharedState, pid: u64, linked_pid: u64) -> bool {
    let Some(entry) = shared.process_bodies.get(&pid) else {
        return false;
    };
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => scheduled.0.links().contains(&linked_pid),
        ProcessSlot::Executing(metadata) => metadata.links.contains(&linked_pid),
        ProcessSlot::Absent => false,
    }
}

#[cfg(feature = "threads")]
impl SharedState {
    pub(super) fn process_info(&self, pid: u64, item: ProcessInfoItem) -> Option<ProcessInfoValue> {
        self.process_table.get(pid)?;
        let entry = self.process_bodies.get(&pid)?;
        match &*lock_or_recover(&entry) {
            ProcessSlot::Present(scheduled) => process_info_from_process(&scheduled.0, item),
            ProcessSlot::Executing(metadata) => process_info_from_metadata(metadata, item),
            ProcessSlot::Absent => None,
        }
    }
}

#[cfg(feature = "threads")]
fn process_info_from_process(process: &Process, item: ProcessInfoItem) -> Option<ProcessInfoValue> {
    if matches!(process.status(), ProcessStatus::Exited(_)) {
        return None;
    }
    Some(match item {
        ProcessInfoItem::CurrentFunction => {
            ProcessInfoValue::CurrentFunction(process.current_mfa())
        }
        ProcessInfoItem::HeapSize => ProcessInfoValue::HeapSize(process.heap().total_used()),
        ProcessInfoItem::MessageQueueLen => {
            ProcessInfoValue::MessageQueueLen(process.mailbox().message_count())
        }
        ProcessInfoItem::RegisteredName => ProcessInfoValue::RegisteredName(None),
        ProcessInfoItem::Status => ProcessInfoValue::Status(status_from_process(process.status())?),
        ProcessInfoItem::TrapExit => ProcessInfoValue::TrapExit(process.trap_exit()),
        ProcessInfoItem::Priority => ProcessInfoValue::Priority(process.priority()),
        ProcessInfoItem::Links => ProcessInfoValue::Links(process.links().to_vec()),
        ProcessInfoItem::Monitors => ProcessInfoValue::Monitors(
            process
                .monitors()
                .iter()
                .map(|monitor| ProcessMonitorInfo {
                    watcher: monitor.watcher(),
                    target: monitor.target(),
                })
                .collect(),
        ),
    })
}

#[cfg(feature = "threads")]
fn process_info_from_metadata(
    metadata: &ProcessMetadata,
    item: ProcessInfoItem,
) -> Option<ProcessInfoValue> {
    Some(match item {
        ProcessInfoItem::CurrentFunction => ProcessInfoValue::CurrentFunction(metadata.current_mfa),
        ProcessInfoItem::HeapSize => ProcessInfoValue::HeapSize(metadata.heap_size),
        ProcessInfoItem::MessageQueueLen => {
            ProcessInfoValue::MessageQueueLen(metadata.message_queue_len)
        }
        ProcessInfoItem::RegisteredName => ProcessInfoValue::RegisteredName(None),
        ProcessInfoItem::Status => ProcessInfoValue::Status(ProcessInfoStatus::Running),
        ProcessInfoItem::TrapExit => ProcessInfoValue::TrapExit(metadata.trap_exit),
        ProcessInfoItem::Priority => ProcessInfoValue::Priority(metadata.priority),
        ProcessInfoItem::Links => ProcessInfoValue::Links(metadata.links.clone()),
        ProcessInfoItem::Monitors => ProcessInfoValue::Monitors(
            metadata
                .monitors
                .iter()
                .map(|monitor| ProcessMonitorInfo {
                    watcher: monitor.watcher(),
                    target: monitor.target(),
                })
                .collect(),
        ),
    })
}

#[cfg(feature = "threads")]
fn status_from_process(status: ProcessStatus) -> Option<ProcessInfoStatus> {
    match status {
        ProcessStatus::New | ProcessStatus::Running | ProcessStatus::Yielded => {
            Some(ProcessInfoStatus::Running)
        }
        ProcessStatus::Waiting => Some(ProcessInfoStatus::Waiting),
        ProcessStatus::Suspended => Some(ProcessInfoStatus::Suspended),
        ProcessStatus::Exited(_) => None,
    }
}

#[cfg(feature = "threads")]
pub(super) fn namespace_registry(
    shared: &SharedState,
    namespace: NamespaceId,
) -> Option<Arc<ModuleRegistry>> {
    shared
        .namespace_store
        .get(&namespace)
        .map(|entry| Arc::clone(entry.value()))
}
#[cfg(feature = "threads")]
pub(super) fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(feature = "threads")]
impl Scheduler {
    /// Enqueue an immediate atom message into a live process mailbox and wake
    /// the process if it is parked.
    ///
    /// Embedders use this as a host-to-process wake primitive (e.g. activity
    /// completion markers). Delivery must succeed in every live slot state: a
    /// process currently executing a slice receives the message through its
    /// pending metadata, which the scheduler merges into the mailbox at
    /// store-back and then resumes the process if it suspended meanwhile —
    /// otherwise a completion racing the suspend transition is lost and the
    /// process sleeps forever.
    ///
    /// The wake applies to plain receives and message-wakeable suspends
    /// (`ProcessContext::request_suspend`). A process parked under a *gated*
    /// suspension (`request_await_suspend`, an in-flight dirty call, a hook
    /// suspend) keeps the message in its mailbox but stays parked until its
    /// own completion event arrives — waking it would re-execute the parked
    /// call instruction and repeat its host side effect.
    ///
    /// Returns false only when no live process exists for `target_pid`.
    #[must_use]
    pub fn enqueue_atom_message(&self, target_pid: u64, atom: crate::atom::Atom) -> bool {
        let delivered =
            timer_integration::deliver_term_to_mailbox(&self.shared, target_pid, Term::atom(atom));
        if delivered {
            execution::wake_process(&self.shared, target_pid);
        }
        delivered
    }
}

#[cfg(feature = "threads")]
impl IoWakeTarget for SharedState {
    fn wake_with_io_result(&self, pid: u64, term: Term) {
        // Identity-resolved at publish time: the bridge completes the
        // host-await suspension the submitting native registered. A stale
        // completion (the await already timed out and re-entered) is
        // dropped instead of being applied blind.
        let Some(payload) = suspension::SuspensionResultPayload::host(term) else {
            return;
        };
        let _published = self.publish_suspension_result_current(
            pid,
            crate::process::SuspensionKind::HostAwait,
            payload,
        );
        execution::wake_process(self, pid);
    }

    fn send_io_message(&self, pid: u64, term: Term) {
        let Some(entry) = self.process_bodies.get(&pid) else {
            return;
        };
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(process) = &mut *slot {
            process.0.mailbox_mut().push_owned(term);
        } else if let ProcessSlot::Executing(metadata) = &mut *slot {
            metadata.pending_io_messages.push(term);
        }
        drop(slot);
        drop(entry);
        if pid == self.standard_io_pid {
            let mut wait_set = lock_or_recover(&self.wait_set);
            wait_set.woken.push((pid, 0));
            self.wake_condvar.notify_all();
        } else {
            execution::wake_process(self, pid);
        }
    }
}

#[cfg(feature = "threads")]
#[cfg(test)]
mod supervision_tests;
#[cfg(feature = "threads")]
#[cfg(test)]
mod tests;
