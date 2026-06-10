//! Lost-wakeup regression test: host message delivery racing NIF suspend.
//!
//! Embedders deliver completion markers with `enqueue_atom_message` while the
//! target process may still be executing the very slice that is about to
//! suspend (two-phase await: dispatch, then suspend until the marker
//! arrives). Delivery must succeed in that window and the process must be
//! resumed once it parks — otherwise the marker is dropped, the wake is lost,
//! and the process sleeps forever.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use beamr::atom::Atom;
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::{Capability, NativeEntry, ProcessContext};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

/// Test phases: 0 = not started, 1 = native is mid-slice (Executing),
/// 2 = test delivered the marker, native may suspend.
static PHASE: AtomicUsize = AtomicUsize::new(0);

fn await_marker(_args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if PHASE.load(Ordering::Acquire) == 0 {
        // First invocation: hold the slice open until the test has delivered
        // the marker while this process is still Executing, then suspend —
        // exactly the dispatch→deliver→suspend interleaving that loses the
        // wake without pending-merge handling.
        PHASE.store(1, Ordering::Release);
        while PHASE.load(Ordering::Acquire) != 2 {
            std::thread::yield_now();
        }
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    // Re-invoked after the wakeup: report completion.
    Ok(Term::small_int(42))
}

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

#[test]
fn marker_delivered_while_executing_resumes_the_suspending_process() {
    let registry = Arc::new(ModuleRegistry::new());
    let mut awaiting = module(
        Atom::OK,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    awaiting.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 0,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: await_marker,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    });
    let awaiting = registry.insert(awaiting);

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");

    let pid = scheduler.spawn_process(&awaiting);
    while PHASE.load(Ordering::Acquire) != 1 {
        std::thread::yield_now();
    }
    // The process slot is Executing right now: the native holds the slice
    // open. Delivery must still succeed (pending-metadata path).
    assert!(
        scheduler.enqueue_atom_message(pid, Atom::ERROR),
        "delivery to an executing process must not be dropped"
    );
    PHASE.store(2, Ordering::Release);

    // The process suspends after delivery; the merged marker must resume it.
    // A lost wakeup hangs forever, so the wait is bounded.
    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let outcome = scheduler.run_until_exit(pid);
        let _ = sender.send(outcome);
        scheduler.shutdown();
    });
    let (reason, result) = receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("suspended process was never resumed: the wakeup was lost");
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result.root(), Term::small_int(42));
    waiter.join().expect("waiter thread panicked");
}

/// Dirty-call phases: 0 = not started, 1 = dirty native running,
/// 2 = test delivered an unrelated message, native may finish.
static DIRTY_PHASE: AtomicUsize = AtomicUsize::new(0);

fn dirty_await_release(_args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    DIRTY_PHASE.store(1, Ordering::Release);
    while DIRTY_PHASE.load(Ordering::Acquire) != 2 {
        std::thread::yield_now();
    }
    Ok(Term::small_int(7))
}

#[test]
fn delivery_during_in_flight_dirty_call_does_not_resume_the_process() {
    let registry = Arc::new(ModuleRegistry::new());
    let mut awaiting = module(
        Atom::ERROR,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    awaiting.resolved_imports.push(ResolvedImport {
        module: Atom::ERROR,
        function: Atom::ERROR,
        arity: 0,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: dirty_await_release,
            dirty_kind: Some(beamr::scheduler::dirty::DirtySchedulerKind::Cpu),
            capability: Capability::Pure,
        }),
    });
    let awaiting = registry.insert(awaiting);

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            dirty_cpu_threads: Some(1),
            dirty_io_threads: Some(1),
            dirty_queue_depth: Some(8),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");

    let pid = scheduler.spawn_process(&awaiting);
    while DIRTY_PHASE.load(Ordering::Acquire) != 1 {
        std::thread::yield_now();
    }
    // An unrelated mailbox message arrives while the dirty call is still in
    // flight. It must be delivered, but it must NOT resume the process — a
    // premature resume re-executes the dirty call instruction in an illegal
    // state and kills the workflow.
    assert!(scheduler.enqueue_atom_message(pid, Atom::OK));
    std::thread::sleep(Duration::from_millis(100));
    DIRTY_PHASE.store(2, Ordering::Release);

    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let outcome = scheduler.run_until_exit(pid);
        let _ = sender.send(outcome);
        scheduler.shutdown();
    });
    let (reason, result) = receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("dirty call never completed");
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result.root(), Term::small_int(7));
    waiter.join().expect("waiter thread panicked");
}
