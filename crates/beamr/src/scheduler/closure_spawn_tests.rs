//! Tests for `Scheduler::spawn_link_closure` — the linked thunk-child spawn
//! with a deep-copied environment.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::loader::Instruction;
use crate::loader::LambdaEntry;
use crate::loader::decode::compact::Operand;
use crate::module::{Module, ModuleOrigin, ModuleRegistry};
use crate::process::{ExitReason, Process};
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{write_closure, write_export_fun};

/// Build a module whose lambda `unique_id` points at code that returns `x0`
/// (a zero-arity thunk with one free variable) and whose `echo/0` export does
/// the same for export-fun coverage.
fn thunk_module(name: Atom, echo_export: Atom, unique_id: u64) -> Module {
    let label = 1;
    let code = vec![Instruction::Label { label }, Instruction::Return];
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::from([((echo_export, 0), label)]),
        label_index: HashMap::from([(label, 0)]),
        code,
        function_table: Vec::new(),
        line_table: Vec::new(),
        literals: Vec::new(),
        constant_pool: crate::constant_pool::ConstantPool::new(),
        resolved_imports: Vec::new(),
        lambdas: vec![LambdaEntry {
            function: echo_export,
            arity: 1,
            label,
            num_free: 1,
            unique_id,
        }],
        string_table: Vec::new(),
        line_info: Vec::new(),
    }
}

/// Build a module whose lambda parks forever in `Wait` (a hanging thunk).
fn waiting_module(name: Atom, function: Atom, unique_id: u64) -> Module {
    let label = 1;
    let code = vec![
        Instruction::Label { label },
        Instruction::Wait {
            fail: Operand::Label(label),
        },
    ];
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
        label_index: HashMap::from([(label, 0)]),
        code,
        function_table: Vec::new(),
        line_table: Vec::new(),
        literals: Vec::new(),
        constant_pool: crate::constant_pool::ConstantPool::new(),
        resolved_imports: Vec::new(),
        lambdas: vec![LambdaEntry {
            function,
            arity: 0,
            label,
            num_free: 0,
            unique_id,
        }],
        string_table: Vec::new(),
        line_info: Vec::new(),
    }
}

struct Harness {
    scheduler: Scheduler,
    atoms: Arc<AtomTable>,
    module: Arc<Module>,
    parent: u64,
}

fn harness(unique_id: u64) -> Harness {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let registry = Arc::new(ModuleRegistry::new());
    let module_name = atoms.intern("closure_spawn_fixture");
    let echo = atoms.intern("echo");
    let module = registry.insert(thunk_module(module_name, echo, unique_id));
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let parent = scheduler.spawn_test_process(false);
    Harness {
        scheduler,
        atoms,
        module,
        parent,
    }
}

/// Allocate a heap binary on `host`'s heap and return its term.
fn host_binary(host: &mut Process, bytes: &[u8]) -> Term {
    let words = 2 + crate::term::binary::packed_word_count(bytes.len());
    let slice = host
        .heap_mut()
        .alloc_slice(words)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    crate::term::binary::write_binary(slice, bytes).unwrap_or_else(|| panic!("write_binary"))
}

/// Allocate a thunk closure (arity 0) on `host`'s heap.
fn host_closure(host: &mut Process, module: &Module, unique_id: u64, free_vars: &[Term]) -> Term {
    let words = 7 + free_vars.len();
    let slice = host
        .heap_mut()
        .alloc_slice(words)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    write_closure(
        slice,
        module.name,
        0,
        0,
        module.generation(),
        unique_id,
        free_vars,
    )
    .unwrap_or_else(|| panic!("write_closure"))
}

fn binary_bytes(term: Term) -> Vec<u8> {
    BinaryRef::new(term)
        .map(|binary| binary.as_bytes().to_vec())
        .unwrap_or_else(|| panic!("exit result should be a binary, got {term:?}"))
}

#[test]
fn spawn_link_closure_runs_thunk_with_deep_copied_env() {
    let fixture = harness(11);
    // The closure and its captured binary live on a HOST heap that is dropped
    // before the child runs: only a deep copy into the child heap survives.
    let mut host = Process::new(9_999, 64);
    let payload = b"deep-copy-proof";
    let captured = host_binary(&mut host, payload);
    let closure = host_closure(&mut host, &fixture.module, 11, &[captured]);

    let child = fixture
        .scheduler
        .spawn_link_closure(fixture.parent, closure)
        .unwrap_or_else(|error| panic!("spawn_link_closure: {error}"));
    assert!(fixture.scheduler.is_linked(fixture.parent, child));
    drop(host);

    let (reason, result) = fixture.scheduler.run_until_exit(child);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(binary_bytes(result.root()), payload.to_vec());
    fixture.scheduler.shutdown();
}

