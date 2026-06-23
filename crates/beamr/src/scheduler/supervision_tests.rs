//! Supervision integration tests — verify that exit signal propagation,
//! DOWN message delivery, and cascade deaths work correctly through the
//! scheduler's cleanup_exited_process path.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

use dashmap::{DashMap, DashSet};

use super::*;
use crate::atom::Atom;
use crate::ets::{EtsTableMetadata, EtsTableType, Protection};
use crate::io::RingConfig;
use crate::io::resource::{FD_RESOURCE_WORDS, FdInner, FdState, write_fd_resource};
use crate::process::registry::ProcessTable;
use crate::process::{ProcessStatus, RemotePid};
use crate::scheduler::execution::{
    cleanup_exited_process, cleanup_if_tombstoned_after_store, store_runnable_process,
};
use crate::supervision::link::LinkSet;
use crate::supervision::monitor::MonitorSet;
use crate::term::boxed::{self, Tuple};

/// Helper: insert a running process into shared state with the given pid.
fn insert_process(shared: &SharedState, pid: u64) -> u64 {
    insert_process_in(shared, pid, NamespaceId::DEFAULT)
}

/// Helper: insert a running process assigned to `namespace`.
fn insert_process_in(shared: &SharedState, pid: u64, namespace: NamespaceId) -> u64 {
    shared.process_table.spawn_with_pid(pid);
    let mut process = Process::new(pid, 64);
    process.set_namespace_id(namespace);
    process
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process {pid} starts: {error}"));
    shared.process_bodies.insert(
        pid,
        std::sync::Mutex::new(ProcessSlot::Present(ScheduledProcess(process))),
    );
    pid
}

/// Helper: read a tuple from the front of a process's mailbox.
fn read_mailbox_tuple(shared: &SharedState, pid: u64) -> Option<Vec<Term>> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
        return None;
    };
    process.mailbox_mut().drain_arrival();
    let msg = process.mailbox().front_for_test()?;
    let tuple = Tuple::new(msg)?;
    let mut elems = Vec::with_capacity(tuple.arity());
    for i in 0..tuple.arity() {
        elems.push(tuple.get(i).unwrap_or(Term::NIL));
    }
    Some(elems)
}

/// Helper: check if a process is still in the process table.
fn is_alive(shared: &SharedState, pid: u64) -> bool {
    shared.process_table.get(pid).is_some()
}

/// Helper: add a bidirectional link between two process bodies.
fn add_link(shared: &SharedState, a: u64, b: u64) {
    if let Some(entry) = shared.process_bodies.get(&a) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
            p.add_link(b);
        }
    }
    if let Some(entry) = shared.process_bodies.get(&b) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
            p.add_link(a);
        }
    }
}

fn add_remote_link(shared: &SharedState, pid: u64, remote: RemotePid) {
    let entry = shared
        .process_bodies
        .get(&pid)
        .unwrap_or_else(|| panic!("process {pid} exists"));
    let mut slot = lock_or_recover(&entry);
    let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
        panic!("process {pid} is present");
    };
    assert!(process.add_remote_link(remote));
}

/// Helper: set trap_exit on a process.
fn set_trap_exit(shared: &SharedState, pid: u64, value: bool) {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
            p.set_trap_exit(value);
        }
    }
}

/// Helper: add a monitor from watcher to target. Returns the reference.
fn add_monitor(shared: &SharedState, watcher_pid: u64, target_pid: u64) -> u64 {
    let mut ms = lock_or_recover(&shared.monitor_set);
    let reference = ms.allocate_reference_pub();
    let monitor = crate::process::Monitor::new(reference, watcher_pid, target_pid);
    ms.register_monitor(reference, monitor, target_pid);
    drop(ms);

    if let Some(entry) = shared.process_bodies.get(&watcher_pid) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
            p.add_monitor(monitor);
        }
    }
    if let Some(entry) = shared.process_bodies.get(&target_pid) {
        let mut slot = lock_or_recover(&entry);
        if let ProcessSlot::Present(ScheduledProcess(p)) = &mut *slot {
            p.add_monitor(monitor);
        }
    }

    reference
}

