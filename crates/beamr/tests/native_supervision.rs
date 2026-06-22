//! End-to-end supervision tests for native processes (NATIVE-002).
//!
//! Every test boots a REAL multi-threaded scheduler and drives native processes
//! through the SAME pid-keyed supervision fabric as bytecode processes — there
//! is no native-specific link/monitor/exit path. Coverage: R1 links across
//! native<->BEAM (`propagate_exit`), R2 monitor DOWN (`deliver_down_messages`),
//! R3 `NativeOutcome::Stop` -> `cleanup_exited_process`, R4 trap_exit EXIT
//! receipt (drained by the shared store-back), R5 the handler factory, and R6
//! supervisor restart of a crashed native child via that factory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use beamr::atom::{Atom, AtomTable};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::native::native_process::{
    NativeContext, NativeHandler, NativeHandlerFactory, NativeOutcome,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::term::boxed::Tuple;

// ── Test harness ────────────────────────────────────────────────────────────

fn label_index(code: &[Instruction]) -> HashMap<u32, usize> {
    code.iter()
        .enumerate()
        .filter_map(|(ip, instruction)| match instruction {
            Instruction::Label { label } => Some((*label, ip)),
            _ => None,
        })
        .collect()
}

fn module(name: Atom, exports: HashMap<(Atom, u8), u32>, code: Vec<Instruction>) -> Module {
    let label_index = label_index(&code);
    Module {
        name,
        generation: 0,
        origin: ModuleOrigin::Preloaded,
        exports,
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

/// `loop/0`: `receive Msg -> Msg end` — parks until any message arrives, then
/// returns it in x(0) and exits Normal. Used both as a live BEAM partner and as
/// a watcher whose first delivered message is asserted on.
fn collector_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("native_sup_collector");
    let function = atoms.intern("loop");
    let mut exports = HashMap::new();
    exports.insert((function, 0), 1);
    module(
        name,
        exports,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Label { label: 10 },
            Instruction::LoopRec {
                fail: Operand::Label(20),
                destination: Operand::X(0),
            },
            Instruction::RemoveMessage,
            Instruction::Return,
            Instruction::Label { label: 20 },
            Instruction::Wait {
                fail: Operand::Label(10),
            },
        ],
    )
}

fn scheduler_with(atoms: &Arc<AtomTable>, registry: Arc<ModuleRegistry>) -> Arc<Scheduler> {
    Arc::new(
        Scheduler::with_code_server(
            SchedulerConfig::default(),
            registry,
            Arc::clone(atoms),
            Arc::new(BifRegistryImpl::new()),
        )
        .unwrap_or_else(|error| panic!("scheduler starts: {error}")),
    )
}

fn spawn_collector(scheduler: &Arc<Scheduler>, atoms: &AtomTable) -> u64 {
    let collector_mod = atoms.intern("native_sup_collector");
    let loop_fn = atoms.intern("loop");
    let pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    // Let it reach its receive and park.
    std::thread::sleep(Duration::from_millis(50));
    pid
}

fn run_until_exit_bounded(
    scheduler: &Arc<Scheduler>,
    pid: u64,
) -> (ExitReason, beamr::ets::OwnedTerm) {
    let (sender, completion) = std::sync::mpsc::channel();
    let scheduler_for_wait = Arc::clone(scheduler);
    std::thread::spawn(move || {
        let _ = sender.send(scheduler_for_wait.run_until_exit(pid));
    });
    // Return the OWNED term, not `result.root()`: a boxed exit value (e.g. a
    // DOWN tuple) lives in the `OwnedTerm`'s storage, so handing back a bare
    // `Term` would dangle once the local `OwnedTerm` drops. Callers that need
    // the term call `.root()` while this value is still alive.
    completion
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("process {pid} never exited"))
}

