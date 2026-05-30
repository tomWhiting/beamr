//! End-to-end proof that beamr executes BEAM instructions correctly.
//!
//! Each test builds a module from raw instructions, spawns a process,
//! runs the interpreter, and verifies the computed result. No .beam
//! files — pure instruction-level proof that the engine works.

use beamr::atom::AtomTable;
use beamr::interpreter::{ExecutionResult, run};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::loader::decode::TypeTestOp;
use beamr::module::Module;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::BifRegistryImpl;
use beamr::process::{ExitReason, Process};
use beamr::term::Term;
use beamr::term::boxed::{Cons, Tuple};
use std::collections::HashMap;

fn module(name: beamr::atom::Atom, code: Vec<Instruction>) -> Module {
    Module {
        name,
        exports: HashMap::new(),
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
    let mut bifs = BifRegistryImpl::new();
    register_gate1_bifs(&mut bifs, &atoms).expect("register BIFs");
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
            Instruction::Move { source: Operand::Integer(17), destination: Operand::X(0) },
            Instruction::Move { source: Operand::Integer(25), destination: Operand::X(1) },
            // X0 = erlang:'+'(X0, X1) → 42
            Instruction::CallExt { arity: Operand::Unsigned(2), import: Operand::Unsigned(0) },
            Instruction::Return,
        ],
    );
    m.resolved_imports.push(import);

    let mut process = Process::new(1, 64);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Exited(ExitReason::Normal)));
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
            Instruction::Move { source: Operand::Atom(Some(error)), destination: Operand::X(0) },
            // case X0 of :ok → 1; :error → 2; _ → -1
            Instruction::SelectVal {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Atom(Some(ok)), Operand::Label(10),
                    Operand::Atom(Some(error)), Operand::Label(20),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move { source: Operand::Integer(1), destination: Operand::X(0) },
            Instruction::Jump { target: Operand::Label(100) },
            Instruction::Label { label: 20 },
            Instruction::Move { source: Operand::Integer(2), destination: Operand::X(0) },
            Instruction::Jump { target: Operand::Label(100) },
            Instruction::Label { label: 99 },
            Instruction::Move { source: Operand::Integer(-1), destination: Operand::X(0) },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Exited(ExitReason::Normal)));
    assert_eq!(process.x_reg(0), Term::small_int(2));
}

#[test]
fn proof_3_data_structures_build_tuple_and_list() {
    let m = module(
        beamr::atom::Atom::OK,
        vec![
            Instruction::TestHeap { heap_need: Operand::Unsigned(16), live: Operand::Unsigned(0) },
            // Build list [1, 2, 3]
            Instruction::PutList {
                head: Operand::Integer(3), tail: Operand::Atom(None), destination: Operand::X(0),
            },
            Instruction::PutList {
                head: Operand::Integer(2), tail: Operand::X(0), destination: Operand::X(0),
            },
            Instruction::PutList {
                head: Operand::Integer(1), tail: Operand::X(0), destination: Operand::X(0),
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
                source: Operand::X(1), index: Operand::Unsigned(0), destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Exited(ExitReason::Normal)));

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
            Instruction::Move { source: Operand::Integer(10), destination: Operand::X(0) },
            Instruction::Move { source: Operand::Integer(32), destination: Operand::X(1) },
            Instruction::Allocate { stack_need: Operand::Unsigned(1), live: Operand::Unsigned(0) },
            Instruction::Call { arity: Operand::Unsigned(2), label: Operand::Label(10) },
            // Save result in Y0, then move back to X0
            Instruction::Move { source: Operand::X(0), destination: Operand::Y(0) },
            Instruction::Move { source: Operand::Y(0), destination: Operand::X(0) },
            Instruction::Deallocate { words: Operand::Unsigned(1) },
            Instruction::Return,
            // add(a, b): return a + b (manual add via BIF-less approach: just return a for now)
            // Actually: use put_tuple2 to return {a, b} as proof both args arrived
            Instruction::Label { label: 10 },
            Instruction::TestHeap { heap_need: Operand::Unsigned(4), live: Operand::Unsigned(2) },
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::X(0), Operand::X(1)]),
            },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Exited(ExitReason::Normal)));

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
            Instruction::TestHeap { heap_need: Operand::Unsigned(4), live: Operand::Unsigned(0) },
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
                source: Operand::X(0), index: Operand::Unsigned(1), destination: Operand::X(1),
            },
            // Guard: is_integer(X1)?
            Instruction::TypeTest {
                op: TypeTestOp::IsInteger,
                fail: Operand::Label(99),
                value: Operand::X(1),
            },
            // All guards passed — return the extracted integer
            Instruction::Move { source: Operand::X(1), destination: Operand::X(0) },
            Instruction::Return,
            // Guard failed
            Instruction::Label { label: 99 },
            Instruction::Move { source: Operand::Integer(-1), destination: Operand::X(0) },
            Instruction::Return,
        ],
    );

    let mut process = Process::new(1, 64);
    assert_eq!(run(&mut process, &m), Ok(ExecutionResult::Exited(ExitReason::Normal)));
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn proof_6_reduction_counting_yields_and_resumes() {
    let m = module(
        beamr::atom::Atom::OK,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Move { source: Operand::Integer(99), destination: Operand::X(0) },
            Instruction::CallOnly { arity: Operand::Unsigned(0), label: Operand::Label(1) },
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

#[test]
fn proof_9_compile_gleam_load_beam_execute_get_42() {
    let atoms = AtomTable::new();
    let mut bifs = BifRegistryImpl::new();
    register_gate1_bifs(&mut bifs, &atoms).expect("register BIFs");

    let bytes = include_bytes!("fixtures/proof.beam");
    let parsed = beamr::loader::load_beam_chunks(bytes, &atoms).expect("proof.beam loads");

    let answer = atoms.intern("answer");
    let export = parsed.exports.iter().find(|e| e.function == answer && e.arity == 0);
    assert!(export.is_some(), "proof module exports answer/0");
    let entry_label = export.unwrap().label;

    let module = Module {
        name: parsed.name,
        exports: parsed.exports.iter().map(|e| ((e.function, e.arity), e.label)).collect(),
        code: parsed.instructions,
        literals: parsed.literals,
        resolved_imports: Vec::new(),
        lambdas: parsed.lambdas,
        string_table: parsed.string_table,
        line_info: parsed.line_info,
    };

    let mut process = Process::new(1, 256);

    // Find the instruction index for the entry label
    let entry_ip = module.code.iter().position(|instr| {
        matches!(instr, Instruction::Label { label } if *label == entry_label)
    }).expect("entry label exists in code");

    process.set_x_reg(0, Term::NIL);
    process.set_code_position(Some(beamr::process::CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));

    let result = run(&mut process, &module);
    assert_eq!(result, Ok(ExecutionResult::Exited(ExitReason::Normal)),
        "proof:answer/0 should exit normally");
    assert_eq!(process.x_reg(0), Term::small_int(42),
        "proof:answer/0 should return 42");
}