fn make_executing(shared: &SharedState, pid: u64) -> Process {
    let entry = shared
        .process_bodies
        .get(&pid)
        .unwrap_or_else(|| panic!("process {pid} exists"));
    let mut slot = lock_or_recover(&entry);
    match std::mem::take(&mut *slot) {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            let metadata = ProcessMetadata {
                namespace_id: process.namespace_id(),
                links: process.links().to_vec(),
                remote_links: process.remote_links().to_vec(),
                monitors: process.monitors().to_vec(),
                trap_exit: process.trap_exit(),
                priority: process.priority(),
                current_mfa: process.current_mfa(),
                heap_size: process.heap().total_used(),
                binary_heap_size: process.virtual_binary_heap(),
                message_queue_len: process.mailbox().message_count(),
                group_leader: process.group_leader(),
                logical_clock: process.logical_clock(),
                capabilities: process.capabilities().clone(),
                pending_exit_messages: Vec::new(),
                pending_down_messages: Vec::new(),
                pending_io_messages: Vec::new(),
                pending_distribution_payloads: Vec::new(),
                pending_local_messages: Vec::new(),
                pending_ets_transfer_messages: Vec::new(),
                pending_udp_messages: Vec::new(),
                pending_tcp_messages: Vec::new(),
            };
            *slot = ProcessSlot::Executing(metadata);
            process
        }
        other => {
            *slot = other;
            panic!("process {pid} is present before executing transition");
        }
    }
}

fn allocate_fd_resource_for_process(shared: &SharedState, pid: u64, inner: Arc<FdInner>) -> RawFd {
    let fd = inner.fd();
    let entry = shared
        .process_bodies
        .get(&pid)
        .expect("process body exists");
    let mut slot = lock_or_recover(&entry);
    let ProcessSlot::Present(ScheduledProcess(process)) = &mut *slot else {
        panic!("process {pid} is present");
    };
    let ptr = process
        .heap_mut()
        .alloc(FD_RESOURCE_WORDS)
        .expect("fd resource allocation fits");
    // SAFETY: heap allocation returned the fixed FdResource word count.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, FD_RESOURCE_WORDS) };
    let term = write_fd_resource(words, inner).expect("fd resource writer fits");
    process.set_x_reg(0, term);
    fd
}

fn pipe_read_fd() -> RawFd {
    let mut fds = [0; 2];
    // SAFETY: `fds` points to two valid RawFd slots for libc to initialize.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0);
    // SAFETY: close the write end so tests only manage the read end.
    let _closed = unsafe { libc::close(fds[1]) };
    fds[0]
}

/// Captures the kernel identity (`st_dev`, `st_ino`) of an open descriptor.
/// Each `pipe(2)` instance has a unique inode, so this uniquely identifies the
/// descriptor independently of its fd *number*.
fn fd_identity(fd: RawFd) -> (libc::dev_t, libc::ino_t) {
    // SAFETY: `fstat` fully writes the stat buffer on success; we zero-init it
    // first so a failure path never reads uninitialized memory.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd, &mut st) };
    assert_eq!(rc, 0, "expected an open fd to fstat successfully");
    (st.st_dev, st.st_ino)
}

/// Reuse-immune closure check: returns `true` iff the descriptor that had
/// identity `original` is now closed, even under cargo's parallel test runner
/// where another thread may concurrently recycle the freed fd number.
///
/// - `fstat` fails (EBADF) -> the number is closed -> original closed.
/// - `fstat` succeeds with a *different* identity -> the number was recycled to
///   a new file/pipe, which still proves the original was closed.
/// - `fstat` succeeds with the *same* identity -> genuinely still open.
fn fd_closed_since(fd: RawFd, original: (libc::dev_t, libc::ino_t)) -> bool {
    // SAFETY: see `fd_identity`.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd, &mut st) };
    if rc != 0 {
        return true;
    }
    (st.st_dev, st.st_ino) != original
}

fn ets_metadata(name: Option<Atom>, owner: u64) -> EtsTableMetadata {
    EtsTableMetadata::new(name, 0, EtsTableType::Set, Protection::Protected, owner)
}

fn ets_metadata_with_heir(
    name: Option<Atom>,
    owner: u64,
    heir_pid: u64,
    data: Term,
) -> EtsTableMetadata {
    let mut metadata = ets_metadata(name, owner);
    metadata.heir = Some(crate::ets::EtsHeir {
        pid: heir_pid,
        data: crate::ets::copy_term_to_ets(data).expect("heir data copies into ETS"),
    });
    metadata
}

