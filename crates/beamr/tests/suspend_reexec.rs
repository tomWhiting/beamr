//! Regression tests for suspension at tail-call (`call_ext_last` /
//! `call_ext_only`) native call sites.
//!
//! The aion Gleam SDK shape that broke: a closure like
//! `fn() { ffi.sleep(duration.to_milliseconds(d)) }` — the suspending
//! native's argument is computed by a CROSS-MODULE call inside the
//! re-executed expression, so the closure body compiles to
//! `allocate; call_ext to_ms; call_ext_last sleep, 0`. The eager y-frame
//! pop in `call_ext_last` ran again on wake re-execution, double-popping
//! the stack: the native's eventual return landed back at the caller's
//! `call_fun` site with the result in x0, which was then CALLED as a
//! function (`bad function term {ok, <<"fired">>}`).
//!
//! Precomputed-argument closures (`call_ext_only`, no frame) were safe and
//! guard the baseline here. Host-published completions
//! (`wake_with_result`) at tail-call park sites must continue by RETURNING
//! to the caller instead of advancing past the function's last
//! instruction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use beamr::atom::{Atom, AtomTable};
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::bifs::register_gate1_bifs;
use beamr::native::gate3_bifs::register_gate3_bifs;
use beamr::native::process_bifs::register_gate2_bifs;
use beamr::native::{BifRegistryImpl, Capability, NativeFn, ProcessContext};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::term::binary::{packed_word_count, write_binary};
use beamr::term::boxed::{Tuple, write_tuple};

const FIRED: &[u8] = b"fired";

/// Native run counter keyed by the sleep argument (each test passes a
/// distinct X, so the helper's X*1000 is unique per test while pids
/// collide across concurrently running schedulers).
fn runs() -> &'static Mutex<HashMap<i64, usize>> {
    static RUNS: OnceLock<Mutex<HashMap<i64, usize>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_count(ms: i64) -> usize {
    runs()
        .lock()
        .expect("runs lock")
        .get(&ms)
        .copied()
        .unwrap_or(0)
}

fn record_run(args: &[Term]) -> Result<(i64, usize), Term> {
    let ms = args
        .first()
        .and_then(|term| term.as_small_int())
        .ok_or_else(|| Term::atom(Atom::BADARG))?;
    let mut runs = runs().lock().expect("runs lock");
    let entry = runs.entry(ms).or_insert(0);
    *entry += 1;
    Ok((ms, *entry))
}

/// `beamr_suspend_reexec_test:sleep/1`, re-execution flavor: parks
/// message-wakeably; the post-wake re-execution returns `{ok, <<"fired">>}`.
fn reexec_sleep(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (_ms, run) = record_run(args)?;
    if run == 1 {
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    let fired = context.alloc_binary(FIRED)?;
    context.alloc_tuple(&[Term::atom(Atom::OK), fired])
}

/// `beamr_suspend_reexec_test:sleep/1`, gated flavor: parks under a
/// result-gated host await; the embedder publishes `{ok, <<"fired">>}`
/// with `Scheduler::wake_with_result`, so the native must never re-run.
fn gated_sleep(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let (_ms, run) = record_run(args)?;
    if run == 1 {
        context.request_await_suspend(None);
        return Ok(Term::NIL);
    }
    Err(Term::atom(Atom::BADARG))
}

type FixtureOutcome = (
    ExitReason,
    beamr::ets::OwnedTerm,
    Option<String>,
    Arc<AtomTable>,
);

fn run_fixture(
    entry_function: &str,
    x: i64,
    native: NativeFn,
    wake: impl FnOnce(&Scheduler, u64),
) -> FixtureOutcome {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let bif_registry = Arc::new(BifRegistryImpl::new());
    register_gate1_bifs(bif_registry.as_ref(), &atom_table).expect("gate1 bifs register");
    register_gate2_bifs(bif_registry.as_ref(), &atom_table).expect("gate2 bifs register");
    register_gate3_bifs(bif_registry.as_ref(), &atom_table).expect("gate3 bifs register");
    bif_registry
        .register(
            atom_table.intern("beamr_suspend_reexec_test"),
            atom_table.intern("sleep"),
            1,
            native,
            Capability::Pure,
        )
        .expect("native registers");

    let module_registry = Arc::new(ModuleRegistry::new());
    for bytes in [
        include_bytes!("fixtures/suspend_reexec_helper.beam").as_slice(),
        include_bytes!("fixtures/suspend_reexec_fixture.beam").as_slice(),
    ] {
        let (_module, unresolved) =
            load_module(bytes, &atom_table, &module_registry, bif_registry.as_ref())
                .expect("fixture loads");
        assert!(
            unresolved.imports().is_empty(),
            "fixture has unresolved imports: {:?}",
            unresolved.imports()
        );
    }

    let scheduler = Scheduler::with_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        Arc::clone(&module_registry),
        Arc::clone(&atom_table),
        Arc::clone(&bif_registry),
    )
    .expect("scheduler starts");

    let pid = scheduler
        .spawn(
            atom_table.intern("suspend_reexec_fixture"),
            atom_table.intern(entry_function),
            vec![Term::small_int(x)],
        )
        .unwrap_or_else(|error| panic!("spawn suspend_reexec_fixture:{entry_function}/1: {error}"));

    // Wait until the native's first execution has requested suspension,
    // then deliver the wake.
    let ms = x * 1000;
    let started = std::time::Instant::now();
    while run_count(ms) < 1 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "native was never executed"
        );
        std::thread::yield_now();
    }
    wake(&scheduler, pid);

    let (reason, result) = scheduler.run_until_exit(pid);
    let exit_error = scheduler
        .take_exit_error(pid)
        .map(|error| error.to_string());
    scheduler.shutdown();
    (reason, result, exit_error, atom_table)
}

