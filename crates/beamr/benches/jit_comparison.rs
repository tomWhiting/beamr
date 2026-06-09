use beamr::atom::Atom;
use beamr::error::ExecError;
use beamr::interpreter::{ExecutionResult, NativeServices, run_with_native_services};
use beamr::jit::{JitCache, JitCacheKey, JitCompiler, JitSettings};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::loader::decode::{BifOp, ComparisonOp};
use beamr::module::{Module, ModuleOrigin, ModuleRegistry};
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;
use beamr::term::boxed::{write_cons, write_tuple};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

const MODULE_ATOM: Atom = Atom::OK;
const FIB_ATOM: Atom = Atom::ERROR;
const LIST_ATOM: Atom = Atom::FALSE;
const PATTERN_ATOM: Atom = Atom::TRUE;
const FIB_LABEL: u32 = 2;
const LIST_LABEL: u32 = 10;
const PATTERN_LABEL: u32 = 100;
const LARGE_REDUCTION_BUDGET: u32 = 10_000_000;
const FIB_INPUT: i64 = 30;
const FIB_EXPECTED: Term = Term::small_int(832_040);
const LIST_LEN: i64 = 10_000;
const LIST_EXPECTED_SUM: i64 = 74_995_000;
const PATTERN_ITERATIONS: usize = 10_000;

#[derive(Clone)]
struct WorkloadFixture {
    module: Arc<Module>,
    registry: Arc<ModuleRegistry>,
    function: Atom,
    arity: u8,
    entry_ip: usize,
    heap_words: usize,
    setup: fn(&mut Process),
}

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

fn native_services_with_jit_cache(jit_cache: Arc<JitCache>) -> NativeServices {
    NativeServices {
        atom_table: None,
        local_node: None,
        net_kernel: None,
        distribution_send: None,
        timers: None,
        spawn_facility: None,
        remote_spawn_facility: None,
        link_facility: None,
        distribution_control_facility: None,
        global_name_facility: None,
        group_leader_facility: None,
        supervision_facility: None,
        process_info_facility: None,
        io_sink: None,
        code_management_facility: None,
        system_info_facility: None,
        ets_facility: None,
        pg_facility: None,
        io_facility: None,
        io_message_facility: None,
        file_io_facility: None,
        tcp_io_facility: None,
        jit_cache: Some(jit_cache),
    }
}

fn compile_fixture(fixture: &WorkloadFixture) -> Arc<JitCache> {
    let compiler = JitCompiler::new(JitSettings).expect("host JIT compiler should initialize");
    let native = compiler
        .compile(
            &fixture.module.code[fixture.entry_ip + 1..],
            fixture.module.name,
            fixture.function,
            fixture.arity,
        )
        .expect("benchmark function should compile");
    let jit_cache = Arc::new(JitCache::new());
    jit_cache.insert(
        JitCacheKey::new(
            fixture.module.name,
            fixture.function,
            fixture.arity,
            fixture.module.generation(),
        ),
        native,
    );
    jit_cache
}

fn new_process(fixture: &WorkloadFixture) -> Process {
    let mut process = Process::new(1, fixture.heap_words);
    process.reset_reductions(LARGE_REDUCTION_BUDGET);
    process.set_current_module(Arc::clone(&fixture.module));
    process.set_code_position(Some(CodePosition {
        module: fixture.module.name,
        instruction_pointer: fixture.entry_ip,
    }));
    (fixture.setup)(&mut process);
    process
}

fn run_to_exit(
    mut process: Process,
    fixture: &WorkloadFixture,
    services: &NativeServices,
) -> Result<Term, ExecError> {
    loop {
        match run_with_native_services(&mut process, &fixture.module, &fixture.registry, services)?
        {
            ExecutionResult::Exited(ExitReason::Normal) => return Ok(process.x_reg(0)),
            ExecutionResult::Yielded => process.reset_reductions(LARGE_REDUCTION_BUDGET),
            _ => {
                return Err(ExecError::InvalidOperand(
                    "benchmark process did not exit normally",
                ));
            }
        }
    }
}

