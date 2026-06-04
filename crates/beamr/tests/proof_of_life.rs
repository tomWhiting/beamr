//! End-to-end proof that beamr executes BEAM instructions correctly.
//!
//! Each test builds a module from raw instructions, spawns a process,
//! runs the interpreter, and verifies the computed result. No .beam
//! files — pure instruction-level proof that the engine works.

use beamr::atom::AtomTable;
use beamr::interpreter::{ExecutionResult, run};
use beamr::loader::Instruction;
use beamr::loader::decode::TypeTestOp;
use beamr::loader::decode::compact::Operand;
use beamr::module::Module;
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::process::{ExitReason, Process};
use beamr::term::Term;
use beamr::term::boxed::{Cons, Tuple};
use std::collections::HashMap;

fn module(name: beamr::atom::Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(i, instr)| {
            if let Instruction::Label { label } = instr {
                Some((*label, i))
            } else {
                None
            }
        })
        .collect();
    Module {
        name,
        generation: 0,
        exports: HashMap::new(),
        label_index,
        code,
        literals: Vec::new(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    }
}

#[test]
fn proof_1_arithmetic_add_two_numbers_via_bif() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let erlang = atoms.intern("erlang");
    let plus = atoms.intern("+");

    let import = beamr::module::ResolvedImport {
        module: erlang,
        function: plus,
        arity: 2,
        target: beamr::module::ResolvedImportTarget::Native(
            bifs.lookup(erlang, plus, 2).expect("+ BIF exists"),
        ),
    };

    let mut m = module(
        atoms.intern("proof"),
        vec![
            // X0 = 17, X1 = 25
            Instruction::Move {
                source: Operand::Integer(17),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(25),
                destination: Operand::X(1),
            },
            // X0 = erlang:'+'(X0, X1) → 42
            Instruction::CallExt {
                arity: Operand::Unsigned(2),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    m.resolved_imports.push(import);

    let mut process = Process::new(1, 64);
    assert_eq!(
        run(&mut process, &m),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn proof_2_branching_case_expression_selects_correct_arm() {
    let atoms = AtomTable::new();
    let ok = beamr::atom::Atom::OK;
    let error = beamr::atom::Atom::ERROR;

    let m = module(
        atoms.intern("proof"),
        vec![
            // X0 = :error
            Instruction::Move {
                source: Operand::Atom(Some(error)),
                destination: Operand::X(0),
            },
            // case X0 of :ok → 1; :error → 2; _ → -1
            Instruction::SelectVal {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Atom(Some(ok)),
                    Operand::Label(10),
                    Operand::Atom(Some(error)),
                    Operand::Label(20),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(0),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 20 },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(0),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(0),
            },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(
        run(&mut process, &m),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(2));
}

#[test]
fn proof_3_data_structures_build_tuple_and_list() {
    let m = module(
        beamr::atom::Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(16),
                live: Operand::Unsigned(0),
            },
            // Build list [1, 2, 3]
            Instruction::PutList {
                head: Operand::Integer(3),
                tail: Operand::Atom(None),
                destination: Operand::X(0),
            },
            Instruction::PutList {
                head: Operand::Integer(2),
                tail: Operand::X(0),
                destination: Operand::X(0),
            },
            Instruction::PutList {
                head: Operand::Integer(1),
                tail: Operand::X(0),
                destination: Operand::X(0),
            },
            // Build tuple {ok, [1,2,3]}
            Instruction::PutTuple2 {
                destination: Operand::X(1),
                elements: Operand::List(vec![
                    Operand::Atom(Some(beamr::atom::Atom::OK)),
                    Operand::X(0),
                ]),
            },
            // Extract first element of tuple → should be :ok
            Instruction::GetTupleElement {
                source: Operand::X(1),
                index: Operand::Unsigned(0),
                destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(
        run(&mut process, &m),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );

    // X0 = [1, 2, 3]
    let cons = Cons::new(process.x_reg(0)).expect("list");
    assert_eq!(cons.head(), Term::small_int(1));
    let cons2 = Cons::new(cons.tail()).expect("list tail");
    assert_eq!(cons2.head(), Term::small_int(2));
    let cons3 = Cons::new(cons2.tail()).expect("list tail tail");
    assert_eq!(cons3.head(), Term::small_int(3));
    assert!(cons3.tail().is_nil());

    // X1 = {ok, [1,2,3]}
    let tuple = Tuple::new(process.x_reg(1)).expect("tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(beamr::atom::Atom::OK)));

    // X2 = first element of tuple = :ok
    assert_eq!(process.x_reg(2), Term::atom(beamr::atom::Atom::OK));
}

#[test]
fn proof_4_function_calls_with_stack_frames() {
    let atoms = AtomTable::new();
    let m = module(
        atoms.intern("proof"),
        vec![
            // main: call add(10, 32), return result
            Instruction::Label { label: 1 },
            Instruction::Move {
                source: Operand::Integer(10),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Integer(32),
                destination: Operand::X(1),
            },
            Instruction::Allocate {
                stack_need: Operand::Unsigned(1),
                live: Operand::Unsigned(0),
            },
            Instruction::Call {
                arity: Operand::Unsigned(2),
                label: Operand::Label(10),
            },
            // Save result in Y0, then move back to X0
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(0),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(1),
            },
            Instruction::Return,
            // add(a, b): return a + b (manual add via BIF-less approach: just return a for now)
            // Actually: use put_tuple2 to return {a, b} as proof both args arrived
            Instruction::Label { label: 10 },
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(4),
                live: Operand::Unsigned(2),
            },
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::X(0), Operand::X(1)]),
            },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(
        run(&mut process, &m),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );

    let tuple = Tuple::new(process.x_reg(0)).expect("result tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::small_int(10)));
    assert_eq!(tuple.get(1), Some(Term::small_int(32)));
}

#[test]
fn proof_5_type_guards_and_pattern_matching() {
    let m = module(
        beamr::atom::Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(4),
                live: Operand::Unsigned(0),
            },
            // X0 = {ok, 42}
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![
                    Operand::Atom(Some(beamr::atom::Atom::OK)),
                    Operand::Integer(42),
                ]),
            },
            // Guard: is_tuple(X0)?
            Instruction::TypeTest {
                op: TypeTestOp::IsTuple,
                fail: Operand::Label(99),
                value: Operand::X(0),
            },
            // Guard: tuple arity == 2?
            Instruction::TestArity {
                fail: Operand::Label(99),
                tuple: Operand::X(0),
                arity: Operand::Unsigned(2),
            },
            // Extract element 1 (the 42)
            Instruction::GetTupleElement {
                source: Operand::X(0),
                index: Operand::Unsigned(1),
                destination: Operand::X(1),
            },
            // Guard: is_integer(X1)?
            Instruction::TypeTest {
                op: TypeTestOp::IsInteger,
                fail: Operand::Label(99),
                value: Operand::X(1),
            },
            // All guards passed — return the extracted integer
            Instruction::Move {
                source: Operand::X(1),
                destination: Operand::X(0),
            },
            Instruction::Return,
            // Guard failed
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(
        run(&mut process, &m),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn proof_6_reduction_counting_yields_and_resumes() {
    let m = module(
        beamr::atom::Atom::OK,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(0),
            },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );

    let mut process = Process::new(1, 64);
    process.reset_reductions(5);

    // First run: should yield after 5 reductions, not crash
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Yielded));
    assert_eq!(process.reduction_counter(), 0);

    // Resume: give 3 more reductions, should yield again
    process.reset_reductions(3);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Yielded));
}

