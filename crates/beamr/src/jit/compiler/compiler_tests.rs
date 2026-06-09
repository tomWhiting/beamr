use super::{JitCompiler, JitError, JitSettings};
use crate::atom::Atom;
use crate::jit::RootLocation;
use crate::jit::ir_common::{JIT_DEOPT_SENTINEL, X_REGISTER_COUNT};
use crate::jit::ir_exceptions::{
    JIT_STATUS_DEOPT, JIT_STATUS_EXCEPTION, JIT_STATUS_NORMAL, JIT_STATUS_YIELD, JitReturn,
};
use crate::jit::type_info::{FunctionSignature, TypeDescriptor};
use crate::loader::Instruction;
use crate::loader::decode::{BifOp, ComparisonOp, Operand, TypeTestOp};
use crate::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use crate::process::{JitRuntimeContext, Process};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple, write_cons, write_tuple};
use std::collections::HashMap;

type RawJitFn = extern "C" fn(*mut u64, *mut Process) -> JitReturn;

fn call_native(native: &crate::jit::types::NativeCode, registers: &mut [u64]) -> u64 {
    let mut process = Process::new(0, 233);
    call_native_with_process(native, registers, &mut process)
}

fn call_native_with_process(
    native: &crate::jit::types::NativeCode,
    registers: &mut [u64],
    process: &mut Process,
) -> u64 {
    let returned = raw_jit_fn(native)(registers.as_mut_ptr(), process);
    assert_eq!(returned.status, JIT_STATUS_NORMAL);
    returned.value
}

fn call_native_with_process_x_regs(
    native: &crate::jit::types::NativeCode,
    process: &mut Process,
) -> u64 {
    let registers = process.x_regs_mut().as_mut_ptr().cast::<u64>();
    let returned = raw_jit_fn(native)(registers, process);
    assert_eq!(returned.status, JIT_STATUS_NORMAL);
    returned.value
}

fn call_native_status(
    native: &crate::jit::types::NativeCode,
    registers: &mut [u64],
    process: &mut Process,
) -> JitReturn {
    raw_jit_fn(native)(registers.as_mut_ptr(), process)
}

fn raw_jit_fn(native: &crate::jit::types::NativeCode) -> RawJitFn {
    // SAFETY: `NativeCode::call_ptr` is produced by `JitCompiler::compile`
    // with the test ABI `extern "C" fn(*mut u64, *mut Process) -> JitReturn`.
    unsafe { std::mem::transmute(native.call_ptr()) }
}

fn test_module(name: Atom, code: Vec<Instruction>) -> Module {
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

fn int_int_signature(name: &str) -> FunctionSignature {
    FunctionSignature {
        name: name.to_owned(),
        arity: 2,
        param_types: vec![TypeDescriptor::Int, TypeDescriptor::Int],
        return_type: TypeDescriptor::Int,
    }
}

#[test]
fn typed_add_returns_small_int_result() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = int_int_signature("add");
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(0),
                        Operand::X(0),
                        Operand::X(1),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            2,
            signature,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(5).raw(), Term::small_int(3).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(8).raw());
}

#[test]
fn typed_add_overflow_deopts_for_bignum_promotion() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = int_int_signature("add");
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(0),
                        Operand::X(0),
                        Operand::X(1),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            2,
            signature,
        )
        .unwrap();
    let mut registers = vec![
        Term::small_int(Term::SMALL_INT_MAX).raw(),
        Term::small_int(1).raw(),
    ];
    let mut process = Process::new(0, 233);
    let returned = call_native_status(&native, &mut registers, &mut process);

    assert_eq!(returned.status, JIT_STATUS_DEOPT);
    assert_eq!(returned.value, JIT_DEOPT_SENTINEL as u64);
}

#[test]
fn typed_div_by_zero_takes_badarith_fail_label() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = int_int_signature("div");
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(3),
                        Operand::X(0),
                        Operand::X(1),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Move {
                    source: Operand::Atom(Some(Atom::BADARITH)),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            2,
            signature,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(10).raw(), Term::small_int(0).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::atom(Atom::BADARITH).raw());
}