fn make_shared_state() -> Arc<SharedState> {
    let module_registry = Arc::new(ModuleRegistry::new());
    let namespace_store = DashMap::new();
    namespace_store.insert(NamespaceId::DEFAULT, Arc::clone(&module_registry));
    let atom_table = Arc::new(crate::atom::AtomTable::new());
    let distribution = DistributionConfig::default();
    let distribution_connections = crate::distribution::connection::ConnectionManager::new(
        Arc::clone(&atom_table),
        Arc::clone(&distribution.resolver),
    );
    let net_kernel = Arc::new(crate::distribution::NetKernel::new(
        crate::distribution::connection::ConnectionManager::new(
            Arc::clone(&atom_table),
            distribution.resolver.clone(),
        ),
    ));

    Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry,
        namespace_store,
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        dirty_cpu: dirty::DirtyPool::with_queue_depth("dirty-test-cpu", 1, 1),
        dirty_io: dirty::DirtyPool::with_queue_depth("dirty-test-io", 1, 1),
        next_pid: AtomicU64::new(100),
        wait_set: std::sync::Mutex::new(WaitSet::default()),
        wake_condvar: std::sync::Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: exit_tombstones::BoundedTombstones::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        suspensions: DashMap::new(),
        suspension_results: DashMap::new(),
        pending_resumes: DashMap::new(),
        link_set: std::sync::Mutex::new(LinkSet::new()),
        monitor_set: std::sync::Mutex::new(MonitorSet::new()),
        hook: crate::hook::Hook::new(),
        distribution,
        distribution_connections,
        control_router: crate::distribution::remote_link::ControlRouter::new(),
        process_registry: DashMap::new(),
        timers: Arc::new(std::sync::Mutex::new(crate::timer::TimerWheel::new())),
        expired_receive_timers: DashMap::new(),
        output_sink: std::sync::Mutex::new(Arc::new(crate::io::NullSink)),
        io_ring: None,
        io_registry: None,
        io_bridge: std::sync::Mutex::new(None),
        io_facility: None,
        atom_table,
        ets_registry: Arc::new(crate::ets::EtsRegistry::new()),
        pg_registry: Arc::new(crate::distribution::pg::PgRegistry::new(
            &crate::atom::AtomTable::with_common_atoms(),
        )),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
        park_gap_hook: Mutex::new(None),
        file_io_ring: Arc::from(crate::io::create_ring(RingConfig::default())),
        file_io_pending: DashMap::new(),
        file_io_orphans: DashMap::new(),
        file_io_results: DashMap::new(),
        file_io_canceled: DashSet::new(),
        standard_io_pid: u64::MAX,
        _standard_io_server: crate::io::StandardIoServer::new(
            u64::MAX,
            Arc::from(crate::io::create_ring(RingConfig::default())),
            &crate::atom::AtomTable::new(),
        ),
        local_node: crate::distribution::Node::new(crate::atom::Atom::new(0), 0),
        net_kernel,
        jit_profiler: Arc::new(crate::jit::JitProfiler::new(1000)),
        jit_cache: Arc::new(crate::jit::JitCache::new()),
        replay_driver: None,
        replay_mode: false,
        nif_private_data: None,
    })
}

#[test]
fn cleanup_exited_process_closes_fd_resources_owned_by_process() {
    let shared = make_shared_state();
    let pid = insert_process(&shared, 1);
    let inner = Arc::new(FdInner::new(pipe_read_fd(), pid));
    allocate_fd_resource_for_process(&shared, pid, Arc::clone(&inner));

    cleanup_exited_process(&shared, pid, ExitReason::Normal);

    assert_eq!(inner.state(), FdState::Closed);
}

#[test]
fn cleanup_exited_process_does_not_explicitly_close_fd_resources_owned_elsewhere() {
    let shared = make_shared_state();
    let pid = insert_process(&shared, 1);
    let inner = Arc::new(FdInner::new(pipe_read_fd(), 999));
    let fd = allocate_fd_resource_for_process(&shared, pid, Arc::clone(&inner));
    let fd_id = fd_identity(fd);

    cleanup_exited_process(&shared, pid, ExitReason::Normal);

    assert!(!fd_closed_since(fd, fd_id));
    drop(inner);
    assert!(fd_closed_since(fd, fd_id));
}

#[test]
fn process_terminate_closes_owned_fd_resources_before_heap_reset() {
    let mut process = Process::new(42, 64);
    let fd = pipe_read_fd();
    let fd_id = fd_identity(fd);
    let ptr = process
        .heap_mut()
        .alloc(FD_RESOURCE_WORDS)
        .expect("fd resource allocation fits");
    // SAFETY: heap allocation returned the fixed FdResource word count.
    let words = unsafe { std::slice::from_raw_parts_mut(ptr, FD_RESOURCE_WORDS) };
    let term =
        write_fd_resource(words, Arc::new(FdInner::new(fd, 42))).expect("fd resource writer fits");
    process.set_x_reg(0, term);

    process.terminate(ExitReason::Normal);

    assert!(fd_closed_since(fd, fd_id));
}

#[test]
fn cleanup_exited_process_deletes_tables_owned_by_process() {
    let shared = make_shared_state();
    let owner = insert_process(&shared, 11);
    let survivor = insert_process(&shared, 12);
    let owned_name = shared.atom_table.intern("owned_table");
    let owned_id = shared.create_table(ets_metadata(Some(owned_name), owner));
    let other_id = shared.create_table(ets_metadata(None, survivor));

    cleanup_exited_process(&shared, owner, ExitReason::Normal);

    assert!(shared.lookup_table(owned_id).is_none());
    assert_eq!(shared.lookup_table_by_name(owned_name), None);
    assert!(shared.lookup_table(other_id).is_some());
}

