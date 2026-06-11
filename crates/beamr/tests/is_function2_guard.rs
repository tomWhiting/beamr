//! Regression test for the is_function/2 guard on erlc-compiled code.
//!
//! Opcode 115 (is_function2) carries three operands — fail label, function
//! source, and arity — but the decoder used to route it through the generic
//! two-operand `type_test` helper, dropping the arity. Every compiled
//! `is_function(F, N)` guard then crashed the process with
//! `InvalidOperand("is_function2 operands")` instead of branching.

use beamr::atom::AtomTable;
use beamr::loader::{Instruction, prepare_module};
use beamr::module::{Module, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;

use beamr::interpreter::{ExecutionResult, run};

fn load_fixture(atoms: &AtomTable) -> Module {
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    let (module, _report) = prepare_module(
        include_bytes!("fixtures/is_function2_guard.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("is_function2 guard fixture prepares");
    module
}

fn call(module: &Module, atoms: &AtomTable, function: &str) -> Term {
    let function = atoms.intern(function);
    let label = *module
        .exports
        .get(&(function, 0))
        .expect("export exists");
    let entry_ip = module
        .code
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Label { label: candidate } if *candidate == label))
        .expect("export label exists");
    let mut process = Process::new(1, 4096);
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));

    assert_eq!(
        run(&mut process, module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    process.x_reg(0)
}

#[test]
fn guard_passes_for_a_fun_of_the_stated_arity() {
    let atoms = AtomTable::new();
    let module = load_fixture(&atoms);
    let matched = atoms.intern("matched");
    assert_eq!(
        call(&module, &atoms, "matching_arity"),
        Term::atom(matched)
    );
}

#[test]
fn guard_falls_through_for_a_fun_of_the_wrong_arity() {
    let atoms = AtomTable::new();
    let module = load_fixture(&atoms);
    let fallthrough = atoms.intern("fallthrough");
    assert_eq!(
        call(&module, &atoms, "wrong_arity"),
        Term::atom(fallthrough)
    );
}

#[test]
fn guard_falls_through_for_a_non_fun() {
    let atoms = AtomTable::new();
    let module = load_fixture(&atoms);
    let fallthrough = atoms.intern("fallthrough");
    assert_eq!(call(&module, &atoms, "not_a_fun"), Term::atom(fallthrough));
}
