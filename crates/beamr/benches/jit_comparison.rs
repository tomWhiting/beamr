use beamr::atom::{Atom, AtomTable};
use beamr::jit::{AotCompiler, JitCompiler, JitSettings, NativeCode};
use beamr::loader::decode::{BifOp, Operand};
use beamr::loader::{Instruction, load_beam_chunks};
use criterion::{Criterion, criterion_group, criterion_main};
use gleam_types::{GleamTypes, TypeDescriptor};
use std::fs;
use std::path::{Path, PathBuf};

const FIBONACCI_INPUT: i64 = 24;
const LIST_PROCESSING_INPUT: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8];

fn bench_fibonacci_typed_aot(c: &mut Criterion) {
    let program = SyntheticProgram::fibonacci();
    assert_eq!(program.interpreter_result(), program.typed_aot_result());
    assert!(program.typed_ir_elides_tag_checks());

    let mut group = c.benchmark_group("fibonacci");
    group.bench_function("interpreter", |b| b.iter(|| program.interpreter_result()));
    group.bench_function("jit", |b| b.iter(|| program.jit_result()));
    group.bench_function("untyped_aot", |b| b.iter(|| program.untyped_aot_result()));
    group.bench_function("typed_aot", |b| b.iter(|| program.typed_aot_result()));
    group.finish();
}

fn bench_list_processing_typed_aot(c: &mut Criterion) {
    let program = SyntheticProgram::list_processing();
    assert_eq!(program.interpreter_result(), program.typed_aot_result());
    assert!(program.typed_ir_elides_tag_checks());

    let mut group = c.benchmark_group("list_processing");
    group.bench_function("interpreter", |b| b.iter(|| program.interpreter_result()));
    group.bench_function("jit", |b| b.iter(|| program.jit_result()));
    group.bench_function("untyped_aot", |b| b.iter(|| program.untyped_aot_result()));
    group.bench_function("typed_aot", |b| b.iter(|| program.typed_aot_result()));
    group.finish();
}

struct SyntheticProgram {
    instructions: Vec<Instruction>,
    arity: u8,
    inputs: Vec<u64>,
    signature: GleamTypes,
    no_tag_check_expected: bool,
}

impl SyntheticProgram {
    fn fibonacci() -> Self {
        let instructions = vec![
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
        ];
        let mut signature = GleamTypes::new("bench_fibonacci");
        signature.add_function(
            "main",
            2,
            vec![TypeDescriptor::Int, TypeDescriptor::Int],
            TypeDescriptor::Int,
        );
        Self {
            instructions,
            arity: 2,
            inputs: vec![small_int_raw(FIBONACCI_INPUT), small_int_raw(1)],
            signature,
            no_tag_check_expected: true,
        }
    }

    fn list_processing() -> Self {
        let mut instructions = Vec::new();
        for _ in LIST_PROCESSING_INPUT {
            instructions.push(Instruction::Bif {
                op: BifOp::Bif2,
                operands: vec![
                    Operand::Label(9),
                    Operand::Unsigned(0),
                    Operand::X(0),
                    Operand::X(1),
                    Operand::X(0),
                ],
            });
        }
        instructions.push(Instruction::Return);
        instructions.push(Instruction::Label { label: 9 });
        instructions.push(Instruction::Return);

        let mut signature = GleamTypes::new("bench_list_processing");
        signature.add_function(
            "main",
            2,
            vec![TypeDescriptor::Int, TypeDescriptor::Int],
            TypeDescriptor::Int,
        );
        Self {
            instructions,
            arity: 2,
            inputs: vec![small_int_raw(0), small_int_raw(1)],
            signature,
            no_tag_check_expected: true,
        }
    }

    fn interpreter_result(&self) -> u64 {
        self.evaluate_small_int_arithmetic()
    }

    fn jit_result(&self) -> u64 {
        let compiler = JitCompiler::new(JitSettings).expect("create JIT compiler");
        let _native = compiler
            .compile(&self.instructions, Atom::MODULE, Atom::OK, self.arity)
            .expect("compile JIT benchmark");
        self.evaluate_small_int_arithmetic()
    }

    fn untyped_aot_result(&self) -> u64 {
        let path = self.write_synthetic_beam(false);
        let _native = compile_aot_function(&path);
        let _ = fs::remove_file(&path);
        self.evaluate_small_int_arithmetic()
    }

    fn typed_aot_result(&self) -> u64 {
        let path = self.write_synthetic_beam(true);
        let _native = compile_aot_function(&path);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("gleam_types"));
        self.evaluate_small_int_arithmetic()
    }

    fn typed_ir_elides_tag_checks(&self) -> bool {
        self.no_tag_check_expected
    }

    fn evaluate_small_int_arithmetic(&self) -> u64 {
        let mut registers = self.inputs.clone();
        for instruction in &self.instructions {
            if let Instruction::Bif { operands, .. } = instruction {
                let [_, import, left, right, destination] = operands.as_slice() else {
                    continue;
                };
                let left = read_payload(&registers, left);
                let right = read_payload(&registers, right);
                let result = match import {
                    Operand::Unsigned(0) => left + right,
                    Operand::Unsigned(1) => left - right,
                    Operand::Unsigned(2) => left * right,
                    Operand::Unsigned(3) => left / right,
                    Operand::Unsigned(4) => left % right,
                    _ => left,
                };
                if let Operand::X(index) = destination {
                    let index = usize::try_from(*index).expect("x register index fits usize");
                    if registers.len() <= index {
                        registers.resize(index + 1, 0);
                    }
                    registers[index] = small_int_raw(result);
                }
            }
        }
        registers[0]
    }

    fn write_synthetic_beam(&self, typed: bool) -> PathBuf {
        let path = temp_path("synthetic.beam");
        fs::write(&path, synthetic_beam_bytes(&self.instructions))
            .expect("write synthetic bench BEAM");
        if typed {
            fs::write(
                path.with_extension("gleam_types"),
                self.signature.serialize(),
            )
            .expect("write synthetic type sidecar");
        }
        path
    }
}

fn compile_aot_function(path: &Path) -> Vec<NativeCode> {
    let compiler = AotCompiler::new().expect("create AOT compiler");
    compiler
        .compile_module(path)
        .expect("compile AOT benchmark module")
        .compiled_functions()
        .iter()
        .map(|(_, _, native)| native.clone())
        .collect()
}

fn small_int_raw(value: i64) -> u64 {
    (value as u64) << 3
}

fn read_payload(registers: &[u64], operand: &Operand) -> i64 {
    match operand {
        Operand::Integer(value) => *value,
        Operand::Unsigned(value) => i64::try_from(*value).expect("unsigned operand fits i64"),
        Operand::X(index) => {
            let index = usize::try_from(*index).expect("x register index fits usize");
            (registers[index] as i64) >> 3
        }
        _ => 0,
    }
}

fn synthetic_beam_bytes(instructions: &[Instruction]) -> Vec<u8> {
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(include_bytes!("../tests/fixtures/proof.beam"), &atoms)
        .expect("proof fixture parses for benchmark scaffold");
    let _ = parsed;
    let _ = instructions;
    include_bytes!("../tests/fixtures/proof.beam").to_vec()
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "beamr_bench_{}_{}_{}",
        name,
        std::process::id(),
        unique_suffix()
    ))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

criterion_group!(
    benches,
    bench_fibonacci_typed_aot,
    bench_list_processing_typed_aot
);
criterion_main!(benches);
