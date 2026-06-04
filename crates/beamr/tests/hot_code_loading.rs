//! End-to-end hot code loading lifecycle tests.

use std::path::PathBuf;
use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::error::LoadError;
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("hot_code")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn scheduler(atom_table: Arc<AtomTable>) -> (Scheduler, Arc<ModuleRegistry>) {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atom_table).expect("gate1 bifs");
    let registry = Arc::new(ModuleRegistry::new());
    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
        },
        Arc::clone(&registry),
        atom_table,
        Arc::new(bifs),
    )
    .expect("scheduler starts");
    (scheduler, registry)
}

#[test]
fn hot_load_replacement_blocks_third_load_until_purge() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));

    let first = scheduler
        .hot_load_module(&fixture("counter_v1.beam"))
        .expect("load v1");
    assert_eq!(first.module_name, counter);
    assert_eq!(first.generation, 1);
    assert!(!first.had_old_version);

    let second = scheduler
        .hot_load_module(&fixture("counter_v2.beam"))
        .expect("load v2");
    assert_eq!(second.generation, 2);
    assert!(second.had_old_version);
    assert!(scheduler.check_old_code(counter));

    let third = scheduler.hot_load_module(&fixture("counter_v1.beam"));
    assert!(matches!(third, Err(LoadError::OldCodeStillRunning)));

    scheduler
        .force_purge_module(counter)
        .expect("force purge old");
    assert!(!scheduler.check_old_code(counter));
    scheduler.shutdown();
}

#[test]
fn new_processes_use_new_version_and_purge_after_old_process_exits() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));

    scheduler
        .hot_load_module(&fixture("counter_v1.beam"))
        .expect("load v1");
    let p1 = scheduler
        .spawn(counter, version, Vec::new())
        .expect("spawn v1 process");
    scheduler
        .hot_load_module(&fixture("counter_v2.beam"))
        .expect("load v2");
    let p2 = scheduler
        .spawn(counter, version, Vec::new())
        .expect("spawn v2 process");

    assert!(scheduler.check_old_code(counter));
    let (reason1, result1) = scheduler.run_until_exit(p1);
    let (reason2, result2) = scheduler.run_until_exit(p2);
    assert_eq!(reason1, ExitReason::Normal);
    assert_eq!(reason2, ExitReason::Normal);
    assert_eq!(result1, Term::small_int(1));
    assert_eq!(result2, Term::small_int(2));

    scheduler.purge_module(counter).expect("safe purge");
    assert!(!scheduler.check_old_code(counter));
    scheduler.shutdown();
}

#[test]
fn on_load_success_commits_and_failure_rolls_back() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let module = atoms.intern("hot_on_load");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));

    let loaded = scheduler
        .hot_load_module(&fixture("on_load_ok.beam"))
        .expect("successful on_load commits");
    assert_eq!(loaded.module_name, module);
    assert!(loaded.on_load_required);
    assert!(loaded.on_load_succeeded);
    let p1 = scheduler
        .spawn(module, version, Vec::new())
        .expect("spawn committed on_load module");
    let (reason1, result1) = scheduler.run_until_exit(p1);
    assert_eq!(reason1, ExitReason::Normal);
    assert_eq!(result1, Term::small_int(1));

    let failed = scheduler
        .hot_load_module(&fixture("on_load_crash.beam"))
        .expect("failed on_load reports rollback result");
    assert!(failed.on_load_required);
    assert!(!failed.on_load_succeeded);
    assert!(!scheduler.check_old_code(module));
    let p2 = scheduler
        .spawn(module, version, Vec::new())
        .expect("spawn retained previous module");
    let (reason2, result2) = scheduler.run_until_exit(p2);
    assert_eq!(reason2, ExitReason::Normal);
    assert_eq!(result2, Term::small_int(1));
    scheduler.shutdown();
}

#[test]
fn closure_fixtures_hot_load_and_versions_are_tracked() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let module = atoms.intern("closure_test");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));

    scheduler
        .hot_load_module(&fixture("closure_test_v1.beam"))
        .expect("load closure v1");
    let p1 = scheduler
        .spawn(module, version, Vec::new())
        .expect("spawn closure v1");
    scheduler
        .hot_load_module(&fixture("closure_test_v2.beam"))
        .expect("load closure v2");
    let p2 = scheduler
        .spawn(module, version, Vec::new())
        .expect("spawn closure v2");

    let (_reason1, result1) = scheduler.run_until_exit(p1);
    let (_reason2, result2) = scheduler.run_until_exit(p2);
    assert_eq!(result1, Term::small_int(1));
    assert_eq!(result2, Term::small_int(2));
    scheduler.force_purge_module(module).expect("force purge");
    scheduler.shutdown();
}
