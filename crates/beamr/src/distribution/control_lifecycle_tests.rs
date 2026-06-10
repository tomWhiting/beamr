use super::*;
use crate::term::boxed::{write_external_pid, write_tuple};

#[derive(Default)]
struct RecordingDiagnostics {
    unknown: Vec<i64>,
    malformed: Vec<Option<ControlOp>>,
}

impl ControlDiagnostics for RecordingDiagnostics {
    fn unknown_opcode(&mut self, opcode: i64) {
        self.unknown.push(opcode);
    }
    fn malformed_control(&mut self, op: Option<ControlOp>) {
        self.malformed.push(op);
    }
}

#[derive(Default)]
struct RecordingHandler {
    calls: Vec<ControlOp>,
}

impl ControlMessageHandler for RecordingHandler {
    fn handle_send(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::Send);
    }
    fn handle_reg_send(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::RegSend);
    }
    fn handle_link(&mut self, _from: DistributedPid, _ft: Term, _to: DistributedPid, _tt: Term) {
        self.calls.push(ControlOp::Link);
    }
    fn handle_unlink(&mut self, _from: DistributedPid, _to: DistributedPid) {
        self.calls.push(ControlOp::Unlink);
    }
    fn handle_exit(&mut self, _from: DistributedPid, _to: DistributedPid, _r: ExitReason) {
        self.calls.push(ControlOp::Exit);
    }
    fn handle_exit2(&mut self, _from: DistributedPid, _to: DistributedPid, _r: ExitReason) {
        self.calls.push(ControlOp::Exit2);
    }
    fn handle_monitor_p(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::MonitorP);
    }
    fn handle_demonitor_p(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::DemonitorP);
    }
    fn handle_monitor_p_exit(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::MonitorPExit);
    }
    fn handle_spawn_request(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::SpawnRequest);
    }
    fn handle_spawn_reply(&mut self, _tuple: Tuple) {
        self.calls.push(ControlOp::SpawnReply);
    }
}

#[derive(Default)]
struct RecordingSink {
    sent: Vec<(Atom, OutboundControlMessage)>,
}

impl ControlMessageSink for RecordingSink {
    fn send_control(
        &mut self,
        node: Atom,
        message: OutboundControlMessage,
    ) -> Result<(), LifecycleError> {
        self.sent.push((node, message));
        Ok(())
    }
}

fn local_pid(pid: u64) -> Term {
    Term::pid(pid)
}

fn remote_pid(heap: &mut [u64], node: Atom, pid: u64) -> Term {
    write_external_pid(heap, node, pid, 0).expect("external pid fits")
}

fn tuple(heap: &mut [u64], elements: &[Term]) -> Term {
    write_tuple(heap, elements).expect("tuple fits")
}

fn control_tuple(heap: &mut [u64], op: ControlOp) -> Term {
    let elements = match op {
        ControlOp::Send => [
            Term::small_int(op.opcode()),
            Term::atom(Atom::NIL),
            local_pid(2),
            Term::NIL,
            Term::NIL,
        ],
        ControlOp::RegSend => [
            Term::small_int(op.opcode()),
            local_pid(1),
            Term::atom(Atom::NIL),
            Term::atom(Atom::OK),
            Term::NIL,
        ],
        ControlOp::Link | ControlOp::Unlink => [
            Term::small_int(op.opcode()),
            local_pid(1),
            local_pid(2),
            Term::NIL,
            Term::NIL,
        ],
        ControlOp::Exit | ControlOp::Exit2 => [
            Term::small_int(op.opcode()),
            local_pid(1),
            local_pid(2),
            Term::atom(Atom::ERROR),
            Term::NIL,
        ],
        ControlOp::MonitorP | ControlOp::DemonitorP => [
            Term::small_int(op.opcode()),
            local_pid(1),
            local_pid(2),
            Term::small_int(123),
            Term::NIL,
        ],
        ControlOp::MonitorPExit => [
            Term::small_int(op.opcode()),
            local_pid(1),
            local_pid(2),
            Term::small_int(123),
            Term::atom(Atom::ERROR),
        ],
        ControlOp::SpawnRequest | ControlOp::SpawnReply => [
            Term::small_int(op.opcode()),
            local_pid(1),
            local_pid(2),
            Term::NIL,
            Term::NIL,
        ],
    };
    tuple(heap, &elements)
}

