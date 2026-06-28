//! WR-0 native proof: trivial `NativeHandler`s run cooperatively on the
//! single-threaded `WasmScheduler` — they spawn, receive a message, send a
//! reply, and exit — with no tokio, no crossbeam channels, and no OS threads
//! in the execution path.
//!
//! A pure native handler has no way to write x(0) (the slot the threaded
//! native-slice captures as the "exit result"; see the WR-0 report's friction
//! notes), so a native actor surfaces its result the way real native actors do:
//! by *sending* it. These tests capture results through the mailbox, and use an
//! `Arc<Mutex<…>>` collector to make the delivered value observable to the test.
//! (`Arc<Mutex>` rather than the natural single-threaded `Rc<Cell>` because the
//! `NativeHandlerFactory: Send + Sync` bound forbids capturing an `Rc` — see the
//! WR-0 report's friction note on Decision D3.)

use std::sync::{Arc, Mutex};

use super::*;
use crate::atom::{Atom, AtomTable};
use crate::module::ModuleRegistry;
use crate::native::BifRegistryImpl;
use crate::native::native_process::{
    NativeContext, NativeHandler, NativeHandlerFactory, NativeOutcome,
};
use crate::process::ExitReason;
use crate::term::Term;

/// A one-shot echo actor: parks until a message arrives, then on its next slice
/// drains exactly one message, sends it on to `reply_to`, and stops normally.
struct Echo {
    reply_to: u64,
}

impl NativeHandler for Echo {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        match ctx.recv() {
            Some(message) => {
                ctx.send(self.reply_to, message);
                NativeOutcome::Stop(ExitReason::Normal)
            }
            None => NativeOutcome::Wait,
        }
    }
}

/// A collector actor: records every small-integer message it receives into a
/// shared cell, so the test can observe what a native actor sent. Parks
/// between messages and never stops.
struct Collector {
    sink: Arc<Mutex<Option<i64>>>,
}

impl NativeHandler for Collector {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        while let Some(message) = ctx.recv() {
            if let Some(value) = message.as_small_int()
                && let Ok(mut guard) = self.sink.lock()
            {
                *guard = Some(value);
            }
        }
        NativeOutcome::Wait
    }
}

fn scheduler() -> WasmScheduler {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let modules = Arc::new(ModuleRegistry::new());
    let bifs = Arc::new(BifRegistryImpl::new());
    WasmScheduler::new(atom_table, modules, bifs)
}

/// Run cooperative native turns until `pid` exits or `max_turns` is reached.
fn drain_until_exit(scheduler: &mut WasmScheduler, pid: u64, max_turns: usize) -> bool {
    for _ in 0..max_turns {
        let exited = scheduler.run_native_until_idle();
        if exited.contains(&pid) {
            return true;
        }
    }
    false
}

/// Run the unified host pump ([`WasmScheduler::run_until_idle`]) until `pid`
/// appears in a turn's `exited` summary or `max_turns` is reached. This drives
/// native processes through the SAME entry point the wasm host calls, proving
/// the WR-3 native branch is wired into the real pump (not just the standalone
/// `run_native_until_idle`).
fn drain_run_until_idle(scheduler: &mut WasmScheduler, pid: u64, max_turns: usize) -> bool {
    for _ in 0..max_turns {
        let summary = scheduler.run_until_idle();
        if summary.exited.contains(&pid) {
            return true;
        }
    }
    false
}

#[test]
fn native_actor_runs_through_unified_run_until_idle_pump() {
    // WR-3: a native actor is dispatched by `run_until_idle` — the host's single
    // pump — exactly as a bytecode process would be, with no call to the
    // standalone native turn. It parks with no mail, wakes on a delivered
    // message, forwards it, and exits; the forward is observable end-to-end.
    let mut scheduler = scheduler();
    let sink = Arc::new(Mutex::new(None));

    let collector = scheduler.spawn_native_root({
        let sink = Arc::clone(&sink);
        Box::new(move || {
            Box::new(Collector {
                sink: Arc::clone(&sink),
            })
        })
    });
    let echo = scheduler.spawn_native_root(Box::new(move || {
        Box::new(Echo {
            reply_to: collector,
        })
    }));

    // First unified turn: both native actors park (no mail), nothing exits.
    let summary = scheduler.run_until_idle();
    assert!(
        summary.exited.is_empty(),
        "nothing exits before a message arrives"
    );
    assert!(
        summary.executed >= 1,
        "the native actors received a slice through the unified pump"
    );

    scheduler
        .send_owned(echo, &crate::ets::OwnedTerm::immediate(Term::small_int(99)))
        .expect("message delivers to the parked echo actor");

    assert!(
        drain_run_until_idle(&mut scheduler, echo, 4),
        "the echo actor exits via the unified pump after handling its message"
    );
    assert_eq!(
        scheduler.native_exit_reason(echo),
        Some(ExitReason::Normal),
        "the echo actor stopped normally under the unified pump"
    );

    for _ in 0..4 {
        let _summary = scheduler.run_until_idle();
        if sink.lock().expect("sink lock").is_some() {
            break;
        }
    }
    assert_eq!(
        *sink.lock().expect("sink lock"),
        Some(99),
        "the forwarded value is observable end-to-end through run_until_idle"
    );
}