/// Poll `predicate` until it yields `Some`, or panic after `timeout`.
fn poll_until<T>(timeout: Duration, mut predicate: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = predicate() {
            return value;
        }
        assert!(Instant::now() < deadline, "condition never became true");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn still_alive(scheduler: &Arc<Scheduler>, pid: u64) -> bool {
    scheduler.process_table().get(pid).is_some()
}

// ── Native handlers & helpers ───────────────────────────────────────────────

/// A worker that, per message: stops abnormally on `crash`, stops normally on
/// `stop`, and otherwise echoes the message to `reply_to` (proving liveness).
struct Worker {
    reply_to: Option<u64>,
    crash: Atom,
    stop: Atom,
}

impl NativeHandler for Worker {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        while let Some(message) = ctx.recv() {
            if message == Term::atom(self.crash) {
                return NativeOutcome::Stop(ExitReason::Error);
            }
            if message == Term::atom(self.stop) {
                return NativeOutcome::Stop(ExitReason::Normal);
            }
            if let Some(reply_to) = self.reply_to {
                ctx.send(reply_to, message);
            }
        }
        NativeOutcome::Wait
    }
}

/// A clonable child spec; `child_factory` turns it into a fresh single-shot
/// [`NativeHandlerFactory`] (so a supervisor can rebuild a child repeatedly).
type ChildSpec = Arc<dyn Fn() -> Box<dyn NativeHandler> + Send + Sync>;

fn worker_spec(reply_to: Option<u64>, crash: Atom, stop: Atom) -> ChildSpec {
    Arc::new(move || {
        Box::new(Worker {
            reply_to,
            crash,
            stop,
        })
    })
}

fn worker_factory(reply_to: Option<u64>, crash: Atom, stop: Atom) -> NativeHandlerFactory {
    Box::new(move || {
        Box::new(Worker {
            reply_to,
            crash,
            stop,
        })
    })
}

fn child_factory(spec: &ChildSpec) -> NativeHandlerFactory {
    let spec = Arc::clone(spec);
    Box::new(move || spec())
}

/// Shared sink recording the `(source_pid, reason)` of each trapped EXIT.
type ExitLog = Arc<Mutex<Vec<(Option<u64>, Option<Atom>)>>>;

/// A native supervisor: on its first slice it traps exits (the native
/// `process_flag(trap_exit, true)`) and spawns a child LINKED to itself via the
/// stored spec. On each `{'EXIT', child, reason}` it records the source/reason
/// and, when `restart` is set, rebuilds a fresh child from the same spec —
/// reusing `ctx.spawn_native`, with no bespoke restart policy.
struct Supervisor {
    spec: ChildSpec,
    restart: bool,
    started: bool,
    child_pid: Arc<Mutex<Option<u64>>>,
    exits: ExitLog,
}

impl Supervisor {
    fn spawn_child(&self, ctx: &mut NativeContext<'_>) {
        let me = ctx.self_pid();
        let pid = ctx
            .spawn_native(child_factory(&self.spec), Some(me))
            .expect("supervisor spawns native child");
        *self.child_pid.lock().expect("child_pid lock") = Some(pid);
    }
}

impl NativeHandler for Supervisor {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.started {
            ctx.set_trap_exit(true);
            self.spawn_child(ctx);
            self.started = true;
            return NativeOutcome::Wait;
        }
        while let Some(message) = ctx.recv() {
            let Some(tuple) = Tuple::new(message) else {
                continue;
            };
            if tuple.arity() == 3 && tuple.get(0) == Some(Term::atom(Atom::EXIT)) {
                let source = tuple.get(1).and_then(Term::as_pid);
                let reason = tuple.get(2).and_then(Term::as_atom);
                self.exits
                    .lock()
                    .expect("exits lock")
                    .push((source, reason));
                if self.restart {
                    self.spawn_child(ctx);
                }
            }
        }
        NativeOutcome::Wait
    }
}

/// Spawn a [`Supervisor`] and return its pid plus the shared child-pid / exit
/// observation handles.
fn spawn_supervisor(
    scheduler: &Arc<Scheduler>,
    spec: ChildSpec,
    restart: bool,
) -> (u64, Arc<Mutex<Option<u64>>>, ExitLog) {
    let child_pid = Arc::new(Mutex::new(None));
    let exits: ExitLog = Arc::new(Mutex::new(Vec::new()));
    let (child_h, exits_h) = (Arc::clone(&child_pid), Arc::clone(&exits));
    let pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(Supervisor {
                spec: Arc::clone(&spec),
                restart,
                started: false,
                child_pid: Arc::clone(&child_h),
                exits: Arc::clone(&exits_h),
            })
        }))
        .expect("spawn supervisor");
    (pid, child_pid, exits)
}

