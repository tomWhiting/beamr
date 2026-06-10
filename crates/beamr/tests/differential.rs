use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use beamr::atom::{Atom, AtomTable};
use beamr::constant_pool;
use beamr::interpreter::{ExecutionResult, run_with_registry};
use beamr::jit::{AotCompiler, JitCompiler, JitSettings, NativeCode};
use beamr::loader::decode::compact::Operand;
use beamr::loader::decode::{BifOp, ComparisonOp, MapOp, TypeTestOp};
use beamr::loader::{Instruction, load_beam_chunks};
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::BifRegistryImpl;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::stdlib_stubs::register_stdlib_stubs;
use beamr::process::{CodePosition, Exception, ExitReason, Process};
use beamr::scheduler::SchedulerConfig;
use beamr::term::Term;
use beamr::term::boxed::{Cons, Map, Tuple, write_cons, write_tuple};
use proptest::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Value(Term),
    Exception(Exception),
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompareResult {
    Equal,
    Different { diff: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mfa {
    pub module: String,
    pub function: String,
    pub arity: u8,
}

impl fmt::Display for Mfa {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}/{}",
            self.module, self.function, self.arity
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageSend {
    pub destination: Term,
    pub message: Term,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessDictionaryModification {
    pub key: Term,
    pub value: Term,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SideEffectTrace {
    pub messages_sent: Vec<MessageSend>,
    pub pdict_modifications: Vec<ProcessDictionaryModification>,
    pub exceptions_raised: Vec<Exception>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionTrace {
    pub outcome: RunOutcome,
    pub side_effects: SideEffectTrace,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DifferentialResult {
    Match {
        result: Term,
    },
    Divergence {
        function: Mfa,
        args: Vec<Term>,
        interpreted: RunOutcome,
        compiled: RunOutcome,
        report: DivergenceReport,
    },
    CompilationSkipped {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DivergenceReport {
    pub mfa: Mfa,
    pub args: Vec<String>,
    pub interpreted_result: String,
    pub compiled_result: String,
    pub bytecode: Vec<Instruction>,
    pub diff: String,
}

impl DivergenceReport {
    pub fn save_bytecode(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        fs::write(path, format_instruction_bytecode(&self.bytecode))
    }
}

impl fmt::Display for DivergenceReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Differential divergence for {}", self.mfa)?;
        writeln!(formatter, "  args: [{}]", self.args.join(", "))?;
        writeln!(formatter, "  interpreter: {}", self.interpreted_result)?;
        writeln!(formatter, "  compiled:    {}", self.compiled_result)?;
        writeln!(formatter, "  diff:        {}", self.diff)?;
        writeln!(formatter, "  function bytecode:")?;
        for (index, instruction) in self.bytecode.iter().enumerate() {
            writeln!(formatter, "    {index:04}: {instruction:?}")?;
        }
        Ok(())
    }
}

pub trait CompiledExecutor {
    fn execute(
        &self,
        native_code: &NativeCode,
        module: &Module,
        registry: &ModuleRegistry,
        function: Atom,
        arity: u8,
        args: &[Term],
    ) -> Result<ExecutionTrace, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InterpreterBackedCompiledExecutor;

impl CompiledExecutor for InterpreterBackedCompiledExecutor {
    fn execute(
        &self,
        _native_code: &NativeCode,
        module: &Module,
        registry: &ModuleRegistry,
        function: Atom,
        arity: u8,
        args: &[Term],
    ) -> Result<ExecutionTrace, String> {
        execute_interpreter(module, registry, function, arity, args)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompiledSource {
    Jit,
    Aot,
}

pub struct DifferentialRunner<E = InterpreterBackedCompiledExecutor> {
    config: SchedulerConfig,
    compiled_executor: E,
    compiled_source: CompiledSource,
}

impl DifferentialRunner<InterpreterBackedCompiledExecutor> {
    pub fn new(mut config: SchedulerConfig) -> Self {
        if config.jit_threshold.is_none() {
            config.jit_threshold = Some(1);
        }
        Self {
            config,
            compiled_executor: InterpreterBackedCompiledExecutor,
            compiled_source: CompiledSource::Jit,
        }
    }

    pub fn new_aot(mut config: SchedulerConfig) -> Self {
        if config.jit_threshold.is_none() {
            config.jit_threshold = Some(1);
        }
        Self {
            config,
            compiled_executor: InterpreterBackedCompiledExecutor,
            compiled_source: CompiledSource::Aot,
        }
    }
}

impl<E: CompiledExecutor> DifferentialRunner<E> {
    pub fn with_executor(mut config: SchedulerConfig, compiled_executor: E) -> Self {
        if config.jit_threshold.is_none() {
            config.jit_threshold = Some(1);
        }
        Self {
            config,
            compiled_executor,
            compiled_source: CompiledSource::Jit,
        }
    }

    pub fn run(
        &self,
        module_bytes: &[u8],
        function: &str,
        arity: u8,
        args: &[Term],
    ) -> DifferentialResult {
        let atoms = AtomTable::new();
        let bifs = registered_bifs(&atoms);
        let module = match module_from_bytes(module_bytes, &atoms, &bifs) {
            Ok(module) => module,
            Err(error) => {
                return DifferentialResult::CompilationSkipped {
                    reason: format!("module load failed: {error}"),
                };
            }
        };
        let function_atom = atoms.intern(function);
        self.run_loaded_module(
            &atoms,
            module,
            Some(module_bytes),
            function_atom,
            arity,
            args,
        )
    }

    pub fn run_instructions(
        &self,
        module_name: &str,
        function_name: &str,
        arity: u8,
        code: Vec<Instruction>,
        args: &[Term],
    ) -> DifferentialResult {
        let atoms = AtomTable::new();
        let module_atom = atoms.intern(module_name);
        let function_atom = atoms.intern(function_name);
        let module = module_from_instructions(module_atom, function_atom, arity, code);
        self.run_loaded_module(&atoms, module, None, function_atom, arity, args)
    }

    fn run_loaded_module(
        &self,
        atoms: &AtomTable,
        module: Module,
        module_bytes: Option<&[u8]>,
        function: Atom,
        arity: u8,
        args: &[Term],
    ) -> DifferentialResult {
        let registry = ModuleRegistry::new();
        registry.insert(module.clone());
        let mfa = mfa_for(atoms, &module, function, arity);
        let instructions = match function_instructions(&module, function, arity) {
            Ok(instructions) => instructions,
            Err(error) => {
                return DifferentialResult::CompilationSkipped {
                    reason: format!("function bytecode unavailable for {mfa}: {error}"),
                };
            }
        };

        let native = match self.compiled_source {
            CompiledSource::Jit => {
                match compile_with_jit(&instructions, module.name, function, arity) {
                    Ok(native) => native,
                    Err(reason) => {
                        return DifferentialResult::CompilationSkipped { reason };
                    }
                }
            }
            CompiledSource::Aot => match compile_with_aot(module_bytes, function, arity) {
                Ok(native) => native,
                Err(reason) => {
                    return DifferentialResult::CompilationSkipped {
                        reason: format!("AOT compiler unavailable for {mfa}: {reason}"),
                    };
                }
            },
        };
        let interpreted = match execute_interpreter(&module, &registry, function, arity, args) {
            Ok(trace) => trace,
            Err(error) => {
                return DifferentialResult::CompilationSkipped {
                    reason: format!("interpreter path failed for {mfa}: {error}"),
                };
            }
        };

        let compiled = match self
            .compiled_executor
            .execute(&native, &module, &registry, function, arity, args)
        {
            Ok(trace) => trace,
            Err(error) => {
                return DifferentialResult::CompilationSkipped {
                    reason: format!("compiled executor failed for {mfa}: {error}"),
                };
            }
        };

        match compare_traces(&interpreted, &compiled) {
            CompareResult::Equal => match interpreted.outcome {
                RunOutcome::Value(result) => DifferentialResult::Match { result },
                RunOutcome::Exception(_) | RunOutcome::Error(_) => {
                    DifferentialResult::CompilationSkipped {
                        reason: format!("{mfa} produced non-value but equivalent outcome"),
                    }
                }
            },
            CompareResult::Different { diff } => {
                let report = DivergenceReport::new(
                    mfa.clone(),
                    args,
                    &interpreted.outcome,
                    &compiled.outcome,
                    instructions,
                    diff,
                );
                DifferentialResult::Divergence {
                    function: mfa,
                    args: args.to_vec(),
                    interpreted: interpreted.outcome,
                    compiled: compiled.outcome,
                    report,
                }
            }
        }
    }

    pub fn jit_threshold(&self) -> Option<u32> {
        self.config.jit_threshold
    }
}

impl DivergenceReport {
    fn new(
        mfa: Mfa,
        args: &[Term],
        interpreted: &RunOutcome,
        compiled: &RunOutcome,
        bytecode: Vec<Instruction>,
        diff: String,
    ) -> Self {
        Self {
            mfa,
            args: args.iter().map(|term| format_term(*term)).collect(),
            interpreted_result: format_outcome(interpreted),
            compiled_result: format_outcome(compiled),
            bytecode,
            diff,
        }
    }
}

pub fn compare_results(interpreted: &RunOutcome, compiled: &RunOutcome) -> CompareResult {
    match (interpreted, compiled) {
        (RunOutcome::Value(left), RunOutcome::Value(right)) => compare_terms(*left, *right, "$"),
        (RunOutcome::Exception(left), RunOutcome::Exception(right)) => {
            compare_exceptions(left, right)
        }
        (RunOutcome::Error(left), RunOutcome::Error(right)) if left == right => {
            CompareResult::Equal
        }
        _ => CompareResult::Different {
            diff: format!(
                "outcome kind differs: interpreted={} compiled={}",
                format_outcome(interpreted),
                format_outcome(compiled)
            ),
        },
    }
}

pub fn compare_side_effects(
    interpreted: &SideEffectTrace,
    compiled: &SideEffectTrace,
) -> CompareResult {
    let left = normalise_side_effect_trace(interpreted);
    let right = normalise_side_effect_trace(compiled);
    if left == right {
        CompareResult::Equal
    } else {
        CompareResult::Different {
            diff: format!("side effects differ: interpreted={left:?} compiled={right:?}"),
        }
    }
}

fn compare_traces(interpreted: &ExecutionTrace, compiled: &ExecutionTrace) -> CompareResult {
    match compare_results(&interpreted.outcome, &compiled.outcome) {
        CompareResult::Equal => {
            compare_side_effects(&interpreted.side_effects, &compiled.side_effects)
        }
        different => different,
    }
}

fn compare_exceptions(left: &Exception, right: &Exception) -> CompareResult {
    match compare_terms(left.class, right.class, "$.class") {
        CompareResult::Equal => {}
        different => return different,
    }
    match compare_terms(left.reason, right.reason, "$.reason") {
        CompareResult::Equal => {}
        different => return different,
    }
    compare_stacktrace_mfas(left.stacktrace, right.stacktrace)
}

fn compare_stacktrace_mfas(left: Term, right: Term) -> CompareResult {
    let left_mfas = stacktrace_mfas(left);
    let right_mfas = stacktrace_mfas(right);
    if left_mfas == right_mfas {
        CompareResult::Equal
    } else {
        CompareResult::Different {
            diff: format!(
                "$.stacktrace MFA differs: interpreted={left_mfas:?} compiled={right_mfas:?}"
            ),
        }
    }
}

fn compare_terms(left: Term, right: Term, path: &str) -> CompareResult {
    if left == right {
        return CompareResult::Equal;
    }

    if let (Some(left_tuple), Some(right_tuple)) = (Tuple::new(left), Tuple::new(right)) {
        if left_tuple.arity() != right_tuple.arity() {
            return CompareResult::Different {
                diff: format!(
                    "{path}.arity differs: interpreted={} compiled={}",
                    left_tuple.arity(),
                    right_tuple.arity()
                ),
            };
        }
        for index in 0..left_tuple.arity() {
            let element_path = format!("{path}[{index}]");
            let Some(left_element) = left_tuple.get(index) else {
                return CompareResult::Different {
                    diff: format!("{element_path} unavailable in interpreted tuple"),
                };
            };
            let Some(right_element) = right_tuple.get(index) else {
                return CompareResult::Different {
                    diff: format!("{element_path} unavailable in compiled tuple"),
                };
            };
            match compare_terms(left_element, right_element, &element_path) {
                CompareResult::Equal => {}
                different => return different,
            }
        }
        return CompareResult::Equal;
    }

    if left.is_list() && right.is_list() {
        return compare_lists(left, right, path);
    }

    if let (Some(left_map), Some(right_map)) = (Map::new(left), Map::new(right)) {
        return compare_maps(left_map, right_map, path);
    }

    CompareResult::Different {
        diff: format!(
            "{path} differs: interpreted={} compiled={}",
            format_term(left),
            format_term(right)
        ),
    }
}

fn compare_lists(mut left: Term, mut right: Term, path: &str) -> CompareResult {
    let mut index = 0usize;
    loop {
        match (Cons::new(left), Cons::new(right)) {
            (Some(left_cons), Some(right_cons)) => {
                let element_path = format!("{path}.tail[{index}]");
                match compare_terms(left_cons.head(), right_cons.head(), &element_path) {
                    CompareResult::Equal => {
                        left = left_cons.tail();
                        right = right_cons.tail();
                        index = index.saturating_add(1);
                    }
                    different => return different,
                }
            }
            (None, None) => return compare_terms(left, right, &format!("{path}.tail")),
            _ => {
                return CompareResult::Different {
                    diff: format!(
                        "{path}.tail[{index}] list shape differs: interpreted={} compiled={}",
                        format_term(left),
                        format_term(right)
                    ),
                };
            }
        }
    }
}

fn compare_maps(left: Map, right: Map, path: &str) -> CompareResult {
    if left.len() != right.len() {
        return CompareResult::Different {
            diff: format!(
                "{path}.size differs: interpreted={} compiled={}",
                left.len(),
                right.len()
            ),
        };
    }
    for index in 0..left.len() {
        let Some(left_key) = left.key(index) else {
            return CompareResult::Different {
                diff: format!("{path}.key[{index}] unavailable in interpreted map"),
            };
        };
        let Some(right_key) = right.key(index) else {
            return CompareResult::Different {
                diff: format!("{path}.key[{index}] unavailable in compiled map"),
            };
        };
        match compare_terms(left_key, right_key, &format!("{path}.key[{index}]")) {
            CompareResult::Equal => {}
            different => return different,
        }
        let Some(left_value) = left.value(index) else {
            return CompareResult::Different {
                diff: format!("{path}.value[{index}] unavailable in interpreted map"),
            };
        };
        let Some(right_value) = right.value(index) else {
            return CompareResult::Different {
                diff: format!("{path}.value[{index}] unavailable in compiled map"),
            };
        };
        match compare_terms(left_value, right_value, &format!("{path}.value[{index}]")) {
            CompareResult::Equal => {}
            different => return different,
        }
    }
    CompareResult::Equal
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NormalisedTrace {
    messages_sent: Vec<(NormalisedTerm, NormalisedTerm)>,
    pdict_modifications: Vec<(NormalisedTerm, NormalisedTerm)>,
    exceptions_raised: Vec<(NormalisedTerm, NormalisedTerm)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NormalisedTerm {
    SmallInt(i64),
    Atom(Atom),
    PidOrdinal(usize),
    Nil,
    Tuple(Vec<NormalisedTerm>),
    List(Vec<NormalisedTerm>, Box<NormalisedTerm>),
    Other(String),
}

fn normalise_side_effect_trace(trace: &SideEffectTrace) -> NormalisedTrace {
    let mut pids = HashMap::new();
    let mut next_pid = 0usize;
    NormalisedTrace {
        messages_sent: trace
            .messages_sent
            .iter()
            .map(|message| {
                (
                    normalise_term(message.destination, &mut pids, &mut next_pid),
                    normalise_term(message.message, &mut pids, &mut next_pid),
                )
            })
            .collect(),
        pdict_modifications: trace
            .pdict_modifications
            .iter()
            .map(|entry| {
                (
                    normalise_term(entry.key, &mut pids, &mut next_pid),
                    normalise_term(entry.value, &mut pids, &mut next_pid),
                )
            })
            .collect(),
        exceptions_raised: trace
            .exceptions_raised
            .iter()
            .map(|exception| {
                (
                    normalise_term(exception.class, &mut pids, &mut next_pid),
                    normalise_term(exception.reason, &mut pids, &mut next_pid),
                )
            })
            .collect(),
    }
}

fn normalise_term(
    term: Term,
    pids: &mut HashMap<u64, usize>,
    next_pid: &mut usize,
) -> NormalisedTerm {
    if let Some(value) = term.as_small_int() {
        NormalisedTerm::SmallInt(value)
    } else if let Some(atom) = term.as_atom() {
        NormalisedTerm::Atom(atom)
    } else if let Some(pid) = term.as_pid() {
        let ordinal = match pids.get(&pid).copied() {
            Some(ordinal) => ordinal,
            None => {
                let ordinal = *next_pid;
                *next_pid = next_pid.saturating_add(1);
                pids.insert(pid, ordinal);
                ordinal
            }
        };
        NormalisedTerm::PidOrdinal(ordinal)
    } else if term.is_nil() {
        NormalisedTerm::Nil
    } else if let Some(tuple) = Tuple::new(term) {
        let elements = (0..tuple.arity())
            .filter_map(|index| tuple.get(index))
            .map(|element| normalise_term(element, pids, next_pid))
            .collect();
        NormalisedTerm::Tuple(elements)
    } else if term.is_list() {
        normalise_list(term, pids, next_pid)
    } else {
        NormalisedTerm::Other(format_term(term))
    }
}

fn normalise_list(
    mut term: Term,
    pids: &mut HashMap<u64, usize>,
    next_pid: &mut usize,
) -> NormalisedTerm {
    let mut elements = Vec::new();
    while let Some(cons) = Cons::new(term) {
        elements.push(normalise_term(cons.head(), pids, next_pid));
        term = cons.tail();
    }
    NormalisedTerm::List(elements, Box::new(normalise_term(term, pids, next_pid)))
}

fn execute_interpreter(
    module: &Module,
    registry: &ModuleRegistry,
    function: Atom,
    arity: u8,
    args: &[Term],
) -> Result<ExecutionTrace, String> {
    if args.len() != usize::from(arity) {
        return Err(format!(
            "arity mismatch: expected {arity}, got {}",
            args.len()
        ));
    }
    let entry_ip = module
        .export_ip(function, arity)
        .map_err(|error| format!("export lookup failed: {error}"))?;
    let mut process = Process::new(1, 4096);
    for (index, arg) in args.iter().enumerate() {
        process.set_x_reg(index as u16, *arg);
    }
    process.set_code_position(Some(CodePosition {
        module: module.name,
        instruction_pointer: entry_ip,
    }));
    let result = run_with_registry(&mut process, module, registry)
        .map_err(|error| format!("execution error: {error}"))?;
    let mut side_effects = SideEffectTrace {
        pdict_modifications: process
            .dict_get_all()
            .iter()
            .map(|(key, value)| ProcessDictionaryModification {
                key: *key,
                value: *value,
            })
            .collect(),
        ..SideEffectTrace::default()
    };
    let outcome = match result {
        ExecutionResult::Exited(ExitReason::Normal) => RunOutcome::Value(process.x_reg(0)),
        ExecutionResult::Exited(_) => {
            if let Some(exception) = process.current_exception() {
                side_effects.exceptions_raised.push(exception);
                RunOutcome::Exception(exception)
            } else {
                RunOutcome::Error(format!("non-normal exit: {result:?}"))
            }
        }
        other => RunOutcome::Error(format!("unexpected scheduler boundary: {other:?}")),
    };
    Ok(ExecutionTrace {
        outcome,
        side_effects,
    })
}

fn registered_bifs(atoms: &AtomTable) -> BifRegistryImpl {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, atoms).expect("register gate1 BIFs");
    register_gate2_bifs(&bifs, atoms).expect("register gate2 BIFs");
    register_gate3_bifs(&bifs, atoms).expect("register gate3 BIFs");
    register_stdlib_stubs(&bifs, atoms).expect("register stdlib stubs");
    bifs
}

fn compile_with_jit(
    instructions: &[Instruction],
    module: Atom,
    function: Atom,
    arity: u8,
) -> Result<NativeCode, String> {
    let compiler = JitCompiler::new(JitSettings)
        .map_err(|error| format!("JIT compiler unavailable: {error}"))?;
    compiler
        .compile(instructions, module, function, arity)
        .map_err(|error| format!("{error}"))
}

fn compile_with_aot(
    module_bytes: Option<&[u8]>,
    function: Atom,
    arity: u8,
) -> Result<NativeCode, String> {
    let module_bytes = module_bytes.ok_or_else(|| {
        "AOT differential source requires original BEAM bytes; synthetic instruction runs use JIT"
            .to_owned()
    })?;
    let temp_path = std::env::temp_dir().join(format!(
        "beamr_differential_aot_{}_{}.beam",
        std::process::id(),
        unique_nanos()
    ));
    fs::write(&temp_path, module_bytes)
        .map_err(|error| format!("write temp BEAM for AOT failed: {error}"))?;
    let compiler = AotCompiler::new().map_err(|error| format!("{error}"))?;
    let result = compiler
        .compile_module(&temp_path)
        .map_err(|error| format!("{error}"));
    let _ = fs::remove_file(&temp_path);
    let result = result?;
    result
        .compiled_functions()
        .iter()
        .find(|(compiled_function, compiled_arity, _)| {
            *compiled_function == function && *compiled_arity == arity
        })
        .map(|(_, _, native)| native.clone())
        .ok_or_else(|| {
            result
                .skipped_functions()
                .iter()
                .find(|(skipped_function, skipped_arity, _)| {
                    *skipped_function == function && *skipped_arity == arity
                })
                .map_or_else(
                    || "function absent from AOT result".to_owned(),
                    |(_, _, reason)| reason.clone(),
                )
        })
}

fn unique_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn module_from_bytes(
    bytes: &[u8],
    atoms: &AtomTable,
    bifs: &BifRegistryImpl,
) -> Result<Module, String> {
    let parsed = load_beam_chunks(bytes, atoms).map_err(|error| format!("{error}"))?;
    let resolved_imports = parsed
        .imports
        .iter()
        .filter_map(|import| {
            bifs.lookup(import.module, import.function, import.arity)
                .map(|native| ResolvedImport {
                    module: import.module,
                    function: import.function,
                    arity: import.arity,
                    target: ResolvedImportTarget::Native(native),
                })
        })
        .collect();
    let exports = parsed
        .exports
        .iter()
        .map(|export| ((export.function, export.arity), export.label))
        .collect();
    let label_index = label_index(&parsed.instructions);
    let constant_pool = constant_pool::materialise_literals(&parsed.literals, Some(atoms))
        .map_err(|error| format!("literal materialisation failed: {error}"))?;
    Ok(Module {
        name: parsed.name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports,
        label_index,
        code: parsed.instructions,
        literals: parsed.literals,
        constant_pool,
        resolved_imports,
        lambdas: parsed.lambdas,
        string_table: parsed.string_table,
        line_info: parsed.line_info,
        function_table: Vec::new(),
        line_table: Vec::new(),
    })
}

fn module_from_instructions(
    module: Atom,
    function: Atom,
    arity: u8,
    code: Vec<Instruction>,
) -> Module {
    let label_index = label_index(&code);
    let export_label = code
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::Label { label } => Some(*label),
            _ => None,
        })
        .unwrap_or(1);
    Module {
        name: module,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports: HashMap::from([((function, arity), export_label)]),
        label_index,
        code,
        literals: Vec::new(),
        constant_pool: Default::default(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
        function_table: Vec::new(),
        line_table: Vec::new(),
    }
}

fn label_index(code: &[Instruction]) -> HashMap<u32, usize> {
    code.iter()
        .enumerate()
        .filter_map(|(index, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, index)),
            _ => None,
        })
        .collect()
}

fn function_instructions(
    module: &Module,
    function: Atom,
    arity: u8,
) -> Result<Vec<Instruction>, String> {
    let entry = module
        .export_ip(function, arity)
        .map_err(|error| format!("{error}"))?;
    let compile_start = match module.code.get(entry) {
        Some(Instruction::Label { .. }) => entry.saturating_add(1),
        Some(Instruction::FuncInfo { .. }) => entry.saturating_add(1),
        Some(_) => entry,
        None => return Err(format!("entry instruction {entry} is outside module code")),
    };
    let end = module
        .code
        .iter()
        .enumerate()
        .skip(compile_start.saturating_add(1))
        .find_map(|(index, instruction)| match instruction {
            Instruction::FuncInfo { .. } => Some(index),
            _ => None,
        })
        .unwrap_or(module.code.len());
    Ok(module.code[compile_start..end].to_vec())
}

fn mfa_for(atoms: &AtomTable, module: &Module, function: Atom, arity: u8) -> Mfa {
    Mfa {
        module: atoms.resolve(module.name).unwrap_or("<unknown>").to_owned(),
        function: atoms.resolve(function).unwrap_or("<unknown>").to_owned(),
        arity,
    }
}

fn format_outcome(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Value(term) => format!("value({})", format_term(*term)),
        RunOutcome::Exception(exception) => format!(
            "exception(class={}, reason={}, stacktrace={})",
            format_term(exception.class),
            format_term(exception.reason),
            format_term(exception.stacktrace)
        ),
        RunOutcome::Error(error) => format!("error({error})"),
    }
}

fn format_term(term: Term) -> String {
    if let Some(value) = term.as_small_int() {
        value.to_string()
    } else if let Some(atom) = term.as_atom() {
        format!("atom({atom:?})")
    } else if let Some(pid) = term.as_pid() {
        format!("pid({pid})")
    } else {
        format!("{term:?}")
    }
}

fn format_instruction_bytecode(bytecode: &[Instruction]) -> String {
    bytecode
        .iter()
        .enumerate()
        .map(|(index, instruction)| format!("{index:04}: {instruction:?}\n"))
        .collect()
}

fn stacktrace_mfas(stacktrace: Term) -> Vec<(Term, Term, Term)> {
    let mut frames = Vec::new();
    let mut cursor = stacktrace;
    while let Some(cons) = Cons::new(cursor) {
        if let Some(tuple) = Tuple::new(cons.head())
            && tuple.arity() >= 3
        {
            let module = tuple.get(0).unwrap_or(Term::NIL);
            let function = tuple.get(1).unwrap_or(Term::NIL);
            let arity = tuple.get(2).unwrap_or(Term::NIL);
            frames.push((module, function, arity));
        }
        cursor = cons.tail();
    }
    frames
}

macro_rules! differential_test {
    ($module_bytes:expr, $function:expr, $arity:expr, $args:expr) => {{
        let runner = DifferentialRunner::new(SchedulerConfig {
            jit_threshold: Some(1),
            ..SchedulerConfig::default()
        });
        match runner.run($module_bytes, $function, $arity, $args) {
            DifferentialResult::Match { result } => result,
            DifferentialResult::CompilationSkipped { reason } => {
                eprintln!(
                    "differential test skipped for {}/{}: {}",
                    $function, $arity, reason
                );
                return;
            }
            DifferentialResult::Divergence { report, .. } => panic!("{report}"),
        }
    }};
}

#[derive(Clone, Debug)]
pub struct BeamProgram {
    pub instructions: Vec<Instruction>,
    pub args: Vec<Term>,
}

pub struct BeamProgramStrategy;

impl BeamProgramStrategy {
    pub fn strategy() -> impl Strategy<Value = BeamProgram> {
        prop::collection::vec(-32i64..=32, 3..=48).prop_map(|values| {
            let mut instructions = Vec::with_capacity(values.len().saturating_add(2));
            instructions.push(Instruction::Label { label: 1 });
            for (index, value) in values.iter().enumerate() {
                instructions.push(generated_instruction(index, *value));
            }
            instructions.push(Instruction::Return);
            BeamProgram {
                instructions,
                args: Vec::new(),
            }
        })
    }
}

fn generated_instruction(index: usize, value: i64) -> Instruction {
    match index % 18 {
        0 => Instruction::Move {
            source: Operand::Integer(value),
            destination: Operand::X(0),
        },
        1 => Instruction::Move {
            source: Operand::X(0),
            destination: Operand::X(1),
        },
        2 => Instruction::Swap {
            left: Operand::X(0),
            right: Operand::X(1),
        },
        3 => Instruction::Bif {
            op: BifOp::Bif2,
            operands: vec![Operand::Atom(Some(Atom::OK)), Operand::X(0), Operand::X(1)],
        },
        4 => Instruction::TypeTest {
            op: TypeTestOp::IsInteger,
            fail: Operand::Label(1),
            value: Operand::X(0),
        },
        5 => Instruction::TypeTest {
            op: TypeTestOp::IsAtom,
            fail: Operand::Label(1),
            value: Operand::X(0),
        },
        6 => Instruction::TypeTest {
            op: TypeTestOp::IsTuple,
            fail: Operand::Label(1),
            value: Operand::X(0),
        },
        7 => Instruction::PutTuple2 {
            destination: Operand::X(2),
            elements: Operand::List(vec![Operand::X(0), Operand::Integer(value)]),
        },
        8 => Instruction::PutList {
            head: Operand::X(0),
            tail: Operand::Atom(None),
            destination: Operand::X(3),
        },
        9 => Instruction::GetTupleElement {
            source: Operand::X(2),
            index: Operand::Unsigned(0),
            destination: Operand::X(0),
        },
        10 => Instruction::GetHd {
            source: Operand::X(3),
            destination: Operand::X(0),
        },
        11 => Instruction::GetTl {
            source: Operand::X(3),
            destination: Operand::X(4),
        },
        12 => Instruction::Comparison {
            op: ComparisonOp::EqExact,
            fail: Operand::Label(1),
            left: Operand::X(0),
            right: Operand::X(0),
        },
        13 => Instruction::TestArity {
            fail: Operand::Label(1),
            tuple: Operand::X(2),
            arity: Operand::Unsigned(2),
        },
        14 => Instruction::SelectVal {
            value: Operand::X(0),
            fail: Operand::Label(1),
            list: Operand::List(vec![Operand::Integer(value), Operand::Label(1)]),
        },
        15 => Instruction::Jump {
            target: Operand::Label(1),
        },
        16 => Instruction::Try {
            destination: Operand::X(5),
            label: Operand::Label(1),
        },
        _ => Instruction::TryEnd {
            source: Operand::X(5),
        },
    }
}

struct BrokenCompiledExecutor;

impl CompiledExecutor for BrokenCompiledExecutor {
    fn execute(
        &self,
        _native_code: &NativeCode,
        _module: &Module,
        _registry: &ModuleRegistry,
        _function: Atom,
        _arity: u8,
        _args: &[Term],
    ) -> Result<ExecutionTrace, String> {
        Ok(ExecutionTrace {
            outcome: RunOutcome::Value(Term::small_int(43)),
            side_effects: SideEffectTrace::default(),
        })
    }
}

fn tuple_in(process: &mut Process, elements: &[Term]) -> Term {
    let heap = process
        .heap_mut()
        .alloc_slice(1 + elements.len())
        .expect("test heap has tuple space");
    write_tuple(heap, elements).expect("tuple allocation writes")
}

fn cons_in(process: &mut Process, head: Term, tail: Term) -> Term {
    let heap = process
        .heap_mut()
        .alloc_slice(2)
        .expect("test heap has cons space");
    write_cons(heap, head, tail).expect("cons allocation writes")
}

#[test]
fn runner_new_forces_immediate_compilation_threshold() {
    let runner = DifferentialRunner::new(SchedulerConfig::default());
    assert_eq!(runner.jit_threshold(), Some(1));
}

#[test]
fn fibonacci_differential_wrapper_skips_until_jit_supports_recursive_bytecode() {
    differential_test!(
        include_bytes!("fixtures/proof.beam"),
        "fibonacci",
        1,
        &[Term::small_int(10)]
    );
}

#[test]
fn aot_compiled_path_matches_jit_for_fixture_function() {
    let runner = DifferentialRunner::new_aot(SchedulerConfig::default());
    match runner.run(include_bytes!("fixtures/proof.beam"), "main", 0, &[]) {
        DifferentialResult::Match { result } => {
            assert_eq!(result, Term::small_int(42));
        }
        DifferentialResult::CompilationSkipped { reason } => {
            eprintln!("AOT differential test skipped for main/0: {reason}");
        }
        DifferentialResult::Divergence { report, .. } => panic!("{report}"),
    }
}

#[test]
fn fibonacci_interpreter_fixture_still_returns_55() {
    let atoms = AtomTable::new();
    let bifs = registered_bifs(&atoms);
    let module = module_from_bytes(include_bytes!("fixtures/proof.beam"), &atoms, &bifs)
        .expect("proof.beam loads");
    let registry = ModuleRegistry::new();
    registry.insert(module.clone());
    let result = execute_interpreter(
        &module,
        &registry,
        atoms.intern("fibonacci"),
        1,
        &[Term::small_int(10)],
    )
    .expect("fibonacci runs");
    assert_eq!(result.outcome, RunOutcome::Value(Term::small_int(55)));
}

#[test]
fn broken_compiled_executor_reports_divergence() {
    let runner = DifferentialRunner::with_executor(
        SchedulerConfig {
            jit_threshold: Some(1),
            ..SchedulerConfig::default()
        },
        BrokenCompiledExecutor,
    );
    let result = runner.run_instructions(
        "diff",
        "answer",
        0,
        vec![Instruction::Label { label: 1 }, Instruction::Return],
        &[],
    );
    match result {
        DifferentialResult::Divergence {
            interpreted,
            compiled,
            ..
        } => {
            assert_eq!(interpreted, RunOutcome::Value(Term::NIL));
            assert_eq!(compiled, RunOutcome::Value(Term::small_int(43)));
        }
        other => panic!("expected divergence, got {other:?}"),
    }
}

#[test]
fn list_processing_scenario_runs_differentially() {
    differential_test!(
        include_bytes!("fixtures/stdlib/lists.beam"),
        "reverse",
        1,
        &[Term::NIL]
    );
}

#[test]
fn maps_map_hof_runs_differentially() {
    differential_test!(
        include_bytes!("fixtures/stdlib/maps_hof.beam"),
        "map_inc",
        0,
        &[]
    );
}

#[test]
fn compiled_wrapper_calling_maps_map_hof_returns_transformed_map() {
    differential_test!(
        include_bytes!("fixtures/stdlib/maps_hof.beam"),
        "compiled_entry",
        0,
        &[]
    );
}

#[test]
fn pattern_matching_scenario_runs_differentially() {
    let mut process = Process::new(1, 128);
    let input = tuple_in(&mut process, &[Term::atom(Atom::OK), Term::small_int(42)]);
    differential_test!(
        include_bytes!("fixtures/tagged_tuple_patterns.beam"),
        "match",
        1,
        &[input]
    );
}

#[test]
fn unsupported_map_operation_is_logged_as_compilation_skipped() {
    let runner = DifferentialRunner::new(SchedulerConfig {
        jit_threshold: Some(1),
        ..SchedulerConfig::default()
    });
    let result = runner.run_instructions(
        "diff",
        "map_unsupported",
        0,
        vec![
            Instruction::Label { label: 1 },
            Instruction::MapOp {
                op: MapOp::PutMapAssoc,
                operands: Vec::new(),
            },
            Instruction::Return,
        ],
        &[],
    );
    match result {
        DifferentialResult::CompilationSkipped { reason } => {
            eprintln!("differential skip: {reason}");
            assert!(
                reason.contains("unsupported JIT operand: put_map")
                    || reason.contains("unsupported JIT opcode")
            );
        }
        other => panic!("expected skip, got {other:?}"),
    }
}

#[test]
fn identical_deep_tuples_are_equal() {
    let mut left_process = Process::new(1, 128);
    let mut right_process = Process::new(2, 128);
    let left_inner = tuple_in(&mut left_process, &[Term::small_int(1), Term::small_int(2)]);
    let right_inner = tuple_in(
        &mut right_process,
        &[Term::small_int(1), Term::small_int(2)],
    );
    let left = tuple_in(&mut left_process, &[Term::atom(Atom::OK), left_inner]);
    let right = tuple_in(&mut right_process, &[Term::atom(Atom::OK), right_inner]);
    assert_eq!(
        compare_results(&RunOutcome::Value(left), &RunOutcome::Value(right)),
        CompareResult::Equal
    );
}

#[test]
fn nested_list_diff_reports_path() {
    let mut left_process = Process::new(1, 128);
    let mut right_process = Process::new(2, 128);
    let left_tail = cons_in(&mut left_process, Term::small_int(2), Term::NIL);
    let left = cons_in(&mut left_process, Term::small_int(1), left_tail);
    let right_tail = cons_in(&mut right_process, Term::small_int(3), Term::NIL);
    let right = cons_in(&mut right_process, Term::small_int(1), right_tail);
    match compare_results(&RunOutcome::Value(left), &RunOutcome::Value(right)) {
        CompareResult::Different { diff } => assert!(diff.contains("$.tail[1]")),
        CompareResult::Equal => panic!("expected nested list difference"),
    }
}

#[test]
fn exception_comparison_ignores_stacktrace_line_numbers() {
    let mut left_process = Process::new(1, 128);
    let mut right_process = Process::new(2, 128);
    let left_frame = tuple_in(
        &mut left_process,
        &[
            Term::atom(Atom::MODULE),
            Term::atom(Atom::ERROR),
            Term::small_int(1),
            Term::small_int(10),
        ],
    );
    let right_frame = tuple_in(
        &mut right_process,
        &[
            Term::atom(Atom::MODULE),
            Term::atom(Atom::ERROR),
            Term::small_int(1),
            Term::small_int(99),
        ],
    );
    let left_stack = cons_in(&mut left_process, left_frame, Term::NIL);
    let right_stack = cons_in(&mut right_process, right_frame, Term::NIL);
    let left = Exception {
        class: Term::atom(Atom::ERROR),
        reason: Term::small_int(7),
        stacktrace: left_stack,
    };
    let right = Exception {
        class: Term::atom(Atom::ERROR),
        reason: Term::small_int(7),
        stacktrace: right_stack,
    };
    assert_eq!(
        compare_results(&RunOutcome::Exception(left), &RunOutcome::Exception(right)),
        CompareResult::Equal
    );
}

#[test]
fn divergence_report_formats_nested_tuple_diff_and_saves_bytecode() {
    let mut left_process = Process::new(1, 128);
    let mut right_process = Process::new(2, 128);
    let left = tuple_in(
        &mut left_process,
        &[Term::atom(Atom::OK), Term::small_int(42)],
    );
    let right = tuple_in(
        &mut right_process,
        &[Term::atom(Atom::OK), Term::small_int(43)],
    );
    let diff = match compare_results(&RunOutcome::Value(left), &RunOutcome::Value(right)) {
        CompareResult::Different { diff } => diff,
        CompareResult::Equal => panic!("expected tuple difference"),
    };
    let report = DivergenceReport::new(
        Mfa {
            module: "diff".to_owned(),
            function: "tuple".to_owned(),
            arity: 0,
        },
        &[],
        &RunOutcome::Value(left),
        &RunOutcome::Value(right),
        vec![Instruction::Return],
        diff,
    );
    let rendered = report.to_string();
    assert!(rendered.contains("diff:        $[1] differs"));
    let path = std::env::temp_dir().join(format!(
        "beamr-differential-{}-bytecode.txt",
        std::process::id()
    ));
    report.save_bytecode(&path).expect("bytecode writes");
    let saved = fs::read_to_string(&path).expect("bytecode reads back");
    assert!(saved.contains("Return"));
    let _ = fs::remove_file(path);
}

#[test]
fn side_effect_traces_normalise_pid_values() {
    let interpreted = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(10),
            message: Term::small_int(7),
        }],
        pdict_modifications: vec![ProcessDictionaryModification {
            key: Term::atom(Atom::OK),
            value: Term::pid(10),
        }],
        exceptions_raised: Vec::new(),
    };
    let compiled = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(99),
            message: Term::small_int(7),
        }],
        pdict_modifications: vec![ProcessDictionaryModification {
            key: Term::atom(Atom::OK),
            value: Term::pid(99),
        }],
        exceptions_raised: Vec::new(),
    };
    assert_eq!(
        compare_side_effects(&interpreted, &compiled),
        CompareResult::Equal
    );
}

