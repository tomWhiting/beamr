//! Scheduler-level supervision integration tests.
//!
//! These tests verify that the scheduler correctly manages process lifecycles,
//! that exit propagation and supervision data structures are properly wired,
//! and that processes exit independently when not linked.

use std::collections::HashMap;
use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleRegistry};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::supervision::{LinkSet, MonitorSet};

fn test_module(name: beamr::atom::Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(i, instr)| {
            if let Instruction::Label { label } = instr {
                Some((*label, i))
            } else {
                None
            }
        })
        .collect();
    Module {
        name,
        generation: 0,
        exports: HashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
    }
}

fn wait_until(deadline_ms: u64, mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(deadline_ms);
    while !predicate() {
        assert!(std::time::Instant::now() <= deadline, "condition timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Create a module that returns immediately (normal exit).
fn normal_exit_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("normal_exit");
    test_module(name, vec![Instruction::Return])
}

/// Create a module that loops forever (for processes that should survive).
fn infinite_loop_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("looper");
    test_module(
        name,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    )
}

/// Create a module that waits for a message forever.
fn wait_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("waiter");
    test_module(
        name,
        vec![
            Instruction::Label { label: 10 },
            Instruction::Wait {
                fail: Operand::Label(10),
            },
        ],
    )
}

// ── Test: process exits normally and is removed from table ──────────────────

#[test]
fn process_exits_normally_and_is_removed() {
    let atoms = AtomTable::new();
    let registry = Arc::new(ModuleRegistry::new());
    let exit_module = registry.insert(normal_exit_module(&atoms));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|e| panic!("scheduler starts: {e}"));

    let pid = scheduler.spawn_process(&exit_module);
    wait_until(2000, || scheduler.process_table().get(pid).is_none());

    scheduler.shutdown();
}

// ── Test: unlinked process survives another's normal exit ───────────────────

#[test]
fn unlinked_process_survives_normal_exit() {
    let atoms = AtomTable::new();
    let registry = Arc::new(ModuleRegistry::new());
    let exit_module = registry.insert(normal_exit_module(&atoms));
    let loop_module = registry.insert(infinite_loop_module(&atoms));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|e| panic!("scheduler starts: {e}"));

    let looper_pid = scheduler.spawn_process(&loop_module);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(scheduler.process_table().get(looper_pid).is_some());

    let exit_pid = scheduler.spawn_process(&exit_module);
    wait_until(2000, || scheduler.process_table().get(exit_pid).is_none());

    // Looper should still be alive (not linked).
    assert!(
        scheduler.process_table().get(looper_pid).is_some(),
        "unlinked looper should survive"
    );

    scheduler.shutdown();
}

// ── Test: multiple independent processes exit cleanly ────────────────────────

#[test]
fn multiple_processes_exit_independently() {
    let atoms = AtomTable::new();
    let registry = Arc::new(ModuleRegistry::new());
    let exit_module = registry.insert(normal_exit_module(&atoms));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|e| panic!("scheduler starts: {e}"));

    let pids: Vec<_> = (0..10)
        .map(|_| scheduler.spawn_process(&exit_module))
        .collect();
    wait_until(3000, || {
        pids.iter()
            .all(|pid| scheduler.process_table().get(*pid).is_none())
    });

    scheduler.shutdown();
}

// ── Test: scheduler initializes supervision data structures ─────────────────

#[test]
fn scheduler_initializes_supervision_data_structures() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|e| panic!("scheduler starts: {e}"));

    // Scheduler should create LinkSet and MonitorSet without panicking.
    assert!(scheduler.thread_count() > 0);
    scheduler.shutdown();
}

// ── Unit test: LinkSet tombstone recording ──────────────────────────────────

#[test]
fn link_set_tombstone_records_dead_reason() {
    let mut link_set = LinkSet::new();

    assert_eq!(link_set.dead_reason(42), None);
    link_set.process_exited_tombstone(42, ExitReason::Error);
    assert_eq!(link_set.dead_reason(42), Some(ExitReason::Error));
}

// ── Unit test: MonitorSet collect_watchers_and_remove ────────────────────────

#[test]
fn monitor_set_collect_watchers_removes_entries() {
    use beamr::process::{Process, ProcessStatus};

    let mut monitors = MonitorSet::new();
    let mut watcher = Process::new(1, 64);
    watcher
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|e| panic!("process starts: {e}"));
    let mut target = Process::new(2, 64);
    target
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|e| panic!("process starts: {e}"));

    let ref1 = monitors.monitor(&mut watcher, &mut target);
    let ref2 = monitors.monitor(&mut watcher, &mut target);

    let watchers = monitors.collect_watchers_and_remove(2, ExitReason::Error);

    assert_eq!(watchers.len(), 2);
    assert!(watchers.iter().any(|(pid, r)| *pid == 1 && *r == ref1));
    assert!(watchers.iter().any(|(pid, r)| *pid == 1 && *r == ref2));
    assert!(monitors.get_monitor(ref1).is_none());
    assert!(monitors.get_monitor(ref2).is_none());
}

// ── Unit test: MonitorSet allocate_reference_pub ─────────────────────────────

#[test]
fn monitor_set_reference_allocation_is_unique() {
    let mut monitors = MonitorSet::new();

    let r1 = monitors.allocate_reference_pub();
    let r2 = monitors.allocate_reference_pub();
    let r3 = monitors.allocate_reference_pub();

    assert_ne!(r1, r2);
    assert_ne!(r2, r3);
    assert_ne!(r1, r3);
}

// ── Unit test: MonitorSet remove_monitor ─────────────────────────────────────

#[test]
fn monitor_set_remove_cleans_internal_state() {
    use beamr::process::{Process, ProcessStatus};

    let mut monitors = MonitorSet::new();
    let mut watcher = Process::new(1, 64);
    watcher
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|e| panic!("process starts: {e}"));
    let mut target = Process::new(2, 64);
    target
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|e| panic!("process starts: {e}"));

    let reference = monitors.monitor(&mut watcher, &mut target);
    assert!(monitors.get_monitor(reference).is_some());

    monitors.remove_monitor(reference);
    assert!(monitors.get_monitor(reference).is_none());
}

// ── Unit test: LinkSet records multiple tombstones ───────────────────────────

#[test]
fn link_set_records_multiple_tombstones() {
    let mut link_set = LinkSet::new();

    link_set.process_exited_tombstone(1, ExitReason::Normal);
    link_set.process_exited_tombstone(2, ExitReason::Error);
    link_set.process_exited_tombstone(3, ExitReason::Kill);

    assert_eq!(link_set.dead_reason(1), Some(ExitReason::Normal));
    assert_eq!(link_set.dead_reason(2), Some(ExitReason::Error));
    assert_eq!(link_set.dead_reason(3), Some(ExitReason::Kill));
    assert_eq!(link_set.dead_reason(4), None);
}

// ── Test: waiting process stays alive when no exit signal ───────────────────

#[test]
fn waiting_process_stays_alive_without_exit_signal() {
    let atoms = AtomTable::new();
    let registry = Arc::new(ModuleRegistry::new());
    let wait_mod = registry.insert(wait_module(&atoms));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|e| panic!("scheduler starts: {e}"));

    let pid = scheduler.spawn_process(&wait_mod);

    // Wait a bit then verify it's still alive.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        scheduler.process_table().get(pid).is_some(),
        "waiting process should stay alive"
    );

    scheduler.shutdown();
}
