//! Pluggable node-name resolution for distribution.
//!
//! Beamr does not use EPMD for node discovery. Distribution callers provide a
//! resolver that maps node names directly to socket addresses. The trait uses a
//! boxed future so resolvers can be shared behind `Arc<dyn NodeResolver>` while
//! still presenting an asynchronous resolve contract.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

/// Future returned by node resolvers.
pub type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SocketAddr, ResolveError>> + Send + 'a>>;

/// Shared resolver handle used by distribution components.
pub type Resolver = Arc<dyn NodeResolver + Send + Sync>;

/// Failures that can occur while resolving a distributed node name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No configured address exists for the requested node name.
    NotFound,
    /// The resolver could not complete because of an external network failure.
    NetworkError(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("node name was not found"),
            Self::NetworkError(message) => {
                write!(formatter, "node resolver network error: {message}")
            }
        }
    }
}

impl Error for ResolveError {}

/// Asynchronous node-name resolver for distributed connections.
pub trait NodeResolver: Send + Sync {
    /// Resolve a node name to the socket address used for distribution traffic.
    fn resolve<'a>(&'a self, name: &'a str) -> ResolveFuture<'a>;
}

/// Resolver backed by an immutable static map of node names to addresses.
#[derive(Debug, Clone)]
pub struct StaticResolver {
    nodes: HashMap<String, SocketAddr>,
}

impl StaticResolver {
    /// Create a static resolver from the supplied node-address map.
    #[must_use]
    pub fn new(map: HashMap<String, SocketAddr>) -> Self {
        Self { nodes: map }
    }
}

impl NodeResolver for StaticResolver {
    fn resolve<'a>(&'a self, name: &'a str) -> ResolveFuture<'a> {
        let result = self.nodes.get(name).copied().ok_or(ResolveError::NotFound);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    fn block_on_ready(future: ResolveFuture<'_>) -> Result<SocketAddr, ResolveError> {
        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);
        let mut future = future;
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("resolver test future should be ready immediately"),
        }
    }

    struct MockResolver {
        address: SocketAddr,
    }

    impl NodeResolver for MockResolver {
        fn resolve<'a>(&'a self, _name: &'a str) -> ResolveFuture<'a> {
            let address = self.address;
            Box::pin(async move { Ok(address) })
        }
    }

    #[test]
    fn mock_resolver_returns_configured_address() {
        let address = SocketAddr::from(([127, 0, 0, 1], 43_699));
        let resolver = MockResolver { address };

        assert_eq!(
            block_on_ready(resolver.resolve("beam@localhost")),
            Ok(address)
        );
    }

    #[test]
    fn static_resolver_resolves_configured_nodes() {
        let first = SocketAddr::from(([127, 0, 0, 1], 43_700));
        let second = SocketAddr::from(([127, 0, 0, 1], 43_701));
        let third = SocketAddr::from(([127, 0, 0, 1], 43_702));
        let mut nodes = HashMap::new();
        nodes.insert("first@localhost".to_string(), first);
        nodes.insert("second@localhost".to_string(), second);
        nodes.insert("third@localhost".to_string(), third);
        let resolver = StaticResolver::new(nodes);

        assert_eq!(
            block_on_ready(resolver.resolve("first@localhost")),
            Ok(first)
        );
        assert_eq!(
            block_on_ready(resolver.resolve("second@localhost")),
            Ok(second)
        );
        assert_eq!(
            block_on_ready(resolver.resolve("third@localhost")),
            Ok(third)
        );
        assert_eq!(
            block_on_ready(resolver.resolve("missing@localhost")),
            Err(ResolveError::NotFound)
        );
    }
}
