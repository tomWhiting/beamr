use super::construction::{BinaryBuilder, bs_put_binary, bs_put_integer, finalize_builder};
use super::matching::{Endian, MatchContext, SegmentFlags, decode_integer};
use super::*;
use crate::atom::Atom;
use crate::loader::{Instruction, Literal};
use crate::module::{Module, ModuleOrigin};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Float, ProcBin, SubBinary};
use crate::term::shared_binary::{SharedBinary, write_proc_bin};
use crate::term::sub_binary::SUB_BINARY_WORDS;
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

fn binary_term(process: &mut Process, bytes: &[u8]) -> Term {
    let words = 2 + packed_word_count(bytes.len());
    let ptr = process.heap_mut().alloc(words).expect("test heap fits");
    let heap = heap_slice(ptr, words);
    write_binary(heap, bytes).expect("test binary fits")
}

fn proc_bin_term(process: &mut Process, shared: &SharedBinary) -> Term {
    let ptr = process.heap_mut().alloc(3).expect("test heap fits");
    let heap = heap_slice(ptr, 3);
    write_proc_bin(heap, shared).expect("test proc bin fits")
}

fn start_context(process: &mut Process, source_bytes: &[u8]) -> (Module, Term) {
    let module = module(vec![Instruction::Label { label: 9 }]);
    let source = binary_term(process, source_bytes);
    process.set_x_reg(0, source);
    binary_op(
        process,
        &module,
        BinaryOp::BsStartMatch3,
        &[Operand::Label(9), Operand::X(0), Operand::X(1)],
    )
    .expect("start match");
    (module, process.x_reg(1))
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
            available: 2
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
        &module,
        builder,
        &Operand::Integer(65),
        &Operand::Unsigned(8),
        &Operand::Unsigned(1),
        &Operand::Atom(None),
    )
    .expect("put integer");
    let source = binary_term(&mut process, &[66, 67]);
    process.set_x_reg(1, source);
    bs_put_binary(&mut process, &module, builder, &Operand::X(1)).expect("put binary");

    let builder_state = BinaryBuilder::new(builder).expect("builder context");
    assert_eq!(builder_state.write_position_bits(), 24);
    let result = finalize_builder(&mut process, builder).expect("final binary");
    assert_eq!(
        Binary::new(result).expect("binary").as_bytes(),
        &[65, 66, 67]
    );
}

#[test]
fn interpreter_binary_builder_promotes_large_result_to_proc_bin() {
    let mut process = Process::new(1, 64);
    let module = module(Vec::new());
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsInitWritable,
        &[Operand::Unsigned(100), Operand::X(0)],
    )
    .expect("builder init");
    let builder = process.x_reg(0);

    for index in 0..100_i64 {
        bs_put_integer(
            &mut process,
            &module,
            builder,
            &Operand::Integer(index),
            &Operand::Unsigned(8),
            &Operand::Unsigned(1),
            &Operand::Atom(None),
        )
        .expect("put integer");
    }

    let result = finalize_builder(&mut process, builder).expect("final binary");
    let expected: Vec<u8> = (0..100).collect();
    assert_eq!(
        BinaryRef::new(result).expect("binary ref").as_bytes(),
        expected.as_slice()
    );
    assert!(ProcBin::new(result).is_some());
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
            &module,
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
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(0)]
        ),
        Ok(InstructionOutcome::Continue)
    );
}

#[test]
fn interpreter_binary_match_extracts_sub_binary_from_proc_bin_without_copying() {
    let mut process = Process::new(1, 64);
    let module = module(vec![Instruction::Label { label: 9 }]);
    let bytes: Vec<u8> = (0_u8..=255).cycle().take(1024 * 1024).collect();
    let shared = SharedBinary::new(bytes);
    let source = proc_bin_term(&mut process, &shared);
    process.set_x_reg(0, source);
    assert_eq!(shared.ref_count(), 2);

    binary_op(
        &mut process,
        &module,
        BinaryOp::BsStartMatch3,
        &[Operand::Label(9), Operand::X(0), Operand::X(1)],
    )
    .expect("start match");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsSkipBits2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(10 * u8::BITS as u64),
            Operand::Unsigned(1),
            Operand::Atom(None),
        ],
    )
    .expect("skip offset");
    let before_available = process.heap().available();
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetBinary2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(10 * u8::BITS as u64),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("get proc bin slice");

    assert_eq!(
        before_available - process.heap().available(),
        SUB_BINARY_WORDS
    );
    let sub_binary = SubBinary::new(process.x_reg(2)).expect("sub binary result");
    assert_eq!(sub_binary.parent(), source);
    assert_eq!(
        BinaryRef::new(process.x_reg(2))
            .expect("binary ref")
            .as_bytes(),
        &shared.as_bytes()[10..20]
    );
    assert_eq!(shared.ref_count(), 2);
}