#[test]
fn proof_7_hello_beam_fixture_loads_and_reports_imports() {
    let atoms = AtomTable::new();
    let bytes = include_bytes!("fixtures/hello.beam");
    let module = beamr::loader::load_beam_chunks(bytes, &atoms).expect("hello.beam loads");

    assert_eq!(atoms.resolve(module.name), Some("hello"));
    assert!(!module.instructions.is_empty());
    assert!(!module.imports.is_empty());
    assert!(!module.exports.is_empty());
}

#[test]
fn proof_8_proof_beam_gleam_114_fixture_loads_with_two_byte_compact_encoding() {
    let atoms = AtomTable::new();
    let bytes = include_bytes!("fixtures/proof.beam");
    let module = beamr::loader::load_beam_chunks(bytes, &atoms).expect("proof.beam loads");

    assert_eq!(atoms.resolve(module.name), Some("proof"));
    assert!(!module.instructions.is_empty());
    assert!(!module.exports.is_empty());
}

fn load_proof_module(atoms: &AtomTable, bifs: &BifRegistryImpl) -> Module {
    let bytes = include_bytes!("fixtures/proof.beam");
    let parsed = beamr::loader::load_beam_chunks(bytes, atoms).expect("proof.beam loads");
    let mut resolved = Vec::new();
    for imp in &parsed.imports {
        let native = bifs.lookup(imp.module, imp.function, imp.arity);
        if let Some(entry) = native {
            resolved.push(beamr::module::ResolvedImport {
                module: imp.module,
                function: imp.function,
                arity: imp.arity,
                target: beamr::module::ResolvedImportTarget::Native(entry),
            });
        }
    }
    let exports: HashMap<_, _> = parsed
        .exports
        .iter()
        .map(|e| ((e.function, e.arity), e.label))
        .collect();
    let label_index = parsed
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(i, instr)| {
            if let Instruction::Label { label } = instr {
                Some((*label, i))
            } else {
                None
            }
        })
        .collect();
    Module {
        name: parsed.name,
        generation: 0,
        exports,
        label_index,
        code: parsed.instructions,
        literals: parsed.literals,
        resolved_imports: resolved,
        lambdas: parsed.lambdas,
        string_table: parsed.string_table,
        line_info: parsed.line_info,
    }
}

