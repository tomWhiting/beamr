//! End-to-end gate tests for the native-process core (NATIVE-001).
//!
//! Every test boots a REAL multi-threaded scheduler and drives native
//! processes through the same machinery as bytecode processes:
//!
//! * BEAM -> native -> BEAM round trip (the `Send` opcode delivers to a native
//!   process; the native handler replies; a BEAM process observes the reply).
//! * native reply to a message delivered FROM Rust (host `enqueue_atom_message`).
//! * the park/wait path (a native process parks on an empty mailbox and wakes
//!   on a later delivery — the existing 3-phase park-gap, reused).
//! * self-send deferred through `pending_local_messages` (observed next slice).
//! * `ctx.spawn_native` (a native process spawns a working native child).
//!
//! The bytecode-path replay-determinism of native sends is covered by the
//! in-crate unit tests in `scheduler::execution::native_slice` (which exercise
//! the real `LocalSendFacility` Present arm under `replay_mode`).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry};
use beamr::native::BifRegistryImpl;
use beamr::native::native_process::{NativeContext, NativeHandler, NativeOutcome};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

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
/// returns it in x(0) and exits Normal.
fn collector_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("native_collector");
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

/// `fire/1`: receives a target PID in x(0), moves `message` into x(1), then
/// runs the real `Send` opcode (`Target ! message`) and returns.
fn sender_module(atoms: &AtomTable, message: Atom) -> Module {
    let name = atoms.intern("native_sender");
    let function = atoms.intern("fire");
    let mut exports = HashMap::new();
    exports.insert((function, 1), 1);
    module(
        name,
        exports,
        vec![
            Instruction::Label { label: 1 },
            Instruction::Move {
                source: Operand::Atom(Some(message)),
                destination: Operand::X(1),
            },
            Instruction::Send,
            Instruction::Return,
        ],
    )
}

/// A native handler that forwards every message it drains to a fixed reply
/// target, then parks. The reply target is captured at construction (the
/// gen_server "reply-to" pattern), so no sender PID need travel in the message.
struct Forwarder {
    reply_to: u64,
}

impl NativeHandler for Forwarder {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        while let Some(message) = ctx.recv() {
            ctx.send(self.reply_to, message);
        }
        NativeOutcome::Wait
    }
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

fn run_until_exit_bounded(
    scheduler: &Arc<Scheduler>,
    pid: u64,
) -> (ExitReason, beamr::ets::OwnedTerm) {
    let (sender, completion) = std::sync::mpsc::channel();
    let scheduler_for_wait = Arc::clone(scheduler);
    std::thread::spawn(move || {
        let _ = sender.send(scheduler_for_wait.run_until_exit(pid));
    });
    // Return the OWNED term, not `result.root()`: a boxed exit value lives in
    // the `OwnedTerm`'s storage, so handing back a bare `Term` would dangle
    // once the local `OwnedTerm` drops. Callers `.root()` it while it's alive.
    completion
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("process {pid} never exited"))
}

#[test]
fn beam_to_native_to_beam_round_trip() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let ping = atoms.intern("ping");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    registry.insert(sender_module(&atoms, ping));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");
    let sender_mod = atoms.intern("native_sender");
    let fire_fn = atoms.intern("fire");

    // BEAM collector parks on its receive.
    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    std::thread::sleep(Duration::from_millis(50));

    // Native echo replies to the collector.
    let echo_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(Forwarder {
                reply_to: collector_pid,
            })
        }))
        .expect("spawn native echo");
    std::thread::sleep(Duration::from_millis(50));

    // BEAM sender fires `echo_pid ! ping` through the real Send opcode.
    let _sender_pid = scheduler
        .spawn(
            sender_mod,
            fire_fn,
            vec![Term::try_pid(echo_pid).expect("echo pid fits")],
        )
        .expect("spawn sender");

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(ping),
        "the BEAM collector must observe the native echo's reply"
    );

    scheduler.shutdown();
}

#[test]
fn native_replies_to_message_sent_from_rust() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let ping = atoms.intern("ping");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");

    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    std::thread::sleep(Duration::from_millis(50));

    let echo_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(Forwarder {
                reply_to: collector_pid,
            })
        }))
        .expect("spawn native echo");
    std::thread::sleep(Duration::from_millis(50));

    // Deliver a Term to the native process FROM Rust (host send).
    assert!(
        scheduler.enqueue_atom_message(echo_pid, ping),
        "host delivery to the native process must succeed"
    );

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(result.root(), Term::atom(ping));

    scheduler.shutdown();
}

#[test]
fn native_parks_on_empty_mailbox_then_wakes_on_delivery() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let ping = atoms.intern("ping");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");

    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    let echo_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(Forwarder {
                reply_to: collector_pid,
            })
        }))
        .expect("spawn native echo");

    // Let the native echo reach its receive and PARK on an empty mailbox
    // (NativeOutcome::Wait -> the existing 3-phase park-gap). Only then deliver.
    std::thread::sleep(Duration::from_millis(100));
    assert!(scheduler.enqueue_atom_message(echo_pid, ping));

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(ping),
        "a delivery after the native process parked must wake it (no loss)"
    );

    scheduler.shutdown();
}

/// On its first slice this handler sends a marker to its OWN pid, then parks.
/// The self-send must NOT be visible in the same slice (it lands in
/// `pending_local_messages` and is merged at store-back); on the next slice the
/// handler drains it and forwards it to the collector.
struct SelfSender {
    reply_to: u64,
    marker: Term,
    sent: bool,
    observed_same_slice: Arc<AtomicBool>,
}

