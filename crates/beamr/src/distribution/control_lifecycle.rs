//! Distribution control-message dispatch table and cross-node lifecycle state.
//!
//! The external distribution protocol represents control operations as tuples
//! whose first element is the numeric operation code. This module keeps that
//! numeric table explicit, ignores unknown future opcodes, and provides small
//! seams that the scheduler/connection layers can plug into as distribution is
//! completed.

use crate::{
    atom::Atom,
    process::ExitReason,
    term::{Term, boxed::Tuple, pid_ref::PidRef},
};

/// Distribution control operation codes understood by beamr.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum ControlOp {
    /// LINK: `{1, FromPid, ToPid}`.
    Link = 1,
    /// SEND: `{2, Cookie, ToPid}`.
    Send = 2,
    /// EXIT: `{3, FromPid, ToPid, Reason}`.
    Exit = 3,
    /// UNLINK: `{4, FromPid, ToPid}`.
    Unlink = 4,
    /// REG_SEND: `{6, FromPid, Cookie, ToName}`.
    RegSend = 6,
    /// EXIT2: `{8, FromPid, ToPid, Reason}`.
    Exit2 = 8,
    /// MONITOR_P: `{19, FromPid, ToPid, Ref}`.
    MonitorP = 19,
    /// DEMONITOR_P: `{20, FromPid, ToPid, Ref}`.
    DemonitorP = 20,
    /// MONITOR_P_EXIT: `{21, FromPid, ToPid, Ref, Reason}`.
    MonitorPExit = 21,
    /// SPAWN_REQUEST: `{29, ...}`.
    SpawnRequest = 29,
    /// SPAWN_REPLY: `{31, ...}`.
    SpawnReply = 31,
}

impl ControlOp {
    /// Decode a numeric control opcode.
    #[must_use]
    pub const fn from_opcode(opcode: i64) -> Option<Self> {
        match opcode {
            1 => Some(Self::Link),
            2 => Some(Self::Send),
            3 => Some(Self::Exit),
            4 => Some(Self::Unlink),
            6 => Some(Self::RegSend),
            8 => Some(Self::Exit2),
            19 => Some(Self::MonitorP),
            20 => Some(Self::DemonitorP),
            21 => Some(Self::MonitorPExit),
            29 => Some(Self::SpawnRequest),
            31 => Some(Self::SpawnReply),
            _ => None,
        }
    }

    /// Numeric opcode written on the wire.
    #[must_use]
    pub const fn opcode(self) -> i64 {
        self as i64
    }
}

/// Collision-safe process identity for distribution lifecycle state.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DistributedPid {
    /// Remote node for external PIDs; `None` means this is a local immediate PID.
    pub node: Option<Atom>,
    /// PID number component.
    pub pid_number: u64,
    /// PID serial component. Local immediate PIDs use serial zero.
    pub serial: u64,
}

impl DistributedPid {
    /// Convert a local or remote PID term into a stable identity.
    pub fn from_term(term: Term) -> Option<Self> {
        let pid = PidRef::new(term)?;
        Some(Self {
            node: pid.node(),
            pid_number: pid.pid_number(),
            serial: pid.serial(),
        })
    }

    /// Construct a local PID identity.
    #[must_use]
    pub const fn local(pid_number: u64) -> Self {
        Self { node: None, pid_number, serial: 0 }
    }

    /// Construct a remote PID identity.
    #[must_use]
    pub const fn remote(node: Atom, pid_number: u64, serial: u64) -> Self {
        Self { node: Some(node), pid_number, serial }
    }
}

/// Outbound control message captured before distribution framing/ETF encoding.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutboundControlMessage {
    /// Operation to send.
    pub op: ControlOp,
    /// Source PID term.
    pub from: Term,
    /// Destination PID term.
    pub to: Term,
    /// Exit reason for EXIT/EXIT2 messages.
    pub reason: Option<ExitReason>,
}

impl OutboundControlMessage {
    /// Build a LINK control.
    #[must_use]
    pub const fn link(from: Term, to: Term) -> Self {
        Self { op: ControlOp::Link, from, to, reason: None }
    }

    /// Build an UNLINK control.
    #[must_use]
    pub const fn unlink(from: Term, to: Term) -> Self {
        Self { op: ControlOp::Unlink, from, to, reason: None }
    }

    /// Build an EXIT control propagated through a link.
    #[must_use]
    pub const fn exit(from: Term, to: Term, reason: ExitReason) -> Self {
        Self { op: ControlOp::Exit, from, to, reason: Some(reason) }
    }

