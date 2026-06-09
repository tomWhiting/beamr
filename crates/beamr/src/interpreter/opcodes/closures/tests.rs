use super::*;
use crate::atom::AtomTable;
use crate::interpreter::{ExecutionResult, run_with_registry};
use crate::loader::{Instruction, LambdaEntry};
use crate::module::{ModuleOrigin, ModuleRegistry};
use crate::process::ExitReason;
use std::collections::HashMap;
use std::sync::Arc;

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

fn jump_ip(outcome: InstructionOutcome) -> usize {
    match outcome {
        InstructionOutcome::Jump(position) => position.instruction_pointer,
        other => panic!("expected jump, got {other:?}"),
    }
}

#[test]
fn make_fun_captures_exact_free_variables() {
    let mut module = module(Atom::OK, Vec::new());
    module.lambdas.push(LambdaEntry {
        function: Atom::OK,
        arity: 1,
        label: 7,
        num_free: 2,
        unique_id: 11,
    });
    let mut process = Process::new(1, 16);
    process.set_x_reg(0, Term::small_int(42));
    process.set_x_reg(1, Term::small_int(99));
    process.set_x_reg(2, Term::small_int(123));

    let outcome = make_fun(&mut process, &module, &[Operand::Unsigned(0)]);

    assert_eq!(outcome, Ok(InstructionOutcome::Continue));
    let closure = Closure::new(process.x_reg(0)).expect("closure term");
    assert_eq!(closure.arity(), 1);
    assert_eq!(closure.num_free(), 2);
    assert_eq!(closure.generation(), module.generation());
    assert_eq!(closure.unique_id(), 11);
    assert_eq!(closure.free_var(0), Some(Term::small_int(42)));
    assert_eq!(closure.free_var(1), Some(Term::small_int(99)));
    assert_eq!(closure.free_var(2), None);
}

#[test]
fn make_fun_with_no_free_variables_creates_empty_closure() {
    let mut module = module(Atom::OK, Vec::new());
    module.lambdas.push(LambdaEntry {
        function: Atom::OK,
        arity: 0,
        label: 1,
        num_free: 0,
        unique_id: 12,
    });
    let mut process = Process::new(1, 8);

    make_fun(&mut process, &module, &[Operand::Unsigned(0)]).expect("make_fun succeeds");

    let closure = Closure::new(process.x_reg(0)).expect("closure term");
    assert_eq!(closure.arity(), 0);
    assert_eq!(closure.num_free(), 0);
}

#[test]
fn call_fun_restores_captured_variables_and_jumps_to_lambda_label() {
    let mut module = module(
        Atom::OK,
        vec![Instruction::Return, Instruction::Label { label: 10 }],
    );
    module.lambdas.push(LambdaEntry {
        function: Atom::OK,
        arity: 1,
        label: 10,
        num_free: 1,
        unique_id: 13,
    });
    let mut process = Process::new(1, 16);
    process.set_x_reg(0, Term::small_int(7));
    process.set_x_reg(1, Term::small_int(42));
    make_fun(&mut process, &module, &[Operand::Unsigned(0)]).expect("make_fun succeeds");
    let fun = process.x_reg(0);
    process.set_x_reg(0, Term::small_int(11));
    process.set_x_reg(1, fun);

    let outcome = call_fun(&mut process, &module, &Operand::Unsigned(1), 99, None)
        .expect("call_fun succeeds");

    assert_eq!(jump_ip(outcome), 1);
    assert_eq!(process.x_reg(1), Term::small_int(7));
    assert_eq!(process.stack().len(), 1);
}

#[test]
fn call_fun_reports_badfun_and_badarity() {
    let mut module = module(Atom::OK, vec![Instruction::Label { label: 1 }]);
    module.lambdas.push(LambdaEntry {
        function: Atom::OK,
        arity: 2,
        label: 1,
        num_free: 0,
        unique_id: 14,
    });
    let mut process = Process::new(1, 16);
    process.set_x_reg(1, Term::small_int(42));
    assert_eq!(
        call_fun(&mut process, &module, &Operand::Unsigned(1), 1, None),
        Err(ExecError::Badfun {
            term: Term::small_int(42)
        })
    );

    process.set_x_reg(0, Term::small_int(1));
    make_fun(&mut process, &module, &[Operand::Unsigned(0)]).expect("make_fun succeeds");
    let fun = process.x_reg(0);
    process.set_x_reg(0, Term::small_int(10));
    process.set_x_reg(1, fun);
    assert_eq!(
        call_fun(&mut process, &module, &Operand::Unsigned(1), 1, None),
        Err(ExecError::Badarity {
            fun,
            args: vec![Term::small_int(10)],
        })
    );
}

