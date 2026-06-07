pub mod dirty;
mod execution;
mod module_management;
mod process_slot;
pub mod run_queue;
mod spawning;
pub mod steal;
mod supervision_integration;
mod test_helpers;
mod timer_integration;
use self::dirty::DirtyPool;
use self::execution::scheduler_loop;
use self::spawning::SpawnRequest;
use crate::atom::AtomTable;
use crate::error::ExecError;
use crate::ets::{EtsTable, EtsTableId, EtsTableMetadata};
use crate::hook::Hook;
use crate::io::{IoSink, NullSink};
use crate::module::ModuleRegistry;
use crate::namespace::NamespaceId;
use crate::native::{
    AllCapabilitiesPolicy, BifRegistryImpl, CapabilityPolicy, ProcessInfoItem, ProcessInfoStatus,
    ProcessInfoValue, ProcessMonitorInfo,
};
use crate::process::registry::ProcessTable;
use crate::process::{ExitReason, Process, ProcessStatus};
use crate::scheduler::dirty::DirtyResult;
use crate::supervision::link::LinkSet;
use crate::supervision::monitor::MonitorSet;
use crate::term::Term;
use crate::timer::TimerWheel;
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
pub use module_management::{HotLoadResult, PurgeResult};
use process_slot::{ProcessMetadata, ProcessSlot};
use run_queue::{PriorityStealers, RunQueue};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
pub const DEFAULT_REDUCTION_BUDGET: u32 = crate::process::DEFAULT_REDUCTION_BUDGET;
#[derive(Clone, Debug, Default)]
pub struct SchedulerConfig {
    pub thread_count: Option<usize>,
    pub dirty_cpu_threads: Option<usize>,
    pub dirty_io_threads: Option<usize>,
    pub dirty_queue_depth: Option<usize>,
}
pub(super) struct SharedState {
    shutdown: AtomicBool,
    process_table: ProcessTable,
    module_registry: Arc<ModuleRegistry>,
    namespace_store: DashMap<NamespaceId, Arc<ModuleRegistry>>,
    next_namespace_id: AtomicU64,
    atom_table: Arc<AtomTable>,
    ets_tables: DashMap<EtsTableId, Arc<dyn EtsTable>>,
    ets_named_tables: DashMap<crate::atom::Atom, EtsTableId>,
    next_ets_table_id: AtomicU64,
    bif_registry: Arc<BifRegistryImpl>,
    capability_policy: Arc<dyn CapabilityPolicy>,
    spawn_counter: AtomicUsize,
    thread_count: usize,
    pub(super) dirty_cpu: DirtyPool,
    pub(super) dirty_io: DirtyPool,
    next_pid: AtomicU64,
    wait_set: Mutex<WaitSet>,
    wake_condvar: Condvar,
    process_bodies: DashMap<u64, Mutex<ProcessSlot>>,
    exit_tombstones: DashMap<u64, ExitReason>,
    exit_results: DashMap<u64, Term>,
    exit_errors: DashMap<u64, ExecError>,
    exit_exceptions: DashMap<u64, crate::process::Exception>,
    async_results: DashMap<u64, Term>,
    dirty_results: DashMap<u64, DirtyResult>,
    link_set: Mutex<LinkSet>,
    monitor_set: Mutex<MonitorSet>,
    hook: Hook,
    timers: Arc<Mutex<TimerWheel>>,
    output_sink: Mutex<Arc<dyn IoSink>>,
    #[cfg(test)]
    idle_parks: AtomicUsize,
}

impl SharedState {
    pub(super) fn create_table(&self, mut metadata: EtsTableMetadata) -> EtsTableId {
        let table_id = self.next_ets_table_id.fetch_add(1, Ordering::Relaxed);
        metadata.id = table_id;
        let table: Arc<dyn EtsTable> = Arc::new(MetadataOnlyTable { metadata });
        if let Some(name) = table.metadata().name {
            self.ets_named_tables.insert(name, table_id);
        }
        self.ets_tables.insert(table_id, table);
        table_id
    }