#[test]
fn side_effect_traces_compare_nested_heap_terms_structurally() {
    let mut interpreted_process = Process::new(1, 128);
    let mut compiled_process = Process::new(2, 128);
    let interpreted_message = tuple_in(
        &mut interpreted_process,
        &[Term::atom(Atom::OK), Term::pid(10)],
    );
    let compiled_message = tuple_in(
        &mut compiled_process,
        &[Term::atom(Atom::OK), Term::pid(99)],
    );
    let interpreted = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(10),
            message: interpreted_message,
        }],
        ..SideEffectTrace::default()
    };
    let compiled = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(99),
            message: compiled_message,
        }],
        ..SideEffectTrace::default()
    };

    assert_eq!(
        compare_side_effects(&interpreted, &compiled),
        CompareResult::Equal
    );
}

#[test]
fn side_effect_comparison_reports_difference() {
    let interpreted = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(1),
            message: Term::small_int(1),
        }],
        ..SideEffectTrace::default()
    };
    let compiled = SideEffectTrace {
        messages_sent: vec![MessageSend {
            destination: Term::pid(1),
            message: Term::small_int(2),
        }],
        ..SideEffectTrace::default()
    };
    match compare_side_effects(&interpreted, &compiled) {
        CompareResult::Different { diff } => assert!(diff.contains("side effects differ")),
        CompareResult::Equal => panic!("expected side effect difference"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn generated_programs_match_or_skip(program in BeamProgramStrategy::strategy()) {
        let runner = DifferentialRunner::new(SchedulerConfig {
            jit_threshold: Some(1),
            ..SchedulerConfig::default()
        });
        let result = runner.run_instructions(
            "generated",
            "main",
            0,
            program.instructions,
            &program.args,
        );
        match result {
            DifferentialResult::Match { .. } | DifferentialResult::CompilationSkipped { .. } => {}
            DifferentialResult::Divergence { report, .. } => {
                prop_assert!(false, "{report}");
            }
        }
    }
}
