//! Unit tests for decoded `bs_create_bin` segment handling.
//!
//! Operand shapes mirror what the loader produces for real compiler
//! output: `[Fail, Alloc, Live, Unit, Dst, List]` with six operands per
//! segment in the list.

use std::collections::HashMap;

use crate::atom::Atom;
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::interpreter::opcodes::binary::{binary_op, heap_slice};
use crate::loader::decode::BinaryOp;
use crate::loader::decode::compact::Operand;
use crate::loader::{Instruction, Literal};
use crate::module::{Module, ModuleOrigin};
use crate::process::{CodePosition, Process};
use crate::term::Term;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::binary_ref::BinaryRef;

/// Arbitrary module-local atom indices standing in for segment type names
/// (`integer`, `binary`, `string`, `all`, ...) whose identities the handler
/// must not depend on.
const TYPE_INTEGER: u32 = 75;
const TYPE_BINARY: u32 = 71;
const TYPE_STRING: u32 = 73;
const ALL: u32 = 70;
const LITTLE: u32 = 92;

fn module(literals: Vec<Literal>, string_table: Vec<u8>) -> Module {
    let code = vec![Instruction::Label { label: 9 }];
    let constant_pool =
        crate::constant_pool::materialise_literals(&literals, None).expect("literal pool");
    Module {
        name: Atom::OK,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::new(),
        label_index: [(9, 0)].into_iter().collect(),
        code,
        literals,
        constant_pool,
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table,
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

fn atom(index: u32) -> Operand {
    Operand::Atom(Some(Atom::new(index)))
}

fn segment(
    type_atom: Operand,
    unit: u64,
    flags: Operand,
    source: Operand,
    size: Operand,
) -> Vec<Operand> {
    vec![
        type_atom,
        Operand::Unsigned(1),
        Operand::Unsigned(unit),
        flags,
        source,
        size,
    ]
}

fn create_bin(
    process: &mut Process,
    module: &Module,
    fail: u32,
    segments: Vec<Vec<Operand>>,
) -> Result<InstructionOutcome, ExecError> {
    let fields: Vec<Operand> = segments.into_iter().flatten().collect();
    binary_op(
        process,
        module,
        BinaryOp::BsCreateBin,
        &[
            Operand::Label(fail),
            Operand::Unsigned(0),
            Operand::Unsigned(1),
            Operand::Unsigned(1),
            Operand::X(0),
            Operand::List(fields),
        ],
    )
}

fn result_bytes(process: &Process) -> Vec<u8> {
    BinaryRef::new(process.x_reg(0))
        .expect("binary result")
        .as_bytes()
        .to_vec()
}

#[test]
fn bs_create_bin_integer_register_segment() {
    let mut process = Process::new(1, 64);
    let module = module(Vec::new(), Vec::new());
    process.set_x_reg(1, Term::small_int(65));
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(8),
    )];
    assert_eq!(
        create_bin(&mut process, &module, 0, segments),
        Ok(InstructionOutcome::Continue)
    );
    assert_eq!(result_bytes(&process), vec![65]);
}

#[test]
fn bs_create_bin_integer_endianness_from_literal_flag_list() {
    let literals = vec![Literal::List(
        vec![Literal::Atom(Atom::new(LITTLE))],
        Box::new(Literal::Nil),
    )];
    let module = module(literals, Vec::new());

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(0x4142));
    let little = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Literal(0),
        Operand::X(1),
        Operand::Integer(16),
    )];
    create_bin(&mut process, &module, 0, little).expect("little-endian segment");
    assert_eq!(result_bytes(&process), vec![0x42, 0x41]);

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(0x4142));
    let big = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(16),
    )];
    create_bin(&mut process, &module, 0, big).expect("big-endian segment");
    assert_eq!(result_bytes(&process), vec![0x41, 0x42]);
}

#[test]
fn bs_create_bin_integer_truncates_and_sign_extends() {
    let module = module(Vec::new(), Vec::new());

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(300));
    let truncated = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(8),
    )];
    create_bin(&mut process, &module, 0, truncated).expect("truncating segment");
    assert_eq!(result_bytes(&process), vec![44]);

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(-2));
    let negative = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(16),
    )];
    create_bin(&mut process, &module, 0, negative).expect("negative segment");
    assert_eq!(result_bytes(&process), vec![0xff, 0xfe]);

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(-1));
    let wide = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(128),
    )];
    create_bin(&mut process, &module, 0, wide).expect("sign-extended segment");
    assert_eq!(result_bytes(&process), vec![0xff; 16]);
}

#[test]
fn bs_create_bin_append_and_binary_all_segments_concatenate() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    let head = binary_term(&mut process, b"hello");
    let tail = binary_term(&mut process, b" world");
    process.set_x_reg(1, head);
    process.set_x_reg(2, tail);
    let segments = vec![
        segment(
            Operand::Atom(Some(Atom::APPEND)),
            8,
            Operand::Atom(None),
            Operand::X(1),
            atom(ALL),
        ),
        segment(
            atom(TYPE_BINARY),
            8,
            Operand::Atom(None),
            Operand::TypedRegister {
                register: Box::new(Operand::X(2)),
                type_index: 1,
            },
            atom(ALL),
        ),
    ];
    create_bin(&mut process, &module, 0, segments).expect("binary concat");
    assert_eq!(result_bytes(&process), b"hello world");
}

#[test]
fn bs_create_bin_string_table_segment() {
    let module = module(Vec::new(), b"lit!".to_vec());
    let mut process = Process::new(1, 64);
    let segments = vec![segment(
        atom(TYPE_STRING),
        8,
        Operand::Atom(None),
        Operand::Unsigned(0),
        Operand::Integer(3),
    )];
    create_bin(&mut process, &module, 0, segments).expect("string segment");
    assert_eq!(result_bytes(&process), b"lit");
}