/// A do-nothing handler used for factory/spawn-counting tests.
struct Idle;

impl NativeHandler for Idle {
    fn handle(&mut self, _ctx: &mut NativeContext<'_>) -> NativeOutcome {
        NativeOutcome::Wait
    }
}

/// A watcher recording the fields of the first `{'DOWN', Ref, process, Pid,
/// Reason}` it drains, decoded IN-HEAP (so the reference sub-term is read
/// directly, not deep-copied across the exit boundary).
struct DownRecorder {
    record: Arc<Mutex<Option<DownFields>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DownFields {
    arity: usize,
    tag_is_down: bool,
    reference: Option<u64>,
    kind_is_process: bool,
    pid: Option<u64>,
    reason: Option<Atom>,
}

impl NativeHandler for DownRecorder {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        while let Some(message) = ctx.recv() {
            let Some(tuple) = Tuple::new(message) else {
                continue;
            };
            if tuple.get(0) != Some(Term::atom(Atom::DOWN)) {
                continue;
            }
            let reference = tuple
                .get(1)
                .and_then(beamr::term::boxed::Reference::new)
                .map(|reference| reference.id());
            *self.record.lock().expect("record lock") = Some(DownFields {
                arity: tuple.arity(),
                tag_is_down: true,
                reference,
                kind_is_process: tuple.get(2) == Some(Term::atom(Atom::PROCESS)),
                pid: tuple.get(3).and_then(Term::as_pid),
                reason: tuple.get(4).and_then(Term::as_atom),
            });
        }
        NativeOutcome::Wait
    }
}

/// Build a scheduler with the BEAM collector module registered, plus the
/// `crash`/`stop` control atoms.
fn setup() -> (Arc<AtomTable>, Arc<Scheduler>, Atom, Atom) {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let crash = atoms.intern("crash");
    let stop = atoms.intern("stop");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));
    (atoms, scheduler, crash, stop)
}

// ── R1: links propagate across native <-> BEAM ──────────────────────────────

#[test]
fn crashing_native_propagates_exit_to_linked_beam() {
    let (atoms, scheduler, crash, stop) = setup();
    let beam = spawn_collector(&scheduler, &atoms);
    let native = scheduler
        .spawn_native_link(beam, worker_factory(None, crash, stop))
        .expect("spawn linked native");
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        scheduler.is_linked(beam, native),
        "pair is bidirectionally linked"
    );

    // Crash the native partner abnormally; the link propagates to the BEAM peer.
    assert!(scheduler.enqueue_atom_message(native, crash));

    let (reason, _result) = run_until_exit_bounded(&scheduler, beam);
    assert_eq!(
        reason,
        ExitReason::Error,
        "the linked BEAM process dies with the propagated abnormal reason"
    );
    scheduler.shutdown();
}

#[test]
fn crashing_beam_propagates_exit_to_linked_native() {
    let (atoms, scheduler, crash, stop) = setup();
    let beam = spawn_collector(&scheduler, &atoms);
    let native = scheduler
        .spawn_native_link(beam, worker_factory(None, crash, stop))
        .expect("spawn linked native");
    std::thread::sleep(Duration::from_millis(50));

    // Crash the BEAM partner abnormally via the pid-keyed exit-signal path.
    scheduler
        .exit_signal(0, beam, ExitReason::Error)
        .expect("exit signal to beam");

    let (reason, _result) = run_until_exit_bounded(&scheduler, native);
    assert_eq!(
        reason,
        ExitReason::Error,
        "the linked native process is terminated by the propagated exit signal"
    );
    scheduler.shutdown();
}

// ── R2: monitors deliver DOWN for native targets ────────────────────────────