#[test]
fn cleanup_exited_process_transfers_table_to_live_heir() {
    let shared = make_shared_state();
    let owner = insert_process(&shared, 11);
    let heir = insert_process(&shared, 12);
    let transfer_atom = shared.atom_table.intern("ETS-TRANSFER");
    let data = Term::small_int(77);
    let owned_id = shared.create_table(ets_metadata_with_heir(None, owner, heir, data));

    cleanup_exited_process(&shared, owner, ExitReason::Normal);

    let table = shared.lookup_table(owned_id).expect("table transferred");
    assert_eq!(table.metadata().owner.get(), heir);
    assert_eq!(
        table.check_access(heir, crate::ets::AccessOp::Write),
        Ok(())
    );
    let message = read_mailbox_tuple(&shared, heir).expect("ETS-TRANSFER delivered");
    assert_eq!(message.len(), 4);
    assert_eq!(message[0], Term::atom(transfer_atom));
    assert_eq!(message[1], Term::small_int(owned_id as i64));
    assert_eq!(message[2], Term::pid(owner));
    assert_eq!(message[3], data);
}

#[test]
fn cleanup_exited_process_transfers_table_to_executing_heir() {
    let shared = make_shared_state();
    let owner = insert_process(&shared, 11);
    let heir = insert_process(&shared, 12);
    let transfer_atom = shared.atom_table.intern("ETS-TRANSFER");
    let data = Term::small_int(77);
    let owned_id = shared.create_table(ets_metadata_with_heir(None, owner, heir, data));
    let executing_heir = make_executing(&shared, heir);

    cleanup_exited_process(&shared, owner, ExitReason::Normal);

    let table = shared.lookup_table(owned_id).expect("table transferred");
    assert_eq!(table.metadata().owner.get(), heir);
    assert!(
        read_mailbox_tuple(&shared, heir).is_none(),
        "executing heir receives message after store-back"
    );

    store_runnable_process(&shared, executing_heir);
    let message = read_mailbox_tuple(&shared, heir).expect("ETS-TRANSFER delivered");
    assert_eq!(message.len(), 4);
    assert_eq!(message[0], Term::atom(transfer_atom));
    assert_eq!(message[1], Term::small_int(owned_id as i64));
    assert_eq!(message[2], Term::pid(owner));
    assert_eq!(message[3], data);
}

#[test]
fn cleanup_exited_process_deletes_table_when_heir_is_not_live() {
    let shared = make_shared_state();
    let owner = insert_process(&shared, 11);
    let owned_id =
        shared.create_table(ets_metadata_with_heir(None, owner, 99, Term::small_int(77)));

    cleanup_exited_process(&shared, owner, ExitReason::Normal);

    assert!(shared.lookup_table(owned_id).is_none());
}

#[test]
fn linked_process_dies_on_error_exit() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    add_link(&shared, a, b);

    cleanup_exited_process(&shared, a, ExitReason::Error);

    assert!(!is_alive(&shared, a), "process A should be removed");
    assert!(
        !is_alive(&shared, b),
        "linked process B should die from error exit"
    );
}

#[test]
fn linked_process_survives_normal_exit() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    add_link(&shared, a, b);

    cleanup_exited_process(&shared, a, ExitReason::Normal);

    assert!(!is_alive(&shared, a), "process A should be removed");
    assert!(
        is_alive(&shared, b),
        "linked process B should survive normal exit"
    );
}

#[test]
fn trap_exit_receives_exit_message() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    add_link(&shared, a, b);
    set_trap_exit(&shared, b, true);

    cleanup_exited_process(&shared, a, ExitReason::Error);

    assert!(!is_alive(&shared, a), "process A should be removed");
    assert!(is_alive(&shared, b), "trapping process B should survive");

    let msg = read_mailbox_tuple(&shared, b)
        .unwrap_or_else(|| panic!("B should have received EXIT message"));
    assert_eq!(msg.len(), 3, "EXIT message should be a 3-tuple");
    assert_eq!(msg[0], Term::atom(Atom::EXIT), "first element is EXIT atom");
    assert_eq!(msg[1].as_pid(), Some(1), "second element is dead PID");
    assert_eq!(msg[2], Term::atom(Atom::ERROR), "third element is reason");
}

#[test]
fn kill_propagates_killed_and_non_trapping_processes_die() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    let c = insert_process(&shared, 3);
    add_link(&shared, a, b);
    add_link(&shared, b, c);

    cleanup_exited_process(&shared, a, ExitReason::Kill);

    assert!(!is_alive(&shared, a), "process A removed");
    assert!(
        !is_alive(&shared, b),
        "B dies from killed signal (not trapping)"
    );
    assert!(!is_alive(&shared, c), "cascade kills C too");

    assert_eq!(
        shared.exit_tombstones.get(&b),
        Some(ExitReason::Killed),
        "B tombstone should be Killed"
    );
    assert_eq!(
        shared.exit_tombstones.get(&c),
        Some(ExitReason::Killed),
        "C tombstone should be Killed"
    );
}

