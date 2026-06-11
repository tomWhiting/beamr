//! Regression tests for residual exception state after a caught raise.
//!
//! `try_case` consumes the caught exception into x0-x2; it must also clear
//! the process's `current_exception`. Without that, a process that caught
//! and handled an exception and then exited normally still surfaced the
//! handled exception to the embedder through `take_exit_exception`.

use std::sync::Arc;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

fn start_scheduler(atoms: &AtomTable) -> (Scheduler, Arc<ModuleRegistry>) {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, atoms).expect("gate1 bifs register");
    let registry = Arc::new(ModuleRegistry::new());
    load_module(
        include_bytes!("fixtures/caught_exception_exit.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("caught_exception_exit fixture loads");
    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");
    (scheduler, registry)
}

#[test]
fn handled_exception_does_not_surface_at_normal_exit() {
    let atoms = AtomTable::with_common_atoms();
    let (scheduler, _registry) = start_scheduler(&atoms);

    let pid = scheduler
        .spawn(
            atoms.intern("caught_exception_exit"),
            atoms.intern("catch_then_normal"),
            vec![],
        )
        .expect("spawn catch_then_normal/0");
    let (reason, result) = scheduler.run_until_exit(pid);
    let exit_exception = scheduler.take_exit_exception(pid);
    scheduler.shutdown();

    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result.root(), Term::atom(Atom::OK));
    assert!(
        exit_exception.is_none(),
        "a handled exception must not surface as a phantom crash: {:?}",
        exit_exception.map(|exception| exception.format_with_atoms(&atoms))
    );
}

#[test]
fn rethrown_unmatched_class_keeps_the_original_class() {
    let atoms = AtomTable::with_common_atoms();
    let (scheduler, _registry) = start_scheduler(&atoms);

    let pid = scheduler
        .spawn(
            atoms.intern("caught_exception_exit"),
            atoms.intern("rethrow_unmatched_class"),
            vec![],
        )
        .expect("spawn rethrow_unmatched_class/0");
    let (reason, _result) = scheduler.run_until_exit(pid);
    let exit_exception = scheduler
        .take_exit_exception(pid)
        .expect("rethrown throw surfaces as an exit exception");
    scheduler.shutdown();

    assert_eq!(reason, ExitReason::Error);
    let boom = atoms.intern("boom");
    assert_eq!(exit_exception.view().class, Term::atom(Atom::THROW));
    assert_eq!(exit_exception.view().reason, Term::atom(boom));
}
