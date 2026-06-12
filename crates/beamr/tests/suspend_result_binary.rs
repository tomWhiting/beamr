//! Regression tests for binaries delivered through the suspension protocol.
//!
//! Two delivery shapes are covered, both with payloads above and below the
//! 64-byte heap-binary/ProcBin threshold:
//!
//! 1. The aion engine shape: a workflow process calls a native that parks
//!    via the message-wakeable `request_suspend`, the embedder wakes it
//!    with a mailbox marker, the native re-executes and returns
//!    `{ok, Binary}` allocated on the process heap through
//!    `ProcessContext::alloc_binary`, and the resumed BEAM code then *uses*
//!    the binary (byte_size, binary pattern match, tail sub-binary).
//!
//! 2. The host-API shape: a native parks via the gated
//!    `request_await_suspend` and the embedder publishes `{ok, Binary}`
//!    with `Scheduler::wake_with_result`, building the term on scratch
//!    storage that is freed immediately after publishing — the published
//!    result must own its bytes across the publish-to-apply window.

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
use beamr::term::boxed::{Tuple, write_tuple};
use beamr::term::shared_binary::{alloc_binary, alloc_binary_word_count};

/// Native run counter keyed by payload size: each test uses a distinct
/// size, and pids collide across the schedulers of concurrently running
/// tests.
fn runs() -> &'static Mutex<HashMap<usize, usize>> {
    static RUNS: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_count(size: usize) -> usize {
    runs()
        .lock()
        .expect("runs lock")
        .get(&size)
        .copied()
        .unwrap_or(0)
}

fn record_run(size: usize) -> usize {
    let mut runs = runs().lock().expect("runs lock");
    let entry = runs.entry(size).or_insert(0);
    *entry += 1;
    *entry
}

fn payload_size_arg(args: &[Term]) -> Result<usize, Term> {
    args.first()
        .and_then(|term| term.as_small_int())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| Term::atom(Atom::BADARG))
}

fn payload_bytes(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

/// `beamr_suspend_binary_test:await_payload/1`, re-execution flavor: the
/// first execution parks the process message-wakeably; the post-wake
/// re-execution builds `{ok, <<Size bytes>>}` on the calling process heap
/// and returns it.
fn await_payload(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let size = payload_size_arg(args)?;
    if record_run(size) == 1 {
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    let bytes = payload_bytes(size);
    let payload = context.alloc_binary(&bytes)?;
    context.alloc_tuple(&[Term::atom(Atom::OK), payload])
}

/// `beamr_suspend_binary_test:await_payload/1`, gated flavor: parks under a
/// result-gated host await; the embedder publishes the `{ok, Binary}`
/// result with `Scheduler::wake_with_result`, so the native never re-runs.
fn gated_await_payload(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let size = payload_size_arg(args)?;
    if record_run(size) == 1 {
        context.request_await_suspend(None);
        return Ok(Term::NIL);
    }
    // wake_with_result applies the published result directly into x0; a
    // second execution means the await was wrongly re-executed.
    Err(Term::atom(Atom::BADARG))
}

fn run_fixture(
    payload_size: i64,
    native: NativeFn,
    wake: impl FnOnce(&Scheduler, u64),
) -> (ExitReason, beamr::ets::OwnedTerm, Option<String>) {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let bif_registry = Arc::new(BifRegistryImpl::new());
    register_gate1_bifs(bif_registry.as_ref(), &atom_table).expect("gate1 bifs register");
    register_gate2_bifs(bif_registry.as_ref(), &atom_table).expect("gate2 bifs register");
    register_gate3_bifs(bif_registry.as_ref(), &atom_table).expect("gate3 bifs register");
    bif_registry
        .register(
            atom_table.intern("beamr_suspend_binary_test"),
            atom_table.intern("await_payload"),
            1,
            native,
            Capability::Pure,
        )
        .expect("native registers");

    let module_registry = Arc::new(ModuleRegistry::new());
    let bytes = include_bytes!("fixtures/suspend_binary_fixture.beam");
    let (_module, unresolved) =
        load_module(bytes, &atom_table, &module_registry, bif_registry.as_ref())
            .expect("fixture loads");
    assert!(
        unresolved.imports().is_empty(),
        "fixture has unresolved imports: {:?}",
        unresolved.imports()
    );

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
            atom_table.intern("suspend_binary_fixture"),
            atom_table.intern("run"),
            vec![Term::small_int(payload_size)],
        )
        .expect("spawn suspend_binary_fixture:run/1");

    // Wait until the native's first execution has requested suspension,
    // then deliver the wake.
    let size = usize::try_from(payload_size).expect("payload size fits usize");
    let started = std::time::Instant::now();
    while run_count(size) < 1 {
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
    (reason, result, exit_error)
}

fn assert_result_shape(
    size: i64,
    reason: ExitReason,
    owned_result: &beamr::ets::OwnedTerm,
    exit_error: Option<&str>,
) {
    let result = owned_result.root();
    assert_eq!(
        reason,
        ExitReason::Normal,
        "size {size}: process died (exit_error: {exit_error:?})"
    );
    let tuple = Tuple::new(result)
        .unwrap_or_else(|| panic!("size {size}: result is not a tuple: {result:?}"));
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)), "size {size}");
    assert_eq!(tuple.get(1), Some(Term::small_int(size)), "size {size}");
    assert_eq!(tuple.get(2), Some(Term::small_int(0)), "size {size}");
    assert_eq!(tuple.get(3), Some(Term::small_int(size - 1)), "size {size}");
}

