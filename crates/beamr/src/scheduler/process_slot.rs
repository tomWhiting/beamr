use std::net::SocketAddr;
use std::sync::Arc;

use crate::ets::{EtsTableId, OwnedTerm};
use crate::io::resource::FdInner;
use crate::namespace::NamespaceId;
use crate::process::{ExitReason, Monitor, Priority, RemotePid};
use crate::term::Term;

use super::ScheduledProcess;

/// Source endpoint for a pending trapped EXIT message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(super) enum PendingExitSource {
    /// Local immediate PID source.
    Local(u64),
    /// Remote boxed PID source.
    Remote(RemotePid),
}

pub(super) struct PendingEtsTransferMessage {
    pub(super) table_id: EtsTableId,
    pub(super) from_pid: u64,
    pub(super) data: OwnedTerm,
}

/// A UDP datagram queued for delivery to a process currently executing on a scheduler thread.
pub struct UdpActiveMessage {
    pub fd: Arc<FdInner>,
    pub bytes: Vec<u8>,
    pub addr: SocketAddr,
}

/// A TCP data chunk queued for delivery to a process currently executing on a scheduler thread.
pub struct TcpActiveMessage {
    pub fd: Arc<FdInner>,
    pub bytes: Vec<u8>,
}

pub(super) struct ProcessMetadata {
    pub(super) namespace_id: NamespaceId,
    pub(super) links: Vec<u64>,
    pub(super) remote_links: Vec<RemotePid>,
    pub(super) monitors: Vec<Monitor>,
    pub(super) trap_exit: bool,
    pub(super) priority: Priority,
    pub(super) current_mfa: Option<(crate::atom::Atom, crate::atom::Atom, u8)>,
    pub(super) heap_size: usize,
    pub(super) binary_heap_size: usize,
    pub(super) message_queue_len: usize,
    pub(super) group_leader: Term,
    pub(super) pending_exit_messages: Vec<(PendingExitSource, ExitReason)>,
    pub(super) pending_down_messages: Vec<(u64, u64, ExitReason)>,
    pub(super) pending_io_messages: Vec<Term>,
    pub(super) pending_distribution_payloads: Vec<Vec<u8>>,
    pub(super) pending_ets_transfer_messages: Vec<PendingEtsTransferMessage>,
    pub(super) pending_udp_messages: Vec<UdpActiveMessage>,
    pub(super) pending_tcp_messages: Vec<TcpActiveMessage>,
}

impl ProcessMetadata {
    pub(super) fn add_link(&mut self, pid: u64, self_pid: u64) {
        if pid != self_pid && !self.links.contains(&pid) {
            self.links.push(pid);
        }
    }

    pub(super) fn remove_link(&mut self, pid: u64) {
        self.links.retain(|linked_pid| *linked_pid != pid);
    }

    pub(super) fn add_remote_link(&mut self, pid: RemotePid) {
        if !self.remote_links.contains(&pid) {
            self.remote_links.push(pid);
        }
    }

    pub(super) fn remove_remote_link(&mut self, pid: RemotePid) {
        self.remote_links.retain(|linked_pid| *linked_pid != pid);
    }

    pub(super) fn add_monitor(&mut self, monitor: Monitor) {
        self.monitors.push(monitor);
    }

    pub(super) fn remove_monitor(&mut self, reference: u64) {
        self.monitors
            .retain(|monitor| monitor.reference() != reference);
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Default)]
pub(super) enum ProcessSlot {
    Present(ScheduledProcess),
    Executing(ProcessMetadata),
    #[default]
    Absent,
}