#[test]
fn killed_signal_is_trappable_by_linked_process() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    add_link(&shared, a, b);
    set_trap_exit(&shared, b, true);

    cleanup_exited_process(&shared, a, ExitReason::Kill);

    assert!(!is_alive(&shared, a), "A removed");
    assert!(
        is_alive(&shared, b),
        "B traps exits and survives killed signal"
    );

    let msg =
        read_mailbox_tuple(&shared, b).unwrap_or_else(|| panic!("B should receive EXIT message"));
    assert_eq!(msg[0], Term::atom(Atom::EXIT));
    assert_eq!(msg[1].as_pid(), Some(1));
    assert_eq!(
        msg[2],
        Term::atom(Atom::KILLED),
        "reason is killed, not kill"
    );
}

#[test]
fn monitor_delivers_down_message_on_exit() {
    let shared = make_shared_state();
    let watcher = insert_process(&shared, 1);
    let target = insert_process(&shared, 2);
    let reference = add_monitor(&shared, watcher, target);

    cleanup_exited_process(&shared, target, ExitReason::Error);

    assert!(!is_alive(&shared, target), "target should be removed");
    assert!(is_alive(&shared, watcher), "watcher stays alive");

    let msg = read_mailbox_tuple(&shared, watcher)
        .unwrap_or_else(|| panic!("watcher should have received DOWN message"));
    assert_eq!(msg.len(), 5, "DOWN message should be a 5-tuple");
    assert_eq!(msg[0], Term::atom(Atom::DOWN), "first element is DOWN");
    let ref_term = boxed::Reference::new(msg[1])
        .unwrap_or_else(|| panic!("second element should be a boxed reference"));
    assert_eq!(ref_term.id(), reference, "reference matches");
    assert_eq!(
        msg[2],
        Term::atom(Atom::PROCESS),
        "third element is 'process'"
    );
    assert_eq!(msg[3].as_pid(), Some(2), "fourth element is dead PID");
    assert_eq!(msg[4], Term::atom(Atom::ERROR), "fifth element is reason");
}

#[test]
fn monitor_down_for_executing_watcher_is_delivered_on_store_back() {
    let shared = make_shared_state();
    let watcher = insert_process(&shared, 1);
    let target = insert_process(&shared, 2);
    let reference = add_monitor(&shared, watcher, target);
    let process = make_executing(&shared, watcher);

    cleanup_exited_process(&shared, target, ExitReason::Error);
    store_runnable_process(&shared, process);

    let msg = read_mailbox_tuple(&shared, watcher)
        .unwrap_or_else(|| panic!("executing watcher receives pending DOWN"));
    assert_eq!(msg.len(), 5, "DOWN message should be a 5-tuple");
    assert_eq!(msg[0], Term::atom(Atom::DOWN));
    let ref_term = boxed::Reference::new(msg[1]).unwrap_or_else(|| panic!("reference in DOWN"));
    assert_eq!(ref_term.id(), reference);
    assert_eq!(msg[2], Term::atom(Atom::PROCESS));
    assert_eq!(msg[3].as_pid(), Some(target));
    assert_eq!(msg[4], Term::atom(Atom::ERROR));
}

#[test]
fn cascade_link_exit_propagates_through_chain() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    let c = insert_process(&shared, 3);
    let d = insert_process(&shared, 4);
    add_link(&shared, a, b);
    add_link(&shared, b, c);
    add_link(&shared, c, d);

    cleanup_exited_process(&shared, a, ExitReason::Error);

    assert!(!is_alive(&shared, a), "A removed");
    assert!(!is_alive(&shared, b), "B dies from link to A");
    assert!(!is_alive(&shared, c), "C dies from cascade via B");
    assert!(!is_alive(&shared, d), "D dies from cascade via C");
}

#[test]
fn monitor_and_link_both_fire_on_exit() {
    let shared = make_shared_state();
    let target = insert_process(&shared, 1);
    let linked = insert_process(&shared, 2);
    let watcher = insert_process(&shared, 3);
    add_link(&shared, target, linked);
    let reference = add_monitor(&shared, watcher, target);

    cleanup_exited_process(&shared, target, ExitReason::Error);

    assert!(!is_alive(&shared, target), "target removed");
    assert!(!is_alive(&shared, linked), "linked process dies");
    assert!(is_alive(&shared, watcher), "watcher stays alive");

    let msg =
        read_mailbox_tuple(&shared, watcher).unwrap_or_else(|| panic!("watcher receives DOWN"));
    assert_eq!(msg[0], Term::atom(Atom::DOWN));
    let ref_term = boxed::Reference::new(msg[1]).unwrap_or_else(|| panic!("reference in DOWN"));
    assert_eq!(ref_term.id(), reference);
}

