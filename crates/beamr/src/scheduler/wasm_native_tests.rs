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
use crate::native::native_process::{NativeContext, NativeHandler, NativeOutcome};
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