/// The aion shape: a plain mailbox marker wakes the re-entrant suspend and
/// the native builds the binary result on the process heap.
fn assert_reexecution_round_trip(size: i64) {
    let (reason, owned_result, exit_error) = run_fixture(size, await_payload, |scheduler, pid| {
        assert!(
            scheduler.enqueue_atom_message(pid, Atom::OK),
            "marker delivery failed"
        );
    });
    assert_result_shape(size, reason, &owned_result, exit_error.as_deref());
}

/// The host-API shape: the embedder publishes `{ok, Binary}` built on
/// scratch storage through `wake_with_result` and frees the scratch
/// immediately — the published result must own its bytes.
fn assert_host_publish_round_trip(size: i64) {
    let (reason, owned_result, exit_error) =
        run_fixture(size, gated_await_payload, |scheduler, pid| {
            let bytes = payload_bytes(usize::try_from(size).expect("payload size fits usize"));
            let binary_words = alloc_binary_word_count(bytes.len());
            let mut scratch = vec![0_u64; binary_words + 3];
            let (binary_heap, tuple_heap) = scratch.split_at_mut(binary_words);
            let binary = alloc_binary(binary_heap, &bytes).expect("scratch binary fits");
            let result =
                write_tuple(tuple_heap, &[Term::atom(Atom::OK), binary]).expect("scratch tuple");
            // The mirror is registered by request_await_suspend before the
            // native returns, so publishing may race only the spawn — retry
            // until the suspension is publishable.
            let started = std::time::Instant::now();
            while !scheduler.wake_with_result(pid, result) {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(10),
                    "result was never published"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            // Free and poison the scratch storage while the process may not
            // have resumed yet: the published result must not point here.
            scratch.fill(0xFFFF_FFFF_FFFF_FFFF);
            drop(scratch);
        });
    assert_result_shape(size, reason, &owned_result, exit_error.as_deref());
}

#[test]
fn suspension_result_heap_binary_survives_resume() {
    // At and below the 64-byte threshold the payload is an inline heap
    // binary; this guards the baseline the refc tests compare against.
    assert_reexecution_round_trip(64);
}

#[test]
fn suspension_result_refc_binary_survives_resume() {
    // 65 bytes crosses the heap-binary/ProcBin boundary.
    assert_reexecution_round_trip(65);
}

#[test]
fn suspension_result_large_refc_binary_survives_resume() {
    // Arbitrarily large payloads must round-trip too.
    assert_reexecution_round_trip(64 * 1024);
}

#[test]
fn host_published_refc_binary_result_is_owned_across_the_publish_window() {
    assert_host_publish_round_trip(66);
}

#[test]
fn host_published_large_binary_result_is_owned_across_the_publish_window() {
    assert_host_publish_round_trip(32 * 1024);
}