#[test]
fn cross_namespace_link_exit_propagates() {
    let shared = make_shared_state();
    let ns1 = NamespaceId(1);
    let ns2 = NamespaceId(2);
    let a = insert_process_in(&shared, 1, ns1);
    let b = insert_process_in(&shared, 2, ns2);
    add_link(&shared, a, b);

    cleanup_exited_process(&shared, a, ExitReason::Error);

    assert!(!is_alive(&shared, a), "source process removed");
    assert!(
        !is_alive(&shared, b),
        "linked process in another namespace dies from error exit"
    );
}

#[test]
fn cross_namespace_monitor_delivers_down_message() {
    let shared = make_shared_state();
    let ns1 = NamespaceId(1);
    let ns2 = NamespaceId(2);
    let watcher = insert_process_in(&shared, 1, ns1);
    let target = insert_process_in(&shared, 2, ns2);
    let reference = add_monitor(&shared, watcher, target);

    cleanup_exited_process(&shared, target, ExitReason::Error);

    assert!(!is_alive(&shared, target), "target should be removed");
    assert!(is_alive(&shared, watcher), "watcher stays alive");
    let msg = read_mailbox_tuple(&shared, watcher)
        .unwrap_or_else(|| panic!("watcher should have received cross-namespace DOWN"));
    assert_eq!(msg[0], Term::atom(Atom::DOWN));
    let ref_term = boxed::Reference::new(msg[1]).unwrap_or_else(|| panic!("reference in DOWN"));
    assert_eq!(ref_term.id(), reference);
    assert_eq!(msg[3].as_pid(), Some(target));
    assert_eq!(msg[4], Term::atom(Atom::ERROR));
}

#[test]
fn exit_signal_tombstones_executing_non_trapping_process() {
    let shared = make_shared_state();
    let parent = insert_process(&shared, 1);
    let child = insert_process(&shared, 2);
    add_link(&shared, parent, child);

    let process = make_executing(&shared, child);

    cleanup_exited_process(&shared, parent, ExitReason::Error);

    assert!(
        shared.exit_tombstones.contains_key(&child),
        "executing child should receive a tombstone"
    );
    store_runnable_process(&shared, process);
    assert!(cleanup_if_tombstoned_after_store(&shared, child));
    assert!(
        !is_alive(&shared, child),
        "tombstoned child should be cleaned"
    );
}

#[test]
fn exit_signal_queues_message_for_executing_trapping_process() {
    let shared = make_shared_state();
    let parent = insert_process(&shared, 1);
    let child = insert_process(&shared, 2);
    add_link(&shared, parent, child);
    set_trap_exit(&shared, child, true);

    let process = make_executing(&shared, child);

    cleanup_exited_process(&shared, parent, ExitReason::Error);

    assert!(
        !shared.exit_tombstones.contains_key(&child),
        "trapping child should not be tombstoned"
    );
    store_runnable_process(&shared, process);
    let msg = read_mailbox_tuple(&shared, child)
        .unwrap_or_else(|| panic!("pending EXIT message delivered on store-back"));
    assert_eq!(msg[0], Term::atom(Atom::EXIT));
    assert_eq!(msg[1], Term::pid(parent));
    assert_eq!(msg[2], Term::atom(Atom::ERROR));
}

#[test]
fn normal_exit_signal_queues_message_for_executing_trapping_process() {
    // Regression for the Executing-arm normal-exit drop bug: a NORMAL linked
    // exit to a trapping process that is MID-SLICE (`ProcessSlot::Executing`)
    // MUST still deliver `{'EXIT', Pid, normal}`, exactly as the Present arm
    // does. The Executing arm previously gated message delivery on
    // `reason != Normal`, so it silently dropped `{EXIT, src, normal}` —
    // diverging from OTP (a trapping process receives `{'EXIT', Pid, normal}`
    // for a normal linked exit) and from the Present arm. The child must NOT be
    // tombstoned or killed (a normal exit never kills), and the message must be
    // drained into the mailbox at the slice boundary by `store_runnable_process`.
    let shared = make_shared_state();
    let parent = insert_process(&shared, 1);
    let child = insert_process(&shared, 2);
    add_link(&shared, parent, child);
    set_trap_exit(&shared, child, true);

    let process = make_executing(&shared, child);

    cleanup_exited_process(&shared, parent, ExitReason::Normal);

    assert!(
        !shared.exit_tombstones.contains_key(&child),
        "a normal exit must not tombstone a linked trapping executing child"
    );
    assert!(
        is_alive(&shared, child),
        "a trapped normal exit must not kill the executing child"
    );
    store_runnable_process(&shared, process);
    let msg = read_mailbox_tuple(&shared, child)
        .unwrap_or_else(|| panic!("normal EXIT message must be delivered on store-back"));
    assert_eq!(msg[0], Term::atom(Atom::EXIT));
    assert_eq!(msg[1], Term::pid(parent));
    assert_eq!(msg[2], Term::atom(Atom::NORMAL));
}