    pub(super) fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.ets_tables.get(&id).map(|table| Arc::clone(&table))
    }

    pub(super) fn lookup_table_by_name(&self, name: crate::atom::Atom) -> Option<EtsTableId> {
        self.ets_named_tables.get(&name).map(|entry| *entry.value())
    }

    pub(super) fn delete_table(&self, id: EtsTableId) -> bool {
        let Some((_id, table)) = self.ets_tables.remove(&id) else {
            return false;
        };
        if let Some(name) = table.metadata().name {
            self.ets_named_tables
                .remove_if(&name, |_name, indexed_id| *indexed_id == id);
        }
        true
    }

    pub(super) fn delete_tables_owned_by(&self, owner: u64) -> usize {
        let table_ids = self
            .ets_tables
            .iter()
            .filter_map(|entry| {
                if entry.value().metadata().owner == owner {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let deleted = table_ids.len();
        for table_id in table_ids {
            let _removed = self.delete_table(table_id);
        }
        deleted
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
}

struct MetadataOnlyTable {
    metadata: EtsTableMetadata,
}

impl EtsTable for MetadataOnlyTable {
    fn metadata(&self) -> &EtsTableMetadata {
        &self.metadata
    }

    fn insert(&self, _tuple: Term) -> Result<(), crate::ets::EtsError> {
        Ok(())
    }

    fn lookup(&self, _key: Term) -> Vec<Term> {
        Vec::new()
    }

    fn delete_key(&self, _key: Term) -> bool {
        false
    }

    fn tab2list(&self) -> Vec<Term> {
        Vec::new()
    }
}

#[derive(Default)]
struct WaitSet {
    waiting: std::collections::HashMap<u64, usize>,
    woken: Vec<(u64, usize)>,
}
pub(super) struct ScheduledProcess(Process);
// SAFETY: Process is not Send at the public API boundary. The scheduler is the
// sole owner of process execution, storing each body behind a mutex-protected
// ProcessSlot. Workers take exclusive ownership before executing a time slice.
unsafe impl Send for ScheduledProcess {}
pub struct Scheduler {
    shared: Arc<SharedState>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    inject_queues: Vec<Arc<SegQueue<SpawnRequest>>>,
    worker_names: Vec<String>,
}
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
        let thread_count = configured_thread_count(config.thread_count);
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
        let namespace_store = DashMap::new();
        namespace_store.insert(NamespaceId::DEFAULT, Arc::clone(&module_registry));
        let shared = Arc::new(SharedState {
            shutdown: AtomicBool::new(false),
            process_table: ProcessTable::new(),
            module_registry,
            namespace_store,
            next_namespace_id: AtomicU64::new(1),
            atom_table,
            ets_tables: DashMap::new(),
            ets_named_tables: DashMap::new(),
            next_ets_table_id: AtomicU64::new(1),
            bif_registry,
            capability_policy,
            spawn_counter: AtomicUsize::new(0),
            thread_count,
            dirty_cpu,
            dirty_io,
            next_pid: AtomicU64::new(0),
            wait_set: Mutex::new(WaitSet::default()),
            wake_condvar: Condvar::new(),
            process_bodies: DashMap::new(),
            exit_tombstones: DashMap::new(),
            exit_results: DashMap::new(),
            exit_errors: DashMap::new(),
            exit_exceptions: DashMap::new(),
            async_results: DashMap::new(),
            dirty_results: DashMap::new(),
            link_set: Mutex::new(LinkSet::new()),
            monitor_set: Mutex::new(MonitorSet::new()),
            hook: Hook::new(),
            timers: Arc::new(Mutex::new(TimerWheel::new())),
            output_sink: Mutex::new(Arc::new(NullSink)),
            #[cfg(test)]
            idle_parks: AtomicUsize::new(0),
        });
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
    pub fn hook(&self) -> &Hook {
        &self.shared.hook
    }
    #[must_use]
    pub fn timers(&self) -> &Arc<Mutex<TimerWheel>> {
        &self.shared.timers
    }
    pub fn set_output_sink(&self, sink: Arc<dyn IoSink>) {
        *lock_or_recover(&self.shared.output_sink) = sink;
    }
    #[cfg(test)]
    fn idle_park_count(&self) -> usize {
        self.shared.idle_parks.load(Ordering::Acquire)
    }
}
impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn configured_thread_count(override_count: Option<usize>) -> usize {
    override_count
        .filter(|count| *count > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        })
}
fn process_namespace(shared: &SharedState, pid: u64) -> Option<NamespaceId> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => Some(scheduled.0.namespace_id()),
        ProcessSlot::Executing(metadata) => Some(metadata.namespace_id),
        ProcessSlot::Absent => None,
    }
}
fn process_trap_exit(shared: &SharedState, pid: u64) -> Option<bool> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        ProcessSlot::Present(scheduled) => Some(scheduled.0.trap_exit()),
        ProcessSlot::Executing(metadata) => Some(metadata.trap_exit),
        ProcessSlot::Absent => None,
    }
}
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

pub(super) fn namespace_registry(
    shared: &SharedState,
    namespace: NamespaceId,
) -> Option<Arc<ModuleRegistry>> {
    shared
        .namespace_store
        .get(&namespace)
        .map(|entry| Arc::clone(entry.value()))
}
pub(super) fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(test)]
mod supervision_tests;
#[cfg(test)]
mod tests;