#[test]
fn control_op_mapping_is_exact() {
    assert_eq!(ControlOp::from_opcode(1), Some(ControlOp::Link));
    assert_eq!(ControlOp::from_opcode(2), Some(ControlOp::Send));
    assert_eq!(ControlOp::from_opcode(3), Some(ControlOp::Exit));
    assert_eq!(ControlOp::from_opcode(4), Some(ControlOp::Unlink));
    assert_eq!(ControlOp::from_opcode(6), Some(ControlOp::RegSend));
    assert_eq!(ControlOp::from_opcode(8), Some(ControlOp::Exit2));
    assert_eq!(ControlOp::from_opcode(19), Some(ControlOp::MonitorP));
    assert_eq!(ControlOp::from_opcode(20), Some(ControlOp::DemonitorP));
    assert_eq!(ControlOp::from_opcode(21), Some(ControlOp::MonitorPExit));
    assert_eq!(ControlOp::from_opcode(29), Some(ControlOp::SpawnRequest));
    assert_eq!(ControlOp::from_opcode(31), Some(ControlOp::SpawnReply));
    assert_eq!(ControlOp::from_opcode(255), None);
}

#[test]
fn dispatches_each_known_opcode_to_correct_handler() {
    let ops = [
        ControlOp::Send,
        ControlOp::RegSend,
        ControlOp::Link,
        ControlOp::Exit,
        ControlOp::Unlink,
        ControlOp::MonitorP,
        ControlOp::DemonitorP,
        ControlOp::MonitorPExit,
        ControlOp::Exit2,
        ControlOp::SpawnRequest,
        ControlOp::SpawnReply,
    ];
    for op in ops {
        let mut heap = [0_u64; 8];
        let term = control_tuple(&mut heap, op);
        let mut handler = RecordingHandler::default();
        let mut diagnostics = RecordingDiagnostics::default();
        dispatch_control_message(term, &mut handler, &mut diagnostics);
        assert_eq!(handler.calls, vec![op]);
        assert_eq!(diagnostics.unknown, Vec::<i64>::new());
        assert_eq!(diagnostics.malformed, Vec::<Option<ControlOp>>::new());
    }
}

#[test]
fn unknown_opcode_is_logged_and_ignored() {
    let mut heap = [0_u64; 3];
    let term = tuple(&mut heap, &[Term::small_int(999), local_pid(1)]);
    let mut handler = RecordingHandler::default();
    let mut diagnostics = RecordingDiagnostics::default();
    dispatch_control_message(term, &mut handler, &mut diagnostics);
    assert_eq!(handler.calls, Vec::<ControlOp>::new());
    assert_eq!(diagnostics.unknown, vec![999]);
    assert_eq!(diagnostics.malformed, Vec::<Option<ControlOp>>::new());
}

#[test]
fn inbound_link_and_unlink_mutate_collision_safe_link_state() {
    let mut remote_heap = [0_u64; 4];
    let remote = remote_pid(&mut remote_heap, Atom::OK, 1);
    let local = local_pid(1);
    let mut tuple_heap = [0_u64; 4];
    let link_term = tuple(
        &mut tuple_heap,
        &[Term::small_int(ControlOp::Link.opcode()), remote, local],
    );
    let mut state = ControlLifecycleState::default();
    let mut diagnostics = NoopDiagnostics;
    dispatch_control_message(link_term, &mut state, &mut diagnostics);

    let remote_id = DistributedPid::remote(Atom::OK, 1, 0);
    let local_id = DistributedPid::local(1);
    assert!(state.has_link(remote_id, local_id));
    assert!(state.has_link(local_id, remote_id));
    assert_eq!(state.links()[0].left_term, remote);
    assert_eq!(state.links()[0].right_term, local);

    let mut unlink_heap = [0_u64; 4];
    let unlink_term = tuple(
        &mut unlink_heap,
        &[Term::small_int(ControlOp::Unlink.opcode()), remote, local],
    );
    dispatch_control_message(unlink_term, &mut state, &mut diagnostics);
    assert!(!state.has_link(remote_id, local_id));
}

