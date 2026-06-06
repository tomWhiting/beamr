//! Namespace-scoped module loading and spawning tests.

use std::path::PathBuf;
use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::error::{ExecError, LoadError};
use beamr::module::ModuleRegistry;
use beamr::namespace::NamespaceId;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
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
    register_gate2_bifs(&bifs, &atom_table).expect("gate2 bifs");
    register_gate3_bifs(&bifs, &atom_table).expect("gate3 bifs");
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
fn namespaces_load_same_module_name_independently() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, default_registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();
    assert_ne!(ns1, ns2);
    assert_ne!(ns1, NamespaceId::DEFAULT);
    assert_ne!(ns2, NamespaceId::DEFAULT);

    let loaded1 = scheduler
        .load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load v1 into ns1");
    let loaded2 = scheduler
        .load_module_in(ns2, &fixture("counter_v2.beam"))
        .expect("load v2 into ns2");

    assert_eq!(loaded1.module_name, counter);
    assert_eq!(loaded2.module_name, counter);
    assert!(default_registry.lookup(counter).is_none());

    let p1 = scheduler
        .spawn_in(ns1, counter, version, Vec::new())
        .expect("spawn ns1 counter");
    let p2 = scheduler
        .spawn_in(ns2, counter, version, Vec::new())
        .expect("spawn ns2 counter");
    let (reason1, result1) = scheduler.run_until_exit(p1);
    let (reason2, result2) = scheduler.run_until_exit(p2);

    assert_eq!(reason1, ExitReason::Normal);
    assert_eq!(reason2, ExitReason::Normal);
    assert_eq!(result1, Term::small_int(1));
    assert_eq!(result2, Term::small_int(2));
    scheduler.shutdown();
}

#[test]
fn default_namespace_spawn_matches_existing_api() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));

    scheduler
        .hot_load_module(&fixture("counter_v1.beam"))
        .expect("load default counter");
    let p = scheduler
        .spawn(counter, version, Vec::new())
        .expect("spawn default counter");
    let (reason, result) = scheduler.run_until_exit(p);

    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result, Term::small_int(1));
    scheduler.shutdown();
}

#[test]
fn namespace_hot_load_does_not_affect_other_namespace() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();

    scheduler
        .hot_load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load ns1 v1");
    scheduler
        .hot_load_module_in(ns2, &fixture("counter_v1.beam"))
        .expect("load ns2 v1");
    scheduler
        .hot_load_module_in(ns1, &fixture("counter_v2.beam"))
        .expect("hot load ns1 v2");

    let p1 = scheduler
        .spawn_in(ns1, counter, version, Vec::new())
        .expect("spawn ns1 current");
    let p2 = scheduler
        .spawn_in(ns2, counter, version, Vec::new())
        .expect("spawn ns2 current");
    let (_reason1, result1) = scheduler.run_until_exit(p1);
    let (_reason2, result2) = scheduler.run_until_exit(p2);

    assert_eq!(result1, Term::small_int(2));
    assert_eq!(result2, Term::small_int(1));
    scheduler.shutdown();
}

#[test]
fn missing_namespace_errors_are_explicit_for_load_and_undef_for_spawn() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let missing = NamespaceId(9_999);

    let load = scheduler.load_module_in(missing, &fixture("counter_v1.beam"));
    assert_eq!(
        load,
        Err(LoadError::UnknownNamespace { namespace: missing })
    );

    let spawn = scheduler.spawn_in(missing, counter, version, Vec::new());
    assert_eq!(
        spawn,
        Err(ExecError::Undef {
            module: counter,
            function: version,
            arity: 0,
        })
    );
    scheduler.shutdown();
}

#[test]
fn force_purge_module_in_only_affects_target_namespace() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();

    scheduler
        .hot_load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load ns1 v1");
    scheduler
        .hot_load_module_in(ns2, &fixture("counter_v1.beam"))
        .expect("load ns2 v1");
    scheduler
        .hot_load_module_in(ns1, &fixture("counter_v2.beam"))
        .expect("hot load ns1 v2");
    scheduler
        .hot_load_module_in(ns2, &fixture("counter_v2.beam"))
        .expect("hot load ns2 v2");

    let blocked = scheduler.hot_load_module_in(ns1, &fixture("counter_v1.beam"));
    assert!(matches!(blocked, Err(LoadError::OldCodeStillRunning)));

    scheduler
        .force_purge_module_in(ns1, counter)
        .expect("force purge ns1 old code");

    scheduler
        .hot_load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("ns1 accepts new version after purge");

    let still_blocked = scheduler.hot_load_module_in(ns2, &fixture("counter_v1.beam"));
    assert!(matches!(still_blocked, Err(LoadError::OldCodeStillRunning)));

    let p = scheduler
        .spawn_in(ns2, counter, version, Vec::new())
        .expect("spawn ns2 current");
    let (reason, result) = scheduler.run_until_exit(p);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result, Term::small_int(2));
    scheduler.shutdown();
}

