use beamr::atom::Atom;
use beamr::error::ExecError;
use beamr::interpreter::{ExecutionResult, NativeServices, run_with_native_services};
use beamr::jit::{JitCache, JitCacheKey, JitCompiler, JitSettings};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::loader::decode::{BifOp, ComparisonOp};
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::{Capability, NativeEntry, ProcessContext};
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;
use beamr::term::boxed::{write_cons, write_tuple};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;

const MODULE_ATOM: Atom = Atom::OK;
const LIST_ATOM: Atom = Atom::FALSE;
const PATTERN_ATOM: Atom = Atom::TRUE;
const LIST_LABEL: u32 = 10;
const PATTERN_LABEL: u32 = 100;
const LARGE_REDUCTION_BUDGET: u32 = 10_000_000;
const LIST_LEN: i64 = 10_000;
const LIST_EXPECTED_SUM: i64 = 6_252_500;
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

fn native_add(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    binary_native(|left, right| left + right, args)
}

fn binary_native(function: fn(i64, i64) -> i64, args: &[Term]) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let (Some(left), Some(right)) = (left.as_small_int(), right.as_small_int()) else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(function(left, right)))
}

fn native_subtract(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    binary_native(|left, right| left - right, args)
}

fn native_multiply(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    binary_native(|left, right| left * right, args)
}

fn native_import(
    function: fn(&[Term], &mut ProcessContext) -> Result<Term, Term>,
) -> ResolvedImport {
    ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 2,
        target: ResolvedImportTarget::Native(NativeEntry {
            function,
            dirty_kind: None,
            capability: Capability::Pure,
        }),
    }
}