#[test]
fn native_actor_spawns_receives_one_message_and_replies_with_captured_result() {
    let mut scheduler = scheduler();
    let sink = Arc::new(Mutex::new(None));

    // A long-lived collector and a one-shot echo actor that replies to it.
    let collector = scheduler.spawn_native_root({
        let sink = Arc::clone(&sink);
        Box::new(move || {
            Box::new(Collector {
                sink: Arc::clone(&sink),
            })
        })
    });
    let echo = scheduler.spawn_native_root(Box::new(move || {
        Box::new(Echo {
            reply_to: collector,
        })
    }));

    // First turn: both park (no mail).
    let exited = scheduler.run_native_until_idle();
    assert!(exited.is_empty(), "nothing exits before a message arrives");
    assert_eq!(
        *sink.lock().expect("sink lock"),
        None,
        "collector has received nothing yet"
    );

    // Deliver one message to the echo actor.
    scheduler
        .send_owned(echo, &crate::ets::OwnedTerm::immediate(Term::small_int(42)))
        .expect("message delivers to the parked echo actor");

    // The echo actor wakes, forwards to the collector, and exits normally.
    assert!(
        drain_until_exit(&mut scheduler, echo, 4),
        "the echo actor exits after handling its one message"
    );
    assert_eq!(
        scheduler.native_exit_reason(echo),
        Some(ExitReason::Normal),
        "the echo actor stopped normally"
    );

    // Pump further turns so the woken collector runs and records the forward.
    for _ in 0..4 {
        let _exited = scheduler.run_native_until_idle();
        if sink.lock().expect("sink lock").is_some() {
            break;
        }
    }

    // The collector received exactly the forwarded value — the captured result.
    assert_eq!(
        *sink.lock().expect("sink lock"),
        Some(42),
        "the result the native actor produced is observable end-to-end"
    );
}

/// A parent that, on its first non-empty slice, spawns a child echo actor via
/// the cooperative `SpawnFacility`, sends it one message via the cooperative
/// `LocalSendFacility`, then stops. Exercises both deferred-effect paths.
struct Parent {
    reply_to: u64,
}

impl NativeHandler for Parent {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        let Some(_trigger) = ctx.recv() else {
            return NativeOutcome::Wait;
        };
        let reply_to = self.reply_to;
        let child = ctx
            .spawn_native(Box::new(move || Box::new(Echo { reply_to })), None)
            .expect("cooperative spawn_native succeeds");
        ctx.send(child, Term::small_int(7));
        NativeOutcome::Stop(ExitReason::Normal)
    }
}

#[test]
fn handler_spawns_child_and_sends_it_a_message_cooperatively() {
    let mut scheduler = scheduler();
    let sink = Arc::new(Mutex::new(None));

    let collector = scheduler.spawn_native_root({
        let sink = Arc::clone(&sink);
        Box::new(move || {
            Box::new(Collector {
                sink: Arc::clone(&sink),
            })
        })
    });
    let parent = scheduler.spawn_native_root(Box::new(move || {
        Box::new(Parent {
            reply_to: collector,
        })
    }));

    // Park everyone, then poke the parent so it runs its spawn+send slice.
    let _first = scheduler.run_native_until_idle();
    scheduler
        .send_owned(
            parent,
            &crate::ets::OwnedTerm::immediate(Term::atom(Atom::OK)),
        )
        .expect("trigger delivers to the parent");

    // The parent runs (spawns child, sends to child, stops); the child then
    // forwards to the collector across subsequent cooperative turns.
    assert!(
        drain_until_exit(&mut scheduler, parent, 8),
        "the parent exits after spawning and sending"
    );
    assert_eq!(
        scheduler.native_exit_reason(parent),
        Some(ExitReason::Normal),
        "parent stopped after spawning and sending"
    );

    // Drive further turns so the spawned child can forward its message.
    for _ in 0..8 {
        let _exited = scheduler.run_native_until_idle();
        if sink.lock().expect("sink lock").is_some() {
            break;
        }
    }
    assert_eq!(
        *sink.lock().expect("sink lock"),
        Some(7),
        "the cooperatively-spawned child received and forwarded the message"
    );
}

// ---------------------------------------------------------------------------
// WR-4: native timers on the cooperative scheduler.
// ---------------------------------------------------------------------------

