use crate::atom::Atom;
use crate::error::LoadError;

use super::chunks::Literal;
use super::compact::{CompactDecoder, Operand};
use super::instruction::{
    BifOp, BinaryOp, ComparisonOp, Instruction, MapOp, TypeTestOp, instruction_opcode,
};
use super::opcode::opcode_arity;

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
        65 => Instruction::GetList {
            source: operands[0].clone(),
            head: operands[1].clone(),
            tail: operands[2].clone(),
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
        159 => Instruction::IsTaggedTuple {
            fail: operands[0].clone(),
            value: operands[1].clone(),
            arity: operands[2].clone(),
            tag: operands[3].clone(),
        },
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
        173 => Instruction::RecvMarkerReserve {
            dest: operands[0].clone(),
        },
        174 => Instruction::RecvMarkerBind {
            marker: operands[0].clone(),
            label: operands[1].clone(),
        },
        175 => Instruction::RecvMarkerClear {
            marker: operands[0].clone(),
        },
        176 => Instruction::RecvMarkerUse {
            marker: operands[0].clone(),
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

fn binary_op(op: BinaryOp, operands: Vec<Operand>) -> Instruction {
    Instruction::BinaryOp { op, operands }
}

fn map_op(op: MapOp, operands: Vec<Operand>) -> Instruction {
    Instruction::MapOp { op, operands }
}

fn expect_u32(operand: &Operand, context: &str) -> Result<u32, LoadError> {
    match operand {
        Operand::Integer(value) => u32::try_from(*value)
            .map_err(|_| LoadError::DecodeError(format!("{context} value {value} out of range"))),
        Operand::Unsigned(value) => u32::try_from(*value)
            .map_err(|_| LoadError::DecodeError(format!("{context} value {value} out of range"))),
        Operand::Label(value) => Ok(*value),
        other => Err(LoadError::DecodeError(format!(
            "{context} operand was not an integer label: {other:?}"
        ))),
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LoadError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| LoadError::DecodeError("truncated Code chunk header".into()))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_65_decodes_to_get_list() {
        let instructions = decode_instructions(
            &[
                65,   // get_list/3
                0x03, // X0
                0x13, // X1
                0x23, // X2
            ],
            &[],
            &[],
        )
        .expect("decode get_list");

        assert_eq!(
            instructions,
            vec![Instruction::GetList {
                source: Operand::X(0),
                head: Operand::X(1),
                tail: Operand::X(2),
            }]
        );
    }

    #[test]
    fn opcode_159_decodes_to_is_tagged_tuple() {
        let atoms = [Atom::OK];
        let instructions = decode_instructions(&[159, 0x75, 0x03, 0x20, 0x12], &atoms, &[])
            .expect("decode is_tagged_tuple");

        assert_eq!(
            instructions,
            vec![Instruction::IsTaggedTuple {
                fail: Operand::Label(7),
                value: Operand::X(0),
                arity: Operand::Unsigned(2),
                tag: Operand::Atom(Some(Atom::OK)),
            }]
        );
    }

    #[test]
    fn opcodes_173_to_176_decode_to_recv_marker_instructions() {
        let instructions = decode_instructions(
            &[
                173, 0x03, // recv_marker_reserve X0
                174, 0x03, 0x75, // recv_marker_bind X0, label 7
                175, 0x13, // recv_marker_clear X1
                176, 0x13, // recv_marker_use X1
            ],
            &[],
            &[],
        )
        .expect("decode recv_marker opcodes");

        assert_eq!(
            instructions,
            vec![
                Instruction::RecvMarkerReserve {
                    dest: Operand::X(0),
                },
                Instruction::RecvMarkerBind {
                    marker: Operand::X(0),
                    label: Operand::Label(7),
                },
                Instruction::RecvMarkerClear {
                    marker: Operand::X(1),
                },
                Instruction::RecvMarkerUse {
                    marker: Operand::X(1),
                },
            ]
        );
    }

    #[test]
    fn recv_marker_instruction_opcodes_are_available_for_opcode_max_validation() {
        assert_eq!(
            instruction_opcode(&Instruction::RecvMarkerReserve {
                dest: Operand::X(0)
            }),
            Some(173)
        );
        assert_eq!(
            instruction_opcode(&Instruction::RecvMarkerBind {
                marker: Operand::X(0),
                label: Operand::Label(7),
            }),
            Some(174)
        );
        assert_eq!(
            instruction_opcode(&Instruction::RecvMarkerClear {
                marker: Operand::X(0)
            }),
            Some(175)
        );
        assert_eq!(
            instruction_opcode(&Instruction::RecvMarkerUse {
                marker: Operand::X(0)
            }),
            Some(176)
        );
    }
}