fn marker_wake(scheduler: &Scheduler, pid: u64) {
    assert!(
        scheduler.enqueue_atom_message(pid, Atom::OK),
        "marker delivery failed"
    );
}

fn host_result_wake(scheduler: &Scheduler, pid: u64) {
    // Build `{ok, <<"fired">>}` on scratch storage; the publish deep-copies.
    let binary_words = 2 + packed_word_count(FIRED.len());
    let mut scratch = vec![0_u64; binary_words + 3];
    let (binary_heap, tuple_heap) = scratch.split_at_mut(binary_words);
    let fired = write_binary(binary_heap, FIRED).expect("scratch binary fits");
    let result = write_tuple(tuple_heap, &[Term::atom(Atom::OK), fired]).expect("scratch tuple");
    let started = std::time::Instant::now();
    while !scheduler.wake_with_result(pid, result) {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "result was never published"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn assert_done(label: &str, outcome: &FixtureOutcome) {
    let (reason, owned_result, exit_error, atom_table) = outcome;
    let result = owned_result.root();
    assert_eq!(
        *reason,
        ExitReason::Normal,
        "{label}: process died (exit_error: {exit_error:?})"
    );
    let tuple =
        Tuple::new(result).unwrap_or_else(|| panic!("{label}: result is not a tuple: {result:?}"));
    assert_eq!(
        tuple.get(0),
        Some(Term::atom(atom_table.intern("done"))),
        "{label}"
    );
    assert_eq!(
        tuple.get(1),
        Some(Term::small_int(FIRED.len() as i64)),
        "{label}"
    );
}

#[test]
fn cross_module_argument_tail_call_survives_wake_reexecution() {
    // `fun() -> sleep(helper:to_ms(X)) end` parks at `call_ext_last`; the
    // wake must re-execute the call without double-popping the stack.
    let outcome = run_fixture("run", 1, reexec_sleep, marker_wake);
    assert_done("call_ext_last re-execution", &outcome);
}

#[test]
fn precomputed_argument_tail_call_survives_wake_reexecution() {
    // The aion SDK mitigation shape (`call_ext_only`, no stack frame)
    // guards the baseline.
    let outcome = run_fixture("run_precomputed", 2, reexec_sleep, marker_wake);
    assert_done("call_ext_only re-execution", &outcome);
}

#[test]
fn host_result_at_a_call_ext_last_park_returns_to_the_caller() {
    // A completion applied at a tail-call park must pop the parked frame
    // and RETURN to the caller — advancing past the function's last
    // instruction falls off the end of the module.
    let outcome = run_fixture("run", 3, gated_sleep, host_result_wake);
    assert_done("call_ext_last host result", &outcome);
}

#[test]
fn host_result_at_a_call_ext_only_park_returns_to_the_caller() {
    let outcome = run_fixture("run_precomputed", 4, gated_sleep, host_result_wake);
    assert_done("call_ext_only host result", &outcome);
}
