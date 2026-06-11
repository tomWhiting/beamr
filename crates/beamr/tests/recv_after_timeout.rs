//! Regression tests for `receive ... after` timeout delivery.
//!
//! Two scheduler bugs made the after-clause unreachable:
//!
//! 1. `wait_timeout` recorded the fail label (the receive loop) as the
//!    timeout continuation, so timer expiry re-scanned the empty mailbox,
//!    re-executed `wait_timeout`, and re-armed the full timeout — even the
//!    basic empty-mailbox `receive ... after` spun forever and never
//!    reached its after-clause.
//! 2. Every wake path cancelled the receive wheel timer without clearing
//!    `receive_timer_ref`, so after a non-matching message the re-park
//!    skipped re-arming (`register_receive_timer` saw the stale ref) and
//!    the receive lost its timeout permanently.
//!
//! Fixture source: `tests/fixtures/recv_after_timer.erl`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use beamr::atom::AtomTable;
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::{
    BifRegistryImpl, bifs::register_gate1_bifs, gate3_bifs::register_gate3_bifs,
    process_bifs::register_gate2_bifs, stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;

fn start_scheduler(atoms: &Arc<AtomTable>) -> Arc<Scheduler> {
    let bifs = BifRegistryImpl::new();
    register_gate1_bifs(&bifs, atoms).expect("gate 1 bifs");
    register_gate2_bifs(&bifs, atoms).expect("gate 2 bifs");
    register_gate3_bifs(&bifs, atoms).expect("gate 3 bifs");
    register_stdlib_stubs(&bifs, atoms).expect("stdlib stubs");
    let registry = Arc::new(ModuleRegistry::new());
    let (_module, _report) = load_module(
        include_bytes!("fixtures/recv_after_timer.beam"),
        atoms,
        &registry,
        &bifs,
    )
    .expect("recv_after_timer fixture loads");
    Arc::new(
        Scheduler::with_code_server(
            SchedulerConfig {
                thread_count: Some(1),
                ..SchedulerConfig::default()
            },
            registry,
            Arc::clone(atoms),
            Arc::new(bifs),
        )
        .expect("scheduler starts"),
    )
}

/// Waits for the spawned function to exit, with a watchdog so a lost
/// timeout fails the test instead of hanging it.
fn run_with_watchdog(
    scheduler: &Arc<Scheduler>,
    pid: u64,
    context: &str,
) -> (ExitReason, beamr::ets::copy::OwnedTerm) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let scheduler_for_wait = Arc::clone(scheduler);
    std::thread::spawn(move || {
        let _ignored_if_watchdog_fired = sender.send(scheduler_for_wait.run_until_exit(pid));
    });
    receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("{context}: receive-after timeout never fired (lost timeout)"))
}

#[test]
fn empty_mailbox_receive_after_runs_the_after_clause() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let module = atoms.intern("recv_after_timer");
    let function = atoms.intern("plain_after");
    let pid = scheduler
        .spawn(module, function, Vec::new())
        .expect("spawn");
    let (reason, result) = run_with_watchdog(&scheduler, pid, "plain_after");
    assert_eq!(reason, ExitReason::Normal);
    let timed_out = atoms.intern("timed_out");
    assert_eq!(result.root(), Term::atom(timed_out));
    scheduler.shutdown();
}

#[test]
fn timeout_survives_a_non_matching_message_wakeup() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let module = atoms.intern("recv_after_timer");
    let function = atoms.intern("selective");
    let nomatch = atoms.intern("nomatch");
    let pid = scheduler
        .spawn(module, function, Vec::new())
        .expect("spawn");
    // Let the receive park with its 200ms timer, then wake it with a
    // message that matches no clause: the process must re-park with the
    // ORIGINAL deadline still armed.
    std::thread::sleep(Duration::from_millis(50));
    assert!(scheduler.enqueue_atom_message(pid, nomatch));
    let started = Instant::now();
    let (reason, result) = run_with_watchdog(&scheduler, pid, "selective");
    assert_eq!(reason, ExitReason::Normal);
    let timed_out = atoms.intern("timed_out");
    assert_eq!(result.root(), Term::atom(timed_out));
    // The non-matching wake must not stretch the deadline by re-arming the
    // full timeout: well under 200ms remains of the original deadline.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "timeout took implausibly long after a non-matching message"
    );
    scheduler.shutdown();
}

#[test]
fn matching_message_still_completes_the_receive() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let scheduler = start_scheduler(&atoms);
    let module = atoms.intern("recv_after_timer");
    let function = atoms.intern("selective");
    let match_atom = atoms.intern("match");
    let pid = scheduler
        .spawn(module, function, Vec::new())
        .expect("spawn");
    std::thread::sleep(Duration::from_millis(50));
    assert!(scheduler.enqueue_atom_message(pid, match_atom));
    let (reason, result) = run_with_watchdog(&scheduler, pid, "selective match");
    assert_eq!(reason, ExitReason::Normal);
    let matched = atoms.intern("matched");
    assert_eq!(result.root(), Term::atom(matched));
    scheduler.shutdown();
}