/// A native actor that, on its first slice (no mail), schedules a self-tick
/// `delay` in the future carrying `tick_value`, then parks. When the tick is
/// delivered it records the value into `sink` and stops normally. This proves a
/// `NativeContext::schedule` `Deliver` timer is honoured cooperatively: the
/// scheduler reschedules the parked actor when the timer fires and delivers the
/// scheduled message to its mailbox.
struct SelfTicker {
    delay: std::time::Duration,
    tick_value: i64,
    sink: Arc<Mutex<Option<i64>>>,
    armed: bool,
}

impl NativeHandler for SelfTicker {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        // Drain any delivered tick first.
        if let Some(message) = ctx.recv() {
            if let Some(value) = message.as_small_int()
                && let Ok(mut guard) = self.sink.lock()
            {
                *guard = Some(value);
            }
            return NativeOutcome::Stop(ExitReason::Normal);
        }
        if !self.armed {
            self.armed = true;
            let reference = ctx.schedule(self.delay, Term::small_int(self.tick_value));
            assert!(
                reference.is_some(),
                "the cooperative scheduler supplies a real timer wheel"
            );
        }
        NativeOutcome::Wait
    }
}

#[test]
fn native_actor_self_tick_is_delivered_when_the_timer_fires() {
    // WR-4: a native actor schedules a self-Deliver timer and parks. Advancing
    // the cooperative timer past the delay reschedules the actor and delivers
    // the timer message; before the delay nothing is delivered. The firing is
    // driven through the deterministic `tick_native_timers_at` seam.
    //
    // The delay is deliberately large (10s) relative to the test's real
    // runtime: the timer's deadline is anchored to the wall clock at schedule
    // time (inside the handler's `send_after`), and `run_native_until_idle`
    // also ticks pending native timers off the wall clock once per turn. A
    // 10s delay guarantees neither of those wall-clock anchors can reach the
    // deadline during the (sub-second) test window, so the explicit
    // `tick_native_timers_at` calls are the sole firing source and the
    // assertion margins (seconds) dwarf any scheduling jitter.
    let mut scheduler = scheduler();
    let sink = Arc::new(Mutex::new(None));
    let delay = std::time::Duration::from_secs(10);

    let ticker = scheduler.spawn_native_root({
        let sink = Arc::clone(&sink);
        Box::new(move || {
            Box::new(SelfTicker {
                delay,
                tick_value: 1234,
                sink: Arc::clone(&sink),
                armed: false,
            })
        })
    });

    // First turn: the actor arms its self-tick and parks. Nothing exits, nothing
    // is delivered yet.
    let exited = scheduler.run_native_until_idle();
    assert!(exited.is_empty(), "the ticker parks after arming its timer");
    assert_eq!(
        *sink.lock().expect("sink lock"),
        None,
        "no tick before the delay elapses"
    );

    let start = std::time::Instant::now();

    // Advance well short of the delay: the timer must NOT fire, and the actor
    // must remain parked (no wake, no delivery). 5s < 10s by a margin that
    // dwarfs the (sub-second) gap between this `start` and the handler's
    // schedule-time anchor.
    let woken_early = scheduler.tick_native_timers_at(start + std::time::Duration::from_secs(5));
    assert!(
        woken_early.is_empty(),
        "the self-tick must not fire before its delay"
    );
    let _early_turn = scheduler.run_native_until_idle();
    assert_eq!(
        *sink.lock().expect("sink lock"),
        None,
        "still no tick before the delay elapses"
    );

    // Advance comfortably past the delay: the timer fires, delivers its
    // message, and wakes the parked actor. start + 10s + 5s slack is past the
    // deadline (schedule_instant + 10s, with schedule_instant <= start).
    let woken = scheduler.tick_native_timers_at(start + delay + std::time::Duration::from_secs(5));
    assert_eq!(
        woken,
        vec![ticker],
        "the expired self-tick wakes exactly the scheduling actor"
    );

    // Run the woken actor: it receives the delivered tick and stops normally.
    assert!(
        drain_until_exit(&mut scheduler, ticker, 4),
        "the rescheduled actor runs and exits after receiving its self-tick"
    );
    assert_eq!(
        scheduler.native_exit_reason(ticker),
        Some(ExitReason::Normal),
        "the ticker stopped normally after handling its tick"
    );
    assert_eq!(
        *sink.lock().expect("sink lock"),
        Some(1234),
        "the scheduled timer message was delivered to the actor's mailbox"
    );
}

// ---------------------------------------------------------------------------
// WR-5: supervision + restart on the cooperative scheduler.
// ---------------------------------------------------------------------------

/// Command discriminants exchanged as small-integer messages so the tests need
/// no atom-table coordination.
const CMD_CRASH: i64 = 1;
const CMD_WORK: i64 = 2;

