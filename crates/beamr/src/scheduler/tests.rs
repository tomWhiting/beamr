use std::collections::HashMap as StdHashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::*;
use crate::atom::AtomTable;
use crate::hook::HookDecision;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use crate::mailbox::Mailbox;
use crate::process::heap::Heap;
use crate::term::boxed::{self, Tuple};

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
fn hook_records_reduction_yield_metadata_and_can_suspend_then_resume() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("hook_loop");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
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
    let events = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let events_by_hook = Arc::clone(&events);
    let calls_by_hook = Arc::clone(&calls);
    scheduler.hook().register(move |event| {
        events_by_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
        if calls_by_hook.fetch_add(1, Ordering::AcqRel) == 0 {
            HookDecision::Suspend
        } else {
            HookDecision::Continue
        }
    });

    let pid = scheduler.spawn_process(&module);
    wait_until(2_000, || calls.load(Ordering::Acquire) == 1);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "suspended process is held"
    );
    assert!(scheduler.resume_process(pid));
    wait_until(2_000, || calls.load(Ordering::Acquire) > 1);

    let events = events.lock().unwrap_or_else(|error| error.into_inner());
    let first = events.first().copied().expect("hook event recorded");
    assert_eq!(first.pid, pid);
    assert_eq!(first.module, module_name);
    assert_eq!(first.function, function);
    assert_eq!(first.arity, 0);
    assert_eq!(first.reductions_consumed, DEFAULT_REDUCTION_BUDGET);
    drop(events);
    scheduler.shutdown();
}

#[test]
fn hook_fires_when_process_blocks_on_receive() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("hook_wait");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let module = test_module(
        module_name,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_name)),
                function: Operand::Atom(Some(function)),
                arity: Operand::Unsigned(0),
            },
            Instruction::Label { label: 10 },
            Instruction::Wait {
                fail: Operand::Label(10),
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
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_by_hook = Arc::clone(&events);
    scheduler.hook().register(move |event| {
        events_by_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
        HookDecision::Continue
    });

    let pid = scheduler.spawn_process(&module);
    wait_until(2_000, || {
        !events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    });
    let events = events.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(events[0].pid, pid);
    assert_eq!(events[0].module, module_name);
    assert_eq!(events[0].function, function);
    assert_eq!(events[0].arity, 0);
    drop(events);
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

// ── Supervision integration tests ──────────────────────────────────────────

/// Helper: insert a running process into shared state with the given pid.
fn insert_process(shared: &SharedState, pid: u64) -> u64 {
    shared.process_table.spawn_with_pid(pid);
    let mut process = Process::new(pid, 64);
    process
        .transition_to(ProcessStatus::Running)
        .unwrap_or_else(|error| panic!("process {pid} starts: {error}"));
    shared.process_bodies.insert(
        pid,
        std::sync::Mutex::new(Some(ScheduledProcess(process))),
    );
    pid
}

/// Helper: read a tuple from the front of a process's mailbox.
fn read_mailbox_tuple(shared: &SharedState, pid: u64) -> Option<Vec<Term>> {
    let entry = shared.process_bodies.get(&pid)?;
    let mut slot = lock_or_recover(&entry);
    let process = &mut slot.as_mut()?.0;
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
        if let Some(ScheduledProcess(p)) = slot.as_mut() {
            p.add_link(b);
        }
    }
    if let Some(entry) = shared.process_bodies.get(&b) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(p)) = slot.as_mut() {
            p.add_link(a);
        }
    }
}

/// Helper: set trap_exit on a process.
fn set_trap_exit(shared: &SharedState, pid: u64, value: bool) {
    if let Some(entry) = shared.process_bodies.get(&pid) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(p)) = slot.as_mut() {
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

    // Add monitor to watcher process.
    if let Some(entry) = shared.process_bodies.get(&watcher_pid) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(p)) = slot.as_mut() {
            p.add_monitor(monitor);
        }
    }

    // Add monitor to target process.
    if let Some(entry) = shared.process_bodies.get(&target_pid) {
        let mut slot = lock_or_recover(&entry);
        if let Some(ScheduledProcess(p)) = slot.as_mut() {
            p.add_monitor(monitor);
        }
    }

    reference
}

