//! Regression test for erlang:is_function/1 and is_function/2 as callable
//! BIFs.
//!
//! Only the literal-arity `is_function2` test OPCODE was handled; no
//! `is_function` BIF was registered anywhere, so imports of
//! erlang:is_function resolved to `Deferred` (erlang is never a loaded
//! module) and every body-position call or variable-arity guard — which
//! the compiler emits as the guard-BIF instruction with an
//! erlang:is_function import — crashed the process at call time.
//!
//! Fixture source: `tests/fixtures/is_function_bif.erl`.

use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::ets::copy::OwnedTerm;
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::{
    BifRegistryImpl, bifs::register_gate1_bifs, gate3_bifs::register_gate3_bifs,
    process_bifs::register_gate2_bifs, stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::term::boxed::Tuple;

fn start_scheduler(atoms: &Arc<AtomTable>) -> Scheduler {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, atoms).expect("gate 1 bifs");
    register_gate2_bifs(&bifs, atoms).expect("gate 2 bifs");
    register_gate3_bifs(&bifs, atoms).expect("gate 3 bifs");
    register_stdlib_stubs(&bifs, atoms).expect("stdlib stubs");
    let registry = Arc::new(ModuleRegistry::new());
    let (_module, _report) = load_module(
        include_bytes!("fixtures/is_function_bif.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("is_function_bif fixture loads");
    Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        registry,
        Arc::clone(atoms),
        Arc::new(bifs),
    )
    .expect("scheduler starts")
}

/// Runs `is_function_bif:<function>/0` to completion.
fn call(scheduler: &Scheduler, atoms: &AtomTable, function: &str) -> (ExitReason, OwnedTerm) {
    let module = atoms.intern("is_function_bif");
    let function = atoms.intern(function);
    let pid = scheduler
        .spawn(module, function, Vec::new())
        .expect("spawn fixture function");
    scheduler.run_until_exit(pid)
}

fn assert_atom_tuple(atoms: &AtomTable, term: Term, expected: &[&str], context: &str) {
    let tuple = Tuple::new(term).unwrap_or_else(|| panic!("{context}: expected tuple result"));
    assert_eq!(tuple.arity(), expected.len(), "{context}: tuple arity");
    for (index, name) in expected.iter().enumerate() {
        let atom = atoms.intern(name);
        assert_eq!(
            tuple.get(index),
            Some(Term::atom(atom)),
            "{context}: element {index} should be {name}"
        );
    }
}

#[test]
fn body_position_is_function_1_returns_booleans() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let (reason, result) = call(&scheduler, &atoms, "body_one");
    assert_eq!(reason, ExitReason::Normal);
    assert_atom_tuple(&atoms, result.root(), &["true", "false"], "body_one");
    scheduler.shutdown();
}

#[test]
fn body_position_is_function_2_checks_exact_arity() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let (reason, result) = call(&scheduler, &atoms, "body_two");
    assert_eq!(reason, ExitReason::Normal);
    assert_atom_tuple(
        &atoms,
        result.root(),
        &["true", "false", "false"],
        "body_two",
    );
    scheduler.shutdown();
}

#[test]
fn variable_arity_guard_routes_through_the_bif() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let (reason, result) = call(&scheduler, &atoms, "guard_variable_arity");
    assert_eq!(reason, ExitReason::Normal);
    assert_atom_tuple(
        &atoms,
        result.root(),
        &["matched", "fallthrough", "fallthrough"],
        "guard_variable_arity",
    );
    scheduler.shutdown();
}

#[test]
fn negative_arity_in_body_position_is_badarg() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let (reason, _result) = call(&scheduler, &atoms, "arity_badarg");
    assert_eq!(reason, ExitReason::Error);
    scheduler.shutdown();
}