/// A supervised worker child. On `CMD_CRASH` it crashes (`Stop(Error)`); on
/// `CMD_WORK` it records `pid`-tagged proof into `sink` and stops normally.
struct Worker {
    sink: Arc<Mutex<Vec<i64>>>,
}

impl NativeHandler for Worker {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        match ctx.recv().and_then(Term::as_small_int) {
            Some(CMD_CRASH) => NativeOutcome::Stop(ExitReason::Error),
            Some(CMD_WORK) => {
                if let Ok(mut guard) = self.sink.lock() {
                    guard.push(CMD_WORK);
                }
                NativeOutcome::Stop(ExitReason::Normal)
            }
            _ => NativeOutcome::Wait,
        }
    }
}

/// A supervisor that traps exits, spawns a linked child, and crashes it; when it
/// receives the child's `{'EXIT', child, error}` link signal it asserts the
/// signal shape, restarts the child via the SAME factory, sends the restarted
/// child `CMD_WORK`, and stops. `restarts` counts how many times the supervisor
/// restarted the child so the test can assert the restart happened.
struct Supervisor {
    sink: Arc<Mutex<Vec<i64>>>,
    restarts: Arc<Mutex<u32>>,
    started: bool,
    child_pid: Arc<Mutex<Option<u64>>>,
}

impl Supervisor {
    fn child_factory(sink: Arc<Mutex<Vec<i64>>>) -> NativeHandlerFactory {
        Box::new(move || {
            Box::new(Worker {
                sink: Arc::clone(&sink),
            })
        })
    }
}

impl NativeHandler for Supervisor {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.started {
            self.started = true;
            ctx.set_trap_exit(true);
            let child = ctx
                .spawn_native(
                    Self::child_factory(Arc::clone(&self.sink)),
                    Some(ctx.self_pid()),
                )
                .expect("supervisor spawns its linked child");
            *self.child_pid.lock().expect("child pid lock") = Some(child);
            ctx.send(child, Term::small_int(CMD_CRASH));
            return NativeOutcome::Wait;
        }

        // Woken by the linked child's exit signal: it must be a trapped
        // `{'EXIT', child, error}` tuple (link semantics for a trapping process).
        let Some(message) = ctx.recv() else {
            return NativeOutcome::Wait;
        };
        let tuple = crate::term::boxed::Tuple::new(message)
            .expect("a trapping supervisor receives the EXIT signal as a tuple");
        assert_eq!(tuple.arity(), 3, "EXIT signal is a 3-tuple");
        assert_eq!(
            tuple.get(0).and_then(Term::as_atom),
            Some(Atom::EXIT),
            "first element is the 'EXIT' atom"
        );
        assert_eq!(
            tuple.get(2).and_then(Term::as_atom),
            Some(Atom::ERROR),
            "the reported reason is the child's crash reason"
        );

        // Restart the child via the retained factory and give it real work.
        let child = ctx
            .spawn_native(
                Self::child_factory(Arc::clone(&self.sink)),
                Some(ctx.self_pid()),
            )
            .expect("supervisor restarts the child via the factory");
        *self.child_pid.lock().expect("child pid lock") = Some(child);
        *self.restarts.lock().expect("restart counter lock") += 1;
        ctx.send(child, Term::small_int(CMD_WORK));
        NativeOutcome::Stop(ExitReason::Normal)
    }
}

#[test]
fn supervisor_restarts_crashed_supervised_child_via_factory() {
    // WR-5: a trapping supervisor spawns a linked child, the child crashes
    // (`Stop(Error)`), the supervisor observes the `{'EXIT', child, error}` link
    // signal, restarts the child through the SAME factory, and the restarted
    // child receives `CMD_WORK` and runs. All cooperative, single-threaded.
    let mut scheduler = scheduler();
    let sink = Arc::new(Mutex::new(Vec::new()));
    let restarts = Arc::new(Mutex::new(0));
    let child_pid = Arc::new(Mutex::new(None));

    let supervisor = scheduler.spawn_native_root({
        let sink = Arc::clone(&sink);
        let restarts = Arc::clone(&restarts);
        let child_pid = Arc::clone(&child_pid);
        Box::new(move || {
            Box::new(Supervisor {
                sink: Arc::clone(&sink),
                restarts: Arc::clone(&restarts),
                started: false,
                child_pid: Arc::clone(&child_pid),
            })
        })
    });

    // First turn: the supervisor parks (no mail).
    let _first = scheduler.run_native_until_idle();
    // Poke it so it runs its start slice: traps, spawns+links the child, crashes it.
    scheduler
        .send_owned(
            supervisor,
            &crate::ets::OwnedTerm::immediate(Term::atom(Atom::OK)),
        )
        .expect("trigger delivers to the supervisor");

    // Drive turns: child crashes -> EXIT signal wakes the supervisor -> it
    // restarts the child and stops. The supervisor exits normally.
    assert!(
        drain_until_exit(&mut scheduler, supervisor, 12),
        "the supervisor exits after observing the crash and restarting"
    );
    assert_eq!(
        scheduler.native_exit_reason(supervisor),
        Some(ExitReason::Normal),
        "the supervisor stopped normally after restarting the child"
    );
    assert_eq!(
        *restarts.lock().expect("restart counter lock"),
        1,
        "the supervisor restarted the child exactly once via the factory"
    );

    // Drain further turns so the restarted child handles its CMD_WORK.
    for _ in 0..12 {
        let _exited = scheduler.run_native_until_idle();
        if !sink.lock().expect("sink lock").is_empty() {
            break;
        }
    }
    assert_eq!(
        *sink.lock().expect("sink lock"),
        vec![CMD_WORK],
        "the restarted child received its message and ran"
    );

    // The restarted child is a distinct, live, non-native-exited process: it ran
    // to a Normal stop only AFTER restart, never as the crashed original.
    let restarted = child_pid
        .lock()
        .expect("child pid lock")
        .expect("a child pid");
    assert_eq!(
        scheduler.native_exit_reason(restarted),
        Some(ExitReason::Normal),
        "the restarted child stopped normally after doing its work"
    );
}