    /// Build an EXIT2 control for explicit `exit/2` delivery.
    #[must_use]
    pub const fn exit2(from: Term, to: Term, reason: ExitReason) -> Self {
        Self { op: ControlOp::Exit2, from, to, reason: Some(reason) }
    }
}

/// Sink used by lifecycle code to hand encoded-control responsibility to the
/// distribution connection layer.
pub trait ControlMessageSink {
    /// Send a control message to the given remote node.
    fn send_control(
        &mut self,
        node: Atom,
        message: OutboundControlMessage,
    ) -> Result<(), LifecycleError>;
}

/// Errors from lifecycle operations on control messages.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    /// The term was not a valid control tuple for the requested operation.
    MalformedControl,
    /// A PID argument was expected to name a remote node but did not.
    NotRemotePid,
    /// The sink failed to enqueue/write the control message.
    SendFailed,
}

/// Diagnostics hook for forward-compatible ignored messages.
pub trait ControlDiagnostics {
    /// Called when a future/unknown opcode is ignored.
    fn unknown_opcode(&mut self, opcode: i64);
    /// Called when a known opcode has a malformed control tuple.
    fn malformed_control(&mut self, op: Option<ControlOp>);
}

/// No-op diagnostics used in production until the crate adopts a logging facade.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoopDiagnostics;

impl ControlDiagnostics for NoopDiagnostics {
    fn unknown_opcode(&mut self, _opcode: i64) {}
    fn malformed_control(&mut self, _op: Option<ControlOp>) {}
}

/// Handler table for decoded incoming control messages.
pub trait ControlMessageHandler {
    /// Basic SEND.
    fn handle_send(&mut self, _tuple: Tuple) {}
    /// Registered SEND.
    fn handle_reg_send(&mut self, _tuple: Tuple) {}
    /// LINK establishes a cross-node link on the local side.
    fn handle_link(
        &mut self, _from: DistributedPid, _from_term: Term,
        _to: DistributedPid, _to_term: Term,
    ) {}
    /// UNLINK removes a cross-node link on the local side.
    fn handle_unlink(&mut self, _from: DistributedPid, _to: DistributedPid) {}
    /// EXIT propagates a linked-process exit.
    fn handle_exit(&mut self, _from: DistributedPid, _to: DistributedPid, _r: ExitReason) {}
    /// EXIT2 delivers an explicit exit signal.
    fn handle_exit2(&mut self, _from: DistributedPid, _to: DistributedPid, _r: ExitReason) {}
    /// MONITOR_P stub.
    fn handle_monitor_p(&mut self, _tuple: Tuple) {}
    /// DEMONITOR_P stub.
    fn handle_demonitor_p(&mut self, _tuple: Tuple) {}
    /// MONITOR_P_EXIT stub.
    fn handle_monitor_p_exit(&mut self, _tuple: Tuple) {}
    /// SPAWN_REQUEST stub.
    fn handle_spawn_request(&mut self, _tuple: Tuple) {}
    /// SPAWN_REPLY stub.
    fn handle_spawn_reply(&mut self, _tuple: Tuple) {}
}

/// Dispatch a decoded control tuple to the handler for its opcode.
///
/// Unknown opcodes are deliberately ignored after notifying diagnostics so new
/// peers can add protocol operations without breaking older beamr nodes.
pub fn dispatch_control_message<H, D>(term: Term, handler: &mut H, diagnostics: &mut D)
where
    H: ControlMessageHandler,
    D: ControlDiagnostics,
{
    let Some(tuple) = Tuple::new(term) else {
        diagnostics.malformed_control(None);
        return;
    };
    let Some(opcode) = tuple.get(0).and_then(Term::as_small_int) else {
        diagnostics.malformed_control(None);
        return;
    };
    let Some(op) = ControlOp::from_opcode(opcode) else {
        diagnostics.unknown_opcode(opcode);
        return;
    };
    if !dispatch_known(tuple, op, handler) {
        diagnostics.malformed_control(Some(op));
    }
}

fn dispatch_known<H: ControlMessageHandler>(
    tuple: Tuple, op: ControlOp, h: &mut H,
) -> bool {
    match op {
        ControlOp::Send => { h.handle_send(tuple); true }
        ControlOp::RegSend => { h.handle_reg_send(tuple); true }
        ControlOp::Link => dispatch_link(tuple, h),
        ControlOp::Exit => dispatch_exit(tuple, h, false),
        ControlOp::Unlink => dispatch_unlink(tuple, h),
        ControlOp::MonitorP => { h.handle_monitor_p(tuple); true }
        ControlOp::DemonitorP => { h.handle_demonitor_p(tuple); true }
        ControlOp::MonitorPExit => { h.handle_monitor_p_exit(tuple); true }
        ControlOp::Exit2 => dispatch_exit(tuple, h, true),
        ControlOp::SpawnRequest => { h.handle_spawn_request(tuple); true }
        ControlOp::SpawnReply => { h.handle_spawn_reply(tuple); true }
    }
}