#[test]
fn interpreter_binary_match_failures_branch_without_advancing() {
    let mut process = Process::new(1, 64);
    let mut module = module(vec![Instruction::Label { label: 9 }]);
    let literals = vec![Literal::String(b"he".to_vec())];
    module.constant_pool =
        crate::constant_pool::materialise_literals(&literals, None).expect("literal pool");
    module.literals = literals;
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
                Operand::Literal(0)
            ]
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
            Operand::Literal(0),
        ],
    )
    .expect("failed match branches");
    assert_eq!(
        failed,
        InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0
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
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(0)]
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
                Operand::X(2)
            ]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
}

fn match_one_integer(source_bytes: &[u8], size_bits: u64, flags: Operand) -> Term {
    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, source_bytes);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetInteger2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(size_bits),
            Operand::Unsigned(1),
            flags,
            Operand::X(2),
        ],
    )
    .expect("get integer");
    process.x_reg(2)
}

#[test]
fn bs_get_integer_signed_high_bit_byte_is_negative() {
    assert_eq!(
        match_one_integer(&[0xFF], 8, Operand::Unsigned(0x04)).as_small_int(),
        Some(-1)
    );
}

#[test]
fn bs_get_integer_unsigned_high_bit_byte_stays_positive() {
    assert_eq!(
        match_one_integer(&[0xFF], 8, Operand::Atom(None)).as_small_int(),
        Some(255)
    );
}

#[test]
fn bs_get_integer_signed_multibit_sign_extends_to_full_width() {
    assert_eq!(
        match_one_integer(&[0x80, 0x00], 16, Operand::Unsigned(0x04)).as_small_int(),
        Some(-32768)
    );
    assert_eq!(
        match_one_integer(&[0x80, 0x00], 16, Operand::Atom(None)).as_small_int(),
        Some(32768)
    );
}

#[test]
fn bs_get_integer_signed_respects_endianness() {
    assert_eq!(
        match_one_integer(&[0x80, 0x01], 16, Operand::Unsigned(0x04)).as_small_int(),
        Some(-32767)
    );
    assert_eq!(
        match_one_integer(&[0x80, 0x01], 16, Operand::Unsigned(0x06)).as_small_int(),
        Some(384)
    );
}

#[test]
fn decode_integer_sign_extends_sub_byte_width() {
    let signed = SegmentFlags {
        endian: Endian::Big,
        signed: true,
    };
    assert_eq!(decode_integer(&[0xF8], signed), Ok(-8));
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
            &[Operand::Label(9), Operand::X(0), Operand::X(1)]
        ),
        Ok(InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0
        }))
    );
}

#[test]
fn bs_skip_bits_and_test_unit_update_or_branch() {
    let mut process = Process::new(1, 64);
    let (module, ctx) = start_context(&mut process, &[1, 2, 3, 4]);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsSkipBits2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(16),
            Operand::Unsigned(1),
            Operand::Atom(None),
        ],
    )
    .expect("skip");
    assert_eq!(MatchContext::new(ctx).expect("context").position_bits(), 16);
    assert_eq!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsTestUnit,
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(8)]
        ),
        Ok(InstructionOutcome::Continue)
    );
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsSkipBits2,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(32),
                Operand::Unsigned(1),
                Operand::Atom(None)
            ]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
    assert_eq!(MatchContext::new(ctx).expect("context").position_bits(), 16);
}

#[test]
fn bs_test_unit_fails_for_non_divisible_remainder() {
    let mut process = Process::new(1, 64);
    let (module, ctx) = start_context(&mut process, &[1, 2, 3, 4]);
    MatchContext::new(ctx)
        .expect("context")
        .set_position_bits(7);
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsTestUnit,
            &[Operand::Label(9), Operand::X(1), Operand::Unsigned(8)]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
}

#[test]
fn bs_get_float_extracts_64_and_32_bit_values() {
    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, &3.14_f64.to_be_bytes());
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetFloat2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(64),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("float64");
    assert!((Float::new(process.x_reg(2)).expect("float").value() - 3.14).abs() < f64::EPSILON);

    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, &1.5_f32.to_be_bytes());
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetFloat2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(32),
            Operand::Unsigned(1),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("float32");
    assert_eq!(Float::new(process.x_reg(2)).expect("float").value(), 1.5);
}

#[test]
fn bs_get_tail_returns_remaining_or_empty_binary() {
    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, b"abcdefgh");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsSkipBits2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(32),
            Operand::Unsigned(1),
            Operand::Atom(None),
        ],
    )
    .expect("skip");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetTail,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::X(2),
        ],
    )
    .expect("tail");
    assert_eq!(
        Binary::new(process.x_reg(2)).expect("tail").as_bytes(),
        b"efgh"
    );

    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetTail,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::X(3),
        ],
    )
    .expect("empty tail");
    assert_eq!(Binary::new(process.x_reg(3)).expect("tail").as_bytes(), b"");
}

