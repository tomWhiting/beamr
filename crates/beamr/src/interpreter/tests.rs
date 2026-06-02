use super::{ExecutionResult, run};
use crate::atom::{Atom, AtomTable};
use crate::error::ExecError;
use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use crate::loader::{Instruction, Literal};
use crate::module::{Module, ResolvedImport, ResolvedImportTarget};
use crate::native::{NativeEntry, ProcessContext};
use crate::process::{CodePosition, ExitReason, Process};
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::boxed::{Cons, Tuple};
use crate::term::{Term, compare};
use std::collections::HashMap;

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

fn heap_binary(process: &mut Process, bytes: &[u8]) -> Term {
    let words = 2 + packed_word_count(bytes.len());
    let ptr = process.heap_mut().alloc(words).expect("test heap fits");
    // SAFETY: test helper immediately initialises the fresh heap allocation.
    let heap = unsafe { std::slice::from_raw_parts_mut(ptr, words) };
    write_binary(heap, bytes).expect("test binary fits")
}

#[test]
fn single_return_exits_normally() {
    let module = module(Atom::OK, vec![Instruction::Return]);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
}

#[test]
fn call_chain_executes_in_sequence_and_returns() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(0),
                label: Operand::Label(2),
            },
            Instruction::Return,
            Instruction::Label { label: 2 },
            Instruction::Move {
                source: Operand::Integer(42),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
}

#[test]
fn tight_call_loop_yields_at_reduction_budget_and_resumes() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Label { label: 1 },
            Instruction::CallOnly {
                arity: Operand::Unsigned(0),
                label: Operand::Label(1),
            },
        ],
    );
    let mut process = Process::new(1, 32);
    process.reset_reductions(3);

    assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
    assert_eq!(process.reduction_counter(), 0);
    assert_eq!(
        process.code_position(),
        Some(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        })
    );
    process.reset_reductions(1);
    assert_eq!(run(&mut process, &module), Ok(ExecutionResult::Yielded));
}

#[test]
fn func_info_and_move_cover_metadata_register_literals_and_stack() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("sample");
    let function_atom = atoms.intern("main");
    let module = module(
        module_atom,
        vec![
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_atom)),
                function: Operand::Atom(Some(function_atom)),
                arity: Operand::Unsigned(0),
            },
            Instruction::AllocateZero {
                stack_need: Operand::Unsigned(1),
                live: Operand::Unsigned(0),
            },
            Instruction::Move {
                source: Operand::Literal(Literal::Integer(7)),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::Y(0),
            },
            Instruction::Move {
                source: Operand::Y(0),
                destination: Operand::X(1),
            },
            Instruction::Deallocate {
                words: Operand::Unsigned(1),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);
    let before_heap = process.heap().used();

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.current_mfa(), Some((module_atom, function_atom, 0)));
    assert_eq!(process.x_reg(1), Term::small_int(7));
    assert_eq!(process.heap().used(), before_heap);
}

#[test]
fn stack_heap_and_data_opcodes_work() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(6),
                live: Operand::Unsigned(0),
            },
            Instruction::PutList {
                head: Operand::Integer(1),
                tail: Operand::Atom(None),
                destination: Operand::X(0),
            },
            Instruction::PutTuple2 {
                destination: Operand::X(1),
                elements: Operand::List(vec![
                    Operand::Integer(1),
                    Operand::Integer(2),
                    Operand::Integer(3),
                ]),
            },
            Instruction::GetTupleElement {
                source: Operand::X(1),
                index: Operand::Unsigned(0),
                destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 8);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    let cons = Cons::new(process.x_reg(0)).expect("put_list creates cons");
    assert_eq!(cons.head(), Term::small_int(1));
    assert_eq!(cons.tail(), Term::NIL);
    let tuple = Tuple::new(process.x_reg(1)).expect("put_tuple2 creates tuple");
    assert_eq!(tuple.arity(), 3);
    assert_eq!(process.x_reg(2), Term::small_int(1));
}

#[test]
fn bad_tuple_access_and_heap_exhaustion_report_errors() {
    let bad_tuple = module(
        Atom::OK,
        vec![Instruction::GetTupleElement {
            source: Operand::Integer(1),
            index: Operand::Unsigned(0),
            destination: Operand::X(0),
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 8), &bad_tuple),
        Err(ExecError::Badarg)
    );

    let heap_check = module(
        Atom::OK,
        vec![
            Instruction::TestHeap {
                heap_need: Operand::Unsigned(10),
                live: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 8);
    assert_eq!(
        run(&mut process, &heap_check),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert!(process.heap().available() >= 10);
}

fn add_one(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [value] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(value) = value.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(value + 1))
}

#[test]
fn call_ext_invokes_registered_native_and_tail_call_deallocates() {
    let import = ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 1,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add_one,
            is_dirty: false,
        }),
    };
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Integer(41),
                destination: Operand::X(0),
            },
            Instruction::CallExt {
                arity: Operand::Unsigned(1),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(import);
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(42));
    assert_eq!(process.stack().len(), 0);
}

fn add(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let _ = context;
    let [left, right] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(left) = left.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(right) = right.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(left + right))
}

#[test]
fn branching_opcode_sequence_dispatches_like_case_expression() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::PutTuple2 {
                destination: Operand::X(0),
                elements: Operand::List(vec![Operand::Atom(Some(Atom::OK)), Operand::Integer(42)]),
            },
            Instruction::TypeTest {
                op: crate::loader::decode::TypeTestOp::IsTuple,
                fail: Operand::Label(99),
                value: Operand::X(0),
            },
            Instruction::SelectTupleArity {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Unsigned(2),
                    Operand::Label(10),
                    Operand::Unsigned(3),
                    Operand::Label(11),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 11 },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(1),
            },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 32);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(1));
}

