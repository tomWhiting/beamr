use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use beamr::atom::Atom;
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::{Capability, NativeEntry, ProcessContext};
use beamr::process::ExitReason;
use beamr::scheduler::dirty::DirtySchedulerKind;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

static DIRTY_STARTED: AtomicBool = AtomicBool::new(false);
static DIRTY_FINISHED: AtomicBool = AtomicBool::new(false);
static NORMAL_PROGRESS: AtomicUsize = AtomicUsize::new(0);

fn module(name: Atom, code: Vec<Instruction>) -> Module {
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
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
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

fn dirty_sleep_value(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    DIRTY_STARTED.store(true, Ordering::Release);
    std::thread::sleep(Duration::from_millis(200));
    DIRTY_FINISHED.store(true, Ordering::Release);
    Ok(Term::small_int(42))
}

fn dirty_badarg(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    Err(Term::atom(Atom::BADARG))
}

fn normal_progress(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    NORMAL_PROGRESS.fetch_add(1, Ordering::AcqRel);
    Ok(Term::small_int(7))
}

fn native_import(
    function: beamr::native::NativeFn,
    dirty_kind: Option<DirtySchedulerKind>,
) -> ResolvedImport {
    ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 0,
        target: ResolvedImportTarget::Native(NativeEntry {
            function,
            dirty_kind,
            capability: Capability::Pure,
        }),
    }
}

fn call_native_module(name: Atom, import: ResolvedImport) -> Module {
    let mut m = module(
        name,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    m.resolved_imports.push(import);
    m
}

fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn dirty_nif_round_trip_does_not_block_normal_scheduler() {
    DIRTY_STARTED.store(false, Ordering::Release);
    DIRTY_FINISHED.store(false, Ordering::Release);
    NORMAL_PROGRESS.store(0, Ordering::Release);

    let registry = Arc::new(ModuleRegistry::new());
    let dirty_module = registry.insert(call_native_module(
        Atom::OK,
        native_import(dirty_sleep_value, Some(DirtySchedulerKind::Cpu)),
    ));
    let normal_module = registry.insert(call_native_module(
        Atom::ERROR,
        native_import(normal_progress, None),
    ));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            dirty_cpu_threads: Some(1),
            dirty_io_threads: Some(1),
            dirty_queue_depth: Some(8),
            io: None,
            node_name: None,
            creation: None,
            distribution: None,
            jit_threshold: None,
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");

    let dirty_pid = scheduler.spawn_process(&dirty_module);
    assert!(wait_until(|| DIRTY_STARTED.load(Ordering::Acquire)));
    assert!(!DIRTY_FINISHED.load(Ordering::Acquire));

    let normal_pid = scheduler.spawn_process(&normal_module);
    let (normal_reason, normal_result) = scheduler.run_until_exit(normal_pid);
    assert_eq!(normal_reason, ExitReason::Normal);
    assert_eq!(normal_result, Term::small_int(7));
    assert_eq!(NORMAL_PROGRESS.load(Ordering::Acquire), 1);
    assert!(!DIRTY_FINISHED.load(Ordering::Acquire));

    let (dirty_reason, dirty_result) = scheduler.run_until_exit(dirty_pid);
    assert_eq!(dirty_reason, ExitReason::Normal);
    assert_eq!(dirty_result, Term::small_int(42));
    assert!(DIRTY_FINISHED.load(Ordering::Acquire));

    scheduler.shutdown();
}

#[test]
fn dirty_nif_error_resumes_and_raises_exception() {
    let registry = Arc::new(ModuleRegistry::new());
    let dirty_module = registry.insert(call_native_module(
        Atom::OK,
        native_import(dirty_badarg, Some(DirtySchedulerKind::Cpu)),
    ));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            dirty_cpu_threads: Some(1),
            dirty_io_threads: Some(1),
            dirty_queue_depth: Some(8),
            io: None,
            node_name: None,
            creation: None,
            distribution: None,
            jit_threshold: None,
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");

    let pid = scheduler.spawn_process(&dirty_module);
    let (reason, _result) = scheduler.run_until_exit(pid);
    assert_eq!(reason, ExitReason::Error);
    let exception = scheduler
        .take_exit_exception(pid)
        .expect("dirty native error captured exception");
    assert_eq!(exception.class, Term::atom(Atom::ERROR));
    assert_eq!(exception.reason, Term::atom(Atom::BADARG));

    scheduler.shutdown();
}