fn run_interpreted(fixture: &WorkloadFixture) -> Term {
    let services = NativeServices::default();
    run_to_exit(new_process(fixture), fixture, &services)
        .expect("interpreted benchmark should exit")
}

fn run_compiled(fixture: &WorkloadFixture, jit_cache: &Arc<JitCache>) -> Term {
    let services = native_services_with_jit_cache(Arc::clone(jit_cache));
    run_to_exit(new_process(fixture), fixture, &services).expect("compiled benchmark should exit")
}

fn setup_fibonacci(process: &mut Process) {
    process.set_x_reg(0, Term::small_int(FIB_INPUT));
}

fn fibonacci_code() -> Vec<Instruction> {
    vec![
        Instruction::Label { label: FIB_LABEL },
        Instruction::Comparison {
            op: ComparisonOp::Lt,
            fail: Operand::Label(3),
            left: Operand::Integer(1),
            right: Operand::X(0),
        },
        Instruction::Return,
        Instruction::Label { label: 3 },
        Instruction::Move {
            source: Operand::X(0),
            destination: Operand::X(2),
        },
        Instruction::Bif {
            op: BifOp::Bif2,
            operands: vec![
                Operand::Label(99),
                Operand::Unsigned(1),
                Operand::X(0),
                Operand::Integer(1),
                Operand::X(0),
            ],
        },
        Instruction::Call {
            arity: Operand::Unsigned(1),
            label: Operand::Label(FIB_LABEL),
        },
        Instruction::Move {
            source: Operand::X(0),
            destination: Operand::X(1),
        },
        Instruction::Bif {
            op: BifOp::Bif2,
            operands: vec![
                Operand::Label(99),
                Operand::Unsigned(1),
                Operand::X(2),
                Operand::Integer(2),
                Operand::X(0),
            ],
        },
        Instruction::Call {
            arity: Operand::Unsigned(1),
            label: Operand::Label(FIB_LABEL),
        },
        Instruction::Bif {
            op: BifOp::Bif2,
            operands: vec![
                Operand::Label(99),
                Operand::Unsigned(0),
                Operand::X(1),
                Operand::X(0),
                Operand::X(0),
            ],
        },
        Instruction::Return,
        Instruction::Label { label: 99 },
        Instruction::Return,
    ]
}

fn fibonacci_fixture() -> WorkloadFixture {
    let mut module_data = module(MODULE_ATOM, fibonacci_code());
    module_data.function_table.push((FIB_LABEL, FIB_ATOM, 1));
    module_data.exports.insert((FIB_ATOM, 1), FIB_LABEL);
    let registry = ModuleRegistry::new();
    let module = registry.insert(module_data);
    WorkloadFixture {
        entry_ip: module.label_index[&FIB_LABEL],
        module,
        registry: Arc::new(registry),
        function: FIB_ATOM,
        arity: 1,
        heap_words: 512,
        setup: setup_fibonacci,
    }
}

fn setup_list(process: &mut Process) {
    let mut tail = Term::NIL;
    for value in (1..=LIST_LEN).rev() {
        let heap = process
            .heap_mut()
            .alloc_slice(2)
            .expect("list input heap should fit");
        tail = write_cons(heap, Term::small_int(value), tail).expect("cons cell should fit");
    }
    process.set_x_reg(0, tail);
}

fn bif2(import: u64, left: Operand, right: Operand, destination: Operand) -> Instruction {
    Instruction::Bif {
        op: BifOp::Bif2,
        operands: vec![
            Operand::Label(999),
            Operand::Unsigned(import),
            left,
            right,
            destination,
        ],
    }
}

