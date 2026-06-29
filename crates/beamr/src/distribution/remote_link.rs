//! Remote link routing — `DistributionControlFacility` trait and in-memory
//! `ControlRouter` used by the scheduler to dispatch LINK/UNLINK/EXIT control
//! messages across distribution node boundaries.

use std::sync::{Arc, Mutex};

use crate::atom::Atom;
use crate::distribution::control_lifecycle::ControlOp;
use crate::process::{ExitReason, RemotePid};

/// Error returned by outbound distribution control operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RemoteLinkError {
    /// No route or connection is available to the target node.
    NoConnection,
    /// The remote endpoint does not name a process on the expected target node.
    BadTarget,
}

/// Backend used by BIFs and scheduler hooks to route distribution controls.
pub trait DistributionControlFacility: Send + Sync {
    /// Establish a remote link by sending LINK to the remote node.
    fn link_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError>;

    /// Remove a remote link by sending UNLINK to the remote node.
    fn unlink_remote(&self, caller_pid: u64, target: RemotePid) -> Result<(), RemoteLinkError>;

    /// Propagate a local process exit to a linked remote process.
    fn exit_remote(
        &self,
        caller_pid: u64,
        target: RemotePid,
        reason: ExitReason,
    ) -> Result<(), RemoteLinkError>;
}

/// Decoded or recorded remote-link control message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ControlMessage {
    /// Distribution control operation.
    pub op: ControlOp,
    /// Source endpoint.
    pub from: RemotePid,
    /// Target endpoint.
    pub to: RemotePid,
    /// Exit reason for EXIT controls.
    pub reason: Option<ExitReason>,
}

/// In-memory control router used by the scheduler until the wire control reader
/// owns full ETF framing. Tests can inspect recorded messages and inject inbound
/// controls through the same lifecycle methods used by decoded wire messages.
#[derive(Clone, Debug, Default)]
pub struct ControlRouter {
    messages: Arc<Mutex<Vec<ControlMessage>>>,
}

impl ControlRouter {
    /// Create an empty control router.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an outbound LINK control.
    pub fn send_link(&self, local_node: Atom, caller_pid: u64, target: RemotePid) {
        self.push(ControlMessage {
            op: ControlOp::Link,
            from: local_remote_pid(local_node, caller_pid),
            to: target,
            reason: None,
        });
    }

    /// Record an outbound UNLINK control.
    pub fn send_unlink(&self, local_node: Atom, caller_pid: u64, target: RemotePid) {
        self.push(ControlMessage {
            op: ControlOp::Unlink,
            from: local_remote_pid(local_node, caller_pid),
            to: target,
            reason: None,
        });
    }

    /// Record an outbound EXIT control.
    pub fn send_exit(
        &self,
        local_node: Atom,
        caller_pid: u64,
        target: RemotePid,
        reason: ExitReason,
    ) {
        self.push(ControlMessage {
            op: ControlOp::Exit,
            from: local_remote_pid(local_node, caller_pid),
            to: target,
            reason: Some(reason),
        });
        // FUTURE: wire ControlRouter::send_exit onto DistSender once an
        // EXIT-control encoder + link/monitor semantics land. The sender is
        // frame-agnostic, so it reuses the same enqueue path; these EXIT controls
        // are buffered here until then.
    }

    /// Snapshot recorded messages in send order.
    #[must_use]
    pub fn messages(&self) -> Vec<ControlMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn push(&self, message: ControlMessage) {
        self.messages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(message);
    }
}

fn local_remote_pid(local_node: Atom, pid_number: u64) -> RemotePid {
    RemotePid {
        node: local_node,
        pid_number,
        serial: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomTable;

    fn remote_pid(node: Atom, pid_number: u64, serial: u64) -> RemotePid {
        RemotePid {
            node,
            pid_number,
            serial,
        }
    }

    #[test]
    fn send_link_buffers_link_control_with_local_from_and_given_target() {
        let atoms = AtomTable::with_common_atoms();
        let local_node = atoms.intern("a@host");
        let target_node = atoms.intern("b@host");
        let router = ControlRouter::new();
        let target = remote_pid(target_node, 99, 3);

        router.send_link(local_node, 42, target);

        let messages = router.messages();
        assert_eq!(messages.len(), 1);
        let message = messages[0];
        assert_eq!(message.op, ControlOp::Link);
        // `from` is reconstructed as the LOCAL node + caller pid, serial 0.
        assert_eq!(message.from.node, local_node);
        assert_eq!(message.from.pid_number, 42);
        assert_eq!(message.from.serial, 0);
        // `to` is the supplied target verbatim (node/pid/serial preserved).
        assert_eq!(message.to, target);
        assert_eq!(message.reason, None);
    }

    #[test]
    fn send_unlink_buffers_unlink_control_with_no_reason() {
        let atoms = AtomTable::with_common_atoms();
        let local_node = atoms.intern("a@host");
        let target_node = atoms.intern("b@host");
        let router = ControlRouter::new();
        let target = remote_pid(target_node, 7, 1);

        router.send_unlink(local_node, 5, target);

        let messages = router.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].op, ControlOp::Unlink);
        assert_eq!(messages[0].from.node, local_node);
        assert_eq!(messages[0].from.pid_number, 5);
        assert_eq!(messages[0].to, target);
        assert_eq!(messages[0].reason, None);
    }

    #[test]
    fn send_exit_buffers_exit_control_carrying_the_reason() {
        let atoms = AtomTable::with_common_atoms();
        let local_node = atoms.intern("a@host");
        let target_node = atoms.intern("b@host");
        let router = ControlRouter::new();
        let target = remote_pid(target_node, 11, 0);

        router.send_exit(local_node, 8, target, ExitReason::Kill);

        let messages = router.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].op, ControlOp::Exit);
        assert_eq!(messages[0].from.node, local_node);
        assert_eq!(messages[0].from.pid_number, 8);
        assert_eq!(messages[0].to, target);
        assert_eq!(messages[0].reason, Some(ExitReason::Kill));
    }

    #[test]
    fn messages_are_recorded_in_send_order() {
        let atoms = AtomTable::with_common_atoms();
        let local_node = atoms.intern("a@host");
        let target_node = atoms.intern("b@host");
        let router = ControlRouter::new();
        let first = remote_pid(target_node, 1, 0);
        let second = remote_pid(target_node, 2, 0);
        let third = remote_pid(target_node, 3, 0);

        router.send_link(local_node, 100, first);
        router.send_exit(local_node, 100, second, ExitReason::Error);
        router.send_unlink(local_node, 100, third);

        let messages = router.messages();
        let ops: Vec<ControlOp> = messages.iter().map(|message| message.op).collect();
        assert_eq!(
            ops,
            vec![ControlOp::Link, ControlOp::Exit, ControlOp::Unlink],
            "controls must be buffered in the order they were sent"
        );
        let targets: Vec<u64> = messages
            .iter()
            .map(|message| message.to.pid_number)
            .collect();
        assert_eq!(targets, vec![1, 2, 3], "target ordering is preserved");
    }

    #[test]
    fn router_shares_buffer_across_clones() {
        // ControlRouter is Clone with a shared Arc<Mutex<…>> buffer: a control
        // recorded through one handle is visible through a clone.
        let atoms = AtomTable::with_common_atoms();
        let local_node = atoms.intern("a@host");
        let target_node = atoms.intern("b@host");
        let router = ControlRouter::new();
        let clone = router.clone();

        router.send_link(local_node, 1, remote_pid(target_node, 9, 0));

        assert_eq!(
            clone.messages().len(),
            1,
            "a clone observes controls recorded through the original handle"
        );
    }
}
