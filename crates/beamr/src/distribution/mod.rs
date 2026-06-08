//! Distribution identity primitives, node resolution, and connection management.

mod node;
pub mod atom_cache;
pub mod connection;
pub mod handshake;
pub mod resolver;

pub use node::{DEFAULT_NODE_NAME, Node};

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

pub use resolver::{NodeResolver, ResolveError, ResolveFuture, Resolver, StaticResolver};

/// Configuration for beamr distribution services.
#[derive(Clone)]
pub struct DistributionConfig {
    /// Resolver used to map node names to distribution listen addresses.
    pub resolver: Resolver,
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