#[test]
fn typed_mixed_known_unknown_arithmetic_materializes_known_operand_for_untyped_fallback() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "mixed_add".to_owned(),
        arity: 2,
        param_types: vec![TypeDescriptor::Int, TypeDescriptor::String],
        return_type: TypeDescriptor::String,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(0),
                        Operand::X(0),
                        Operand::X(1),
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
            2,
            signature,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(5).raw(), Term::small_int(3).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(8).raw());
}

#[test]
fn typed_div_min_by_minus_one_completes_without_overflow() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(3),
                        Operand::X(0),
                        Operand::X(1),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            2,
            int_int_signature("div"),
        )
        .unwrap();
    // i64::MIN as a tagged value has payload i64::MIN >> 3, which is NOT
    // i64::MIN itself — so the sdiv overflow guard (i64::MIN / -1) cannot
    // fire for valid small-int inputs. Verify it completes normally.
    let mut registers = vec![i64::MIN as u64, (-1i64) as u64];
    let returned = call_native(&native, &mut registers);

    assert_ne!(returned, JIT_DEOPT_SENTINEL as u64);
}

#[test]
fn typed_rem_by_zero_takes_badarith_fail_label() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile_typed(
            &[
                Instruction::Bif {
                    op: BifOp::Bif2,
                    operands: vec![
                        Operand::Label(9),
                        Operand::Unsigned(4),
                        Operand::X(0),
                        Operand::X(1),
                        Operand::X(0),
                    ],
                },
                Instruction::Return,
                Instruction::Label { label: 9 },
                Instruction::Move {
                    source: Operand::Atom(Some(Atom::BADARITH)),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            2,
            int_int_signature("rem"),
        )
        .unwrap();
    let mut registers = vec![Term::small_int(10).raw(), Term::small_int(0).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::atom(Atom::BADARITH).raw());
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
fn compiled_put_list_emits_safepoint_and_allocates_cons() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutList {
                    head: Operand::X(0),
                    tail: Operand::Atom(None),
                    destination: Operand::X(1),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    assert_eq!(native.stack_maps()[0].offset_from_entry, 0);
    assert_eq!(
        native.stack_maps()[0].live_roots,
        vec![RootLocation::Register(0), RootLocation::Register(1)]
    );

    let mut process = Process::new(0, 233);
    let mut registers = vec![Term::small_int(7).raw(), Term::NIL.raw()];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(7).raw());
    let cons = Cons::new(Term::from_raw(registers[1])).unwrap();
    assert_eq!(cons.head(), Term::small_int(7));
    assert_eq!(cons.tail(), Term::NIL);
}

#[test]
fn compiled_get_list_destructures_constructed_cons() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutList {
                    head: Operand::Integer(11),
                    tail: Operand::Atom(None),
                    destination: Operand::X(1),
                },
                Instruction::GetList {
                    source: Operand::X(1),
                    head: Operand::X(2),
                    tail: Operand::X(3),
                },
                Instruction::Move {
                    source: Operand::X(2),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    let mut process = Process::new(0, 233);
    let mut registers = vec![0; 4];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(11).raw());
    assert_eq!(registers[2], Term::small_int(11).raw());
    assert_eq!(registers[3], Term::NIL.raw());
}

#[test]
fn typed_put_list_stores_tagged_int_and_typed_head_load_returns_tagged_result() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "typed_list".to_owned(),
        arity: 1,
        param_types: vec![TypeDescriptor::Int],
        return_type: TypeDescriptor::Int,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::PutList {
                    head: Operand::X(0),
                    tail: Operand::Atom(None),
                    destination: Operand::X(1),
                },
                Instruction::GetHd {
                    source: Operand::X(1),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            1,
            signature,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    let mut process = Process::new(0, 233);
    let mut registers = vec![Term::small_int(31).raw(), 0];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(31).raw());
    let cons = Cons::new(Term::from_raw(registers[1])).unwrap();
    assert_eq!(cons.head(), Term::small_int(31));
}