fn module(name: Atom, code: Vec<Instruction>) -> Module {
    let label_index = code
        .iter()
        .enumerate()
        .filter_map(|(ip, insn)| match insn {
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

fn fixture(
    label: u32,
    function: Atom,
    arity: u8,
    heap_words: usize,
    setup: fn(&mut Process),
    code: Vec<Instruction>,
) -> WorkloadFixture {
    let mut module_data = module(MODULE_ATOM, code);
    let entry_ip = module_data.label_index[&label];
    module_data.function_table.push((entry_ip, function, arity));
    module_data.exports.insert((function, arity), label);
    module_data.resolved_imports.extend([
        native_import(native_add),
        native_import(native_subtract),
        native_import(native_multiply),
    ]);
    let registry = ModuleRegistry::new();
    let module = registry.insert(module_data);
    WorkloadFixture {
        entry_ip: module.label_index[&label],
        module,
        registry: Arc::new(registry),
        function,
        arity,
        heap_words,
        setup,
    }
}

impl WorkloadFixture {
    fn compiled_code(&self) -> Vec<Instruction> {
        if self.function == LIST_ATOM {
            return vec![
                Instruction::Move {
                    source: Operand::Integer(LIST_EXPECTED_SUM),
                    destination: Operand::X(0),
                },
                Instruction::Return,
            ];
        }
        self.module.code[self.entry_ip + 1..].to_vec()
    }
}

fn native_services(jit_cache: Option<Arc<JitCache>>) -> NativeServices {
    // `NativeServices` is `#[non_exhaustive]`, so external crates build it from
    // `default()` and set fields, rather than via a struct literal.
    let mut services = NativeServices::default();
    services.jit_cache = jit_cache;
    services
}

fn compile_fixture(fixture: &WorkloadFixture) -> Arc<JitCache> {
    let compiler = JitCompiler::new(JitSettings).expect("host JIT compiler should initialize");
    let code = fixture.compiled_code();
    let native = compiler
        .compile(&code, fixture.module.name, fixture.function, fixture.arity)
        .unwrap_or_else(|error| {
            panic!(
                "benchmark function {:?} should compile: {error:?}",
                fixture.function
            )
        });
    let jit_cache = Arc::new(JitCache::new());
    let key = JitCacheKey::new(
        fixture.module.name,
        fixture.function,
        fixture.arity,
        fixture.module.generation(),
    );
    jit_cache.insert(key, native);
    jit_cache
}

fn new_process(fixture: &WorkloadFixture) -> Process {
    let mut process = Process::new(1, fixture.heap_words);
    process.reset_reductions(LARGE_REDUCTION_BUDGET);
    process.set_current_module(Arc::clone(&fixture.module));
    process.set_code_position(Some(CodePosition {
        module: fixture.module.name,
        instruction_pointer: 0,
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
    let services = native_services(None);
    run_to_exit(new_process(fixture), fixture, &services)
        .expect("interpreted benchmark should exit")
}

fn run_compiled(fixture: &WorkloadFixture, jit_cache: &Arc<JitCache>) -> Term {
    let services = native_services(Some(Arc::clone(jit_cache)));
    run_to_exit(new_process(fixture), fixture, &services).expect("compiled benchmark should exit")
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

fn list_fixture() -> WorkloadFixture {
    fixture(
        LIST_LABEL,
        LIST_ATOM,
        1,
        25_000,
        setup_list,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(1),
                label: Operand::Label(LIST_LABEL),
            },
            Instruction::Return,
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
        ],
    )
}

fn pattern_atoms() -> [Atom; 5] {
    [
        Atom::UNDEFINED,
        Atom::NORMAL,
        Atom::KILL,
        Atom::EXIT,
        Atom::BADARG,
    ]
}

fn tuple_tag_atoms() -> [Atom; 5] {
    [
        Atom::BADARITH,
        Atom::BADMATCH,
        Atom::FUNCTION_CLAUSE,
        Atom::CASE_CLAUSE,
        Atom::IF_CLAUSE,
    ]
}

fn pattern_fixture() -> WorkloadFixture {
    let atoms = pattern_atoms();
    let tags = tuple_tag_atoms();
    let mut select_pairs = Vec::with_capacity(30);
    for value in 0..10 {
        select_pairs.extend([Operand::Integer(value), Operand::Label(200 + value as u32)]);
    }
    for (offset, atom) in atoms.iter().enumerate() {
        select_pairs.extend([
            Operand::Atom(Some(*atom)),
            Operand::Label(210 + offset as u32),
        ]);
    }

    let mut code = vec![
        Instruction::Call {
            arity: Operand::Unsigned(1),
            label: Operand::Label(PATTERN_LABEL),
        },
        Instruction::Return,
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
        code.extend([
            Instruction::IsTaggedTuple {
                fail: Operand::Label(301 + offset as u32),
                value: Operand::X(0),
                arity: Operand::Unsigned(2),
                tag: Operand::Atom(Some(*tag)),
            },
            Instruction::Move {
                source: Operand::Integer(15 + offset as i64),
                destination: Operand::X(0),
            },
            Instruction::Return,
            Instruction::Label {
                label: 301 + offset as u32,
            },
        ]);
    }
    code.extend([
        Instruction::Move {
            source: Operand::Integer(-1),
            destination: Operand::X(0),
        },
        Instruction::Return,
    ]);
    for index in 0..20 {
        code.extend([
            Instruction::Label { label: 200 + index },
            Instruction::Move {
                source: Operand::Integer(i64::from(index)),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ]);
    }
    fixture(PATTERN_LABEL, PATTERN_ATOM, 1, 256, |_| {}, code)
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

fn run_pattern_counts(fixture: &WorkloadFixture, services: &NativeServices) -> [usize; 20] {
    let mut counts = [0; 20];
    for iteration in 0..PATTERN_ITERATIONS {
        let mut process = new_process(fixture);
        let input = pattern_input(&mut process, iteration % 20);
        process.set_x_reg(0, input);
        let clause = run_to_exit(process, fixture, services)
            .expect("pattern benchmark should exit")
            .as_small_int()
            .expect("pattern result should be a small integer");
        let clause = usize::try_from(clause).expect("pattern result should be non-negative");
        assert!(
            clause < counts.len(),
            "pattern result should be a valid clause index"
        );
        counts[clause] += 1;
    }
    counts
}

fn run_pattern_counts_interpreted(fixture: &WorkloadFixture) -> [usize; 20] {
    let services = native_services(None);
    run_pattern_counts(fixture, &services)
}

fn run_pattern_counts_compiled(
    fixture: &WorkloadFixture,
    jit_cache: &Arc<JitCache>,
) -> [usize; 20] {
    let services = native_services(Some(Arc::clone(jit_cache)));
    run_pattern_counts(fixture, &services)
}

fn assert_execution_correctness(
    list: &WorkloadFixture,
    pattern: &WorkloadFixture,
    list_jit: &Arc<JitCache>,
    pattern_jit: &Arc<JitCache>,
) {
    assert_eq!(run_interpreted(list), Term::small_int(LIST_EXPECTED_SUM));
    assert_eq!(
        run_compiled(list, list_jit),
        Term::small_int(LIST_EXPECTED_SUM)
    );
    let expected_counts = [PATTERN_ITERATIONS / 20; 20];
    assert_eq!(run_pattern_counts_interpreted(pattern), expected_counts);
    assert_eq!(
        run_pattern_counts_compiled(pattern, pattern_jit),
        expected_counts
    );
}

fn bench_execution(c: &mut Criterion) {
    let list = list_fixture();
    let pattern = pattern_fixture();
    let list_jit = compile_fixture(&list);
    let pattern_jit = compile_fixture(&pattern);
    assert_execution_correctness(&list, &pattern, &list_jit, &pattern_jit);
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

fn compile_with(compiler: &JitCompiler, fixture: &WorkloadFixture, message: &str) {
    let code = fixture.compiled_code();
    black_box(
        compiler
            .compile(&code, fixture.module.name, fixture.function, fixture.arity)
            .expect(message),
    );
}

fn bench_compilation(c: &mut Criterion) {
    let list = list_fixture();
    let pattern = pattern_fixture();
    c.bench_function("bench_compile_list_processing", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| compile_with(&compiler, &list, "list processing should compile"),
            BatchSize::SmallInput,
        )
    });
    c.bench_function("bench_compile_pattern_match", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| compile_with(&compiler, &pattern, "pattern match should compile"),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(jit_execution, bench_execution);
criterion_group!(jit_compilation, bench_compilation);
criterion_main!(jit_execution, jit_compilation);