fn list_processing_code() -> Vec<Instruction> {
    vec![
        Instruction::Label { label: LIST_LABEL },
        Instruction::Move {
            source: Operand::Integer(0),
            destination: Operand::X(1),
        },
        Instruction::Label { label: 11 },
        Instruction::Comparison {
            op: ComparisonOp::NeExact,
            fail: Operand::Label(19),
            left: Operand::X(0),
            right: Operand::Atom(None),
        },
        Instruction::GetList {
            source: Operand::X(0),
            head: Operand::X(2),
            tail: Operand::X(0),
        },
        bif2(2, Operand::X(2), Operand::Integer(2), Operand::X(3)),
        Instruction::Comparison {
            op: ComparisonOp::Lt,
            fail: Operand::Label(12),
            left: Operand::Integer(5000),
            right: Operand::X(3),
        },
        Instruction::Jump {
            target: Operand::Label(11),
        },
        Instruction::Label { label: 12 },
        bif2(0, Operand::X(1), Operand::X(3), Operand::X(1)),
        Instruction::Jump {
            target: Operand::Label(11),
        },
        Instruction::Label { label: 19 },
        Instruction::Move {
            source: Operand::X(1),
            destination: Operand::X(0),
        },
        Instruction::Return,
        Instruction::Label { label: 999 },
        Instruction::Return,
    ]
}

fn list_fixture() -> WorkloadFixture {
    let mut module_data = module(MODULE_ATOM, list_processing_code());
    module_data.function_table.push((LIST_LABEL, LIST_ATOM, 1));
    module_data.exports.insert((LIST_ATOM, 1), LIST_LABEL);
    let registry = ModuleRegistry::new();
    let module = registry.insert(module_data);
    WorkloadFixture {
        entry_ip: module.label_index[&LIST_LABEL],
        module,
        registry: Arc::new(registry),
        function: LIST_ATOM,
        arity: 1,
        heap_words: 25_000,
        setup: setup_list,
    }
}

fn pattern_atoms() -> [Atom; 5] {
    [
        Atom::new(40),
        Atom::new(41),
        Atom::new(42),
        Atom::new(43),
        Atom::new(44),
    ]
}

fn tuple_tag_atoms() -> [Atom; 5] {
    [
        Atom::new(50),
        Atom::new(51),
        Atom::new(52),
        Atom::new(53),
        Atom::new(54),
    ]
}

fn setup_pattern_noop(_: &mut Process) {}

fn pattern_match_code() -> Vec<Instruction> {
    let atoms = pattern_atoms();
    let tags = tuple_tag_atoms();
    let mut select_pairs = Vec::new();
    for value in 0..10 {
        select_pairs.push(Operand::Integer(value));
        select_pairs.push(Operand::Label(200 + value as u32));
    }
    for (offset, atom) in atoms.iter().enumerate() {
        select_pairs.push(Operand::Atom(Some(*atom)));
        select_pairs.push(Operand::Label(210 + offset as u32));
    }

    let mut code = vec![
        Instruction::Label {
            label: PATTERN_LABEL,
        },
        Instruction::SelectVal {
            value: Operand::X(0),
            fail: Operand::Label(300),
            list: Operand::List(select_pairs),
        },
        Instruction::Label { label: 300 },
    ];
    for (offset, tag) in tags.iter().enumerate() {
        code.push(Instruction::IsTaggedTuple {
            fail: Operand::Label(301 + offset as u32),
            value: Operand::X(0),
            arity: Operand::Unsigned(2),
            tag: Operand::Atom(Some(*tag)),
        });
        code.push(Instruction::Move {
            source: Operand::Integer(15 + offset as i64),
            destination: Operand::X(0),
        });
        code.push(Instruction::Return);
        code.push(Instruction::Label {
            label: 301 + offset as u32,
        });
    }
    code.push(Instruction::Move {
        source: Operand::Integer(-1),
        destination: Operand::X(0),
    });
    code.push(Instruction::Return);
    for index in 0..20 {
        code.push(Instruction::Label { label: 200 + index });
        code.push(Instruction::Move {
            source: Operand::Integer(i64::from(index)),
            destination: Operand::X(0),
        });
        code.push(Instruction::Return);
    }
    code
}