#[test]
fn utf_get_and_skip_decode_valid_sequences_and_branch_on_invalid() {
    let mut process = Process::new(1, 128);
    let (module, _) = start_context(&mut process, "A¢€𐍈".as_bytes());
    for (reg, expected) in [(2, 0x41), (3, 0xa2), (4, 0x20ac), (5, 0x10348)] {
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsGetUtf8,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(0),
                Operand::Atom(None),
                Operand::X(reg),
            ],
        )
        .expect("utf8");
        assert_eq!(process.x_reg(reg as u16).as_small_int(), Some(expected));
    }

    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, &[0xf0, 0x28, 0x8c, 0x28]);
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsSkipUtf8,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(0),
                Operand::Atom(None)
            ]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
}

#[test]
fn utf16_and_utf32_decode_endianness_and_reject_invalid_codepoints() {
    let mut process = Process::new(1, 128);
    let (module, _) = start_context(&mut process, &[0x00, 0x41, 0xd8, 0x34, 0xdd, 0x1e]);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetUtf16,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("utf16 be");
    assert_eq!(process.x_reg(2).as_small_int(), Some(0x41));
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetUtf16,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::Atom(None),
            Operand::X(3),
        ],
    )
    .expect("utf16 surrogate");
    assert_eq!(process.x_reg(3).as_small_int(), Some(0x1d11e));

    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, &[0x41, 0x00]);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetUtf16,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::Unsigned(0x02),
            Operand::X(2),
        ],
    )
    .expect("utf16 le");
    assert_eq!(process.x_reg(2).as_small_int(), Some(0x41));

    let mut process = Process::new(1, 64);
    let (module, _) = start_context(
        &mut process,
        &[0x00, 0x10, 0xff, 0xff, 0x00, 0x11, 0x00, 0x00],
    );
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetUtf32,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(0),
            Operand::Atom(None),
            Operand::X(2),
        ],
    )
    .expect("utf32");
    assert_eq!(process.x_reg(2).as_small_int(), Some(0x10ffff));
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsSkipUtf32,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::Unsigned(0),
                Operand::Atom(None)
            ]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
}

#[test]
fn bs_get_and_set_position_support_re_reading() {
    let mut process = Process::new(1, 64);
    let (module, _) = start_context(&mut process, &[0xaa, 0xbb]);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetPosition,
        &[Operand::X(1), Operand::X(2), Operand::Unsigned(0)],
    )
    .expect("get pos");
    assert_eq!(process.x_reg(2).as_small_int(), Some(0));
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsSkipBits2,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::Unsigned(8),
            Operand::Unsigned(1),
            Operand::Atom(None),
        ],
    )
    .expect("skip");
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsGetPosition,
        &[Operand::X(1), Operand::X(3), Operand::Unsigned(0)],
    )
    .expect("get pos");
    assert_eq!(process.x_reg(3).as_small_int(), Some(8));
    process.set_x_reg(4, Term::small_int(0));
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsSetPosition,
        &[Operand::X(1), Operand::X(4)],
    )
    .expect("set pos");
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
            Operand::X(5),
        ],
    )
    .expect("get integer");
    assert_eq!(process.x_reg(5).as_small_int(), Some(0xaa));
}

#[test]
fn bs_match_runs_commands_and_rolls_back_position_on_failure() {
    let mut process = Process::new(1, 128);
    let (module, ctx) = start_context(&mut process, &[1, 2, 3, 4, 5]);
    binary_op(
        &mut process,
        &module,
        BinaryOp::BsMatch,
        &[
            Operand::Label(9),
            Operand::X(1),
            Operand::List(vec![
                Operand::List(vec![
                    Operand::Unsigned(2),
                    Operand::Unsigned(0),
                    Operand::Atom(None),
                    Operand::Unsigned(8),
                    Operand::Unsigned(1),
                    Operand::X(2),
                ]),
                Operand::List(vec![Operand::Unsigned(5), Operand::Unsigned(8)]),
                Operand::List(vec![
                    Operand::Unsigned(4),
                    Operand::Unsigned(0),
                    Operand::Atom(None),
                    Operand::Unsigned(16),
                    Operand::Unsigned(1),
                    Operand::X(3),
                ]),
            ]),
        ],
    )
    .expect("bs_match");
    assert_eq!(process.x_reg(2).as_small_int(), Some(1));
    assert_eq!(
        Binary::new(process.x_reg(3)).expect("binary").as_bytes(),
        &[3, 4]
    );
    assert_eq!(MatchContext::new(ctx).expect("context").position_bits(), 32);

    let before = MatchContext::new(ctx).expect("context").position_bits();
    assert!(matches!(
        binary_op(
            &mut process,
            &module,
            BinaryOp::BsMatch,
            &[
                Operand::Label(9),
                Operand::X(1),
                Operand::List(vec![
                    Operand::List(vec![Operand::Unsigned(5), Operand::Unsigned(8)]),
                    Operand::List(vec![
                        Operand::Unsigned(0),
                        Operand::Unsigned(0),
                        Operand::Unsigned(64)
                    ]),
                ])
            ]
        ),
        Ok(InstructionOutcome::Jump(_))
    ));
    assert_eq!(
        MatchContext::new(ctx).expect("context").position_bits(),
        before
    );
}