#[test]
fn bs_create_bin_literal_binary_source_with_register_size() {
    let module = module(vec![Literal::Binary(b"abcdef".to_vec())], Vec::new());
    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(3));
    let segments = vec![segment(
        atom(TYPE_BINARY),
        8,
        Operand::Atom(None),
        Operand::Literal(0),
        Operand::X(1),
    )];
    create_bin(&mut process, &module, 0, segments).expect("literal prefix");
    assert_eq!(result_bytes(&process), b"abc");

    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(9));
    let segments = vec![segment(
        atom(TYPE_BINARY),
        8,
        Operand::Atom(None),
        Operand::Literal(0),
        Operand::X(1),
    )];
    assert_eq!(
        create_bin(&mut process, &module, 0, segments),
        Err(ExecError::Badarg)
    );
}

#[test]
fn bs_create_bin_sized_binary_register_source() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    let source = binary_term(&mut process, b"abcdef");
    process.set_x_reg(1, source);
    process.set_x_reg(2, Term::small_int(3));
    let segments = vec![segment(
        atom(TYPE_BINARY),
        8,
        Operand::Atom(None),
        Operand::X(1),
        Operand::X(2),
    )];
    create_bin(&mut process, &module, 0, segments).expect("sized binary");
    assert_eq!(result_bytes(&process), b"abc");
}

#[test]
fn bs_create_bin_utf8_segment_encodes_codepoint() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(0x20AC));
    let segments = vec![segment(
        Operand::Atom(Some(Atom::UTF8)),
        0,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Atom(Some(Atom::UNDEFINED)),
    )];
    create_bin(&mut process, &module, 0, segments).expect("utf8 segment");
    assert_eq!(result_bytes(&process), vec![0xe2, 0x82, 0xac]);
}

#[test]
fn bs_create_bin_utf8_surrogate_raises_badarg_or_jumps() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(0xD800));
    let surrogate = |process: &mut Process, fail| {
        create_bin(
            process,
            &module,
            fail,
            vec![segment(
                Operand::Atom(Some(Atom::UTF8)),
                0,
                Operand::Atom(None),
                Operand::X(1),
                Operand::Atom(Some(Atom::UNDEFINED)),
            )],
        )
    };
    assert_eq!(surrogate(&mut process, 0), Err(ExecError::Badarg));
    assert_eq!(
        surrogate(&mut process, 9),
        Ok(InstructionOutcome::Jump(CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        }))
    );
}

#[test]
fn bs_create_bin_float_segments() {
    let module = module(
        vec![
            Literal::Float(1.5),
            Literal::List(
                vec![Literal::Atom(Atom::new(LITTLE))],
                Box::new(Literal::Nil),
            ),
        ],
        Vec::new(),
    );

    let mut process = Process::new(1, 64);
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::Literal(0),
        Operand::Integer(64),
    )];
    create_bin(&mut process, &module, 0, segments).expect("float64 big");
    assert_eq!(result_bytes(&process), 1.5_f64.to_bits().to_be_bytes());

    let mut process = Process::new(1, 64);
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Literal(1),
        Operand::Literal(0),
        Operand::Integer(32),
    )];
    create_bin(&mut process, &module, 0, segments).expect("float32 little");
    assert_eq!(result_bytes(&process), 1.5_f32.to_bits().to_le_bytes());
}

#[test]
fn bs_create_bin_bit_level_segments_pack_subbyte_values() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(5));
    let segments = vec![
        segment(
            atom(TYPE_INTEGER),
            1,
            Operand::Atom(None),
            Operand::X(1),
            Operand::Integer(3),
        ),
        segment(
            atom(TYPE_INTEGER),
            1,
            Operand::Atom(None),
            Operand::Integer(0),
            Operand::Integer(5),
        ),
    ];
    create_bin(&mut process, &module, 0, segments).expect("bit segments");
    assert_eq!(result_bytes(&process), vec![0b1010_0000]);
}

#[test]
fn bs_create_bin_non_byte_total_raises_badarg() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    process.set_x_reg(1, Term::small_int(5));
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(3),
    )];
    assert_eq!(
        create_bin(&mut process, &module, 0, segments),
        Err(ExecError::Badarg)
    );
}

#[test]
fn bs_create_bin_non_numeric_source_raises_badarg() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::Atom(Some(Atom::OK)),
        Operand::Integer(8),
    )];
    assert_eq!(
        create_bin(&mut process, &module, 0, segments),
        Err(ExecError::Badarg)
    );
}

#[test]
fn bs_create_bin_reports_gc_needed_when_heap_is_full() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 2);
    process.set_x_reg(1, Term::small_int(65));
    let segments = vec![segment(
        atom(TYPE_INTEGER),
        1,
        Operand::Atom(None),
        Operand::X(1),
        Operand::Integer(8),
    )];
    assert!(matches!(
        create_bin(&mut process, &module, 0, segments),
        Err(ExecError::GcNeeded { .. })
    ));
}

#[test]
fn bs_create_bin_malformed_segment_list_is_invalid_operand() {
    let module = module(Vec::new(), Vec::new());
    let mut process = Process::new(1, 64);
    let result = binary_op(
        &mut process,
        &module,
        BinaryOp::BsCreateBin,
        &[
            Operand::Label(0),
            Operand::Unsigned(0),
            Operand::Unsigned(1),
            Operand::Unsigned(1),
            Operand::X(0),
            Operand::List(vec![Operand::Atom(None), Operand::Unsigned(1)]),
        ],
    );
    assert_eq!(
        result,
        Err(ExecError::InvalidOperand("bs_create_bin segment"))
    );
}
