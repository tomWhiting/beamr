//! Gate test for the cross-process local send fix (`LocalSendFacility`).
//!
//! The bug: a local `B ! Msg` driven through the real `Send` opcode silently
//! dropped, because `messaging::send` only delivered to an in-hand `receiver`
//! and the scheduler always passed `None`. `B`'s `receive` then timed out.
//!
//! This test boots a real multi-threaded scheduler, spawns `B` (which runs a
//! `receive Msg -> Msg end`), then spawns `A` with `B`'s PID as its argument so
//! that `A` performs `B ! Msg` THROUGH THE REAL `Send` OPCODE (not
//! `dispatch_with_receiver`, not a hand-built `MailboxSender`). It asserts `B`
//! actually receives the message, and separately that the receive timeout path
//! still fires when nothing is sent.
//!
//! This test fails on `main` (B times out / the message is dropped) and passes
//! with the fix.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use beamr::atom::{Atom, AtomTable};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ModuleOrigin, ModuleRegistry};
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

/// `receiver/0`: `receive Msg -> Msg end` — blocks forever until a message
/// arrives, then returns it in x(0).
fn receiver_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("gate_receiver");
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

/// `receiver_after/0`: `receive Msg -> Msg after 100 -> timed_out end`.
fn receiver_after_module(atoms: &AtomTable) -> Module {
    let name = atoms.intern("gate_receiver_after");
    let function = atoms.intern("loop");
    let timed_out = atoms.intern("timed_out");
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
            Instruction::WaitTimeout {
                fail: Operand::Label(10),
                timeout: Operand::Unsigned(100),
            },
            Instruction::Timeout,
            Instruction::Move {
                source: Operand::Atom(Some(timed_out)),
                destination: Operand::X(0),
            },
            Instruction::Return,
        ],
    )
}

/// `sender/1`: receives `B`'s PID in x(0), moves the message atom into x(1),
/// then runs the real `Send` opcode (`B ! Msg`) and returns.
fn sender_module(atoms: &AtomTable, message: Atom) -> Module {
    let name = atoms.intern("gate_sender");
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

#[test]
fn cross_process_send_via_real_scheduler_delivers() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(receiver_module(&atoms));
    let ping = atoms.intern("ping");
    registry.insert(sender_module(&atoms, ping));

    let scheduler = Arc::new(
        Scheduler::new(SchedulerConfig::default(), Arc::clone(&registry))
            .unwrap_or_else(|error| panic!("scheduler starts: {error}")),
    );

    let receiver_mod = atoms.intern("gate_receiver");
    let receiver_fn = atoms.intern("loop");
    let sender_mod = atoms.intern("gate_sender");
    let sender_fn = atoms.intern("fire");

    // Spawn B (the receiver), then A (the sender) with B's PID as its argument.
    let b_pid = scheduler
        .spawn(receiver_mod, receiver_fn, Vec::new())
        .expect("spawn receiver");

    // Give B a moment to reach its receive and park.
    std::thread::sleep(Duration::from_millis(50));

    let _a_pid = scheduler
        .spawn(
            sender_mod,
            sender_fn,
            vec![Term::try_pid(b_pid).expect("receiver pid fits")],
        )
        .expect("spawn sender");

    // B must complete its receive (return the delivered message). A watchdog
    // thread bounds the wait so the pre-fix drop fails the test rather than
    // hanging it.
    let (sender, completion) = std::sync::mpsc::channel();
    let scheduler_for_wait = Arc::clone(&scheduler);
    std::thread::spawn(move || {
        let _ = sender.send(scheduler_for_wait.run_until_exit(b_pid));
    });
    let (reason, result) = completion
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| {
            panic!("receiver never got the cross-process message (silent drop regression)")
        });

    assert_eq!(reason, ExitReason::Normal);
    assert_eq!(
        result.root(),
        Term::atom(ping),
        "B should receive the exact message A sent via the Send opcode"
    );

    scheduler.shutdown();
}

#[test]
fn receive_timeout_path_still_fires_when_no_send() {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let registry = Arc::new(ModuleRegistry::new());
    registry.insert(receiver_after_module(&atoms));

    let scheduler = Arc::new(
        Scheduler::new(SchedulerConfig::default(), Arc::clone(&registry))
            .unwrap_or_else(|error| panic!("scheduler starts: {error}")),
    );

    let receiver_mod = atoms.intern("gate_receiver_after");
    let receiver_fn = atoms.intern("loop");
    let pid = scheduler
        .spawn(receiver_mod, receiver_fn, Vec::new())
        .expect("spawn receiver_after");

    let (sender, completion) = std::sync::mpsc::channel();
    let scheduler_for_wait = Arc::clone(&scheduler);
    std::thread::spawn(move || {
        let _ = sender.send(scheduler_for_wait.run_until_exit(pid));
    });
    let (reason, result) = completion
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("receive-after timeout never fired"));

    assert_eq!(reason, ExitReason::Normal);
    let timed_out = atoms.intern("timed_out");
    assert_eq!(
        result.root(),
        Term::atom(timed_out),
        "with no send, the after-clause must run"
    );

    scheduler.shutdown();
}