fn call_gleam_function(
    module: &Module,
    atoms: &AtomTable,
    name: &str,
    args: &[Term],
) -> Result<Term, String> {
    let func = atoms.intern(name);
    let arity = args.len() as u8;
    let export = module
        .exports
        .get(&(func, arity))
        .ok_or_else(|| format!("{name}/{arity} not exported"))?;
    let entry_ip = module
        .code
        .iter()
        .position(|instr| matches!(instr, Instruction::Label { label } if *label == *export))
        .ok_or_else(|| format!("label {export} not found in code"))?;

    let mut process = Process::new(1, 4096);
    for (i, arg) in args.iter().enumerate() {
        process.set_x_reg(i as u8, *arg);
    }
    process.set_code_position(Some(beamr::process::CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));

    match run(&mut process, module) {
        Ok(ExecutionResult::Exited(ExitReason::Normal)) => Ok(process.x_reg(0)),
        Ok(other) => Err(format!("unexpected result: {other:?}")),
        Err(e) => Err(format!("execution error: {e:?}")),
    }
}

#[test]
fn proof_9_gleam_answer_returns_42() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let module = load_proof_module(&atoms, &bifs);

    let result = call_gleam_function(&module, &atoms, "answer", &[]).expect("answer/0 runs");
    assert_eq!(result, Term::small_int(42));
}

#[test]
fn proof_10_gleam_add_computes_17_plus_25() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let module = load_proof_module(&atoms, &bifs);

    let result = call_gleam_function(
        &module,
        &atoms,
        "add",
        &[Term::small_int(17), Term::small_int(25)],
    )
    .expect("add/2 runs");
    assert_eq!(result, Term::small_int(42));
}

#[test]
fn proof_11_gleam_pipeline_transforms_input() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let module = load_proof_module(&atoms, &bifs);

    // run_pipeline(5) → doubled=10, added=20, squared=400
    let result = call_gleam_function(&module, &atoms, "run_pipeline", &[Term::small_int(5)])
        .expect("run_pipeline/1 runs");
    assert_eq!(result, Term::small_int(400));
}

#[test]
fn proof_12_gleam_factorial_computes_recursively() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let module = load_proof_module(&atoms, &bifs);

    // factorial(0) = 1, factorial(5) = 120, factorial(10) = 3628800
    let f0 = call_gleam_function(&module, &atoms, "factorial", &[Term::small_int(0)]);
    let f5 = call_gleam_function(&module, &atoms, "factorial", &[Term::small_int(5)]);
    let f10 = call_gleam_function(&module, &atoms, "factorial", &[Term::small_int(10)]);
    assert_eq!(f0.expect("factorial(0)"), Term::small_int(1));
    assert_eq!(f5.expect("factorial(5)"), Term::small_int(120));
    assert_eq!(f10.expect("factorial(10)"), Term::small_int(3628800));
}

#[test]
fn proof_13_gleam_fibonacci_computes_recursively() {
    let atoms = AtomTable::new();
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, &atoms).expect("register BIFs");
    let module = load_proof_module(&atoms, &bifs);

    // fib(0)=0, fib(1)=1, fib(10)=55
    let f0 = call_gleam_function(&module, &atoms, "fibonacci", &[Term::small_int(0)]);
    let f1 = call_gleam_function(&module, &atoms, "fibonacci", &[Term::small_int(1)]);
    let f10 = call_gleam_function(&module, &atoms, "fibonacci", &[Term::small_int(10)]);
    assert_eq!(f0.expect("fib(0)"), Term::small_int(0));
    assert_eq!(f1.expect("fib(1)"), Term::small_int(1));
    assert_eq!(f10.expect("fib(10)"), Term::small_int(55));
}
