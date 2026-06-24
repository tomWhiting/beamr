//! Cross-node process-group propagation for the scheduler.
//!
//! [`SchedulerPgPropagation`] is the production [`PgPropagation`] backend
//! installed into the scheduler's `PgRegistry` once `SharedState` exists. It
//! replaces the no-op `NullPgPropagation`: every local `pg:join`/`pg:leave`
//! that changes membership is encoded as a `PG_UPDATE` control frame and
//! transmitted to every currently-connected node.
//!
//! ## `Arc`-cycle avoidance
//!
//! `PgRegistry` is a field of `SharedState`, but the propagation backend needs
//! `SharedState` (to reach the connection table and the local node name). Storing
//! an `Arc<SharedState>` here would create a cycle
//! (`SharedState -> PgRegistry -> propagation -> SharedState`) and leak the
//! scheduler forever. We therefore hold a [`Weak<SharedState>`] and upgrade it
//! per-broadcast; if the scheduler has been dropped the upgrade fails and the
//! broadcast is a no-op.

use std::sync::Weak;

use crate::distribution::control::encode_pg_update_frame;
use crate::distribution::pg::{PgPropagation, PgUpdate};

use super::SharedState;
use super::supervision_integration::block_on_distribution_send;

/// Production [`PgPropagation`] backend: encodes membership updates as
/// `PG_UPDATE` control frames and sends them to every connected node.
pub(super) struct SchedulerPgPropagation {
    /// Weak handle to the scheduler shared state. Weak (not `Arc`) to avoid the
    /// `SharedState -> PgRegistry -> propagation -> SharedState` reference cycle.
    pub(super) shared: Weak<SharedState>,
}

impl PgPropagation for SchedulerPgPropagation {
    fn broadcast(&self, update: PgUpdate) {
        // If the scheduler has been dropped, there is nothing to broadcast to.
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // Encode the member as an external PID carrying our local node name so
        // the receiver records a fully-attributed RemoteMember.
        let local_node = shared.local_node.name;
        let Ok(frame) = encode_pg_update_frame(update, local_node, &shared.atom_table) else {
            // Encoding a pg control frame cannot fail for well-formed atoms/pids;
            // a failure here means the update could not be serialised, so there is
            // nothing safe to transmit. Drop it rather than panicking — a dropped
            // membership update is self-correcting on the next join/leave or
            // node-down purge, and `!`-style sends are infallible in BEAM.
            return;
        };
        // Snapshot the connected nodes and send to each. `connected_nodes()` and
        // `block_on_distribution_send` are called with no PgState/propagation lock
        // held (the registry drops its guards before invoking `broadcast`), so a
        // blocking send here cannot deadlock the registry.
        for node in shared.distribution_connections.connected_nodes() {
            let Some(node_name) = shared.atom_table.resolve(node) else {
                continue;
            };
            let _ = block_on_distribution_send(
                &shared.distribution_connections,
                node,
                node_name,
                &frame,
            );
        }
    }
}
