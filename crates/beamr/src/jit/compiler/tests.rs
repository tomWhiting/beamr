use super::{JitCompiler, JitError, JitSettings};
use crate::atom::Atom;
use crate::jit::ir_common::X_REGISTER_COUNT;
use crate::loader::Instruction;
use crate::loader::decode::{BifOp, ComparisonOp, Operand, TypeTestOp};
use crate::term::Term;
use crate::term::boxed::write_tuple;

type RawJitFn = extern "C" fn(*mut u64) -> u64;

fn call_native(native: &crate::jit::types::NativeCode, registers: &mut [u64]) -> u64 {
    let function: RawJitFn = unsafe { std::mem::transmute(native.call_ptr()) };
    function(registers.as_mut_ptr())
}

#[test]
fn compiles_return_only_function() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(&[Instruction::Return], Atom::MODULE, Atom::OK, 0)
        .unwrap();

    assert!(!native.call_ptr().is_null());
    assert!(native.stack_maps().is_empty());
}

#[test]
fn compiled_move_writes_register_file() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Move {
                    source: Operand::Integer(42),
                    destination: Operand::X(1),
                },
                Instruction::Move {
                    source: Operand::X(1),
                    destination: Operand::Y(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![0; X_REGISTER_COUNT as usize + 1];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, 0);
    assert_eq!(registers[1], Term::small_int(42).raw());
    assert_eq!(
        registers[X_REGISTER_COUNT as usize],
        Term::small_int(42).raw()
    );
}

#[test]
fn compiled_swap_reads_before_writing() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Swap {
                    left: Operand::X(0),
                    right: Operand::X(1),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(2).raw(), Term::small_int(3).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(3).raw());
    assert_eq!(registers[0], Term::small_int(3).raw());
    assert_eq!(registers[1], Term::small_int(2).raw());
}

#[test]
fn compiled_add_returns_small_int_result() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(0),
                        Operand::Integer(2),
                        Operand::Integer(3),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![0; 1];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(5).raw());
    assert_eq!(registers[0], Term::small_int(5).raw());
}

#[test]
fn compiled_add_at_end_falls_through_to_return_x0() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Label { label: 1 },
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(0),
                        Operand::Integer(2),
                        Operand::Integer(3),
                        Operand::X(0),
                    ],
                },
                Instruction::Label { label: 9 },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![0; 1];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(5).raw());
}

#[test]
fn compiled_multiply_overflow_takes_fail_label() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(2),
                        Operand::Integer(Term::SMALL_INT_MAX),
                        Operand::Integer(Term::SMALL_INT_MAX),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Move {
                    source: Operand::Integer(99),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![0; 1];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(99).raw());
}

#[test]
fn compiled_branch_takes_fail_label_on_false_comparison() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Comparison {
                    op: ComparisonOp::EqExact,
                    fail: Operand::Label(7),
                    left: Operand::Integer(1),
                    right: Operand::Integer(2),
                },
                Instruction::Move {
                    source: Operand::Integer(10),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 7 },
                Instruction::Move {
                    source: Operand::Integer(20),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![0; 1];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(20).raw());
}

#[test]
fn compiled_is_integer_distinguishes_integer_from_atom() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::TypeTest {
                    op: TypeTestOp::IsInteger,
                    fail: Operand::Label(7),
                    value: Operand::X(0),
                },
                Instruction::Move {
                    source: Operand::Integer(1),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 7 },
                Instruction::Move {
                    source: Operand::Integer(0),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    let mut integer_registers = vec![Term::small_int(42).raw()];
    assert_eq!(
        call_native(&native, &mut integer_registers),
        Term::small_int(1).raw()
    );
    let mut atom_registers = vec![Term::atom(Atom::OK).raw()];
    assert_eq!(
        call_native(&native, &mut atom_registers),
        Term::small_int(0).raw()
    );
}

#[test]
fn compiled_pattern_match_on_ok_tuple() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::IsTaggedTuple {
                    fail: Operand::Label(9),
                    value: Operand::X(0),
                    arity: Operand::Unsigned(2),
                    tag: Operand::Atom(Some(Atom::OK)),
                },
                Instruction::Move {
                    source: Operand::Integer(1),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Move {
                    source: Operand::Integer(0),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut tuple_words = [0; 3];
    let tuple = write_tuple(
        &mut tuple_words,
        &[Term::atom(Atom::OK), Term::small_int(42)],
    )
    .unwrap();
    let mut registers = vec![tuple.raw()];

    assert_eq!(
        call_native(&native, &mut registers),
        Term::small_int(1).raw()
    );
}

#[test]
fn compiled_select_val_dispatches_matching_atom() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::SelectVal {
                    value: Operand::X(0),
                    fail: Operand::Label(9),
                    list: Operand::List(vec![
                        Operand::Atom(Some(Atom::OK)),
                        Operand::Label(2),
                        Operand::Integer(7),
                        Operand::Label(3),
                    ]),
                },
                Instruction::Label { label: 2 },
                Instruction::Move {
                    source: Operand::Integer(20),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 3 },
                Instruction::Move {
                    source: Operand::Integer(30),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Move {
                    source: Operand::Integer(90),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut registers = vec![Term::atom(Atom::OK).raw()];

    assert_eq!(
        call_native(&native, &mut registers),
        Term::small_int(20).raw()
    );
}

#[test]
fn reports_unsupported_opcode() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let error = compiler
        .compile(
            &[Instruction::Generic {
                opcode: 255,
                name: "unknown",
                operands: Vec::new(),
            }],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap_err();

    assert_eq!(
        error,
        JitError::UnsupportedOpcode {
            opcode: "unknown (255)".to_owned()
        }
    );
}