#[test]
fn exit_signal_normal_queues_message_for_executing_trapping_process() {
    // Regression for the `exit_signal` (erlang:exit/2) Executing-arm normal-exit
    // drop bug: `exit_signal(From, Target, normal)` to a trapping process that is
    // MID-SLICE (`ProcessSlot::Executing`) MUST still deliver `{'EXIT', From,
    // normal}`, exactly as the Present arm does. The Executing arm previously
    // gated message delivery on `reason != Normal`, silently dropping
    // `{EXIT, From, normal}` — diverging from OTP (a trapping process receives
    // `{'EXIT', Pid, normal}` for `erlang:exit/2` with reason `normal`) and from
    // the Present arm. The target must NOT be tombstoned or killed (a normal exit
    // never kills), and the message must be drained into the mailbox at the slice
    // boundary by `store_runnable_process`. Exercises the
    // `SchedulerSupervisionFacility::exit_signal` path (NOT the link cascade).
    use crate::native::supervision::SupervisionFacility;

    let shared = make_shared_state();
    let from = insert_process(&shared, 1);
    let target = insert_process(&shared, 2);
    set_trap_exit(&shared, target, true);

    let process = make_executing(&shared, target);

    let facility = supervision_integration::SchedulerSupervisionFacility {
        shared: Arc::clone(&shared),
    };
    facility
        .exit_signal(from, target, ExitReason::Normal)
        .unwrap_or_else(|_| panic!("exit_signal must succeed"));

    assert!(
        !shared.exit_tombstones.contains_key(&target),
        "a normal exit_signal must not tombstone a trapping executing target"
    );
    assert!(
        is_alive(&shared, target),
        "a trapped normal exit_signal must not kill the executing target"
    );
    store_runnable_process(&shared, process);
    let msg = read_mailbox_tuple(&shared, target)
        .unwrap_or_else(|| panic!("normal EXIT message must be delivered on store-back"));
    assert_eq!(msg[0], Term::atom(Atom::EXIT));
    assert_eq!(msg[1], Term::pid(from));
    assert_eq!(msg[2], Term::atom(Atom::NORMAL));
}

#[test]
fn take_links_from_reads_executing_sentinel_links() {
    let shared = make_shared_state();
    let source = insert_process(&shared, 1);
    let linked = insert_process(&shared, 2);
    add_link(&shared, source, linked);

    let _process = make_executing(&shared, source);

    let links = supervision_integration::take_links_from(&shared, source);
    assert_eq!(links, vec![linked]);
}

#[test]
fn sentinel_links_merge_into_body_on_store_back() {
    let shared = make_shared_state();
    let pid = insert_process(&shared, 1);
    let linked = insert_process(&shared, 2);
    let process = make_executing(&shared, pid);

    {
        let entry = shared
            .process_bodies
            .get(&pid)
            .unwrap_or_else(|| panic!("process exists"));
        let mut slot = lock_or_recover(&entry);
        let ProcessSlot::Executing(metadata) = &mut *slot else {
            panic!("slot is executing");
        };
        metadata.add_link(linked, pid);
    }

    store_runnable_process(&shared, process);

    assert!(process_links_contain(&shared, pid, linked));
}

#[test]
fn remote_link_exit_sends_exit_control() {
    let shared = make_shared_state();
    let pid = insert_process(&shared, 1);
    let remote = RemotePid {
        node: Atom::OK,
        pid_number: 42,
        serial: 7,
    };
    add_remote_link(&shared, pid, remote);

    cleanup_exited_process(&shared, pid, ExitReason::Error);

    let messages = shared.control_router.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].op,
        crate::distribution::control_lifecycle::ControlOp::Exit
    );
    assert_eq!(messages[0].from.pid_number, pid);
    assert_eq!(messages[0].to, remote);
    assert_eq!(messages[0].reason, Some(ExitReason::Error));
}

#[test]
fn remote_exit_to_trapping_process_enqueues_remote_exit_tuple() {
    let shared = make_shared_state();
    let target = insert_process(&shared, 1);
    let remote = RemotePid {
        node: Atom::OK,
        pid_number: 42,
        serial: 7,
    };
    add_remote_link(&shared, target, remote);
    set_trap_exit(&shared, target, true);

    supervision_integration::process_remote_exit_signal(&shared, remote, target, ExitReason::Error);

    let tuple = read_mailbox_tuple(&shared, target).expect("remote EXIT message");
    assert_eq!(tuple.len(), 3);
    assert_eq!(tuple[0], Term::atom(Atom::EXIT));
    let source = crate::term::boxed::ExternalPid::new(tuple[1]).expect("remote source pid");
    assert_eq!(source.node(), Some(remote.node));
    assert_eq!(source.pid_number(), remote.pid_number);
    assert_eq!(source.serial(), remote.serial);
    assert_eq!(tuple[2], Term::atom(Atom::ERROR));
    assert!(is_alive(&shared, target));
}