#[test]
fn compiled_get_list_read_is_pure_and_emits_no_safepoint() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::GetList {
                    source: Operand::X(1),
                    head: Operand::X(2),
                    tail: Operand::X(3),
                },
                Instruction::Move {
                    source: Operand::X(3),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert!(native.stack_maps().is_empty());
    let mut cons_words = [0; 2];
    let cons = write_cons(&mut cons_words, Term::small_int(23), Term::NIL).unwrap();
    let mut registers = vec![0, cons.raw(), 0, 0];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::NIL.raw());
    assert_eq!(registers[2], Term::small_int(23).raw());
    assert_eq!(registers[3], Term::NIL.raw());
}

#[test]
fn compiled_get_hd_and_get_tl_read_cons_fields() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutList {
                    head: Operand::Integer(17),
                    tail: Operand::X(4),
                    destination: Operand::X(1),
                },
                Instruction::GetHd {
                    source: Operand::X(1),
                    destination: Operand::X(2),
                },
                Instruction::GetTl {
                    source: Operand::X(1),
                    destination: Operand::X(3),
                },
                Instruction::Move {
                    source: Operand::X(3),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    let mut process = Process::new(0, 233);
    let mut registers = vec![0, 0, 0, 0, Term::NIL.raw()];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::NIL.raw());
    assert_eq!(registers[2], Term::small_int(17).raw());
    assert_eq!(registers[3], Term::NIL.raw());
}

#[test]
fn compiled_get_hd_and_get_tl_reads_are_pure_and_emit_no_safepoint() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::GetHd {
                    source: Operand::X(1),
                    destination: Operand::X(2),
                },
                Instruction::GetTl {
                    source: Operand::X(1),
                    destination: Operand::X(3),
                },
                Instruction::Move {
                    source: Operand::X(2),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert!(native.stack_maps().is_empty());
    let mut cons_words = [0; 2];
    let cons = write_cons(&mut cons_words, Term::small_int(29), Term::NIL).unwrap();
    let mut registers = vec![0, cons.raw(), 0, 0];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(29).raw());
    assert_eq!(registers[2], Term::small_int(29).raw());
    assert_eq!(registers[3], Term::NIL.raw());
}

#[test]
fn compiled_put_tuple2_emits_safepoint_and_allocates_tuple() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutTuple2 {
                    destination: Operand::X(2),
                    elements: Operand::List(vec![Operand::X(0), Operand::Integer(9)]),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    assert_eq!(native.stack_maps()[0].offset_from_entry, 0);
    assert_eq!(
        native.stack_maps()[0].live_roots,
        vec![RootLocation::Register(0), RootLocation::Register(2)]
    );

    let mut process = Process::new(0, 233);
    let mut registers = vec![Term::small_int(4).raw(), 0, 0];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(4).raw());
    let tuple = Tuple::new(Term::from_raw(registers[2])).unwrap();
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::small_int(4)));
    assert_eq!(tuple.get(1), Some(Term::small_int(9)));
}

#[test]
fn typed_put_tuple_stores_tagged_int_and_typed_element_load_returns_tagged_result() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "typed_tuple".to_owned(),
        arity: 1,
        param_types: vec![TypeDescriptor::Int],
        return_type: TypeDescriptor::Int,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::PutTuple2 {
                    destination: Operand::X(1),
                    elements: Operand::List(vec![Operand::X(0)]),
                },
                Instruction::GetTupleElement {
                    source: Operand::X(1),
                    index: Operand::Integer(0),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            1,
            signature,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    let mut process = Process::new(0, 233);
    let mut registers = vec![Term::small_int(41).raw(), 0];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(41).raw());
    let tuple = Tuple::new(Term::from_raw(registers[1])).unwrap();
    assert_eq!(tuple.get(0), Some(Term::small_int(41)));
}