#[test]
fn call_fun_resolves_reloaded_module_by_unique_id() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("hot");
    let callback_atom = atoms.intern("callback@anon");
    let other_atom = atoms.intern("other@anon");
    let callback_id = crate::loader::lambda_unique_id(&atoms, module_atom, callback_atom, 0, 0)
        .expect("callback id");
    let other_id =
        crate::loader::lambda_unique_id(&atoms, module_atom, other_atom, 0, 0).expect("other id");
    let registry = ModuleRegistry::new();

    let mut v1 = module(module_atom, vec![Instruction::Label { label: 10 }]);
    v1.lambdas.push(LambdaEntry {
        function: callback_atom,
        arity: 0,
        label: 10,
        num_free: 0,
        unique_id: callback_id,
    });
    let v1 = registry.insert(v1);
    let mut process = Process::new(1, 16);
    make_fun(&mut process, &v1, &[Operand::Unsigned(0)]).expect("make_fun v1");
    let fun = process.x_reg(0);

    let mut v2 = module(
        module_atom,
        vec![
            Instruction::Label { label: 20 },
            Instruction::Return,
            Instruction::Label { label: 30 },
        ],
    );
    v2.lambdas.push(LambdaEntry {
        function: other_atom,
        arity: 0,
        label: 20,
        num_free: 0,
        unique_id: other_id,
    });
    v2.lambdas.push(LambdaEntry {
        function: callback_atom,
        arity: 0,
        label: 30,
        num_free: 0,
        unique_id: callback_id,
    });
    let v2 = registry.insert(v2);
    process.set_x_reg(0, fun);

    let outcome = call_fun(
        &mut process,
        &v2,
        &Operand::Unsigned(0),
        99,
        Some(&registry),
    )
    .expect("call_fun resolves by unique id");

    assert_eq!(jump_ip(outcome), 2);
    assert_eq!(
        process.current_module().map(|module| module.generation()),
        Some(2)
    );
}

#[test]
fn call_fun_reports_badfun_when_reloaded_lambda_removed() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("hot_removed");
    let callback_atom = atoms.intern("callback@anon");
    let callback_id = crate::loader::lambda_unique_id(&atoms, module_atom, callback_atom, 0, 0)
        .expect("callback id");
    let registry = ModuleRegistry::new();

    let mut v1 = module(module_atom, vec![Instruction::Label { label: 10 }]);
    v1.lambdas.push(LambdaEntry {
        function: callback_atom,
        arity: 0,
        label: 10,
        num_free: 0,
        unique_id: callback_id,
    });
    let v1 = registry.insert(v1);
    let mut process = Process::new(1, 16);
    make_fun(&mut process, &v1, &[Operand::Unsigned(0)]).expect("make_fun v1");
    let fun = process.x_reg(0);
    registry.insert(module(module_atom, vec![Instruction::Label { label: 20 }]));
    let v3 = registry.insert(module(module_atom, vec![Instruction::Label { label: 30 }]));
    process.set_x_reg(0, fun);

    assert_eq!(
        call_fun(
            &mut process,
            &v3,
            &Operand::Unsigned(0),
            99,
            Some(&registry)
        ),
        Err(ExecError::Badfun { term: fun })
    );
}