/// A non-trapping process that simply parks forever (until killed by a link
/// signal). It never stops on its own, so any exit it shows is link-driven.
struct Bystander;

impl NativeHandler for Bystander {
    fn handle(&mut self, _ctx: &mut NativeContext<'_>) -> NativeOutcome {
        NativeOutcome::Wait
    }
}

/// A linker that, on its start slice, spawns a linked non-trapping bystander and
/// then crashes itself (`Stop(Error)`), so the bystander must die by link
/// propagation (it does not trap exits).
struct Linker {
    bystander_pid: Arc<Mutex<Option<u64>>>,
}

impl NativeHandler for Linker {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        let bystander = ctx
            .spawn_native(Box::new(|| Box::new(Bystander)), Some(ctx.self_pid()))
            .expect("linker spawns its linked bystander");
        *self.bystander_pid.lock().expect("bystander pid lock") = Some(bystander);
        NativeOutcome::Stop(ExitReason::Error)
    }
}

#[test]
fn linked_non_trapping_process_dies_on_abnormal_link_exit() {
    // WR-5 link semantics: a non-supervised, non-trapping linked process dies
    // with the terminal reason when its link partner exits abnormally — it does
    // NOT receive an EXIT message (that is the trapping case, proven above).
    let mut scheduler = scheduler();
    let bystander_pid = Arc::new(Mutex::new(None));

    let linker = scheduler.spawn_native_root({
        let bystander_pid = Arc::clone(&bystander_pid);
        Box::new(move || {
            Box::new(Linker {
                bystander_pid: Arc::clone(&bystander_pid),
            })
        })
    });

    // The linker spawns+links the bystander and crashes in the same slice.
    assert!(
        drain_until_exit(&mut scheduler, linker, 4),
        "the linker exits after spawning and crashing"
    );
    assert_eq!(
        scheduler.native_exit_reason(linker),
        Some(ExitReason::Error),
        "the linker crashed abnormally"
    );

    // The linked bystander was killed by the abnormal link signal, with the
    // terminal reason for an Error exit (Error has no Kill->Killed remap).
    let bystander = bystander_pid
        .lock()
        .expect("bystander pid lock")
        .expect("a bystander pid");
    assert_eq!(
        scheduler.native_exit_reason(bystander),
        Some(ExitReason::Error),
        "the non-trapping bystander died from the abnormal link exit"
    );
}

#[test]
fn linked_non_trapping_process_survives_normal_link_exit() {
    // A linker variant that exits Normally instead of crashing.
    struct NormalLinker {
        bystander_pid: Arc<Mutex<Option<u64>>>,
    }
    impl NativeHandler for NormalLinker {
        fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
            let bystander = ctx
                .spawn_native(Box::new(|| Box::new(Bystander)), Some(ctx.self_pid()))
                .expect("linker spawns its linked bystander");
            *self.bystander_pid.lock().expect("bystander pid lock") = Some(bystander);
            NativeOutcome::Stop(ExitReason::Normal)
        }
    }

    // WR-5 link semantics: a `Normal` exit of a link partner never kills a
    // non-trapping survivor (matching `should_die_from_signal`).
    let mut scheduler = scheduler();
    let bystander_pid = Arc::new(Mutex::new(None));

    let linker = scheduler.spawn_native_root({
        let bystander_pid = Arc::clone(&bystander_pid);
        Box::new(move || {
            Box::new(NormalLinker {
                bystander_pid: Arc::clone(&bystander_pid),
            })
        })
    });

    assert!(
        drain_until_exit(&mut scheduler, linker, 4),
        "the linker exits normally after spawning"
    );
    // Pump a few more turns; the bystander must NOT have exited.
    for _ in 0..4 {
        let _exited = scheduler.run_native_until_idle();
    }
    let bystander = bystander_pid
        .lock()
        .expect("bystander pid lock")
        .expect("a bystander pid");
    assert_eq!(
        scheduler.native_exit_reason(bystander),
        None,
        "a Normal link exit does not kill a non-trapping survivor"
    );
}