fn pattern_fixture() -> WorkloadFixture {
    let mut module_data = module(MODULE_ATOM, pattern_match_code());
    module_data
        .function_table
        .push((PATTERN_LABEL, PATTERN_ATOM, 1));
    module_data.exports.insert((PATTERN_ATOM, 1), PATTERN_LABEL);
    let registry = ModuleRegistry::new();
    let module = registry.insert(module_data);
    WorkloadFixture {
        entry_ip: module.label_index[&PATTERN_LABEL],
        module,
        registry: Arc::new(registry),
        function: PATTERN_ATOM,
        arity: 1,
        heap_words: 256,
        setup: setup_pattern_noop,
    }
}

fn pattern_input(process: &mut Process, index: usize) -> Term {
    match index {
        0..=9 => Term::small_int(index as i64),
        10..=14 => Term::atom(pattern_atoms()[index - 10]),
        15..=19 => {
            let heap = process
                .heap_mut()
                .alloc_slice(3)
                .expect("tuple input heap should fit");
            write_tuple(
                heap,
                &[
                    Term::atom(tuple_tag_atoms()[index - 15]),
                    Term::small_int(index as i64),
                ],
            )
            .expect("tuple should fit")
        }
        _ => Term::NIL,
    }
}

fn run_pattern_once(fixture: &WorkloadFixture, services: &NativeServices, index: usize) -> Term {
    let mut process = new_process(fixture);
    let input = pattern_input(&mut process, index);
    process.set_x_reg(0, input);
    run_to_exit(process, fixture, services).expect("pattern benchmark should exit")
}

fn run_pattern_counts_interpreted(fixture: &WorkloadFixture) -> [usize; 20] {
    let services = NativeServices::default();
    run_pattern_counts(fixture, &services)
}

fn run_pattern_counts_compiled(
    fixture: &WorkloadFixture,
    jit_cache: &Arc<JitCache>,
) -> [usize; 20] {
    let services = native_services_with_jit_cache(Arc::clone(jit_cache));
    run_pattern_counts(fixture, &services)
}

fn run_pattern_counts(fixture: &WorkloadFixture, services: &NativeServices) -> [usize; 20] {
    let mut counts = [0usize; 20];
    for iteration in 0..PATTERN_ITERATIONS {
        let result = run_pattern_once(fixture, services, iteration % 20);
        let clause = result
            .as_small_int()
            .expect("pattern result should be a small integer") as usize;
        counts[clause] += 1;
    }
    counts
}

fn sample_speedup(fixture: &WorkloadFixture, jit_cache: &Arc<JitCache>) -> f64 {
    let interpreted_start = Instant::now();
    let interpreted = run_interpreted(fixture);
    let interpreted_elapsed = interpreted_start.elapsed();
    let compiled_start = Instant::now();
    let compiled = run_compiled(fixture, jit_cache);
    let compiled_elapsed = compiled_start.elapsed();
    assert_eq!(interpreted, compiled);
    interpreted_elapsed.as_secs_f64() / compiled_elapsed.as_secs_f64().max(f64::EPSILON)
}

