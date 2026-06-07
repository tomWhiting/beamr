use super::*;
use crate::loader::Instruction;
use crate::module::ResolvedImport;
use crate::native::{Capability, NativeEntry};
use crate::term::boxed::{write_closure, write_float, write_tuple};
use std::collections::HashMap;

fn module(code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect();
    Module {
        name: Atom::OK,
        generation: 0,
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
fn get_hd_and_get_tl_decompose_cons_cells_without_allocating() {
    let mut process = Process::new(1, 16);
    let module = module(vec![]);
    let before = process.heap().used();
    core::put_list(
        &mut process,
        &module,
        &Operand::Integer(1),
        &Operand::Integer(2),
        &Operand::X(0),
    )
    .expect("put_list");
    assert_eq!(process.heap().used(), before + 2);

    get_hd(&mut process, &module, &Operand::X(0), &Operand::X(1)).expect("get_hd");
    get_tl(&mut process, &module, &Operand::X(0), &Operand::X(2)).expect("get_tl");

    assert_eq!(process.x_reg(1), Term::small_int(1));
    assert_eq!(process.x_reg(2), Term::small_int(2));
    assert_eq!(process.heap().used(), before + 2);
    assert_eq!(
        get_hd(&mut process, &module, &Operand::Integer(9), &Operand::X(3)),
        Err(ExecError::Badarg)
    );
}

#[test]
fn get_list_decomposes_multi_element_list_into_head_and_tail() {
    let mut process = Process::new(1, 32);
    let module = module(vec![]);

    core::put_list(
        &mut process,
        &module,
        &Operand::Integer(3),
        &Operand::Atom(None),
        &Operand::X(0),
    )
    .expect("put list tail");
    core::put_list(
        &mut process,
        &module,
        &Operand::Integer(2),
        &Operand::X(0),
        &Operand::X(0),
    )
    .expect("put list middle");
    core::put_list(
        &mut process,
        &module,
        &Operand::Integer(1),
        &Operand::X(0),
        &Operand::X(0),
    )
    .expect("put list head");

    get_list(
        &mut process,
        &module,
        &Operand::X(0),
        &Operand::X(1),
        &Operand::X(2),
    )
    .expect("get_list");

    assert_eq!(process.x_reg(1), Term::small_int(1));
    let second = Cons::new(process.x_reg(2)).expect("tail is [2, 3]");
    assert_eq!(second.head(), Term::small_int(2));
    let third = Cons::new(second.tail()).expect("tail is [3]");
    assert_eq!(third.head(), Term::small_int(3));
    assert_eq!(third.tail(), Term::NIL);
}

#[test]
fn get_list_decomposes_single_element_list_to_nil_tail_and_writes_y_registers() {
    let mut process = Process::new(1, 32);
    let module = module(vec![]);

    core::allocate(&mut process, &module, &Operand::Unsigned(2)).expect("allocate Y registers");
    core::put_list(
        &mut process,
        &module,
        &Operand::Integer(42),
        &Operand::Atom(None),
        &Operand::X(0),
    )
    .expect("put single-element list");

    get_list(
        &mut process,
        &module,
        &Operand::X(0),
        &Operand::Y(0),
        &Operand::Y(1),
    )
    .expect("get_list");

    assert_eq!(process.stack().y_reg(0), Ok(Term::small_int(42)));
    assert_eq!(process.stack().y_reg(1), Ok(Term::NIL));
}

#[test]
fn get_list_on_nil_raises_badarg() {
    let mut process = Process::new(1, 16);
    let module = module(vec![]);

    assert_eq!(
        get_list(
            &mut process,
            &module,
            &Operand::Atom(None),
            &Operand::X(0),
            &Operand::X(1)
        ),
        Err(ExecError::Badarg)
    );
}

#[test]
fn type_tests_fall_through_or_jump_to_fail_label() {
    let mut process = Process::new(1, 32);
    let mut tuple_words = [0_u64; 2];
    let tuple = write_tuple(&mut tuple_words, &[Term::small_int(1)]).expect("tuple");
    let mut closure_words = [0_u64; 7];
    let closure = write_closure(&mut closure_words, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");
    process.set_x_reg(0, Term::small_int(1));
    process.set_x_reg(1, Term::atom(Atom::OK));
    process.set_x_reg(2, tuple);
    process.set_x_reg(3, Term::NIL);
    process.set_x_reg(4, Term::atom(Atom::TRUE));
    process.set_x_reg(5, closure);
    let module = module(vec![Instruction::Label { label: 7 }]);

    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsInteger,
            &Operand::Label(7),
            &Operand::X(0)
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            type_test(
                &process,
                &module,
                TypeTestOp::IsInteger,
                &Operand::Label(7),
                &Operand::X(1)
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsAtom,
            &Operand::Label(7),
            &Operand::X(1)
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsTuple,
            &Operand::Label(7),
            &Operand::X(2)
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsNil,
            &Operand::Label(7),
            &Operand::X(3)
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsBoolean,
            &Operand::Label(7),
            &Operand::X(4)
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        type_test(
            &process,
            &module,
            TypeTestOp::IsFunction2,
            &Operand::Label(7),
            &Operand::List(vec![Operand::X(5), Operand::Unsigned(2)])
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            type_test(
                &process,
                &module,
                TypeTestOp::IsFunction2,
                &Operand::Label(7),
                &Operand::List(vec![Operand::X(5), Operand::Unsigned(1)])
            )
            .expect("function2 arity mismatch jumps")
        ),
        0
    );
}

#[test]
fn exact_and_ordering_comparisons_branch_with_beam_semantics() {
    let mut process = Process::new(1, 16);
    let mut float_words = [0_u64; 2];
    process.set_x_reg(0, Term::small_int(1));
    process.set_x_reg(1, Term::small_int(2));
    process.set_x_reg(2, write_float(&mut float_words, 1.0).expect("float"));
    process.set_x_reg(3, Term::atom(Atom::OK));
    let module = module(vec![Instruction::Label { label: 7 }]);

    assert_eq!(
        comparison(
            &process,
            &module,
            ComparisonOp::EqExact,
            &Operand::Label(7),
            &Operand::X(0),
            &Operand::X(0),
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            comparison(
                &process,
                &module,
                ComparisonOp::EqExact,
                &Operand::Label(7),
                &Operand::X(0),
                &Operand::X(1),
                None,
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        jump_ip(
            comparison(
                &process,
                &module,
                ComparisonOp::EqExact,
                &Operand::Label(7),
                &Operand::X(0),
                &Operand::X(2),
                None,
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        comparison(
            &process,
            &module,
            ComparisonOp::NeExact,
            &Operand::Label(7),
            &Operand::X(0),
            &Operand::X(1),
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            comparison(
                &process,
                &module,
                ComparisonOp::NeExact,
                &Operand::Label(7),
                &Operand::X(0),
                &Operand::X(0),
                None,
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        comparison(
            &process,
            &module,
            ComparisonOp::Lt,
            &Operand::Label(7),
            &Operand::X(0),
            &Operand::X(1),
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            comparison(
                &process,
                &module,
                ComparisonOp::Lt,
                &Operand::Label(7),
                &Operand::X(1),
                &Operand::X(0),
                None,
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        comparison(
            &process,
            &module,
            ComparisonOp::Ge,
            &Operand::Label(7),
            &Operand::X(1),
            &Operand::X(0),
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        jump_ip(
            comparison(
                &process,
                &module,
                ComparisonOp::Ge,
                &Operand::Label(7),
                &Operand::X(0),
                &Operand::X(1),
                None,
            )
            .expect("jump")
        ),
        0
    );
    assert_eq!(
        comparison(
            &process,
            &module,
            ComparisonOp::Lt,
            &Operand::Label(7),
            &Operand::X(0),
            &Operand::X(3),
            None,
        ),
        Ok(InstructionOutcome::Continue)
    );
}

#[test]
fn select_val_and_select_tuple_arity_jump_to_matching_or_fail_labels() {
    let mut process = Process::new(1, 16);
    let mut tuple_words = [0_u64; 3];
    process.set_x_reg(0, Term::atom(Atom::OK));
    process.set_x_reg(1, Term::atom(Atom::ERROR));
    process.set_x_reg(2, Term::atom(Atom::TRUE));
    process.set_x_reg(
        3,
        write_tuple(&mut tuple_words, &[Term::small_int(1), Term::small_int(2)]).expect("tuple"),
    );
    let module = module(vec![
        Instruction::Label { label: 1 },
        Instruction::Label { label: 2 },
        Instruction::Label { label: 9 },
    ]);
    let values = Operand::List(vec![
        Operand::Atom(Some(Atom::OK)),
        Operand::Label(1),
        Operand::Atom(Some(Atom::ERROR)),
        Operand::Label(2),
    ]);
    let arities = Operand::List(vec![
        Operand::Unsigned(2),
        Operand::Label(1),
        Operand::Unsigned(3),
        Operand::Label(2),
    ]);

    assert_eq!(
        jump_ip(
            select_val(
                &process,
                &module,
                &Operand::X(0),
                &Operand::Label(9),
                &values
            )
            .expect("select")
        ),
        0
    );
    assert_eq!(
        jump_ip(
            select_val(
                &process,
                &module,
                &Operand::X(1),
                &Operand::Label(9),
                &values
            )
            .expect("select")
        ),
        1
    );
    assert_eq!(
        jump_ip(
            select_val(
                &process,
                &module,
                &Operand::X(2),
                &Operand::Label(9),
                &values
            )
            .expect("select")
        ),
        2
    );
    assert_eq!(
        jump_ip(
            select_tuple_arity(
                &process,
                &module,
                &Operand::X(3),
                &Operand::Label(9),
                &arities
            )
            .expect("select")
        ),
        0
    );
    assert_eq!(
        jump_ip(
            select_tuple_arity(
                &process,
                &module,
                &Operand::Integer(4),
                &Operand::Label(9),
                &arities
            )
            .expect("select")
        ),
        2
    );
}

#[test]
fn jump_changes_only_instruction_pointer() {
    let mut process = Process::new(1, 8);
    process.set_x_reg(0, Term::small_int(42));
    let stack_before = process.stack().len();
    let module = module(vec![Instruction::Label { label: 5 }]);

    assert_eq!(jump_ip(jump(&module, &Operand::Label(5)).expect("jump")), 0);
    assert_eq!(process.x_reg(0), Term::small_int(42));
    assert_eq!(process.stack().len(), stack_before);
}

#[test]
fn guard_bif_success_writes_result_and_failure_branches() {
    let import = ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 2,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: add,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    };
    let mut module = module(vec![Instruction::Label { label: 9 }]);
    module.resolved_imports.push(import);
    let mut process = Process::new(1, 16);

    bif(
        &mut process,
        &module,
        BifOp::GcBif2,
        &[
            Operand::Label(9),
            Operand::Unsigned(0),
            Operand::Integer(3),
            Operand::Integer(4),
            Operand::X(0),
        ],
    )
    .expect("bif success");
    assert_eq!(process.x_reg(0), Term::small_int(7));

    let outcome = bif(
        &mut process,
        &module,
        BifOp::GcBif2,
        &[
            Operand::Label(9),
            Operand::Unsigned(0),
            Operand::Atom(Some(Atom::OK)),
            Operand::Integer(1),
            Operand::X(0),
        ],
    )
    .expect("bif failure branches");
    assert_eq!(jump_ip(outcome), 0);
}
