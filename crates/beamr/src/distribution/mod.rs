//! Distribution identity primitives, node resolution, and connection management.

pub mod atom_cache;
pub mod connection;
pub mod control;
pub mod control_lifecycle;
pub mod control_monitor;
pub mod etf;
pub mod global;
pub mod remote_link;
pub mod handshake;
mod node;
pub mod resolver;

pub use connection::ConnectionManager;
pub use node::{DEFAULT_NODE_NAME, Node};

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::thread;

use tokio::runtime::Runtime;

pub use resolver::{NodeResolver, ResolveError, ResolveFuture, Resolver, StaticResolver};

/// Configuration for beamr distribution services.
#[derive(Clone)]
pub struct DistributionConfig {
    /// Resolver used to map node names to distribution listen addresses.
    pub resolver: Resolver,
}

/// Synchronous net-kernel facade used by native BIFs.
#[derive(Clone)]
pub struct NetKernel {
    connections: ConnectionManager,
    runtime: Option<Arc<Runtime>>,
}

impl NetKernel {
    /// Create a facade backed by a distribution connection manager.
    #[must_use]
    pub fn new(connections: ConnectionManager) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .ok()
            .map(Arc::new);
        Self {
            connections,
            runtime,
        }
    }

    /// Return the backing connection manager.
    #[must_use]
    pub fn connection_manager(&self) -> &ConnectionManager {
        &self.connections
    }

    /// Connect to `node`, mapping all connection failures to `false`.
    pub fn connect_node(&self, node: crate::atom::Atom) -> bool {
        if self.connections.get_connection(node).is_some() {
            return true;
        }

        let Some(runtime) = self.runtime.as_ref() else {
            return false;
        };
        let connections = self.connections.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            thread::scope(|scope| {
                scope
                    .spawn(|| runtime.block_on(connections.connect_node(node)))
                    .join()
                    .unwrap_or(false)
            })
        } else {
            runtime.block_on(connections.connect_node(node))
        }
    }

    /// Return node-name atoms for active connections.
    #[must_use]
    pub fn nodes(&self) -> Vec<crate::atom::Atom> {
        self.connections.connected_nodes()
    }

    /// Disconnect `node` if connected. Missing connections are already disconnected.
    pub fn disconnect_node(&self, node: crate::atom::Atom) -> bool {
        self.connections.disconnect_node(node)
    }
}

impl fmt::Debug for NetKernel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetKernel")
            .field("connection_count", &self.connections.connection_count())
            .finish()
    }
}

impl Default for DistributionConfig {
    fn default() -> Self {
        Self {
            resolver: Arc::new(StaticResolver::new(HashMap::new())),
        }
    }
}

impl fmt::Debug for DistributionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DistributionConfig")
            .field("resolver", &"<node resolver>")
            .finish()
    }
}