#[test]
fn compiled_get_tuple_element_reads_constructed_tuple() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutTuple2 {
                    destination: Operand::X(1),
                    elements: Operand::List(vec![Operand::Integer(4), Operand::Integer(9)]),
                },
                Instruction::GetTupleElement {
                    source: Operand::X(1),
                    index: Operand::Integer(1),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 1);
    let mut process = Process::new(0, 233);
    let mut registers = vec![0; 2];
    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(9).raw());
    let tuple = Tuple::new(Term::from_raw(registers[1])).unwrap();
    assert_eq!(tuple.get(0), Some(Term::small_int(4)));
    assert_eq!(tuple.get(1), Some(Term::small_int(9)));
}

#[test]
fn compiled_allocation_with_tiny_heap_survives_gc() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::PutTuple2 {
                    destination: Operand::X(1),
                    elements: Operand::List(vec![Operand::X(0)]),
                },
                Instruction::PutTuple2 {
                    destination: Operand::X(2),
                    elements: Operand::List(vec![Operand::X(1), Operand::Integer(8)]),
                },
                Instruction::Return,
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();

    assert_eq!(native.stack_maps().len(), 2);

    let mut process = Process::new(0, 2);
    process.set_x_reg(0, Term::small_int(3));
    let returned = call_native_with_process_x_regs(&native, &mut process);

    assert_eq!(returned, Term::small_int(3).raw());
    let outer = Tuple::new(process.x_reg(2)).unwrap();
    assert_eq!(outer.arity(), 2);
    let inner = Tuple::new(outer.get(0).unwrap()).unwrap();
    assert_eq!(inner.get(0), Some(Term::small_int(3)));
    assert_eq!(outer.get(1), Some(Term::small_int(8)));
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
fn typed_is_integer_guard_elides_known_int_and_returns_tagged_value() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "guard".to_owned(),
        arity: 1,
        param_types: vec![TypeDescriptor::Int],
        return_type: TypeDescriptor::Int,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::TypeTest {
                    op: TypeTestOp::IsInteger,
                    fail: Operand::Label(7),
                    value: Operand::X(0),
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
            1,
            signature,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(55).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(55).raw());
}

#[test]
fn typed_is_atom_guard_on_known_int_jumps_to_fail_label() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "guard".to_owned(),
        arity: 1,
        param_types: vec![TypeDescriptor::Int],
        return_type: TypeDescriptor::Int,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::TypeTest {
                    op: TypeTestOp::IsAtom,
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
            1,
            signature,
        )
        .unwrap();
    let mut registers = vec![Term::small_int(55).raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(0).raw());
}