#[test]
fn environment_larger_than_the_default_heap_forces_doubling_and_survives() {
    let fixture = harness(23);
    // 64 KiB of captured binary vastly exceeds DEFAULT_HEAP_SIZE (233 words),
    // so the copy must retry with a doubled child heap.
    let payload: Vec<u8> = (0..65_536_u32).map(|index| (index % 251) as u8).collect();
    let mut host = Process::new(9_998, 16_384);
    let captured = host_binary(&mut host, &payload);
    let closure = host_closure(&mut host, &fixture.module, 23, &[captured]);

    let child = fixture
        .scheduler
        .spawn_link_closure(fixture.parent, closure)
        .unwrap_or_else(|error| panic!("spawn_link_closure: {error}"));
    drop(host);

    let (reason, result) = fixture.scheduler.run_until_exit(child);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(binary_bytes(result.root()), payload);
    fixture.scheduler.shutdown();
}

#[test]
fn stale_generation_closure_resolves_through_unique_id_fallback() {
    let fixture = harness(31);
    let mut host = Process::new(9_997, 64);
    let captured = host_binary(&mut host, b"stale-gen");
    // Generation 0 never matches the registry-assigned generation (1), so
    // resolution must fall back to the unique-id lambda search.
    let words = host
        .heap_mut()
        .alloc_slice(8)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    let closure = write_closure(words, fixture.module.name, 0, 0, 0, 31, &[captured])
        .unwrap_or_else(|| panic!("write_closure"));

    let child = fixture
        .scheduler
        .spawn_link_closure(fixture.parent, closure)
        .unwrap_or_else(|error| panic!("spawn_link_closure: {error}"));
    let (reason, result) = fixture.scheduler.run_until_exit(child);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(binary_bytes(result.root()), b"stale-gen".to_vec());
    fixture.scheduler.shutdown();
}

#[test]
fn export_fun_thunk_spawns_through_the_export_table() {
    let fixture = harness(41);
    let mut host = Process::new(9_996, 32);
    let words = host
        .heap_mut()
        .alloc_slice(7)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    let echo = fixture.atoms.intern("echo");
    let export = write_export_fun(words, fixture.module.name, echo, 0)
        .unwrap_or_else(|| panic!("write_export_fun"));

    let child = fixture
        .scheduler
        .spawn_link_closure(fixture.parent, export)
        .unwrap_or_else(|error| panic!("spawn_link_closure: {error}"));
    let (reason, _result) = fixture.scheduler.run_until_exit(child);
    assert_eq!(reason, ExitReason::Normal);
    fixture.scheduler.shutdown();
}

#[test]
fn killing_the_parent_kills_the_linked_thunk_child() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let registry = Arc::new(ModuleRegistry::new());
    let module_name = atoms.intern("closure_wait_fixture");
    let function = atoms.intern("hang");
    let module = registry.insert(waiting_module(module_name, function, 53));
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
    )
    .unwrap_or_else(|error| panic!("scheduler starts: {error}"));
    let parent = scheduler.spawn_test_process(false);

    let mut host = Process::new(9_995, 32);
    let closure = host_closure(&mut host, &module, 53, &[]);
    let child = scheduler
        .spawn_link_closure(parent, closure)
        .unwrap_or_else(|error| panic!("spawn_link_closure: {error}"));
    assert!(scheduler.is_linked(parent, child));

    scheduler.terminate_process(parent, ExitReason::Kill);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if scheduler.peek_exit_reason(child).is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "linked child was not killed by the parent's exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    scheduler.shutdown();
}

#[test]
fn non_closure_and_wrong_arity_and_dead_parent_are_typed_errors() {
    let fixture = harness(61);
    // Not a closure at all.
    let result = fixture
        .scheduler
        .spawn_link_closure(fixture.parent, Term::small_int(7));
    assert!(matches!(result, Err(ExecError::Badfun { .. })));

    // A closure whose arity is not zero cannot run as a thunk.
    let mut host = Process::new(9_994, 32);
    let words = host
        .heap_mut()
        .alloc_slice(7)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    let unary = write_closure(
        words,
        fixture.module.name,
        0,
        1,
        fixture.module.generation(),
        61,
        &[],
    )
    .unwrap_or_else(|| panic!("write_closure"));
    let result = fixture.scheduler.spawn_link_closure(fixture.parent, unary);
    assert!(matches!(result, Err(ExecError::Badarity { .. })));

    // A dead parent refuses the spawn outright.
    let dead_parent = fixture.scheduler.spawn_test_process(false);
    fixture
        .scheduler
        .terminate_process(dead_parent, ExitReason::Kill);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fixture.scheduler.process_table().get(dead_parent).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "killed parent never left the process table"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let closure = host_closure(&mut host, &fixture.module, 61, &[]);
    let result = fixture.scheduler.spawn_link_closure(dead_parent, closure);
    assert!(matches!(result, Err(ExecError::Badarg)));

    // An unknown module is a Badfun, nothing is spawned.
    let unknown = fixture.atoms.intern("no_such_module");
    let words = host
        .heap_mut()
        .alloc_slice(7)
        .unwrap_or_else(|error| panic!("host heap alloc: {error}"));
    let orphan =
        write_closure(words, unknown, 0, 0, 1, 61, &[]).unwrap_or_else(|| panic!("write_closure"));
    let result = fixture.scheduler.spawn_link_closure(fixture.parent, orphan);
    assert!(matches!(result, Err(ExecError::Badfun { .. })));

    fixture.scheduler.shutdown();
}