#[test]
fn select_val_and_comparison_sequence_dispatches_like_guarded_case_expression() {
    let module = module(
        Atom::OK,
        vec![
            Instruction::Move {
                source: Operand::Atom(Some(Atom::ERROR)),
                destination: Operand::X(0),
            },
            Instruction::SelectVal {
                value: Operand::X(0),
                fail: Operand::Label(99),
                list: Operand::List(vec![
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Label(10),
                    Operand::Atom(Some(Atom::ERROR)),
                    Operand::Label(11),
                ]),
            },
            Instruction::Label { label: 10 },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 11 },
            Instruction::Comparison {
                op: crate::loader::decode::ComparisonOp::EqExact,
                fail: Operand::Label(99),
                left: Operand::X(0),
                right: Operand::Atom(Some(Atom::ERROR)),
            },
            Instruction::Move {
                source: Operand::Integer(2),
                destination: Operand::X(1),
            },
            Instruction::Jump {
                target: Operand::Label(100),
            },
            Instruction::Label { label: 99 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(1),
            },
            Instruction::Label { label: 100 },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(2));
    assert!(compare::exact_eq(process.x_reg(0), Term::atom(Atom::ERROR)));
}

#[test]
fn guard_bif_failure_branches_without_exiting_process() {
    let mut module = module(
        Atom::OK,
        vec![
            Instruction::Bif {
                op: crate::loader::decode::BifOp::GcBif2,
                operands: vec![
                    Operand::Label(9),
                    Operand::Unsigned(0),
                    Operand::Atom(Some(Atom::OK)),
                    Operand::Integer(1),
                    Operand::X(0),
                ],
            },
            Instruction::Move {
                source: Operand::Integer(1),
                destination: Operand::X(1),
            },
            Instruction::Return,
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(99),
                destination: Operand::X(1),
            },
            Instruction::Return,
        ],
    );
    module.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 2,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add,
            is_dirty: false,
        }),
    });
    let mut process = Process::new(1, 16);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(1), Term::small_int(99));
}

#[test]
fn unknown_opcode_reports_opcode_number() {
    let module = module(
        Atom::OK,
        vec![Instruction::Generic {
            opcode: 222,
            name: "mystery",
            operands: Vec::new(),
        }],
    );
    assert_eq!(
        run(&mut Process::new(1, 8), &module),
        Err(ExecError::UnknownOpcode { opcode: 222 })
    );
}

#[test]
fn proof_of_life_load_spawn_execute_exit_pipeline_fixture() {
    let atoms = AtomTable::new();
    let module_atom = atoms.intern("gleam_fib_fixture");
    let fib_atom = atoms.intern("fib");
    let mut module = module(
        module_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::FuncInfo {
                module: Operand::Atom(Some(module_atom)),
                function: Operand::Atom(Some(fib_atom)),
                arity: Operand::Unsigned(1),
            },
            Instruction::Move {
                source: Operand::Integer(55),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    module.exports.insert((fib_atom, 1), 1);
    let mut process = Process::new(42, 32);
    process.set_x_reg(0, Term::small_int(10));
    process.set_code_position(Some(CodePosition {
        module: module_atom,
        instruction_pointer: 0,
    }));

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(0), Term::small_int(55));
}

#[test]
fn interpreter_binary_opcodes_construct_and_match_binary_patterns() {
    let construct_module = module(
        Atom::OK,
        vec![
            Instruction::BinaryOp {
                op: BinaryOp::BsCreateBin,
                operands: vec![
                    Operand::X(0),
                    Operand::Unsigned(3),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(65),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(66),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                    Operand::List(vec![
                        Operand::Atom(None),
                        Operand::Integer(67),
                        Operand::Unsigned(8),
                        Operand::Unsigned(1),
                        Operand::Atom(None),
                    ]),
                ],
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 64);

    assert_eq!(
        run(&mut process, &construct_module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(
        Binary::new(process.x_reg(0))
            .expect("constructed binary")
            .as_bytes(),
        &[65, 66, 67]
    );

    let module = module(
        Atom::OK,
        vec![
            Instruction::BinaryOp {
                op: BinaryOp::BsStartMatch3,
                operands: vec![Operand::Label(9), Operand::X(0), Operand::X(1)],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetInteger2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(2),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetInteger2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(3),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsGetBinary2,
                operands: vec![
                    Operand::Label(9),
                    Operand::X(1),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::Atom(None),
                    Operand::X(4),
                ],
            },
            Instruction::BinaryOp {
                op: BinaryOp::BsTestTail2,
                operands: vec![Operand::Label(9), Operand::X(1), Operand::Unsigned(0)],
            },
            Instruction::Return,
            Instruction::Label { label: 9 },
            Instruction::Move {
                source: Operand::Integer(-1),
                destination: Operand::X(2),
            },
            Instruction::Return,
        ],
    );
    let mut process = Process::new(1, 96);
    let source = heap_binary(&mut process, &[65, 66, 67]);
    process.set_x_reg(0, source);

    assert_eq!(
        run(&mut process, &module),
        Ok(ExecutionResult::Exited(ExitReason::Normal))
    );
    assert_eq!(process.x_reg(2).as_small_int(), Some(65));
    assert_eq!(process.x_reg(3).as_small_int(), Some(66));
    assert_eq!(
        Binary::new(process.x_reg(4)).expect("rest").as_bytes(),
        &[67]
    );
}