#[test]
fn connection_down_delivers_noconnection_to_all_remote_links_on_node() {
    let shared = make_shared_state();
    let first = insert_process(&shared, 1);
    let second = insert_process(&shared, 2);
    let other_node = insert_process(&shared, 3);
    let node = Atom::OK;
    let remote_a = RemotePid {
        node,
        pid_number: 10,
        serial: 0,
    };
    let remote_b = RemotePid {
        node,
        pid_number: 11,
        serial: 0,
    };
    let remote_c = RemotePid {
        node: Atom::ERROR,
        pid_number: 12,
        serial: 0,
    };
    add_remote_link(&shared, first, remote_a);
    add_remote_link(&shared, first, remote_b);
    add_remote_link(&shared, second, remote_b);
    add_remote_link(&shared, other_node, remote_c);
    set_trap_exit(&shared, first, true);
    set_trap_exit(&shared, second, true);

    supervision_integration::connection_down(&shared, node);

    assert_eq!(
        read_mailbox_tuple(&shared, first).expect("first noconnection")[2],
        Term::atom(Atom::NOCONNECTION)
    );
    assert_eq!(
        read_mailbox_tuple(&shared, second).expect("second noconnection")[2],
        Term::atom(Atom::NOCONNECTION)
    );
    assert!(is_alive(&shared, other_node));
}

/// BLOCKER regression: a reference-bearing message sent to an *Executing*
/// receiver must survive the ETF encode -> store-back decode round trip. Before
/// the fix, decode.rs had no reference arm, so the message decoded as an error
/// and was silently dropped at the store-back `else { continue; }`. This test
/// drives the real `SchedulerLocalSendFacility` (via `build_native_services`)
/// against an Executing receiver — i.e. it exercises the
/// `ProcessSlot::Executing` ETF deferral arm — then stores the receiver back
/// and asserts the `{Ref, ping}` tuple (with an intact ref id) is delivered.
#[test]
fn reference_message_to_executing_receiver_round_trips_via_etf() {
    let shared = make_shared_state();
    let sender_pid = insert_process(&shared, 1);
    let receiver_pid = insert_process(&shared, 2);

    // Build `{Ref, ping}` on a live sender heap; `send_local` reads the term
    // during the call, so this Process must outlive the call.
    let ping = shared.atom_table.intern("ping");
    let mut sender = Process::new(sender_pid, 64);
    let ref_id = 0x0123_4567_89ab_cdef_u64;
    let ref_ptr = sender.heap_mut().alloc(2).expect("reference words");
    // SAFETY: alloc returned exactly the two words write_reference needs.
    let ref_words = unsafe { std::slice::from_raw_parts_mut(ref_ptr, 2) };
    let reference = boxed::write_reference(ref_words, ref_id).expect("reference fits");
    let tuple_ptr = sender.heap_mut().alloc(3).expect("tuple words");
    // SAFETY: alloc returned exactly the three words write_tuple needs.
    let tuple_words = unsafe { std::slice::from_raw_parts_mut(tuple_ptr, 3) };
    let message =
        boxed::write_tuple(tuple_words, &[reference, Term::atom(ping)]).expect("tuple fits");

    // Force the receiver into the Executing branch so the ETF deferral path runs.
    let receiver = make_executing(&shared, receiver_pid);

    let services = supervision_integration::build_native_services(&shared, NamespaceId::DEFAULT);
    let facility = services.local_send.expect("local send facility wired");
    facility
        .send_local(crate::native::local_send::LocalSendRequest {
            target_pid: receiver_pid,
            sender_pid,
            message,
            sender_clock: 1,
            replay_driver: None,
        })
        .expect("local send to executing receiver succeeds");

    // Store the receiver back: this drains pending_local_messages, decoding the
    // ETF payload onto the receiver heap.
    store_runnable_process(&shared, receiver);

    let msg = read_mailbox_tuple(&shared, receiver_pid)
        .unwrap_or_else(|| panic!("executing receiver gets the ref-bearing message"));
    assert_eq!(msg.len(), 2, "message should be a 2-tuple {{Ref, ping}}");
    let decoded_ref =
        boxed::Reference::new(msg[0]).unwrap_or_else(|| panic!("first element is a reference"));
    assert_eq!(
        decoded_ref.id(),
        ref_id,
        "reference id must survive the ETF round trip through the Executing path"
    );
    assert_eq!(msg[1], Term::atom(ping));

    // Keep the sender heap alive until after the facility call has copied/encoded.
    drop(sender);
}
