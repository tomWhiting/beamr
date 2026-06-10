//! NIF private data round-trip tests.
//!
//! `SchedulerConfig::nif_private_data` is the ERTS `enif_priv_data`
//! equivalent: one opaque embedder value per scheduler instance, recoverable
//! from every native call via `ProcessContext::nif_private_data`. The
//! load-bearing property is isolation — two runtimes in one OS process must
//! each hand their own value to their own natives, on both the clean and the
//! dirty native paths, so embedders never need process-wide globals.

use std::collections::HashMap;
use std::sync::Arc;

use beamr::atom::Atom;
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry, ResolvedImport, ResolvedImportTarget};
use beamr::native::{Capability, NativeEntry, ProcessContext};
use beamr::process::ExitReason;
use beamr::scheduler::dirty::DirtySchedulerKind;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

struct EnginePrivateData {
    value: i64,
}

fn read_private_value(_args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let value = context
        .nif_private_data()
        .and_then(|data| data.downcast_ref::<EnginePrivateData>())
        .map_or(-1, |data| data.value);
    Ok(Term::small_int(value))
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

fn call_native_module(name: Atom, dirty_kind: Option<DirtySchedulerKind>) -> Module {
    let mut m = module(
        name,
        vec![
            Instruction::CallExt {
                arity: Operand::Unsigned(0),
                import: Operand::Unsigned(0),
            },
            Instruction::Return,
        ],
    );
    m.resolved_imports.push(ResolvedImport {
        module: Atom::OK,
        function: Atom::OK,
        arity: 0,
        target: ResolvedImportTarget::Native(NativeEntry {
            function: read_private_value,
            dirty_kind,
            capability: Capability::Pure,
        }),
    });
    m
}

fn scheduler_with_private_value(registry: &Arc<ModuleRegistry>, value: i64) -> Scheduler {
    Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            dirty_cpu_threads: Some(1),
            dirty_io_threads: Some(1),
            dirty_queue_depth: Some(8),
            nif_private_data: Some(Arc::new(EnginePrivateData { value })),
            ..SchedulerConfig::default()
        },
        Arc::clone(registry),
    )
    .expect("scheduler starts")
}

fn run_and_take_result(scheduler: &Scheduler, module: &Arc<Module>) -> Term {
    let pid = scheduler.spawn_process(module);
    let (reason, result) = scheduler.run_until_exit(pid);
    assert_eq!(reason, ExitReason::Normal);
    result.root()
}

#[test]
fn clean_natives_see_their_own_schedulers_private_data() {
    let registry = Arc::new(ModuleRegistry::new());
    let clean_module = registry.insert(call_native_module(Atom::OK, None));

    let first = scheduler_with_private_value(&registry, 11);
    let second = scheduler_with_private_value(&registry, 23);

    // Both schedulers are alive at once; each native call must recover the
    // value installed on its own scheduler, never the other's.
    assert_eq!(
        run_and_take_result(&first, &clean_module),
        Term::small_int(11)
    );
    assert_eq!(
        run_and_take_result(&second, &clean_module),
        Term::small_int(23)
    );
    assert_eq!(
        run_and_take_result(&first, &clean_module),
        Term::small_int(11)
    );

    first.shutdown();
    second.shutdown();
}

#[test]
fn dirty_natives_see_their_own_schedulers_private_data() {
    let registry = Arc::new(ModuleRegistry::new());
    let dirty_module = registry.insert(call_native_module(
        Atom::ERROR,
        Some(DirtySchedulerKind::Cpu),
    ));

    let first = scheduler_with_private_value(&registry, 31);
    let second = scheduler_with_private_value(&registry, 47);

    assert_eq!(
        run_and_take_result(&first, &dirty_module),
        Term::small_int(31)
    );
    assert_eq!(
        run_and_take_result(&second, &dirty_module),
        Term::small_int(47)
    );

    first.shutdown();
    second.shutdown();
}

#[test]
fn natives_without_configured_private_data_see_none() {
    let registry = Arc::new(ModuleRegistry::new());
    let clean_module = registry.insert(call_native_module(Atom::OK, None));

    let scheduler = Scheduler::new(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&registry),
    )
    .expect("scheduler starts");

    assert_eq!(
        run_and_take_result(&scheduler, &clean_module),
        Term::small_int(-1)
    );

    scheduler.shutdown();
}