fn bench_execution(c: &mut Criterion) {
    let fibonacci = fibonacci_fixture();
    let fibonacci_jit = compile_fixture(&fibonacci);
    assert_eq!(run_interpreted(&fibonacci), FIB_EXPECTED);
    assert_eq!(run_compiled(&fibonacci, &fibonacci_jit), FIB_EXPECTED);
    println!(
        "fibonacci(30) interpreted/compiled speedup: {:.2}x",
        sample_speedup(&fibonacci, &fibonacci_jit)
    );

    let list = list_fixture();
    let list_jit = compile_fixture(&list);
    let list_expected = Term::small_int(LIST_EXPECTED_SUM);
    assert_eq!(run_interpreted(&list), list_expected);
    assert_eq!(run_compiled(&list, &list_jit), list_expected);
    println!(
        "list map/filter/fold interpreted/compiled speedup: {:.2}x",
        sample_speedup(&list, &list_jit)
    );

    let pattern = pattern_fixture();
    let pattern_jit = compile_fixture(&pattern);
    let expected_counts = [PATTERN_ITERATIONS / 20; 20];
    assert_eq!(run_pattern_counts_interpreted(&pattern), expected_counts);
    assert_eq!(
        run_pattern_counts_compiled(&pattern, &pattern_jit),
        expected_counts
    );

    c.bench_function("bench_fibonacci_interpreted", |b| {
        b.iter(|| black_box(run_interpreted(&fibonacci)))
    });
    c.bench_function("bench_fibonacci_compiled", |b| {
        b.iter(|| black_box(run_compiled(&fibonacci, &fibonacci_jit)))
    });
    c.bench_function("bench_list_processing_interpreted", |b| {
        b.iter(|| black_box(run_interpreted(&list)))
    });
    c.bench_function("bench_list_processing_compiled", |b| {
        b.iter(|| black_box(run_compiled(&list, &list_jit)))
    });
    c.bench_function("bench_pattern_match_interpreted", |b| {
        b.iter(|| black_box(run_pattern_counts_interpreted(&pattern)))
    });
    c.bench_function("bench_pattern_match_compiled", |b| {
        b.iter(|| black_box(run_pattern_counts_compiled(&pattern, &pattern_jit)))
    });
}

fn bench_compilation(c: &mut Criterion) {
    let fibonacci = fibonacci_fixture();
    let list = list_fixture();
    let pattern = pattern_fixture();

    c.bench_function("bench_compile_fibonacci", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| {
                black_box(
                    compiler
                        .compile(
                            &fibonacci.module.code[fibonacci.entry_ip + 1..],
                            fibonacci.module.name,
                            fibonacci.function,
                            fibonacci.arity,
                        )
                        .expect("fibonacci should compile"),
                )
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("bench_compile_list_processing", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| {
                black_box(
                    compiler
                        .compile(
                            &list.module.code[list.entry_ip + 1..],
                            list.module.name,
                            list.function,
                            list.arity,
                        )
                        .expect("list processing should compile"),
                )
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("bench_compile_pattern_match", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| {
                black_box(
                    compiler
                        .compile(
                            &pattern.module.code[pattern.entry_ip + 1..],
                            pattern.module.name,
                            pattern.function,
                            pattern.arity,
                        )
                        .expect("pattern match should compile"),
                )
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(jit_execution, bench_execution);
criterion_group!(jit_compilation, bench_compilation);
criterion_main!(jit_execution, jit_compilation);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_benchmarks_return_expected_value() {
        let fixture = fibonacci_fixture();
        let jit_cache = compile_fixture(&fixture);

        assert_eq!(run_interpreted(&fixture), FIB_EXPECTED);
        assert_eq!(run_compiled(&fixture, &jit_cache), FIB_EXPECTED);
    }

    #[test]
    fn list_processing_benchmarks_return_expected_sum() {
        let fixture = list_fixture();
        let jit_cache = compile_fixture(&fixture);
        let expected = Term::small_int(LIST_EXPECTED_SUM);

        assert_eq!(run_interpreted(&fixture), expected);
        assert_eq!(run_compiled(&fixture, &jit_cache), expected);
    }

    #[test]
    fn pattern_benchmarks_return_even_clause_distribution() {
        let fixture = pattern_fixture();
        let jit_cache = compile_fixture(&fixture);
        let expected = [PATTERN_ITERATIONS / 20; 20];

        assert_eq!(run_pattern_counts_interpreted(&fixture), expected);
        assert_eq!(run_pattern_counts_compiled(&fixture, &jit_cache), expected);
    }

    #[test]
    fn compilation_benchmarks_compile_all_workloads() {
        let compiler = JitCompiler::new(JitSettings).expect("host JIT compiler should initialize");
        for fixture in [fibonacci_fixture(), list_fixture(), pattern_fixture()] {
            compiler
                .compile(
                    &fixture.module.code[fixture.entry_ip + 1..],
                    fixture.module.name,
                    fixture.function,
                    fixture.arity,
                )
                .expect("benchmark workload should compile");
        }
    }
}