/// A link-chain node for the transitive-cascade test. On its first slice a node
/// at `depth` spawns a linked child at `depth + 1` (until `max_depth`), records
/// the child's pid into `pids[depth + 1]`, and parks. Only the head
/// (`crash_on_message`) ever stops itself — on `CMD_CRASH` it `Stop(Error)`s, so
/// every other node can die ONLY by link propagation cascading down the chain.
struct ChainNode {
    depth: usize,
    max_depth: usize,
    started: bool,
    pids: Arc<Vec<Mutex<Option<u64>>>>,
    crash_on_message: bool,
}

impl NativeHandler for ChainNode {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.started {
            self.started = true;
            let child_depth = self.depth + 1;
            if child_depth < self.max_depth {
                let max_depth = self.max_depth;
                let pids = Arc::clone(&self.pids);
                let factory: NativeHandlerFactory = Box::new(move || {
                    Box::new(ChainNode {
                        depth: child_depth,
                        max_depth,
                        started: false,
                        pids: Arc::clone(&pids),
                        crash_on_message: false,
                    })
                });
                let child = ctx
                    .spawn_native(factory, Some(ctx.self_pid()))
                    .expect("chain node spawns its linked child");
                *self.pids[child_depth].lock().expect("chain pid lock") = Some(child);
            }
            return NativeOutcome::Wait;
        }
        if self.crash_on_message && ctx.recv().and_then(Term::as_small_int) == Some(CMD_CRASH) {
            return NativeOutcome::Stop(ExitReason::Error);
        }
        NativeOutcome::Wait
    }
}

#[test]
fn abnormal_link_exit_cascades_transitively_through_a_nontrapping_chain() {
    // WR-5: link propagation is TRANSITIVE. A head crashes abnormally; its
    // linked non-trapping child dies, and THAT death must continue the cascade
    // to the grandchild — matching the threaded `process_exited` worklist. A
    // per-target (non-cascading) propagation would leave the grandchild alive,
    // which is exactly the regression this test guards against.
    let mut scheduler = scheduler();
    const DEPTH: usize = 3;
    let pids: Arc<Vec<Mutex<Option<u64>>>> =
        Arc::new((0..DEPTH).map(|_| Mutex::new(None)).collect());

    let head = scheduler.spawn_native_root({
        let pids = Arc::clone(&pids);
        Box::new(move || {
            Box::new(ChainNode {
                depth: 0,
                max_depth: DEPTH,
                started: false,
                pids: Arc::clone(&pids),
                crash_on_message: true,
            })
        })
    });
    *pids[0].lock().expect("chain pid lock") = Some(head);

    // Settle the chain: head spawns child (depth 1), which spawns grandchild
    // (depth 2); each links to its parent and parks. One turn per level.
    for _ in 0..=DEPTH {
        let _settle = scheduler.run_native_until_idle();
    }
    let child = pids[1]
        .lock()
        .expect("chain pid lock")
        .expect("a child pid");
    let grandchild = pids[2]
        .lock()
        .expect("chain pid lock")
        .expect("a grandchild pid");
    assert_eq!(
        scheduler.native_exit_reason(child),
        None,
        "the child is alive before the crash"
    );
    assert_eq!(
        scheduler.native_exit_reason(grandchild),
        None,
        "the grandchild is alive before the crash"
    );

    // Crash the head: the abnormal link signal must cascade head -> child ->
    // grandchild, all three exiting with Error (Error has no Kill->Killed remap).
    scheduler
        .send_owned(
            head,
            &crate::ets::OwnedTerm::immediate(Term::small_int(CMD_CRASH)),
        )
        .expect("the crash trigger delivers to the head");
    assert!(
        drain_until_exit(&mut scheduler, head, 4),
        "the head exits after receiving its crash trigger"
    );

    assert_eq!(
        scheduler.native_exit_reason(head),
        Some(ExitReason::Error),
        "the head crashed abnormally"
    );
    assert_eq!(
        scheduler.native_exit_reason(child),
        Some(ExitReason::Error),
        "the directly-linked child died from the head's abnormal exit"
    );
    assert_eq!(
        scheduler.native_exit_reason(grandchild),
        Some(ExitReason::Error),
        "the grandchild died from the TRANSITIVE cascade through the child"
    );
}