fn monitor_native_down_case(stop_signal: &str, expected_reason: Atom) {
    let (atoms, scheduler, crash, stop) = setup();
    let record = Arc::new(Mutex::new(None));
    let record_h = Arc::clone(&record);
    let watcher = scheduler
        .spawn_native(Box::new(move || {
            Box::new(DownRecorder {
                record: Arc::clone(&record_h),
            })
        }))
        .expect("spawn watcher");
    let native = scheduler
        .spawn_native(worker_factory(None, crash, stop))
        .expect("spawn native");
    std::thread::sleep(Duration::from_millis(50));

    // The watcher monitors the native process through the pid-keyed facility.
    let reference = scheduler.monitor(watcher, native).expect("monitor native");
    assert!(scheduler.enqueue_atom_message(native, atoms.intern(stop_signal)));

    let down = poll_until(Duration::from_secs(5), || *record.lock().unwrap());
    assert_eq!(
        down,
        DownFields {
            arity: 5,
            tag_is_down: true,
            reference: Some(reference),
            kind_is_process: true,
            pid: Some(native),
            reason: Some(expected_reason),
        },
        "DOWN carries the correct ref, pid, and reason"
    );
    scheduler.shutdown();
}

#[test]
fn monitor_native_delivers_down_on_abnormal_exit() {
    monitor_native_down_case("crash", Atom::ERROR);
}

#[test]
fn monitor_native_delivers_down_on_normal_exit() {
    monitor_native_down_case("stop", Atom::NORMAL);
}

#[test]
fn beam_process_receives_down_for_native_target() {
    // R2 "from a BEAM process": a real BEAM process monitoring a native process
    // is delivered the DOWN through `deliver_down_messages`. The collector's
    // `receive Msg -> Msg end` returns that DOWN as its exit value, so we can
    // decode it and assert every field — ref, kind, pid, reason — exactly as
    // R2's acceptance criterion requires.
    //
    // The reference sub-term DOES survive the exit-value deep copy: the exit
    // boundary copies via `capture_term` -> `copy_term_to_ets`, which copies
    // reference terms (see `ets/copy.rs`'s `Reference` arm). So the ref read
    // back here is the same one `monitor` returned.
    let (atoms, scheduler, crash, stop) = setup();
    let watcher = spawn_collector(&scheduler, &atoms);
    let native = scheduler
        .spawn_native(worker_factory(None, crash, stop))
        .expect("spawn native");
    std::thread::sleep(Duration::from_millis(50));

    let reference = scheduler.monitor(watcher, native).expect("monitor native");
    assert!(scheduler.enqueue_atom_message(native, crash));

    let (reason, message) = run_until_exit_bounded(&scheduler, watcher);
    assert_eq!(
        reason,
        ExitReason::Normal,
        "the BEAM watcher returned after receiving the DOWN for the native target"
    );

    // Decode the returned DOWN as {'DOWN', Ref, 'process', Pid, Reason}.
    // `message` keeps the owned exit storage alive for the duration of the
    // decode below.
    let tuple =
        Tuple::new(message.root()).expect("the BEAM watcher's exit value is the DOWN tuple");
    assert_eq!(tuple.arity(), 5, "DOWN is a 5-tuple");
    assert_eq!(
        tuple.get(0),
        Some(Term::atom(Atom::DOWN)),
        "field 0 is the 'DOWN' tag"
    );
    assert_eq!(
        tuple.get(2),
        Some(Term::atom(Atom::PROCESS)),
        "field 2 is the 'process' kind"
    );
    assert_eq!(
        tuple.get(3).and_then(Term::as_pid),
        Some(native),
        "field 3 is the native target pid"
    );
    assert_eq!(
        tuple.get(4).and_then(Term::as_atom),
        Some(Atom::ERROR),
        "field 4 is the abnormal (error) reason"
    );
    assert_eq!(
        tuple
            .get(1)
            .and_then(beamr::term::boxed::Reference::new)
            .map(|reference| reference.id()),
        Some(reference),
        "field 1 is the reference returned by monitor()"
    );
    scheduler.shutdown();
}