fn dispatch_link<H: ControlMessageHandler>(tuple: Tuple, h: &mut H) -> bool {
    let Some((from, ft, to, tt)) = pid_pair_terms(tuple) else { return false };
    h.handle_link(from, ft, to, tt);
    true
}

fn dispatch_unlink<H: ControlMessageHandler>(tuple: Tuple, h: &mut H) -> bool {
    let Some((from, to)) = pid_pair(tuple) else { return false };
    h.handle_unlink(from, to);
    true
}

fn dispatch_exit<H: ControlMessageHandler>(
    tuple: Tuple, h: &mut H, explicit: bool,
) -> bool {
    let Some((from, to)) = pid_pair(tuple) else { return false };
    let Some(reason) = tuple.get(3).and_then(exit_reason_from_term) else {
        return false;
    };
    if explicit { h.handle_exit2(from, to, reason); }
    else { h.handle_exit(from, to, reason); }
    true
}

fn pid_pair(tuple: Tuple) -> Option<(DistributedPid, DistributedPid)> {
    let (from, _, to, _) = pid_pair_terms(tuple)?;
    Some((from, to))
}

fn pid_pair_terms(
    tuple: Tuple,
) -> Option<(DistributedPid, Term, DistributedPid, Term)> {
    if tuple.arity() < 3 { return None; }
    let ft = tuple.get(1)?;
    let tt = tuple.get(2)?;
    Some((DistributedPid::from_term(ft)?, ft, DistributedPid::from_term(tt)?, tt))
}

/// Convert currently-supported exit reason atoms into runtime exit reasons.
#[must_use]
pub fn exit_reason_from_term(term: Term) -> Option<ExitReason> {
    match term.as_atom()? {
        Atom::NORMAL => Some(ExitReason::Normal),
        Atom::KILL => Some(ExitReason::Kill),
        Atom::KILLED => Some(ExitReason::Killed),
        Atom::ERROR => Some(ExitReason::Error),
        Atom::NOCONNECTION => Some(ExitReason::NoConnection),
        _ => None,
    }
}

/// Testable local lifecycle state for cross-node links and incoming exit signals.
#[derive(Clone, Debug, Default)]
pub struct ControlLifecycleState {
    links: Vec<RemoteLink>,
    delivered_exits: Vec<(DistributedPid, DistributedPid, ExitReason)>,
}

/// Stored cross-node link retaining both original PID terms for outbound control.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RemoteLink {
    /// Left side identity.
    pub left: DistributedPid,
    /// Original left-side PID term.
    pub left_term: Term,
    /// Right side identity.
    pub right: DistributedPid,
    /// Original right-side PID term.
    pub right_term: Term,
}

impl ControlLifecycleState {
    /// Establish a link while preserving insertion order and suppressing duplicates.
    pub fn establish_link(
        &mut self, left: DistributedPid, right: DistributedPid,
    ) -> bool {
        let lt = pid_term(left).unwrap_or(Term::NIL);
        let rt = pid_term(right).unwrap_or(Term::NIL);
        self.establish_link_terms(left, lt, right, rt)
    }

    /// Establish a link retaining the original PID terms for outbound controls.
    pub fn establish_link_terms(
        &mut self, left: DistributedPid, left_term: Term,
        right: DistributedPid, right_term: Term,
    ) -> bool {
        if left == right || self.has_link(left, right) { return false; }
        self.links.push(RemoteLink { left, left_term, right, right_term });
        true
    }

    /// Remove a link in either direction.
    pub fn remove_link(
        &mut self, left: DistributedPid, right: DistributedPid,
    ) -> bool {
        let before = self.links.len();
        self.links.retain(|lk| !same_link(lk.left, lk.right, left, right));
        before != self.links.len()
    }

    /// Return true when a link exists in either direction.
    #[must_use]
    pub fn has_link(&self, left: DistributedPid, right: DistributedPid) -> bool {
        self.links.iter().any(|lk| same_link(lk.left, lk.right, left, right))
    }

    /// Linked pairs in insertion order.
    #[must_use]
    pub fn links(&self) -> &[RemoteLink] { &self.links }