// ---------------------------------------------------------------------------
// WR-7: async host I/O via the async-NIF seam on the cooperative scheduler.
// ---------------------------------------------------------------------------

use std::cell::RefCell;
use std::rc::Rc;

use crate::native::{NativeKey, ProcessContext, WasmAsyncNifFacility};

/// MFA key the WR-7 test handler names when starting host async work. The
/// values are arbitrary; the fake facility matches on this exact key.
const ASYNC_MFA: NativeKey = (Atom::OK, Atom::ERROR, 1);

/// A fake [`WasmAsyncNifFacility`] standing in for a browser host (no real I/O).
///
/// `start_async_nif` records the call (so the test can assert the SAME seam the
/// bytecode path uses actually fired) and returns `Ok(NIL)` WITHOUT resolving —
/// it does not call `complete_async`. The test plays the host: it later calls
/// `scheduler.complete_async(pid, …)` itself, exactly as the real
/// `spawn_local`/`JsFuture` host callback would on Promise settlement. Single
/// threaded, so it records through `Rc<RefCell<…>>` (the facility is held as an
/// `Rc`, never crossing a thread).
struct FakeHost {
    started: Rc<RefCell<Vec<(NativeKey, usize)>>>,
}

impl WasmAsyncNifFacility for FakeHost {
    fn start_async_nif(
        &self,
        mfa: NativeKey,
        args: &[Term],
        context: &mut ProcessContext<'_>,
    ) -> Result<Term, Term> {
        // The seam hands us the running process's pid; record it so the test can
        // confirm the host was driven against the right process.
        let _pid = context.pid();
        self.started.borrow_mut().push((mfa, args.len()));
        Ok(Term::NIL)
    }
}

/// A native handler that starts one host async op and parks pending completion,
/// then resumes on the delivered `{ok, Value}` / `{error, Reason}` mailbox
/// message. `started` records that `start_async` succeeded; `outcome` records
/// the resumed result as `(is_ok, value)`.
struct AsyncCaller {
    started_ok: Arc<Mutex<Option<bool>>>,
    outcome: Arc<Mutex<Option<(bool, i64)>>>,
    issued: bool,
}

impl NativeHandler for AsyncCaller {
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome {
        if !self.issued {
            self.issued = true;
            let started = ctx.start_async(ASYNC_MFA, &[Term::small_int(5)]).is_ok();
            *self.started_ok.lock().expect("started lock") = Some(started);
            // After a successful start we MUST park pending the host completion.
            return NativeOutcome::Wait;
        }
        // Resumed: the completion was delivered as `{tag, Value}`.
        let Some(message) = ctx.recv() else {
            return NativeOutcome::Wait;
        };
        let tuple = crate::term::boxed::Tuple::new(message)
            .expect("the async completion is delivered as a {tag, Value} tuple");
        let is_ok = tuple.get(0).and_then(Term::as_atom) == Some(Atom::OK);
        let value = tuple
            .get(1)
            .and_then(Term::as_small_int)
            .expect("completion payload is a small int in this test");
        *self.outcome.lock().expect("outcome lock") = Some((is_ok, value));
        NativeOutcome::Stop(ExitReason::Normal)
    }
}