#[test]
fn spawn_from_process_inherits_parent_namespace() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let parent = atoms.intern("spawn_parent");
    let spawn_child = atoms.intern("spawn_child");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();

    scheduler
        .load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load counter v1 into ns1");
    scheduler
        .load_module_in(ns1, &fixture("spawn_parent.beam"))
        .expect("load spawner into ns1");
    scheduler
        .load_module_in(ns2, &fixture("counter_v2.beam"))
        .expect("load counter v2 into ns2");
    scheduler
        .load_module_in(ns2, &fixture("spawn_parent.beam"))
        .expect("load spawner into ns2");

    let parent_pid = scheduler
        .spawn_in(ns1, parent, spawn_child, Vec::new())
        .expect("spawn parent in ns1");
    let (parent_reason, child_pid_term) = scheduler.run_until_exit(parent_pid);
    assert_eq!(parent_reason, ExitReason::Normal);
    let child_pid = child_pid_term.as_pid().expect("spawn returns child pid");

    assert_eq!(scheduler.process_namespace(child_pid), Some(ns1));
    let (child_reason, child_result) = scheduler.run_until_exit(child_pid);
    assert_eq!(child_reason, ExitReason::Normal);
    assert_eq!(child_result, Term::small_int(1));
    assert!(scheduler.lookup_module_in(ns2, counter).is_some());
    scheduler.shutdown();
}

#[test]
fn deferred_import_resolves_only_in_calling_namespace() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let counter = atoms.intern("counter");
    let caller = atoms.intern("deferred_caller");
    let call_counter = atoms.intern("call_counter");
    let version = atoms.intern("version");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();

    scheduler
        .hot_load_module(&fixture("counter_v2.beam"))
        .expect("load default-only counter");
    scheduler
        .load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load counter in ns1");
    scheduler
        .load_module_in(ns1, &fixture("deferred_caller.beam"))
        .expect("load caller in ns1");
    scheduler
        .load_module_in(ns2, &fixture("deferred_caller.beam"))
        .expect("load caller in ns2");

    let ok_pid = scheduler
        .spawn_in(ns1, caller, call_counter, Vec::new())
        .expect("spawn deferred caller in ns1");
    let (ok_reason, ok_result) = scheduler.run_until_exit(ok_pid);
    assert_eq!(ok_reason, ExitReason::Normal);
    assert_eq!(ok_result, Term::small_int(1));

    let missing_pid = scheduler
        .spawn_in(ns2, caller, call_counter, Vec::new())
        .expect("spawn deferred caller in ns2");
    let (missing_reason, _missing_result) = scheduler.run_until_exit(missing_pid);
    assert_eq!(missing_reason, ExitReason::Error);
    assert_eq!(
        scheduler.take_exit_error(missing_pid),
        Some(ExecError::Undef {
            module: counter,
            function: version,
            arity: 0,
        })
    );
    scheduler.shutdown();
}

#[test]
fn cross_namespace_pid_message_delivery_remains_global() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let ok = atoms.intern("ok");
    let counter = atoms.intern("counter");
    let (scheduler, _registry) = scheduler(Arc::clone(&atoms));
    let ns1 = scheduler.create_namespace();
    let ns2 = scheduler.create_namespace();
    scheduler
        .load_module_in(ns1, &fixture("counter_v1.beam"))
        .expect("load ns1 module");
    scheduler
        .load_module_in(ns2, &fixture("counter_v2.beam"))
        .expect("load ns2 module");
    let module1 = scheduler
        .lookup_module_in(ns1, counter)
        .expect("ns1 counter loaded");
    let module2 = scheduler
        .lookup_module_in(ns2, counter)
        .expect("ns2 counter loaded");
    let sender = scheduler.spawn_test_process_in(ns1, module1);
    let receiver = scheduler.spawn_test_process_in(ns2, module2);

    assert!(scheduler.enqueue_atom_message(receiver, ok));
    assert_eq!(scheduler.has_message(receiver, Term::atom(ok)), Some(true));
    assert_eq!(scheduler.process_namespace(sender), Some(ns1));
    assert_eq!(scheduler.process_namespace(receiver), Some(ns2));
    scheduler.shutdown();
}
