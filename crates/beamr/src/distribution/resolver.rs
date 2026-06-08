//! Node-name resolution seam for distribution connections.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

/// Error returned when a distribution node name cannot be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// No address is known for the requested node name.
    NotFound,
    /// Resolution failed for an external reason.
    NetworkError(String),
}

/// Boxed future returned by [`NodeResolver`] implementations.
pub type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SocketAddr, ResolveError>> + Send + 'a>>;

/// Resolves Erlang distribution node names to TCP socket addresses.
pub trait NodeResolver: Send + Sync {
    /// Resolve a node name to the TCP address used by the distribution listener.
    fn resolve<'a>(&'a self, name: &'a str) -> ResolveFuture<'a>;
}

/// In-memory resolver useful for tests and static distribution maps.
#[derive(Clone, Default)]
pub struct StaticResolver {
    entries: Arc<RwLock<HashMap<String, SocketAddr>>>,
}

impl StaticResolver {
    /// Create an empty static resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a node mapping.
    pub fn insert(&self, name: impl Into<String>, addr: SocketAddr) {
        let mut entries = self
            .entries
            .write()
            .unwrap_or_else(|error| error.into_inner());
        entries.insert(name.into(), addr);
    }

    /// Return the currently registered address for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<SocketAddr> {
        self.entries
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(name)
            .copied()
    }
}

impl NodeResolver for StaticResolver {
    fn resolve<'a>(&'a self, name: &'a str) -> ResolveFuture<'a> {
        Box::pin(async move { self.get(name).ok_or(ResolveError::NotFound) })
    }
}