#[test]
fn apply_uses_registry_exports_and_rejects_missing_or_private_targets() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("math");
    let add_atom = atoms.intern("add");
    let private_atom = atoms.intern("private");
    let mut target = module(
        module_atom,
        vec![
            Instruction::Label { label: 3 },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Return,
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    target.exports.insert((add_atom, 2), 3);
    let registry = ModuleRegistry::new();
    registry.insert(target);
    let caller = module(Atom::OK, Vec::new());
    let mut process = Process::new(1, 16);
    process.set_current_module(Arc::new(caller.clone()));
    process.set_x_reg(0, Term::small_int(1));
    process.set_x_reg(1, Term::small_int(2));
    process.set_x_reg(2, Term::atom(module_atom));
    process.set_x_reg(3, Term::atom(add_atom));

    let outcome = apply(
        &mut process,
        &registry,
        &Operand::Unsigned(2),
        5,
        caller.name,
    )
    .expect("apply succeeds");

    assert_eq!(jump_ip(outcome), 0);
    assert_eq!(process.stack().len(), 1);
    process.set_x_reg(3, Term::atom(private_atom));
    assert!(matches!(
        apply(&mut process, &registry, &Operand::Unsigned(2), 5, caller.name),
        Err(ExecError::Undef { module, function, arity: 2 })
            if module == module_atom && function == private_atom
    ));
}

#[test]
fn apply_uses_latest_registry_export_after_module_reload() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("math");
    let value_atom = atoms.intern("value");
    let registry = ModuleRegistry::new();

    let mut first_target = module(
        module_atom,
        vec![
            Instruction::Label { label: 5 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    first_target.exports.insert((value_atom, 0), 5);
    registry.insert(first_target);

    let caller = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Atom(Some(module_atom)),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(value_atom)),
                destination: Operand::X(1),
            },
            Instruction::Apply {
                arity: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    let mut first_process = Process::new(1, 32);
    assert_eq!(
        run_with_registry(&mut first_process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(first_process.x_reg(0), Term::small_int(1));

    let mut second_target = module(
        module_atom,
        vec![
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(2),
            },
            Instruction::Label { label: 12 },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    second_target.exports.insert((value_atom, 0), 12);
    registry.insert(second_target);
    let mut second_process = Process::new(2, 32);
    assert_eq!(
        run_with_registry(&mut second_process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(second_process.x_reg(0), Term::small_int(2));
}

#[test]
fn apply_last_deallocates_current_frame_before_jump() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("math");
    let add_atom = atoms.intern("add");
    let mut target = module(module_atom, vec![Instruction::Label { label: 3 }]);
    target.exports.insert((add_atom, 0), 3);
    let registry = ModuleRegistry::new();
    registry.insert(target);
    let mut process = Process::new(1, 16);
    process
        .stack_mut()
        .push_frame(Atom::OK, 123, Arc::new(module(Atom::OK, Vec::new())), 0)
        .expect("frame push");
    process.set_x_reg(0, Term::atom(module_atom));
    process.set_x_reg(1, Term::atom(add_atom));

    let outcome = apply_last(
        &mut process,
        &registry,
        &Operand::Unsigned(0),
        &Operand::Unsigned(0),
        5,
    )
    .expect("apply_last succeeds");

    assert_eq!(jump_ip(outcome), 0);
    assert_eq!(process.stack().len(), 0);
}

#[test]
fn apply_last_uses_latest_registry_export_after_module_reload() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("math");
    let value_atom = atoms.intern("value");
    let registry = ModuleRegistry::new();

    let mut first_target = module(module_atom, vec![Instruction::Label { label: 3 }]);
    first_target.exports.insert((value_atom, 0), 3);
    registry.insert(first_target);
    let mut process = Process::new(1, 16);
    process
        .stack_mut()
        .push_frame(Atom::OK, 123, Arc::new(module(Atom::OK, Vec::new())), 0)
        .expect("frame push");
    process.set_x_reg(0, Term::atom(module_atom));
    process.set_x_reg(1, Term::atom(value_atom));
    assert_eq!(
        jump_ip(
            apply_last(
                &mut process,
                &registry,
                &Operand::Unsigned(0),
                &Operand::Unsigned(0),
                5,
            )
            .expect("apply_last succeeds")
        ),
        0
    );

    let mut second_target = module(
        module_atom,
        vec![Instruction::Return, Instruction::Label { label: 12 }],
    );
    second_target.exports.insert((value_atom, 0), 12);
    registry.insert(second_target);
    let mut reloaded_process = Process::new(2, 16);
    reloaded_process
        .stack_mut()
        .push_frame(Atom::OK, 123, Arc::new(module(Atom::OK, Vec::new())), 0)
        .expect("frame push");
    reloaded_process.set_x_reg(0, Term::atom(module_atom));
    reloaded_process.set_x_reg(1, Term::atom(value_atom));
    let outcome = apply_last(
        &mut reloaded_process,
        &registry,
        &Operand::Unsigned(0),
        &Operand::Unsigned(0),
        5,
    )
    .expect("apply_last after reload succeeds");

    assert_eq!(jump_ip(outcome), 1);
    assert_eq!(reloaded_process.stack().len(), 0);
}

#[test]
fn map_assoc_creates_map_and_get_map_elements_extracts_values() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Label { label: 99 },
            Instruction::Label { label: 100 },
        ],
    );
    let mut process = Process::new(1, 32);
    let empty = empty_map(&mut process);
    process.set_x_reg(0, empty);

    let outcome = map_op(
        &mut process,
        &module,
        MapOp::PutMapAssoc,
        &[
            Operand::Label(99),
            Operand::X(0),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::List(vec![
                Operand::Atom(Some(Atom::OK)),
                Operand::Integer(1),
                Operand::Atom(Some(Atom::ERROR)),
                Operand::Integer(2),
            ]),
        ],
        None,
    );
    assert_eq!(outcome, Ok(InstructionOutcome::Continue));
    let created = Map::new(process.x_reg(1)).expect("map");
    assert_eq!(created.get(Term::atom(Atom::OK)), Some(Term::small_int(1)));
    assert_eq!(Map::new(empty).expect("source map").len(), 0);

    let outcome = map_op(
        &mut process,
        &module,
        MapOp::GetMapElements,
        &[
            Operand::Label(99),
            Operand::X(1),
            Operand::List(vec![Operand::Atom(Some(Atom::OK)), Operand::X(2)]),
        ],
        None,
    );
    assert_eq!(outcome, Ok(InstructionOutcome::Continue));
    assert_eq!(process.x_reg(2), Term::small_int(1));
}

#[test]
fn map_tests_and_exact_update_branch_on_missing_keys() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Label { label: 99 },
            Instruction::Label { label: 100 },
        ],
    );
    let mut process = Process::new(1, 32);
    let source = map_from_pairs(&mut process, &[(Term::atom(Atom::OK), Term::small_int(1))]);
    process.set_x_reg(0, source);

    assert_eq!(
        map_op(
            &mut process,
            &module,
            MapOp::HasMapFields,
            &[
                Operand::Label(99),
                Operand::X(0),
                Operand::List(vec![Operand::Atom(Some(Atom::OK))]),
            ],
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            map_op(
                &mut process,
                &module,
                MapOp::HasMapFields,
                &[
                    Operand::Label(99),
                    Operand::X(0),
                    Operand::List(vec![Operand::Atom(Some(Atom::ERROR))]),
                ],
                None,
            )
            .expect("missing key branches")
        ),
        0
    );
    assert_eq!(
        jump_ip(
            map_op(
                &mut process,
                &module,
                MapOp::GetMapElements,
                &[
                    Operand::Label(99),
                    Operand::X(0),
                    Operand::List(vec![Operand::Atom(Some(Atom::ERROR)), Operand::X(1)]),
                ],
                None,
            )
            .expect("missing key branches")
        ),
        0
    );
    assert_eq!(process.x_reg(1), Term::NIL);

    assert_eq!(
        map_op(
            &mut process,
            &module,
            MapOp::PutMapExact,
            &[
                Operand::Label(99),
                Operand::X(0),
                Operand::X(2),
                Operand::Unsigned(0),
                Operand::List(vec![Operand::Atom(Some(Atom::OK)), Operand::Integer(2)]),
            ],
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        Map::new(process.x_reg(2))
            .expect("updated map")
            .get(Term::atom(Atom::OK)),
        Some(Term::small_int(2))
    );
    assert_eq!(
        Map::new(source)
            .expect("source map")
            .get(Term::atom(Atom::OK)),
        Some(Term::small_int(1))
    );
    assert_eq!(
        jump_ip(
            map_op(
                &mut process,
                &module,
                MapOp::PutMapExact,
                &[
                    Operand::Label(99),
                    Operand::X(0),
                    Operand::X(3),
                    Operand::Unsigned(0),
                    Operand::List(vec![Operand::Atom(Some(Atom::ERROR)), Operand::Integer(2)]),
                ],
                None,
            )
            .expect("missing exact key branches")
        ),
        0
    );
}

#[test]
fn dispatch_and_run_with_registry_execute_new_opcode_families() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("math");
    let function_atom = atoms.intern("answer");
    let caller_atom = atoms.intern("caller");
    let mut target = module(
        module_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    target.exports.insert((function_atom, 0), 1);
    let caller = module(
        caller_atom,
        vec![
            Instruction::Move {
                source: Operand::Atom(Some(module_atom)),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(function_atom)),
                destination: Operand::X(1),
            },
            Instruction::Apply {
                arity: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    let registry = ModuleRegistry::new();
    registry.insert(target);
    let caller = registry.insert(caller);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run_with_registry(&mut process, &caller, &registry),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

fn empty_map(process: &mut Process) -> Term {
    map_from_pairs(process, &[])
}

fn map_from_pairs(process: &mut Process, pairs: &[(Term, Term)]) -> Term {
    let keys: Vec<Term> = pairs.iter().map(|(key, _)| *key).collect();
    let values: Vec<Term> = pairs.iter().map(|(_, value)| *value).collect();
    let words = 2 + pairs.len() * 2;
    let ptr = process.heap_mut().alloc(words).expect("heap allocation");
    let heap = core::heap_slice(ptr, words);
    write_map(heap, &keys, &values).expect("map write")
}
