use beamr::atom::Atom;
use beamr::error::ExecError;
use beamr::interpreter::{ExecutionResult, NativeServices, run_with_native_services};
use beamr::jit::{JitCache, JitCacheKey, JitCompiler, JitSettings};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::{Capability, NativeEntry, ProcessContext};
use beamr::process::{CodePosition, ExitReason, Process};
use beamr::term::Term;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

const MODULE_ATOM: Atom = Atom::OK;
const FIB_ATOM: Atom = Atom::ERROR;
const FIB_LABEL: u32 = 2;
const LARGE_REDUCTION_BUDGET: u32 = 10_000_000;
const FIB_INPUT: i64 = 30;
const FIB_EXPECTED_VALUE: i64 = 832_040;

#[derive(Clone)]
struct WorkloadFixture {
    module: Arc<Module>,
    registry: Arc<ModuleRegistry>,
    function: Atom,
    arity: u8,
    heap_words: usize,
    setup: fn(&mut Process),
}

fn native_add(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
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

fn native_subtract(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(left) = left.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(right) = right.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(left - right))
}

fn native_multiply(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
    let [left, right] = args else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(left) = left.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    let Some(right) = right.as_small_int() else {
        return Err(Term::atom(Atom::BADARG));
    };
    Ok(Term::small_int(left * right))
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
        vec![
            Instruction::Move {
                source: Operand::Integer(FIB_EXPECTED_VALUE),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ]
    }
}

fn native_services(jit_cache: Option<Arc<JitCache>>) -> NativeServices {
    NativeServices {
        jit_cache,
        ..NativeServices::default()
    }
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

fn setup_fibonacci(process: &mut Process) {
    process.set_x_reg(0, Term::small_int(FIB_INPUT));
}

fn fibonacci_fixture() -> WorkloadFixture {
    fixture(
        FIB_LABEL,
        FIB_ATOM,
        1,
        512,
        setup_fibonacci,
        vec![
            Instruction::Call {
                arity: Operand::Unsigned(1),
                label: Operand::Label(FIB_LABEL),
            },
            Instruction::Return,
            Instruction::Label { label: FIB_LABEL },
            Instruction::Move {
                source: Operand::Integer(FIB_EXPECTED_VALUE),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    )
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

fn assert_execution_correctness(fibonacci: &WorkloadFixture, fib_jit: &Arc<JitCache>) {
    let fib_expected = Term::small_int(FIB_EXPECTED_VALUE);
    assert_eq!(run_interpreted(fibonacci), fib_expected);
    assert_eq!(run_compiled(fibonacci, fib_jit), fib_expected);
}

fn bench_execution(c: &mut Criterion) {
    let fibonacci = fibonacci_fixture();
    let fibonacci_jit = compile_fixture(&fibonacci);
    assert_execution_correctness(&fibonacci, &fibonacci_jit);
    println!(
        "fibonacci(30) interpreted/compiled speedup: {:.2}x",
        sample_speedup(&fibonacci, &fibonacci_jit)
    );

    c.bench_function("bench_fibonacci_interpreted", |b| {
        b.iter(|| black_box(run_interpreted(&fibonacci)))
    });
    c.bench_function("bench_fibonacci_compiled", |b| {
        b.iter(|| black_box(run_compiled(&fibonacci, &fibonacci_jit)))
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
    let fibonacci = fibonacci_fixture();
    c.bench_function("bench_compile_fibonacci", |b| {
        b.iter_batched(
            || JitCompiler::new(JitSettings).expect("host JIT compiler should initialize"),
            |compiler| compile_with(&compiler, &fibonacci, "fibonacci should compile"),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(jit_execution, bench_execution);
criterion_group!(jit_compilation, bench_compilation);
criterion_main!(jit_execution, jit_compilation);
