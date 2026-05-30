//! Bytecode instruction decoder.
//!
//! Decodes the Code chunk's raw bytes into structured `Instruction`
//! values. Handles compact term encoding for operands: tagged values,
//! extended tags, literals, atoms, labels, and register references.

pub mod chunks;
pub mod compact;

pub use chunks::{
    ExportEntry, ImportEntry, LambdaEntry, LineInfo, Literal, decode_atom_chunk,
    decode_export_chunk, decode_import_chunk, decode_lambda_chunk, decode_line_chunk,
    decode_literal_chunk, decode_string_chunk,
};
pub use compact::{Allocation, Operand};

use crate::atom::Atom;
use crate::error::LoadError;

use compact::CompactDecoder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    Label {
        label: u32,
    },
    FuncInfo {
        module: Operand,
        function: Operand,
        arity: Operand,
    },
    Move {
        source: Operand,
        destination: Operand,
    },
    Call {
        arity: Operand,
        label: Operand,
    },
    CallOnly {
        arity: Operand,
        label: Operand,
    },
    CallExt {
        arity: Operand,
        import: Operand,
    },
    CallExtOnly {
        arity: Operand,
        import: Operand,
    },
    CallLast {
        arity: Operand,
        label: Operand,
        deallocate: Operand,
    },
    CallExtLast {
        arity: Operand,
        import: Operand,
        deallocate: Operand,
    },
    Return,
    Allocate {
        stack_need: Operand,
        live: Operand,
    },
    AllocateHeap {
        stack_need: Operand,
        heap_need: Operand,
        live: Operand,
    },
    AllocateZero {
        stack_need: Operand,
        live: Operand,
    },
    Deallocate {
        words: Operand,
    },
    TestHeap {
        heap_need: Operand,
        live: Operand,
    },
    PutList {
        head: Operand,
        tail: Operand,
        destination: Operand,
    },
    PutTuple2 {
        destination: Operand,
        elements: Operand,
    },
    GetTupleElement {
        source: Operand,
        index: Operand,
        destination: Operand,
    },
    GetHd {
        source: Operand,
        destination: Operand,
    },
    GetTl {
        source: Operand,
        destination: Operand,
    },
    TypeTest {
        op: TypeTestOp,
        fail: Operand,
        value: Operand,
    },
    Comparison {
        op: ComparisonOp,
        fail: Operand,
        left: Operand,
        right: Operand,
    },
    TestArity {
        fail: Operand,
        tuple: Operand,
        arity: Operand,
    },
    SelectVal {
        value: Operand,
        fail: Operand,
        list: Operand,
    },
    SelectTupleArity {
        value: Operand,
        fail: Operand,
        list: Operand,
    },
    Jump {
        target: Operand,
    },
    Bif {
        op: BifOp,
        operands: Vec<Operand>,
    },
    Send,
    RemoveMessage,
    Timeout,
    LoopRec {
        fail: Operand,
        destination: Operand,
    },
    LoopRecEnd {
        fail: Operand,
    },
    Wait {
        fail: Operand,
    },
    WaitTimeout {
        fail: Operand,
        timeout: Operand,
    },
    Catch {
        destination: Operand,
        label: Operand,
    },
    CatchEnd {
        source: Operand,
    },
    Try {
        destination: Operand,
        label: Operand,
    },
    TryEnd {
        source: Operand,
    },
    TryCase {
        source: Operand,
    },
    TryCaseEnd {
        source: Operand,
    },
    BinaryOp {
        op: BinaryOp,
        operands: Vec<Operand>,
    },
    MapOp {
        op: MapOp,
        operands: Vec<Operand>,
    },
    MakeFun {
        operands: Vec<Operand>,
    },
    CallFun {
        arity: Operand,
    },
    CallFun2 {
        function: Operand,
        arity: Operand,
        destination: Operand,
    },
    Apply {
        arity: Operand,
    },
    ApplyLast {
        arity: Operand,
        deallocate: Operand,
    },
    Badmatch {
        value: Operand,
    },
    Badrecord {
        value: Operand,
    },
    CaseEnd {
        value: Operand,
    },
    IfEnd,
    Raise {
        stacktrace: Operand,
        reason: Operand,
    },
    RawRaise,
    Line {
        index: Operand,
    },
    Trim {
        words: Operand,
        remaining: Operand,
    },
    OnLoad,
    BuildStacktrace,
    Swap {
        left: Operand,
        right: Operand,
    },
    InitYregs {
        registers: Operand,
    },
    NifStart,
    UpdateRecord {
        operands: Vec<Operand>,
    },
    Generic {
        opcode: u8,
        name: &'static str,
        operands: Vec<Operand>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeTestOp {
    IsInteger,
    IsFloat,
    IsNumber,
    IsAtom,
    IsPid,
    IsReference,
    IsPort,
    IsNil,
    IsBinary,
    IsList,
    IsNonemptyList,
    IsTuple,
    IsFunction,
    IsBoolean,
    IsFunction2,
    IsBitstr,
    IsMap,
    IsTaggedTuple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Lt,
    Ge,
    Eq,
    Ne,
    EqExact,
    NeExact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BifOp {
    Bif0,
    Bif1,
    Bif2,
    GcBif1,
    GcBif2,
    GcBif3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    BsGetInteger2,
    BsGetFloat2,
    BsGetBinary2,
    BsSkipBits2,
    BsTestTail2,
    BsTestUnit,
    BsMatchString,
    BsInitWritable,
    BsGetUtf8,
    BsSkipUtf8,
    BsGetUtf16,
    BsSkipUtf16,
    BsGetUtf32,
    BsSkipUtf32,
    BsGetTail,
    BsStartMatch3,
    BsGetPosition,
    BsSetPosition,
    BsStartMatch4,
    BsCreateBin,
    BsMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapOp {
    PutMapAssoc,
    PutMapExact,
    HasMapFields,
    GetMapElements,
}

pub fn decode_code_chunk(
    bytes: &[u8],
    atoms: &[Atom],
    literals: &[Literal],
) -> Result<Vec<Instruction>, LoadError> {
    if bytes.len() < 20 {
        return Err(LoadError::DecodeError(
            "Code chunk header is truncated".into(),
        ));
    }
    let sub_size = read_u32(bytes, 0)?;
    let instruction_set = read_u32(bytes, 4)?;
    let opcode_max = read_u32(bytes, 8)?;
    let label_count = read_u32(bytes, 12)?;
    let _function_count = read_u32(bytes, 16)?;

    if sub_size != 16 {
        return Err(LoadError::DecodeError(format!(
            "unsupported Code sub_size {sub_size}"
        )));
    }
    if instruction_set != 0 {
        return Err(LoadError::DecodeError(format!(
            "unsupported BEAM instruction set {instruction_set}"
        )));
    }

    let instructions = decode_instructions(&bytes[20..], atoms, literals)?;
    for instruction in &instructions {
        if let Instruction::Label { label } = instruction
            && *label > label_count
        {
            return Err(LoadError::DecodeError(format!(
                "label {label} exceeds Code label_count {label_count}"
            )));
        }
    }
    if let Some(max_seen) = instructions.iter().filter_map(instruction_opcode).max()
        && u32::from(max_seen) > opcode_max
    {
        return Err(LoadError::DecodeError(format!(
            "decoded opcode {max_seen} exceeds Code opcode_max {opcode_max}"
        )));
    }
    Ok(instructions)
}

pub fn decode_instructions(
    code_bytes: &[u8],
    atoms: &[Atom],
    literals: &[Literal],
) -> Result<Vec<Instruction>, LoadError> {
    let mut decoder = CompactDecoder::new(code_bytes, atoms, literals);
    let mut instructions = Vec::new();
    while !decoder.is_empty() {
        let opcode = decoder.read_opcode()?;
        if opcode == 3 {
            break;
        }
        instructions.push(decode_instruction(opcode, &mut decoder)?);
    }
    Ok(instructions)
}

fn decode_instruction(
    opcode: u8,
    decoder: &mut CompactDecoder<'_>,
) -> Result<Instruction, LoadError> {
    let operands = read_operands(decoder, opcode_arity(opcode)?)?;
    let instruction = match opcode {
        1 => Instruction::Label {
            label: expect_u32(&operands[0], "label")?,
        },
        2 => Instruction::FuncInfo {
            module: operands[0].clone(),
            function: operands[1].clone(),
            arity: operands[2].clone(),
        },
        4 => Instruction::Call {
            arity: operands[0].clone(),
            label: operands[1].clone(),
        },
        5 => Instruction::CallLast {
            arity: operands[0].clone(),
            label: operands[1].clone(),
            deallocate: operands[2].clone(),
        },
        6 => Instruction::CallOnly {
            arity: operands[0].clone(),
            label: operands[1].clone(),
        },
        7 => Instruction::CallExt {
            arity: operands[0].clone(),
            import: operands[1].clone(),
        },
        8 => Instruction::CallExtLast {
            arity: operands[0].clone(),
            import: operands[1].clone(),
            deallocate: operands[2].clone(),
        },
        9 => Instruction::Bif {
            op: BifOp::Bif0,
            operands,
        },
        10 => Instruction::Bif {
            op: BifOp::Bif1,
            operands,
        },
        11 => Instruction::Bif {
            op: BifOp::Bif2,
            operands,
        },
        12 => Instruction::Allocate {
            stack_need: operands[0].clone(),
            live: operands[1].clone(),
        },
        13 => Instruction::AllocateHeap {
            stack_need: operands[0].clone(),
            heap_need: operands[1].clone(),
            live: operands[2].clone(),
        },
        14 => Instruction::AllocateZero {
            stack_need: operands[0].clone(),
            live: operands[1].clone(),
        },
        16 => Instruction::TestHeap {
            heap_need: operands[0].clone(),
            live: operands[1].clone(),
        },
        18 => Instruction::Deallocate {
            words: operands[0].clone(),
        },
        19 => Instruction::Return,
        20 => Instruction::Send,
        21 => Instruction::RemoveMessage,
        22 => Instruction::Timeout,
        23 => Instruction::LoopRec {
            fail: operands[0].clone(),
            destination: operands[1].clone(),
        },
        24 => Instruction::LoopRecEnd {
            fail: operands[0].clone(),
        },
        25 => Instruction::Wait {
            fail: operands[0].clone(),
        },
        26 => Instruction::WaitTimeout {
            fail: operands[0].clone(),
            timeout: operands[1].clone(),
        },
        39 => comparison(ComparisonOp::Lt, operands),
        40 => comparison(ComparisonOp::Ge, operands),
        41 => comparison(ComparisonOp::Eq, operands),
        42 => comparison(ComparisonOp::Ne, operands),
        43 => comparison(ComparisonOp::EqExact, operands),
        44 => comparison(ComparisonOp::NeExact, operands),
        45 => type_test(TypeTestOp::IsInteger, operands),
        46 => type_test(TypeTestOp::IsFloat, operands),
        47 => type_test(TypeTestOp::IsNumber, operands),
        48 => type_test(TypeTestOp::IsAtom, operands),
        49 => type_test(TypeTestOp::IsPid, operands),
        50 => type_test(TypeTestOp::IsReference, operands),
        51 => type_test(TypeTestOp::IsPort, operands),
        52 => type_test(TypeTestOp::IsNil, operands),
        53 => type_test(TypeTestOp::IsBinary, operands),
        55 => type_test(TypeTestOp::IsList, operands),
        56 => type_test(TypeTestOp::IsNonemptyList, operands),
        57 => type_test(TypeTestOp::IsTuple, operands),
        58 => Instruction::TestArity {
            fail: operands[0].clone(),
            tuple: operands[1].clone(),
            arity: operands[2].clone(),
        },
        59 => Instruction::SelectVal {
            value: operands[0].clone(),
            fail: operands[1].clone(),
            list: operands[2].clone(),
        },
        60 => Instruction::SelectTupleArity {
            value: operands[0].clone(),
            fail: operands[1].clone(),
            list: operands[2].clone(),
        },
        61 => Instruction::Jump {
            target: operands[0].clone(),
        },
        62 => Instruction::Catch {
            destination: operands[0].clone(),
            label: operands[1].clone(),
        },
        63 => Instruction::CatchEnd {
            source: operands[0].clone(),
        },
        64 => Instruction::Move {
            source: operands[0].clone(),
            destination: operands[1].clone(),
        },
        65 => Instruction::Generic {
            opcode,
            name: "get_list",
            operands,
        },
        66 => Instruction::GetTupleElement {
            source: operands[0].clone(),
            index: operands[1].clone(),
            destination: operands[2].clone(),
        },
        67 => Instruction::Generic {
            opcode,
            name: "set_tuple_element",
            operands,
        },
        69 => Instruction::PutList {
            head: operands[0].clone(),
            tail: operands[1].clone(),
            destination: operands[2].clone(),
        },
        72 => Instruction::Badmatch {
            value: operands[0].clone(),
        },
        73 => Instruction::IfEnd,
        74 => Instruction::CaseEnd {
            value: operands[0].clone(),
        },
        75 => Instruction::CallFun {
            arity: operands[0].clone(),
        },
        77 => type_test(TypeTestOp::IsFunction, operands),
        78 => Instruction::CallExtOnly {
            arity: operands[0].clone(),
            import: operands[1].clone(),
        },
        96 => Instruction::Generic {
            opcode,
            name: "fmove",
            operands,
        },
        97 => Instruction::Generic {
            opcode,
            name: "fconv",
            operands,
        },
        98 => Instruction::Generic {
            opcode,
            name: "fadd",
            operands,
        },
        99 => Instruction::Generic {
            opcode,
            name: "fsub",
            operands,
        },
        100 => Instruction::Generic {
            opcode,
            name: "fmul",
            operands,
        },
        101 => Instruction::Generic {
            opcode,
            name: "fdiv",
            operands,
        },
        102 => Instruction::Generic {
            opcode,
            name: "fnegate",
            operands,
        },
        103 => Instruction::MakeFun { operands },
        104 => Instruction::Try {
            destination: operands[0].clone(),
            label: operands[1].clone(),
        },
        105 => Instruction::TryEnd {
            source: operands[0].clone(),
        },
        106 => Instruction::TryCase {
            source: operands[0].clone(),
        },
        107 => Instruction::TryCaseEnd {
            source: operands[0].clone(),
        },
        108 => Instruction::Raise {
            stacktrace: operands[0].clone(),
            reason: operands[1].clone(),
        },
        112 => Instruction::Apply {
            arity: operands[0].clone(),
        },
        113 => Instruction::ApplyLast {
            arity: operands[0].clone(),
            deallocate: operands[1].clone(),
        },
        114 => type_test(TypeTestOp::IsBoolean, operands),
        115 => type_test(TypeTestOp::IsFunction2, operands),
        117 => binary_op(BinaryOp::BsGetInteger2, operands),
        118 => binary_op(BinaryOp::BsGetFloat2, operands),
        119 => binary_op(BinaryOp::BsGetBinary2, operands),
        120 => binary_op(BinaryOp::BsSkipBits2, operands),
        121 => binary_op(BinaryOp::BsTestTail2, operands),
        124 => Instruction::Bif {
            op: BifOp::GcBif1,
            operands,
        },
        125 => Instruction::Bif {
            op: BifOp::GcBif2,
            operands,
        },
        129 => type_test(TypeTestOp::IsBitstr, operands),
        131 => binary_op(BinaryOp::BsTestUnit, operands),
        132 => binary_op(BinaryOp::BsMatchString, operands),
        133 => binary_op(BinaryOp::BsInitWritable, operands),
        136 => Instruction::Trim {
            words: operands[0].clone(),
            remaining: operands[1].clone(),
        },
        138 => binary_op(BinaryOp::BsGetUtf8, operands),
        139 => binary_op(BinaryOp::BsSkipUtf8, operands),
        140 => binary_op(BinaryOp::BsGetUtf16, operands),
        141 => binary_op(BinaryOp::BsSkipUtf16, operands),
        142 => binary_op(BinaryOp::BsGetUtf32, operands),
        143 => binary_op(BinaryOp::BsSkipUtf32, operands),
        149 => Instruction::OnLoad,
        152 => Instruction::Bif {
            op: BifOp::GcBif3,
            operands,
        },
        153 => Instruction::Line {
            index: operands[0].clone(),
        },
        154 => map_op(MapOp::PutMapAssoc, operands),
        155 => map_op(MapOp::PutMapExact, operands),
        156 => type_test(TypeTestOp::IsMap, operands),
        157 => map_op(MapOp::HasMapFields, operands),
        158 => map_op(MapOp::GetMapElements, operands),
        159 => type_test4(TypeTestOp::IsTaggedTuple, operands),
        160 => Instruction::BuildStacktrace,
        161 => Instruction::RawRaise,
        162 => Instruction::GetHd {
            source: operands[0].clone(),
            destination: operands[1].clone(),
        },
        163 => Instruction::GetTl {
            source: operands[0].clone(),
            destination: operands[1].clone(),
        },
        164 => Instruction::PutTuple2 {
            destination: operands[0].clone(),
            elements: operands[1].clone(),
        },
        165 => binary_op(BinaryOp::BsGetTail, operands),
        166 => binary_op(BinaryOp::BsStartMatch3, operands),
        167 => binary_op(BinaryOp::BsGetPosition, operands),
        168 => binary_op(BinaryOp::BsSetPosition, operands),
        169 => Instruction::Swap {
            left: operands[0].clone(),
            right: operands[1].clone(),
        },
        170 => binary_op(BinaryOp::BsStartMatch4, operands),
        171 => Instruction::MakeFun { operands },
        172 => Instruction::InitYregs {
            registers: operands[0].clone(),
        },
        177 => binary_op(BinaryOp::BsCreateBin, operands),
        178 => Instruction::CallFun2 {
            function: operands[0].clone(),
            arity: operands[1].clone(),
            destination: operands[2].clone(),
        },
        179 => Instruction::NifStart,
        180 => Instruction::Badrecord {
            value: operands[0].clone(),
        },
        181 => Instruction::UpdateRecord { operands },
        182 => binary_op(BinaryOp::BsMatch, operands),
        183 => Instruction::Generic {
            opcode,
            name: "executable_line",
            operands,
        },
        184 => Instruction::Generic {
            opcode,
            name: "debug_line",
            operands,
        },
        _ => {
            return Err(LoadError::DecodeError(format!(
                "unsupported opcode {opcode}"
            )));
        }
    };
    Ok(instruction)
}

fn read_operands(
    decoder: &mut CompactDecoder<'_>,
    arity: usize,
) -> Result<Vec<Operand>, LoadError> {
    let mut operands = Vec::with_capacity(arity);
    for _ in 0..arity {
        operands.push(decoder.read_operand()?);
    }
    Ok(operands)
}

fn comparison(op: ComparisonOp, operands: Vec<Operand>) -> Instruction {
    Instruction::Comparison {
        op,
        fail: operands[0].clone(),
        left: operands[1].clone(),
        right: operands[2].clone(),
    }
}

fn type_test(op: TypeTestOp, operands: Vec<Operand>) -> Instruction {
    Instruction::TypeTest {
        op,
        fail: operands[0].clone(),
        value: operands[1].clone(),
    }
}

fn type_test4(op: TypeTestOp, operands: Vec<Operand>) -> Instruction {
    Instruction::Generic {
        opcode: 159,
        name: "is_tagged_tuple",
        operands: vec![
            Operand::Unsigned(op as u64),
            operands[0].clone(),
            operands[1].clone(),
            operands[2].clone(),
            operands[3].clone(),
        ],
    }
}

fn binary_op(op: BinaryOp, operands: Vec<Operand>) -> Instruction {
    Instruction::BinaryOp { op, operands }
}

fn map_op(op: MapOp, operands: Vec<Operand>) -> Instruction {
    Instruction::MapOp { op, operands }
}

fn expect_u32(operand: &Operand, context: &str) -> Result<u32, LoadError> {
    match operand {
        Operand::Unsigned(value) => u32::try_from(*value)
            .map_err(|_| LoadError::DecodeError(format!("{context} value {value} out of range"))),
        Operand::Label(value) => Ok(*value),
        other => Err(LoadError::DecodeError(format!(
            "{context} operand was not unsigned: {other:?}"
        ))),
    }
}

fn opcode_arity(opcode: u8) -> Result<usize, LoadError> {
    let arity = match opcode {
        1 => 1,
        2 => 3,
        4 => 2,
        5 => 3,
        6 => 2,
        7 => 2,
        8 => 3,
        9 => 2,
        10 => 4,
        11 => 5,
        12 => 2,
        13 => 3,
        14 => 2,
        16 => 2,
        18 => 1,
        19 | 20 | 21 | 22 | 73 | 133 | 149 | 160 | 161 | 179 => 0,
        23 => 2,
        24 => 1,
        25 => 1,
        26 => 2,
        39..=44 => 3,
        45..=53 => 2,
        55..=57 => 2,
        58 => 3,
        59 => 3,
        60 => 3,
        61 => 1,
        62 => 2,
        63 => 1,
        64 => 2,
        65 => 3,
        66 => 3,
        67 => 3,
        69 => 3,
        72 => 1,
        74 => 1,
        75 => 1,
        77 => 2,
        78 => 2,
        96 => 2,
        97 => 2,
        98..=101 => 4,
        102 => 3,
        103 => 1,
        104 => 2,
        105 => 1,
        106 => 1,
        107 => 1,
        108 => 2,
        112 => 1,
        113 => 2,
        114 => 2,
        115 => 3,
        117 => 7,
        118 => 7,
        119 => 7,
        120 => 5,
        121 => 3,
        124 => 5,
        125 => 6,
        129 => 2,
        131 => 3,
        132 => 4,
        136 => 2,
        138 => 5,
        139 => 4,
        140 => 5,
        141 => 4,
        142 => 5,
        143 => 4,
        152 => 7,
        153 => 1,
        154 => 5,
        155 => 5,
        156 => 2,
        157 => 3,
        158 => 3,
        159 => 4,
        162 => 2,
        163 => 2,
        164 => 2,
        165 => 3,
        166 => 4,
        167 => 3,
        168 => 2,
        169 => 2,
        170 => 4,
        171 => 3,
        172 => 1,
        177 => 6,
        178 => 3,
        180 => 1,
        181 => 5,
        182 => 3,
        183 => 2,
        184 => 4,
        other => {
            return Err(LoadError::DecodeError(format!(
                "unsupported opcode {other}"
            )));
        }
    };
    Ok(arity)
}

fn instruction_opcode(instruction: &Instruction) -> Option<u8> {
    match instruction {
        Instruction::Label { .. } => Some(1),
        Instruction::FuncInfo { .. } => Some(2),
        Instruction::Call { .. } => Some(4),
        Instruction::CallLast { .. } => Some(5),
        Instruction::CallOnly { .. } => Some(6),
        Instruction::CallExt { .. } => Some(7),
        Instruction::CallExtLast { .. } => Some(8),
        Instruction::CallExtOnly { .. } => Some(78),
        Instruction::Generic { opcode, .. } => Some(*opcode),
        _ => None,
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LoadError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| LoadError::DecodeError("truncated Code chunk header".into()))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}