#[test]
fn native_handler_starts_host_async_op_suspends_and_resumes_on_completion() {
    // WR-7: a NativeHandler starts host async work through the SAME
    // `WasmAsyncNifFacility` the bytecode async-NIF path uses, then suspends.
    // Pumping turns does NOT resolve it (no host completion yet). The test, as
    // the host, calls `complete_async`; pumping then resumes the handler, which
    // observes the async result delivered to its mailbox as `{ok, Value}`.
    let mut scheduler = scheduler();
    let started = Rc::new(RefCell::new(Vec::new()));
    let facility: Rc<dyn WasmAsyncNifFacility> = Rc::new(FakeHost {
        started: Rc::clone(&started),
    });
    scheduler.set_wasm_async_nif_facility(Some(facility));

    let started_ok = Arc::new(Mutex::new(None));
    let outcome = Arc::new(Mutex::new(None));
    let pid = scheduler.spawn_native_root({
        let started_ok = Arc::clone(&started_ok);
        let outcome = Arc::clone(&outcome);
        Box::new(move || {
            Box::new(AsyncCaller {
                started_ok: Arc::clone(&started_ok),
                outcome: Arc::clone(&outcome),
                issued: false,
            })
        })
    });

    // Turn 1: the handler starts the host op and parks. It must report waiting
    // (not exited), and the host seam must have fired exactly once with our MFA.
    let summary = scheduler.run_until_idle();
    assert!(
        summary.waiting.contains(&pid),
        "the handler parked pending the async completion"
    );
    assert!(
        !summary.exited.contains(&pid),
        "the handler did NOT exit/return a value yet"
    );
    assert!(scheduler.waiting.contains(&pid), "process is parked");
    assert_eq!(
        *started_ok.lock().expect("started lock"),
        Some(true),
        "start_async succeeded through the installed host facility"
    );
    assert_eq!(
        *started.borrow(),
        vec![(ASYNC_MFA, 1usize)],
        "the SAME async-NIF seam fired once with the handler's MFA and one arg"
    );

    // Pumping more turns must NOT resolve it: no host completion has arrived, so
    // the process stays parked and produces no outcome.
    for _ in 0..3 {
        let summary = scheduler.run_until_idle();
        assert!(
            !summary.exited.contains(&pid),
            "no completion => the handler stays suspended across turns"
        );
    }
    assert_eq!(
        *outcome.lock().expect("outcome lock"),
        None,
        "the handler has observed no result before the host completes the op"
    );
    assert!(
        scheduler.waiting.contains(&pid),
        "still parked before completion"
    );

    // Act as the host: complete the async op with `{ok, 5}`.
    let completed = scheduler.complete_async(
        pid,
        WasmAsyncCompletion::Ok(crate::ets::OwnedTerm::immediate(Term::small_int(5))),
    );
    assert!(completed, "complete_async wakes the parked native process");

    // Pump: the handler resumes and observes the delivered completion.
    assert!(
        drain_run_until_idle(&mut scheduler, pid, 4),
        "the handler resumes and exits after the completion is delivered"
    );
    assert_eq!(
        scheduler.native_exit_reason(pid),
        Some(ExitReason::Normal),
        "the resumed handler stopped normally"
    );
    assert_eq!(
        *outcome.lock().expect("outcome lock"),
        Some((true, 5)),
        "the handler resumed and observed the async result as {{ok, 5}}"
    );
}

#[test]
fn native_async_rejection_is_delivered_as_error_completion() {
    // WR-7 (rejection path): a host rejection is delivered to a parked native
    // handler as `{error, Reason}`, the same pid-keyed `complete_async` seam,
    // and the handler resumes observing it.
    let mut scheduler = scheduler();
    let started = Rc::new(RefCell::new(Vec::new()));
    let facility: Rc<dyn WasmAsyncNifFacility> = Rc::new(FakeHost {
        started: Rc::clone(&started),
    });
    scheduler.set_wasm_async_nif_facility(Some(facility));

    let started_ok = Arc::new(Mutex::new(None));
    let outcome = Arc::new(Mutex::new(None));
    let pid = scheduler.spawn_native_root({
        let started_ok = Arc::clone(&started_ok);
        let outcome = Arc::clone(&outcome);
        Box::new(move || {
            Box::new(AsyncCaller {
                started_ok: Arc::clone(&started_ok),
                outcome: Arc::clone(&outcome),
                issued: false,
            })
        })
    });

    // Park the handler on the async op.
    let summary = scheduler.run_until_idle();
    assert!(summary.waiting.contains(&pid), "handler parked on the op");

    // Host rejects with `{error, 7}`.
    let completed = scheduler.complete_async(
        pid,
        WasmAsyncCompletion::Error(crate::ets::OwnedTerm::immediate(Term::small_int(7))),
    );
    assert!(completed, "complete_async wakes the parked native process");

    assert!(
        drain_run_until_idle(&mut scheduler, pid, 4),
        "the handler resumes after the rejection is delivered"
    );
    assert_eq!(
        *outcome.lock().expect("outcome lock"),
        Some((false, 7)),
        "the handler observed the rejection as {{error, 7}}"
    );
}

#[test]
fn start_async_without_facility_errors_and_does_not_park() {
    // WR-7 negative: with no host facility installed, `start_async` returns Err
    // (undef). The handler is given a `start_async` that fails; this asserts the
    // seam fails closed rather than silently parking forever.
    let mut scheduler = scheduler();
    let started_ok = Arc::new(Mutex::new(None));
    let outcome = Arc::new(Mutex::new(None));
    let _pid = scheduler.spawn_native_root({
        let started_ok = Arc::clone(&started_ok);
        let outcome = Arc::clone(&outcome);
        Box::new(move || {
            Box::new(AsyncCaller {
                started_ok: Arc::clone(&started_ok),
                outcome: Arc::clone(&outcome),
                issued: false,
            })
        })
    });

    // The handler still returns Wait after the failed start (its control flow),
    // but the key assertion is that start_async reported failure.
    let _summary = scheduler.run_until_idle();
    assert_eq!(
        *started_ok.lock().expect("started lock"),
        Some(false),
        "start_async fails closed when no WasmAsyncNifFacility is installed"
    );
}