    /// Record delivery of an inbound EXIT/EXIT2 signal.
    pub fn deliver_exit(
        &mut self, from: DistributedPid, to: DistributedPid, reason: ExitReason,
    ) {
        self.delivered_exits.push((from, to, reason));
        self.remove_link(from, to);
    }

    /// Delivered exit signals in arrival order.
    #[must_use]
    pub fn delivered_exits(
        &self,
    ) -> &[(DistributedPid, DistributedPid, ExitReason)] {
        &self.delivered_exits
    }
}

impl ControlMessageHandler for ControlLifecycleState {
    fn handle_link(
        &mut self, from: DistributedPid, ft: Term, to: DistributedPid, tt: Term,
    ) {
        self.establish_link_terms(from, ft, to, tt);
    }
    fn handle_unlink(&mut self, from: DistributedPid, to: DistributedPid) {
        self.remove_link(from, to);
    }
    fn handle_exit(
        &mut self, from: DistributedPid, to: DistributedPid, reason: ExitReason,
    ) {
        self.deliver_exit(from, to, reason);
    }
    fn handle_exit2(
        &mut self, from: DistributedPid, to: DistributedPid, reason: ExitReason,
    ) {
        self.deliver_exit(from, to, reason);
    }
}

fn same_link(
    sl: DistributedPid, sr: DistributedPid, l: DistributedPid, r: DistributedPid,
) -> bool {
    (sl == l && sr == r) || (sl == r && sr == l)
}

fn pid_term(pid: DistributedPid) -> Option<Term> {
    if pid.node.is_none() { Term::try_pid(pid.pid_number) } else { None }
}

/// Send a LINK control to a remote PID and record the local half of the link.
pub fn link_remote<S: ControlMessageSink>(
    sink: &mut S, state: &mut ControlLifecycleState,
    local_pid: Term, remote_pid: Term,
) -> Result<(), LifecycleError> {
    let local = DistributedPid::from_term(local_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let remote = DistributedPid::from_term(remote_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let node = remote.node.ok_or(LifecycleError::NotRemotePid)?;
    sink.send_control(node, OutboundControlMessage::link(local_pid, remote_pid))?;
    state.establish_link_terms(local, local_pid, remote, remote_pid);
    Ok(())
}

/// Send an UNLINK control to a remote PID and remove local lifecycle state.
pub fn unlink_remote<S: ControlMessageSink>(
    sink: &mut S, state: &mut ControlLifecycleState,
    local_pid: Term, remote_pid: Term,
) -> Result<(), LifecycleError> {
    let local = DistributedPid::from_term(local_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let remote = DistributedPid::from_term(remote_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let node = remote.node.ok_or(LifecycleError::NotRemotePid)?;
    sink.send_control(
        node, OutboundControlMessage::unlink(local_pid, remote_pid),
    )?;
    state.remove_link(local, remote);
    Ok(())
}

/// Send linked-process EXIT controls for every remote peer linked to
/// `exiting_pid`.
pub fn propagate_remote_exit<S: ControlMessageSink>(
    sink: &mut S, state: &mut ControlLifecycleState,
    exiting_pid: Term, reason: ExitReason,
) -> Result<(), LifecycleError> {
    let source = DistributedPid::from_term(exiting_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let peers: Vec<(DistributedPid, Term)> = state
        .links().iter()
        .filter_map(|link| {
            if link.left == source {
                Some((link.right, link.right_term))
            } else if link.right == source {
                Some((link.left, link.left_term))
            } else {
                None
            }
        })
        .collect();
    for (peer, peer_term) in peers {
        let Some(node) = peer.node else { continue };
        if peer_term == Term::NIL {
            return Err(LifecycleError::MalformedControl);
        }
        sink.send_control(
            node,
            OutboundControlMessage::exit(exiting_pid, peer_term, reason),
        )?;
        state.remove_link(source, peer);
    }
    Ok(())
}

/// Send an explicit EXIT2 control to a remote PID.
pub fn exit2_remote<S: ControlMessageSink>(
    sink: &mut S, from_pid: Term, remote_pid: Term, reason: ExitReason,
) -> Result<(), LifecycleError> {
    let remote = DistributedPid::from_term(remote_pid)
        .ok_or(LifecycleError::MalformedControl)?;
    let node = remote.node.ok_or(LifecycleError::NotRemotePid)?;
    sink.send_control(
        node,
        OutboundControlMessage::exit2(from_pid, remote_pid, reason),
    )
}

#[cfg(test)]
#[path = "control_lifecycle_tests.rs"] mod tests;