#[test]
fn outbound_remote_link_writes_link_control_and_records_state() {
    let mut remote_heap = [0_u64; 4];
    let remote = remote_pid(&mut remote_heap, Atom::OK, 42);
    let local = local_pid(7);
    let mut sink = RecordingSink::default();
    let mut state = ControlLifecycleState::default();
    assert_eq!(link_remote(&mut sink, &mut state, local, remote), Ok(()));
    assert_eq!(
        sink.sent,
        vec![(Atom::OK, OutboundControlMessage::link(local, remote))]
    );
    assert!(state.has_link(
        DistributedPid::local(7),
        DistributedPid::remote(Atom::OK, 42, 0)
    ));
}

#[test]
fn linked_remote_exit_produces_exit_control() {
    let mut remote_heap = [0_u64; 4];
    let remote = remote_pid(&mut remote_heap, Atom::OK, 42);
    let local = local_pid(7);
    let mut sink = RecordingSink::default();
    let mut state = ControlLifecycleState::default();
    assert_eq!(link_remote(&mut sink, &mut state, local, remote), Ok(()));
    sink.sent.clear();
    assert_eq!(
        propagate_remote_exit(&mut sink, &mut state, local, ExitReason::Error),
        Ok(())
    );
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(sink.sent[0].0, Atom::OK);
    assert_eq!(sink.sent[0].1.op, ControlOp::Exit);
    assert_eq!(sink.sent[0].1.from, local);
    assert_eq!(sink.sent[0].1.to, remote);
    assert_eq!(sink.sent[0].1.reason, Some(ExitReason::Error));
    assert!(!state.has_link(
        DistributedPid::local(7),
        DistributedPid::remote(Atom::OK, 42, 0)
    ));
}

#[test]
fn inbound_exit2_delegates_to_exit_delivery_state() {
    let mut remote_heap = [0_u64; 4];
    let remote = remote_pid(&mut remote_heap, Atom::OK, 42);
    let local = local_pid(7);
    let mut state = ControlLifecycleState::default();
    state.establish_link(
        DistributedPid::remote(Atom::OK, 42, 0),
        DistributedPid::local(7),
    );
    let mut tuple_heap = [0_u64; 5];
    let exit2_term = tuple(
        &mut tuple_heap,
        &[
            Term::small_int(ControlOp::Exit2.opcode()),
            remote,
            local,
            Term::atom(Atom::ERROR),
        ],
    );
    let mut diagnostics = NoopDiagnostics;
    dispatch_control_message(exit2_term, &mut state, &mut diagnostics);
    assert_eq!(
        state.delivered_exits(),
        &[(
            DistributedPid::remote(Atom::OK, 42, 0),
            DistributedPid::local(7),
            ExitReason::Error,
        )]
    );
    assert!(!state.has_link(
        DistributedPid::remote(Atom::OK, 42, 0),
        DistributedPid::local(7)
    ));
}

#[test]
fn explicit_exit2_to_remote_pid_writes_exit2_control() {
    let mut remote_heap = [0_u64; 4];
    let remote = remote_pid(&mut remote_heap, Atom::OK, 42);
    let local = local_pid(7);
    let mut sink = RecordingSink::default();
    assert_eq!(
        exit2_remote(&mut sink, local, remote, ExitReason::Kill),
        Ok(())
    );
    assert_eq!(
        sink.sent,
        vec![(
            Atom::OK,
            OutboundControlMessage::exit2(local, remote, ExitReason::Kill)
        )]
    );
}
