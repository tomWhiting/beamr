use std::collections::HashMap as StdHashMap;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use dashmap::DashMap;

use super::*;
use crate::atom::{Atom, AtomTable};
use crate::hook::{Hook, HookDecision};
use crate::io::NullSink;
use crate::loader::Instruction;
use crate::loader::decode::compact::Operand;
use crate::mailbox::Mailbox;
use crate::module::Module;
use crate::process::CodePosition;
use crate::process::heap::{DEFAULT_HEAP_SIZE, Heap};
use crate::process::registry::ProcessTable;
use crate::scheduler::execution::{
    SliceOutcome, cleanup_if_tombstoned_after_store, execute_slice, store_runnable_process,
    take_runnable_process,
};
use crate::supervision::link::LinkSet;
use crate::supervision::monitor::MonitorSet;
use crate::term::{Term, boxed};
use crate::timer::TimerWheel;

fn test_module(name: Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect();
    Module {
        name,
        generation: 0,
        exports: StdHashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
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
fn execute_slice_resumes_yielded_process_with_pinned_module_version() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("slice_pin");
    let registry = Arc::new(ModuleRegistry::new());
    let module_v1 = registry.insert(test_module(
        module_name,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    ));
    let shared = Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry: Arc::clone(&registry),
        namespace_store: {
            let store = DashMap::new();
            store.insert(NamespaceId::DEFAULT, Arc::clone(&registry));
            store
        },
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        next_pid: AtomicU64::new(0),
        wait_set: Mutex::new(WaitSet::default()),
        wake_condvar: Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        async_results: DashMap::new(),
        link_set: Mutex::new(LinkSet::new()),
        monitor_set: Mutex::new(MonitorSet::new()),
        hook: Hook::new(),
        timers: Arc::new(Mutex::new(TimerWheel::new())),
        output_sink: Mutex::new(Arc::new(NullSink)),
        atom_table: Arc::new(crate::atom::AtomTable::new()),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
    });
    let mut process = Process::new(1, DEFAULT_HEAP_SIZE);
    process.set_code_position(Some(CodePosition {
        module: module_name,
        instruction_pointer: 0,
    }));
    process.set_current_module(Arc::clone(&module_v1));

    let _module_v2 = registry.insert(test_module(module_name, vec![Instruction::Return]));

    let SliceOutcome::Requeue(resumed) = execute_slice(&shared, &mut process) else {
        panic!("pinned loop should yield again instead of using reloaded return-only module");
    };
    assert!(
        resumed
            .current_module()
            .is_some_and(|current| Arc::ptr_eq(current, &module_v1))
    );
}

#[test]
fn linked_test_spawn_inherits_parent_group_leader_not_child_pid() {
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process(false);
    let parent_group_leader = Term::pid(77);
    assert!(scheduler.set_test_group_leader(parent, parent_group_leader));

    let child = scheduler
        .spawn_linked_test_process(parent)
        .unwrap_or_else(|error| panic!("linked child starts: {error}"));

    assert_eq!(
        scheduler.test_group_leader(child),
        Some(parent_group_leader)
    );
    assert_ne!(scheduler.test_group_leader(child), Some(Term::pid(child)));
}

#[test]
fn spawn_link_uses_executing_parent_namespace_and_merges_parent_link() {
    let atoms = AtomTable::new();
    let module_name = atoms.intern("spawn_link_child");
    let function = atoms.intern("main");
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&registry),
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let namespace = scheduler.create_namespace();
    let namespace_registry = scheduler
        .shared
        .namespace_store
        .get(&namespace)
        .map(|entry| Arc::clone(&entry))
        .unwrap_or_else(|| panic!("namespace registry exists"));
    let mut module = test_module(module_name, vec![Instruction::Label { label: 7 }]);
    module.exports.insert((function, 0), 7);
    let module = namespace_registry.insert(module);
    scheduler.shutdown();
    let parent = scheduler.spawn_test_process_in(namespace, Arc::clone(&module));

    let process = take_runnable_process(&scheduler.shared, parent)
        .unwrap_or_else(|| panic!("parent body taken"));

    let child = scheduler
        .spawn_link(parent, module_name, function, Vec::new())
        .unwrap_or_else(|error| panic!("spawn_link succeeds with executing parent: {error:?}"));

    assert_eq!(scheduler.process_namespace(parent), Some(namespace));
    assert_eq!(scheduler.process_namespace(child), Some(namespace));
    assert!(process_links_contain(&scheduler.shared, parent, child));
    store_runnable_process(&scheduler.shared, process);
    assert!(scheduler.is_linked(parent, child));
}

#[test]
fn tombstone_after_wait_store_prevents_wait_parking() {
    let shared = Arc::new(SharedState {
        shutdown: AtomicBool::new(false),
        process_table: ProcessTable::new(),
        module_registry: Arc::new(ModuleRegistry::new()),
        namespace_store: {
            let registry = Arc::new(ModuleRegistry::new());
            let store = DashMap::new();
            store.insert(NamespaceId::DEFAULT, registry);
            store
        },
        next_namespace_id: AtomicU64::new(1),
        spawn_counter: AtomicUsize::new(0),
        thread_count: 1,
        next_pid: AtomicU64::new(0),
        wait_set: Mutex::new(WaitSet::default()),
        wake_condvar: Condvar::new(),
        process_bodies: DashMap::new(),
        exit_tombstones: DashMap::new(),
        exit_results: DashMap::new(),
        exit_errors: DashMap::new(),
        exit_exceptions: DashMap::new(),
        async_results: DashMap::new(),
        link_set: Mutex::new(LinkSet::new()),
        monitor_set: Mutex::new(MonitorSet::new()),
        hook: Hook::new(),
        timers: Arc::new(Mutex::new(TimerWheel::new())),
        output_sink: Mutex::new(Arc::new(NullSink)),
        atom_table: Arc::new(crate::atom::AtomTable::new()),
        bif_registry: Arc::new(crate::native::BifRegistryImpl::new()),
        capability_policy: Arc::new(crate::native::AllCapabilitiesPolicy),
        idle_parks: AtomicUsize::new(0),
    });
    let pid = 1;
    shared.process_table.spawn_with_pid(pid);
    let process = Process::new(pid, DEFAULT_HEAP_SIZE);
    shared.process_bodies.insert(
        pid,
        Mutex::new(ProcessSlot::Executing(ProcessMetadata {
            namespace_id: NamespaceId::DEFAULT,
            links: Vec::new(),
            monitors: Vec::new(),
            trap_exit: false,
            group_leader: process.group_leader(),
            pending_exit_messages: Vec::new(),
            pending_down_messages: Vec::new(),
        })),
    );
    shared.exit_tombstones.insert(pid, ExitReason::Error);

    store_runnable_process(&shared, process);
    assert!(cleanup_if_tombstoned_after_store(&shared, pid));

    let ws = lock_or_recover(&shared.wait_set);
    assert!(
        !ws.waiting.contains_key(&pid),
        "tombstoned process must not be parked after store-back"
    );
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
