use super::*;
use crate::atom::Atom;
use crate::loader::Instruction;
use crate::module::Module;
use crate::term::binary::{Binary, write_binary};
use std::collections::HashMap;

fn module(code: Vec<Instruction>) -> Module {
    Module {
        name: Atom::OK,
        exports: HashMap::new(),
        code,
        literals: Vec::new(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    }
}

fn binary_term(process: &mut Process, bytes: &[u8]) -> Term {
    let words = 2 + packed_word_count(bytes.len());
    let ptr = process.heap_mut().alloc(words).expect("test heap fits");
    let heap = heap_slice(ptr, words);
    write_binary(heap, bytes).expect("test binary fits")
}

#[test]
fn interpreter_binary_builder_init_tracks_empty_position_and_capacity() {
    let mut process = Process::new(1, 8);
    let module = module(Vec::new());

    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsInitWritable,
            &[Operand::Unsigned(10), Operand::X(0)],
        ),
        Ok(InstructionOutcome::Continue)
    );
    let builder = BinaryBuilder::new(process.x_reg(0)).expect("builder context");
    assert_eq!(builder.write_position_bits(), 0);
    assert!(builder.capacity_bytes() >= 10);
}

#[test]
fn interpreter_binary_builder_init_reports_gc_needed_when_heap_is_full() {
    let mut process = Process::new(1, 2);
    let module = module(Vec::new());

    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsInitWritable,
            &[Operand::Unsigned(10), Operand::X(0)],
        ),
        Err(ExecError::GcNeeded {
            requested: 5,
            available: 2,
        })
    );
}

#[test]
fn interpreter_binary_builder_appends_integer_and_binary_segments() {
    let mut process = Process::new(1, 32);
    let module = module(Vec::new());
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsInitWritable,
        &[Operand::Unsigned(3), Operand::X(0)],
    )
    .expect("builder init");
    let builder = process.x_reg(0);

    bs_put_integer(
        &mut process,
        builder,
        &Operand::Integer(65),
        &Operand::Unsigned(8),
        &Operand::Unsigned(1),
        &Operand::Atom(None),
    )
    .expect("put integer");
    let source = binary_term(&mut process, &[66, 67]);
    process.set_x_reg(1, source);
    bs_put_binary(&mut process, builder, &Operand::X(1)).expect("put binary");

    let builder_state = BinaryBuilder::new(builder).expect("builder context");
    assert_eq!(builder_state.write_position_bits(), 24);
    let result = finalize_builder(&mut process, builder).expect("final binary");
    assert_eq!(
        Binary::new(result).expect("binary").as_bytes(),
        &[65, 66, 67]
    );
}

#[test]
fn interpreter_binary_builder_rejects_writes_past_capacity() {
    let mut process = Process::new(1, 16);
    let module = module(Vec::new());
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsInitWritable,
        &[Operand::Unsigned(1), Operand::X(0)],
    )
    .expect("builder init");
    let builder = process.x_reg(0);

    assert_eq!(
        bs_put_integer(
            &mut process,
            builder,
            &Operand::Integer(0x4142),
            &Operand::Unsigned(16),
            &Operand::Unsigned(1),
            &Operand::Atom(None),
        ),
        Err(ExecError::Badarg)
    );
    assert_eq!(
        BinaryBuilder::new(builder)
            .expect("builder context")
            .write_position_bits(),
        0
    );
}

#[test]
fn interpreter_binary_match_extracts_fields_and_tail() {
    let mut process = Process::new(1, 64);
    let module = module(vec![Instruction::Label { label: 9 }]);
    let source = binary_term(&mut process, &[65, 66, 67, 68]);
    process.set_x_reg(0, source);

    binary_op(
        &mut process,
        &module,
        BinaryOp::BsStartMatch3,
        &[Operand::Label(9), Operand::X(0), Operand::X(1)],
    )
    .expect("start match");
    assert_eq!(
        MatchContext::new(process.x_reg(1))
            .expect("match context")
            .position_bits(),
        0
    );
    assert_eq!(
        MatchContext::new(process.x_reg(1))
            .expect("match context")
            .total_bits(),
        32
    );

    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetInteger2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(8),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("get first integer");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetInteger2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(8),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(3),
        ],
    )
    .expect("get second integer");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetBinary2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(16),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(4),
        ],
    )
    .expect("get rest binary");

    assert_eq!(process.x_reg(2).as_small_int(), Some(65));
    assert_eq!(process.x_reg(3).as_small_int(), Some(66));
    assert_eq!(
        Binary::new(process.x_reg(4)).expect("rest").as_bytes(),
        &[67, 68]
    );
    assert_eq!(
        Binary::new(process.x_reg(0)).expect("source").as_bytes(),
        &[65, 66, 67, 68]
    );
    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsTestTail2,
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(0)],
        ),
        Ok(InstructionOutcome::Continue)
    );
}

#[test]
fn interpreter_binary_match_failures_branch_without_advancing() {
    let mut process = Process::new(1, 64);
    let module = module(vec![Instruction::Label { label: 9 }]);
    let source = binary_term(&mut process, b"hello");
    process.set_x_reg(0, source);

    binary_op(
        &mut process,
        &module,
        BinaryOp::BsStartMatch3,
        &[Operand::Label(9), Operand::X(0), Operand::X(1)],
    )
    .expect("start match");

    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsMatchString,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(16),
                Operand::Literal(Literal::String(b"he".to_vec())),
            ],
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(
        MatchContext::new(process.x_reg(1))
            .expect("match context")
            .position_bits(),
        16
    );

    let failed = binary_op(
        &mut process,
        &module,
        BinaryOp::BsMatchString,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(16),
            Operand::Literal(Literal::String(b"xx".to_vec())),
        ],
    )
    .expect("failed match branches");
    assert_eq!(
        failed,
        InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        })
    );
    assert_eq!(
        MatchContext::new(process.x_reg(1))
            .expect("match context")
            .position_bits(),
        16
    );

    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsTestTail2,
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(0)],
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsGetInteger2,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(64),
                Operand::Unsigned(1),
                Operand::Atom(None),
                Operand::X(2),
            ],
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
}

#[test]
fn interpreter_binary_start_match_non_binary_branches_to_fail() {
    let mut process = Process::new(1, 16);
    let module = module(vec![Instruction::Label { label: 9 }]);
    process.set_x_reg(0, Term::small_int(12));

    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsStartMatch3,
            &[Operand::Label(9), Operand::X(0), Operand::X(1)],
        ),
        Ok(InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        }))
    );
}
