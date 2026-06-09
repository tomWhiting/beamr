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
