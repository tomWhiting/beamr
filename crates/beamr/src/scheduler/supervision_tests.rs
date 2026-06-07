//! Supervision integration tests — verify that exit signal propagation,
//! DOWN message delivery, and cascade deaths work correctly through the
//! scheduler's cleanup_exited_process path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

use dashmap::DashMap;

use super::*;
use crate::atom::Atom;
use crate::process::ProcessStatus;
use crate::process::registry::ProcessTable;
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
                monitors: process.monitors().to_vec(),
                trap_exit: process.trap_exit(),
                pending_exit_messages: Vec::new(),
                pending_down_messages: Vec::new(),
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

fn make_shared_state() -> Arc<SharedState> {
    let module_registry = Arc::new(ModuleRegistry::new());
    let namespace_store = DashMap::new();
    namespace_store.insert(NamespaceId::DEFAULT, Arc::clone(&module_registry));

    Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry,
        namespace_store,
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        next_pid: AtomicU64::new(100),
        wait_set: std::sync::Mutex::new(WaitSet::default()),
        wake_condvar: std::sync::Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        async_results: DashMap::new(),
        link_set: std::sync::Mutex::new(LinkSet::new()),
        monitor_set: std::sync::Mutex::new(MonitorSet::new()),
        hook: crate::hook::Hook::new(),
        timers: Arc::new(std::sync::Mutex::new(crate::timer::TimerWheel::new())),
        output_sink: std::sync::Mutex::new(Arc::new(crate::io::NullSink)),
        atom_table: Arc::new(crate::atom::AtomTable::new()),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
    })
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
        shared.exit_tombstones.get(&b).map(|r| *r),
        Some(ExitReason::Killed),
        "B tombstone should be Killed"
    );
    assert_eq!(
        shared.exit_tombstones.get(&c).map(|r| *r),
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
fn normal_exit_signal_does_not_queue_message_for_executing_trapping_process() {
    let shared = make_shared_state();
    let parent = insert_process(&shared, 1);
    let child = insert_process(&shared, 2);
    add_link(&shared, parent, child);
    set_trap_exit(&shared, child, true);

    let process = make_executing(&shared, child);

    cleanup_exited_process(&shared, parent, ExitReason::Normal);

    assert!(
        !shared.exit_tombstones.contains_key(&child),
        "normal exit should not tombstone a linked executing child"
    );
    store_runnable_process(&shared, process);
    assert_eq!(read_mailbox_tuple(&shared, child), None);
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