#[test]
fn typed_test_arity_uses_known_tuple_length() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let signature = FunctionSignature {
        name: "arity".to_owned(),
        arity: 1,
        param_types: vec![TypeDescriptor::Tuple(vec![
            TypeDescriptor::Int,
            TypeDescriptor::Int,
        ])],
        return_type: TypeDescriptor::Int,
    };
    let native = compiler
        .compile_typed(
            &[
                Instruction::TestArity {
                    fail: Operand::Label(7),
                    tuple: Operand::X(0),
                    arity: Operand::Integer(3),
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
            1,
            signature,
        )
        .unwrap();
    let mut tuple_words = [0; 3];
    let tuple = write_tuple(&mut tuple_words, &[Term::small_int(1), Term::small_int(2)]).unwrap();
    let mut registers = vec![tuple.raw()];
    let returned = call_native(&native, &mut registers);

    assert_eq!(returned, Term::small_int(0).raw());
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
fn compiled_select_val_does_not_fall_through_after_match() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::SelectVal {
                    value: Operand::X(0),
                    fail: Operand::Label(9),
                    list: Operand::List(vec![Operand::Integer(7), Operand::Label(2)]),
                },
                Instruction::Move {
                    source: Operand::Integer(99),
                    destination: Operand::X(0),
                },
                Instruction::Return,
                Instruction::Label { label: 2 },
                Instruction::Move {
                    source: Operand::Integer(20),
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
    let mut registers = vec![Term::small_int(7).raw()];

    assert_eq!(
        call_native(&native, &mut registers),
        Term::small_int(20).raw()
    );
}

#[test]
fn compiled_zero_arity_is_tagged_tuple_takes_fail_label() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::IsTaggedTuple {
                    fail: Operand::Label(9),
                    value: Operand::X(0),
                    arity: Operand::Unsigned(0),
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
    let mut tuple_words = [0; 1];
    let tuple = write_tuple(&mut tuple_words, &[]).unwrap();
    let mut registers = vec![tuple.raw()];

    assert_eq!(
        call_native(&native, &mut registers),
        Term::small_int(0).raw()
    );
}

#[test]
fn compiled_external_call_falls_back_to_interpreter_and_returns_value() {
    let caller_atom = Atom::MODULE;
    let target_atom = Atom::ERROR;
    let function_atom = Atom::OK;
    let mut caller = test_module(
        caller_atom,
        vec![Instruction::CallExtOnly {
            arity: Operand::Unsigned(1),
            import: Operand::Unsigned(0),
        }],
    );
    caller.resolved_imports.push(ResolvedImport {
        module: target_atom,
        function: function_atom,
        arity: 1,
        target: ResolvedImportTarget::Code {
            module: target_atom,
            label: 1,
        },
    });
    let mut target = test_module(
        target_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Move {
                source: Operand::X(0),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    );
    target.exports.insert((function_atom, 1), 1);
    let registry = ModuleRegistry::new();
    let caller = registry.insert(caller);
    let _target = registry.insert(target);
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(&caller.code, caller_atom, function_atom, 1)
        .unwrap();
    let mut process = Process::new(0, 233);
    process.set_jit_runtime_context(Some(JitRuntimeContext::new(
        caller.as_ref() as *const Module,
        &registry as *const ModuleRegistry,
        std::ptr::null(),
    )));
    let mut registers = vec![Term::small_int(17).raw()];

    let returned = call_native_with_process(&native, &mut registers, &mut process);

    assert_eq!(returned, Term::small_int(17).raw());
    assert_eq!(registers[0], Term::small_int(17).raw());
}

#[test]
fn compiled_try_catches_interpreted_exception_and_exposes_payload() {
    let caller_atom = Atom::MODULE;
    let target_atom = Atom::ERROR;
    let function_atom = Atom::OK;
    let mut caller = test_module(
        caller_atom,
        vec![
            Instruction::Try {
                destination: Operand::Y(0),
                label: Operand::Label(20),
            },
            Instruction::Move {
                source: Operand::Atom(Some(Atom::ERROR)),
                destination: Operand::X(0),
            },
            Instruction::Move {
                source: Operand::Atom(Some(Atom::BADARG)),
                destination: Operand::X(1),
            },
            Instruction::Move {
                source: Operand::Atom(None),
                destination: Operand::X(2),
            },
            Instruction::CallExtOnly {
                arity: Operand::Unsigned(3),
                import: Operand::Unsigned(0),
            },
            Instruction::Label { label: 20 },
            Instruction::TryCase {
                source: Operand::Y(0),
            },
            Instruction::Return,
        ],
    );
    caller.resolved_imports.push(ResolvedImport {
        module: target_atom,
        function: function_atom,
        arity: 3,
        target: ResolvedImportTarget::Code {
            module: target_atom,
            label: 1,
        },
    });
    let mut target = test_module(
        target_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::RawRaise,
            Instruction::Return,
        ],
    );
    target.exports.insert((function_atom, 3), 1);
    let registry = ModuleRegistry::new();
    let caller = registry.insert(caller);
    let _target = registry.insert(target);
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(&caller.code, caller_atom, function_atom, 0)
        .unwrap();
    let mut process = Process::new(0, 233);
    process.set_current_module(caller.clone());
    process.set_jit_runtime_context(Some(JitRuntimeContext::new(
        caller.as_ref() as *const Module,
        &registry as *const ModuleRegistry,
        std::ptr::null(),
    )));
    let mut registers = vec![0; X_REGISTER_COUNT as usize + 3];

    let returned = call_native_status(&native, &mut registers, &mut process);

    assert_eq!(returned.status, JIT_STATUS_NORMAL);
    assert_eq!(returned.value, Term::atom(Atom::ERROR).raw());
    assert_eq!(registers[0], Term::atom(Atom::ERROR).raw());
    assert_eq!(registers[1], Term::atom(Atom::BADARG).raw());
    assert_eq!(registers[2], Term::NIL.raw());
    assert_eq!(process.current_exception(), None);
}

#[test]
fn compiled_external_exception_without_try_propagates_status_and_frame() {
    let caller_atom = Atom::MODULE;
    let target_atom = Atom::ERROR;
    let function_atom = Atom::OK;
    let mut caller = test_module(
        caller_atom,
        vec![Instruction::CallExtOnly {
            arity: Operand::Unsigned(3),
            import: Operand::Unsigned(0),
        }],
    );
    caller.resolved_imports.push(ResolvedImport {
        module: target_atom,
        function: function_atom,
        arity: 3,
        target: ResolvedImportTarget::Code {
            module: target_atom,
            label: 1,
        },
    });
    let mut target = test_module(
        target_atom,
        vec![
            Instruction::Label { label: 1 },
            Instruction::RawRaise,
            Instruction::Return,
        ],
    );
    target.exports.insert((function_atom, 3), 1);
    let registry = ModuleRegistry::new();
    let caller = registry.insert(caller);
    let _target = registry.insert(target);
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(&caller.code, caller_atom, function_atom, 0)
        .unwrap();
    let mut process = Process::new(0, 233);
    process.set_current_module(caller.clone());
    process.set_jit_runtime_context(Some(JitRuntimeContext::new(
        caller.as_ref() as *const Module,
        &registry as *const ModuleRegistry,
        std::ptr::null(),
    )));
    let mut registers = vec![
        Term::atom(Atom::ERROR).raw(),
        Term::atom(Atom::BADARG).raw(),
        Term::NIL.raw(),
    ];

    let returned = call_native_status(&native, &mut registers, &mut process);

    assert_eq!(returned.status, JIT_STATUS_EXCEPTION);
    assert_eq!(returned.value, Term::atom(Atom::BADARG).raw());
    let exception = process
        .current_exception()
        .expect("exception state preserved");
    assert_eq!(exception.class, Term::atom(Atom::ERROR));
    assert_eq!(exception.reason, Term::atom(Atom::BADARG));
    assert!(
        process
            .raw_stacktrace()
            .iter()
            .any(|entry| entry.mfa == Some((caller_atom, function_atom, 0)))
    );
}

#[test]
fn compiled_local_call_charges_reduction_and_yields_when_exhausted() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[
                Instruction::Label { label: 1 },
                Instruction::CallOnly {
                    arity: Operand::Unsigned(0),
                    label: Operand::Label(1),
                },
            ],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut process = Process::new(0, 233);
    process.reset_reductions(3);
    let mut registers = vec![0];

    let returned = call_native_status(&native, &mut registers, &mut process);

    assert_eq!(returned.status, JIT_STATUS_YIELD);
    assert_eq!(returned.value, super::JIT_YIELD_SENTINEL as u64);
    assert_eq!(process.reduction_counter(), 0);
}

#[test]
fn compiled_external_call_returns_deopt_sentinel_without_runtime_context() {
    let compiler = JitCompiler::new(JitSettings).unwrap();
    let native = compiler
        .compile(
            &[Instruction::CallExtOnly {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            }],
            Atom::MODULE,
            Atom::OK,
            0,
        )
        .unwrap();
    let mut process = Process::new(0, 233);
    let mut registers = vec![0];

    let returned = call_native_status(&native, &mut registers, &mut process);

    assert_eq!(returned.status, JIT_STATUS_DEOPT);
    assert_eq!(returned.value, JIT_DEOPT_SENTINEL as u64);
    assert_eq!(registers[0], 0);
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