fn make_shared_state() -> Arc<SharedState> {
    Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: crate::process::registry::ProcessTable::new(),
        module_registry: Arc::new(ModuleRegistry::new()),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        next_pid: AtomicU64::new(100),
        wait_set: std::sync::Mutex::new(WaitSet::default()),
        wake_condvar: std::sync::Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        link_set: std::sync::Mutex::new(crate::supervision::link::LinkSet::new()),
        monitor_set: std::sync::Mutex::new(crate::supervision::monitor::MonitorSet::new()),
        hook: crate::hook::Hook::new(),
        timers: Arc::new(std::sync::Mutex::new(crate::timer::TimerWheel::new())),
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
    assert!(!is_alive(&shared, b), "linked process B should die from error exit");
}

#[test]
fn linked_process_survives_normal_exit() {
    let shared = make_shared_state();
    let a = insert_process(&shared, 1);
    let b = insert_process(&shared, 2);
    add_link(&shared, a, b);

    cleanup_exited_process(&shared, a, ExitReason::Normal);

    assert!(!is_alive(&shared, a), "process A should be removed");
    assert!(is_alive(&shared, b), "linked process B should survive normal exit");
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
    // B and C do NOT trap exits.

    cleanup_exited_process(&shared, a, ExitReason::Kill);

    assert!(!is_alive(&shared, a), "process A removed");
    assert!(!is_alive(&shared, b), "B dies from killed signal (not trapping)");
    assert!(!is_alive(&shared, c), "cascade kills C too");

    // Check tombstones record Killed (not Kill).
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

    // A exits with Kill. The signal propagated to B is Killed, which IS
    // trappable. B should survive and receive {EXIT, 1, killed}.
    cleanup_exited_process(&shared, a, ExitReason::Kill);

    assert!(!is_alive(&shared, a), "A removed");
    assert!(is_alive(&shared, b), "B traps exits and survives killed signal");

    let msg = read_mailbox_tuple(&shared, b)
        .unwrap_or_else(|| panic!("B should receive EXIT message"));
    assert_eq!(msg[0], Term::atom(Atom::EXIT));
    assert_eq!(msg[1].as_pid(), Some(1));
    assert_eq!(msg[2], Term::atom(Atom::KILLED), "reason is killed, not kill");
}

#[test]
fn monitor_delivers_down_message_on_exit() {
    let shared = make_shared_state();
    let watcher = insert_process(&shared, 1);
    let target = insert_process(&shared, 2);
    let reference = add_monitor(&shared, watcher, target);

    cleanup_exited_process(&shared, target, ExitReason::Error);

    assert!(!is_alive(&shared, target), "target should be removed");
    assert!(is_alive(&shared, watcher), "watcher stays alive (monitors are non-fatal)");

    let msg = read_mailbox_tuple(&shared, watcher)
        .unwrap_or_else(|| panic!("watcher should have received DOWN message"));
    assert_eq!(msg.len(), 5, "DOWN message should be a 5-tuple");
    assert_eq!(msg[0], Term::atom(Atom::DOWN), "first element is DOWN");
    // msg[1] is the boxed reference — verify via Reference::new
    let ref_term = boxed::Reference::new(msg[1])
        .unwrap_or_else(|| panic!("second element should be a boxed reference"));
    assert_eq!(ref_term.id(), reference, "reference matches");
    assert_eq!(msg[2], Term::atom(Atom::PROCESS), "third element is 'process'");
    assert_eq!(msg[3].as_pid(), Some(2), "fourth element is dead PID");
    assert_eq!(msg[4], Term::atom(Atom::ERROR), "fifth element is reason");
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

    let msg = read_mailbox_tuple(&shared, watcher)
        .unwrap_or_else(|| panic!("watcher receives DOWN"));
    assert_eq!(msg[0], Term::atom(Atom::DOWN));
    let ref_term = boxed::Reference::new(msg[1])
        .unwrap_or_else(|| panic!("reference in DOWN"));
    assert_eq!(ref_term.id(), reference);
}
