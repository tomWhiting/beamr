use std::collections::HashMap as StdHashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::atom::AtomTable;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use crate::mailbox::Mailbox;
use crate::process::heap::Heap;
use crate::term::boxed;

fn test_module(name: Atom, code: Vec<Instruction>) -> Module {
    Module {
        name,
        exports: StdHashMap::new(),
        code,
        literals: Vec::new(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    }
}

fn wait_until(deadline_ms: u64, mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(deadline_ms);
    while !predicate() {
        assert!(std::time::Instant::now() <= deadline, "condition timed out");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn scheduler_creates_requested_thread_count_and_names() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(4),
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    assert_eq!(scheduler.thread_count(), 4);
    assert_eq!(
        scheduler.worker_names(),
        &[
            "beamr-sched-0",
            "beamr-sched-1",
            "beamr-sched-2",
            "beamr-sched-3"
        ]
    );

    scheduler.shutdown();
}

#[test]
fn scheduler_default_thread_count_matches_available_parallelism() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(SchedulerConfig::default(), registry)
        .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let expected = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    assert_eq!(scheduler.thread_count(), expected);

    scheduler.shutdown();
}

#[test]
fn shutdown_is_idempotent() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    scheduler.shutdown();
    scheduler.shutdown();
}

#[test]
fn single_process_runs_to_completion_and_is_removed() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("simple");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(module_name, vec![Instruction::Return]);
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler.spawn_process(&module);

    wait_until(2_000, || scheduler.process_table().get(pid).is_none());
    scheduler.shutdown();
}

#[test]
fn exported_spawn_starts_at_entry_function_with_args() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("entry_mod");
    let function = atoms.intern("main");
    let mut module = test_module(
        module_name,
        vec![
            Instruction::Label { label: 7 },
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::X(1),
            },
            Instruction::Return,
        ],
    );
    module.exports.insert((function, 1), 7);
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler
        .spawn(
            module_name,
            function,
            vec![Term::try_small_int(42).unwrap_or(Term::NIL)],
        )
        .unwrap_or_else(|error| panic!("spawn succeeds: {error}"));

    wait_until(2_000, || scheduler.process_table().get(pid).is_none());
    scheduler.shutdown();
}

#[test]
fn yielded_process_is_rescheduled() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("loopy");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );
    let module = registry.insert(module);
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pid = scheduler.spawn_process(&module);
    std::thread::sleep(std::time::Duration::from_millis(75));

    assert!(scheduler.process_table().get(pid).is_some());
    scheduler.shutdown();
}

#[test]
fn multiple_processes_fairly_complete() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("multi");
    let registry = Arc::new(ModuleRegistry::new());
    let module = registry.insert(test_module(module_name, vec![Instruction::Return]));
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    let pids: Vec<_> = (0..20).map(|_| scheduler.spawn_process(&module)).collect();

    wait_until(3_000, || {
        pids.iter()
            .all(|pid| scheduler.process_table().get(*pid).is_none())
    });
    scheduler.shutdown();
}

#[test]
fn mailbox_send_wakes_waiting_process_event_driven() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let pid = 42;
    scheduler.shared.process_table.spawn_with_pid(pid);
    {
        let mut wait_set = lock_or_recover(&scheduler.shared.wait_set);
        wait_set.waiting.insert(pid, 0);
    }
    let mailbox = Mailbox::new();
    let sender = mailbox
        .sender()
        .with_wake_notifier(scheduler.wake_notifier(pid));
    let mut receiver_heap = Heap::new(16);

    sender
        .send(Term::small_int(7), &mut receiver_heap)
        .unwrap_or_else(|error| panic!("send succeeds: {error}"));

    let wait_set = lock_or_recover(&scheduler.shared.wait_set);
    assert!(!wait_set.waiting.contains_key(&pid));
    assert!(
        wait_set
            .woken
            .iter()
            .any(|(woken_pid, _)| *woken_pid == pid)
    );
    drop(wait_set);
    scheduler.shutdown();
}

#[test]
fn mailbox_send_does_not_wake_when_copy_fails() {
    let called = Arc::new(AtomicBool::new(false));
    let called_by_wake = Arc::clone(&called);
    let mailbox = Mailbox::new();
    let sender = mailbox.sender().with_wake_notifier(move || {
        called_by_wake.store(true, Ordering::Release);
    });
    let mut receiver_heap = Heap::new(0);
    let mut sender_words = [0_u64; 2];
    let too_large = boxed::write_cons(&mut sender_words, Term::small_int(1), Term::NIL)
        .unwrap_or_else(|| panic!("source cons fits"));

    assert!(sender.send(too_large, &mut receiver_heap).is_err());
    assert!(!called.load(Ordering::Acquire));
}

#[test]
fn idle_threads_park_instead_of_spinning() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(2),
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));

    wait_until(500, || scheduler.idle_park_count() > 0);
    scheduler.shutdown();
}