// ── R3: NativeOutcome::Stop drives cleanup_exited_process ────────────────────

#[test]
fn native_stop_cleans_up_body_and_propagates_to_links() {
    let (atoms, scheduler, crash, stop) = setup();
    let beam = spawn_collector(&scheduler, &atoms);
    let native = scheduler
        .spawn_native_link(beam, worker_factory(None, crash, stop))
        .expect("spawn linked native");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        scheduler.is_native(native),
        Some(true),
        "native before exit"
    );

    assert!(scheduler.enqueue_atom_message(native, crash));

    // The linked partner dying confirms Stop flowed through cleanup -> propagate.
    let (reason, _result) = run_until_exit_bounded(&scheduler, beam);
    assert_eq!(reason, ExitReason::Error);

    // R3: the native body is gone from BOTH the process table and process_bodies.
    poll_until(Duration::from_secs(5), || {
        (scheduler.process_table().get(native).is_none() && scheduler.is_native(native).is_none())
            .then_some(())
    });
    scheduler.shutdown();
}

#[test]
fn native_normal_stop_does_not_kill_non_trapping_link() {
    let (atoms, scheduler, crash, stop) = setup();
    let beam = spawn_collector(&scheduler, &atoms);
    let native = scheduler
        .spawn_native_link(beam, worker_factory(None, crash, stop))
        .expect("spawn linked native");
    std::thread::sleep(Duration::from_millis(50));

    // Native exits NORMALLY: a non-trapping link must survive.
    assert!(scheduler.enqueue_atom_message(native, stop));
    poll_until(Duration::from_secs(5), || {
        scheduler
            .process_table()
            .get(native)
            .is_none()
            .then_some(())
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        still_alive(&scheduler, beam),
        "a normal native exit must not kill its non-trapping linked partner"
    );
    scheduler.shutdown();
}

// ── R4: trap_exit native receives {EXIT, source, reason} and is not killed ───

fn trap_exit_case(stop_signal: &str, expected_reason: Atom) {
    let (atoms, scheduler, crash, stop) = setup();
    let (supervisor, child_pid, exits) =
        spawn_supervisor(&scheduler, worker_spec(None, crash, stop), false);

    // Wait for the supervisor to trap and spawn its linked child.
    let child = poll_until(Duration::from_secs(5), || *child_pid.lock().unwrap());
    assert!(scheduler.enqueue_atom_message(child, atoms.intern(stop_signal)));

    // The trapping supervisor must RECEIVE {EXIT, child, reason}, not die.
    let recorded = poll_until(Duration::from_secs(5), || {
        exits.lock().unwrap().first().copied()
    });
    assert_eq!(recorded, (Some(child), Some(expected_reason)));
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        still_alive(&scheduler, supervisor),
        "a trapped exit must not kill the supervisor"
    );
    assert_eq!(
        scheduler.is_native(supervisor),
        Some(true),
        "the supervisor is still a live native process"
    );
    scheduler.shutdown();
}

#[test]
fn trap_exit_native_receives_abnormal_exit_message() {
    trap_exit_case("crash", Atom::ERROR);
}

#[test]
fn trap_exit_native_receives_normal_exit_message() {
    trap_exit_case("stop", Atom::NORMAL);
}