impl NativeHandler for SelfSender {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.sent {
            let me = ctx.self_pid();
            ctx.send(me, self.marker);
            // The self-sent message must not be in the mailbox yet.
            if ctx.has_messages() {
                self.observed_same_slice.store(true, Ordering::SeqCst);
            }
            self.sent = true;
            return NativeOutcome::Wait;
        }
        if let Some(message) = ctx.recv() {
            ctx.send(self.reply_to, message);
            return NativeOutcome::Stop(ExitReason::Normal);
        }
        NativeOutcome::Wait
    }
}

#[test]
fn native_self_send_is_delivered_next_slice() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let marker_atom = atoms.intern("selfmsg");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");

    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    std::thread::sleep(Duration::from_millis(50));

    let observed_same_slice = Arc::new(AtomicBool::new(false));
    let observed_for_handler = Arc::clone(&observed_same_slice);
    let marker = Term::atom(marker_atom);
    let _native_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(SelfSender {
                reply_to: collector_pid,
                marker,
                sent: false,
                observed_same_slice: Arc::clone(&observed_for_handler),
            })
        }))
        .expect("spawn self-sender");

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(marker_atom),
        "the self-sent marker must reach the collector via the next slice"
    );
    assert!(
        !observed_same_slice.load(Ordering::SeqCst),
        "the self-send must NOT be visible in the same slice it was sent"
    );

    scheduler.shutdown();
}

/// On its first slice this handler schedules a self-tick `delay` in the future
/// via `ctx.schedule` and parks (`Wait`). The tick must NOT be in the mailbox
/// in the scheduling slice (it is a future delivery), so `has_messages()` is
/// recorded as `early_tick` if it is wrongly already present. When the tick is
/// delivered the scheduler wakes the process; on that next slice the handler
/// drains the tick, forwards it to the collector, and stops.
struct SelfTicker {
    reply_to: u64,
    tick: Term,
    delay: Duration,
    scheduled: bool,
    early_tick: Arc<AtomicBool>,
}

impl NativeHandler for SelfTicker {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.scheduled {
            let reference = ctx.schedule(self.delay, self.tick);
            assert!(
                reference.is_some(),
                "a scheduler-backed native context must hand out a timer ref"
            );
            // The self-tick is a FUTURE delivery: nothing may be in the
            // mailbox yet in the slice that scheduled it.
            if ctx.has_messages() {
                self.early_tick.store(true, Ordering::SeqCst);
            }
            self.scheduled = true;
            return NativeOutcome::Wait;
        }
        if let Some(message) = ctx.recv() {
            ctx.send(self.reply_to, message);
            return NativeOutcome::Stop(ExitReason::Normal);
        }
        NativeOutcome::Wait
    }
}

#[test]
fn native_self_tick_is_delivered_after_delay() {
    // The key test: a native process schedules a self-tick via ctx.schedule and
    // is woken by the delivered tick on a LATER slice. The real scheduler thread
    // drives the wheel (timer_integration::tick_timers), so no manual driving or
    // sleep-polling is needed — the test only waits for the collector to exit.
    //
    // Falsifiable: before this change ctx had no timer access (and Deliver
    // timers were never delivered to a mailbox), so the tick would never arrive,
    // the collector would never receive it, and run_until_exit would time out.
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let tick_atom = atoms.intern("tick");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");

    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    std::thread::sleep(Duration::from_millis(50));

    let early_tick = Arc::new(AtomicBool::new(false));
    let early_for_handler = Arc::clone(&early_tick);
    let tick = Term::atom(tick_atom);
    let _ticker_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(SelfTicker {
                reply_to: collector_pid,
                tick,
                delay: Duration::from_millis(30),
                scheduled: false,
                early_tick: Arc::clone(&early_for_handler),
            })
        }))
        .expect("spawn self-ticker");

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(tick_atom),
        "the scheduled self-tick must be delivered to the native process and forwarded"
    );
    assert!(
        !early_tick.load(Ordering::SeqCst),
        "the self-tick must NOT be present in the slice that scheduled it"
    );

    scheduler.shutdown();
}

/// A native handler that, on its first slice, spawns a native child via
/// `ctx.spawn_native` and sends it a message, then stops.
struct Parent {
    collector: u64,
    payload: Term,
    spawned: bool,
}

impl NativeHandler for Parent {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if self.spawned {
            return NativeOutcome::Wait;
        }
        let collector = self.collector;
        let child = ctx
            .spawn_native(
                Box::new(move || {
                    Box::new(Forwarder {
                        reply_to: collector,
                    })
                }),
                None,
            )
            .expect("native child spawns");
        ctx.send(child, self.payload);
        self.spawned = true;
        NativeOutcome::Stop(ExitReason::Normal)
    }
}

#[test]
fn native_handler_spawns_working_native_child() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let ping = atoms.intern("ping");
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(collector_module(&atoms));
    let scheduler = scheduler_with(&atoms, Arc::clone(&registry));

    let collector_mod = atoms.intern("native_collector");
    let loop_fn = atoms.intern("loop");

    let collector_pid = scheduler
        .spawn(collector_mod, loop_fn, Vec::new())
        .expect("spawn collector");
    std::thread::sleep(Duration::from_millis(50));

    let payload = Term::atom(ping);
    let _parent_pid = scheduler
        .spawn_native(Box::new(move || {
            Box::new(Parent {
                collector: collector_pid,
                payload,
                spawned: false,
            })
        }))
        .expect("spawn native parent");

    let (reason, result) = run_until_exit_bounded(&scheduler, collector_pid);
    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(ping),
        "the native child spawned via ctx.spawn_native must deliver the reply"
    );

    scheduler.shutdown();
}