// FOLLOW-UP: the EXECUTING-arm native trap_exit drain is NOT directly exercised
// here. Both R4 tests above have the supervisor PARKED (`ProcessSlot::Present`)
// when the linked child's exit arrives, so the signal is delivered straight to
// the mailbox via `process_exit_signal`'s Present arm. The untested path is the
// EXECUTING arm: when the trap_exit native supervisor is MID-SLICE
// (`ProcessSlot::Executing`) as the child exits, the signal is queued into
// `metadata.pending_exit_messages` (the `ProcessSlot::Executing` branch of
// `process_exit_signal` in scheduler/supervision_integration.rs) and drained at
// the slice boundary by `store_runnable_process`
// (scheduler/execution/core.rs) into the mailbox, observed next slice — without
// killing the process.
//
// This gap is left documented rather than filled with a flaky/hanging test, per
// the brief. Forcing a native process to be `Executing` at the instant its
// child's exit propagates requires pinning it mid-slice, and there is no test
// hook to do so. The only available lever is to busy-spin inside the handler to
// hold the slot open; that was tried and rejected: a non-yielding spin
// saturates a scheduler worker thread and — under the concurrent test harness —
// starves the other tests in this binary into spurious failures, so it is both
// unsafe and effectively non-deterministic.
//
// The drain MECHANISM is not native-specific and IS otherwise covered: the
// `pending_exit_messages` queue-and-drain path makes no native/bytecode
// distinction (the same `Executing` arm and `store_runnable_process` serve the
// bytecode trap_exit path), and the Present-arm native delivery is covered by
// the two tests above. Closing this gap cleanly needs a scheduler hook that
// parks a native process in `Executing` deterministically — tracked separately.

// ── R5: factory builds fresh, independent handlers ──────────────────────────

#[test]
fn factory_builds_a_fresh_independent_instance_each_call() {
    let next_id = Arc::new(AtomicUsize::new(0));
    let ids = Arc::new(Mutex::new(Vec::new()));
    let (next_h, ids_h) = (Arc::clone(&next_id), Arc::clone(&ids));
    let factory: NativeHandlerFactory = Box::new(move || {
        let id = next_h.fetch_add(1, Ordering::SeqCst);
        ids_h.lock().unwrap().push(id);
        Box::new(Idle)
    });

    let _first = factory();
    let _second = factory();

    assert_eq!(
        *ids.lock().unwrap(),
        vec![0, 1],
        "each call builds a distinct, independently-stated handler"
    );
}

#[test]
fn spawn_native_invokes_factory_exactly_once() {
    let (_atoms, scheduler, _crash, _stop) = setup();
    let constructions = Arc::new(AtomicUsize::new(0));
    let constructions_h = Arc::clone(&constructions);
    let pid = scheduler
        .spawn_native(Box::new(move || {
            constructions_h.fetch_add(1, Ordering::SeqCst);
            Box::new(Idle)
        }))
        .expect("spawn native");
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(
        constructions.load(Ordering::SeqCst),
        1,
        "spawn_native invokes the factory exactly once for the initial handler"
    );
    assert_eq!(scheduler.is_native(pid), Some(true));
    scheduler.shutdown();
}

// ── R6: a supervisor restarts a crashed native child via the factory ────────

#[test]
fn supervisor_restarts_crashed_native_child_via_factory() {
    let (atoms, scheduler, crash, stop) = setup();
    let ping = atoms.intern("ping");

    // The replacement child replies `ping` to this BEAM collector when live.
    let collector = spawn_collector(&scheduler, &atoms);
    let (_supervisor, child_pid, exits) =
        spawn_supervisor(&scheduler, worker_spec(Some(collector), crash, stop), true);

    // First child comes up; crash it. The supervisor traps the EXIT and
    // restarts via the stored factory.
    let first_child = poll_until(Duration::from_secs(5), || *child_pid.lock().unwrap());
    assert!(scheduler.enqueue_atom_message(first_child, crash));
    let second_child = poll_until(Duration::from_secs(5), || {
        (*child_pid.lock().unwrap()).filter(|pid| *pid != first_child)
    });

    assert_ne!(second_child, first_child, "restart produced a NEW pid");
    assert_eq!(
        exits.lock().unwrap().first().copied(),
        Some((Some(first_child), Some(Atom::ERROR))),
        "the supervisor trapped the crashed child's exit"
    );

    // The replacement is a live native process built from fresh factory state.
    let is_native = poll_until(Duration::from_secs(5), || scheduler.is_native(second_child));
    assert!(is_native, "the restarted child is native");

    // Prove it is fully scheduled and live: it replies to a message.
    assert!(scheduler.enqueue_atom_message(second_child, ping));
    let (reason, result) = run_until_exit_bounded(&scheduler, collector);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(ping),
        "the restarted native child receives and replies to a message"
    );
    scheduler.shutdown();
}
