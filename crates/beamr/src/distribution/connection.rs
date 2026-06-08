//! TCP connection table and lifecycle management for distribution links.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::atom::{Atom, AtomTable};
use crate::distribution::resolver::NodeResolver;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Error returned while creating an outbound distribution TCP connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    /// The node resolver could not turn the node name into a socket address.
    ResolveFailure,
    /// The remote address refused the TCP connection.
    ConnectionRefused,
    /// Resolution succeeded but the TCP connect did not finish before the configured timeout.
    Timeout,
    /// TCP connection failed for an I/O reason other than refusal.
    Io(String),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResolveFailure => formatter.write_str("distribution node resolution failed"),
            Self::ConnectionRefused => formatter.write_str("distribution TCP connection refused"),
            Self::Timeout => formatter.write_str("distribution TCP connection timed out"),
            Self::Io(error) => write!(formatter, "distribution TCP connection failed: {error}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Reason a distribution connection left the active table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConnectionDownReason {
    /// The peer closed its side of the connection cleanly.
    PeerClosed,
    /// A read operation reported an error.
    ReadError,
    /// A write operation reported an error.
    WriteError,
}

/// Event emitted when a connection is removed from the active connection table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ConnectionDownEvent {
    /// Node name key whose connection went down.
    pub node: Atom,
    /// Why the connection was removed.
    pub reason: ConnectionDownReason,
}

type ConnectionDownCallback = dyn Fn(ConnectionDownEvent) + Send + Sync + 'static;
type InboundIdentifier = dyn Fn(SocketAddr) -> Option<Atom> + Send + Sync + 'static;

/// Per-manager callback registration for connection-down notifications.
#[derive(Clone, Default)]
pub struct ConnectionDownHook {
    callback: Arc<RwLock<Option<Arc<ConnectionDownCallback>>>>,
}

impl ConnectionDownHook {
    /// Create an empty connection-down callback slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace the connection-down callback.
    pub fn register<F>(&self, callback: F)
    where
        F: Fn(ConnectionDownEvent) + Send + Sync + 'static,
    {
        let mut slot = self
            .callback
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(Arc::new(callback));
    }

    /// Remove the registered callback.
    pub fn unregister(&self) {
        let mut slot = self
            .callback
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *slot = None;
    }

    /// Return true when a callback is registered.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.callback
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    fn invoke(&self, event: ConnectionDownEvent) {
        let callback = self
            .callback
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(callback) = callback {
            callback(event);
        }
    }
}

/// Active distribution TCP connection shared by distribution subsystems.
pub struct DistConnection {
    node: Atom,
    peer_addr: SocketAddr,
    writer: Mutex<OwnedWriteHalf>,
    down: AtomicBool,
    manager: Weak<ConnectionManagerInner>,
}

impl DistConnection {
    fn new(
        node: Atom,
        peer_addr: SocketAddr,
        writer: OwnedWriteHalf,
        manager: Weak<ConnectionManagerInner>,
    ) -> Self {
        Self {
            node,
            peer_addr,
            writer: Mutex::new(writer),
            down: AtomicBool::new(false),
            manager,
        }
    }

    /// Node-name atom used as this connection's table key.
    #[must_use]
    pub fn node(&self) -> Atom {
        self.node
    }

    /// TCP peer address for diagnostics and tests.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Return true after this connection has observed a terminal read/write failure.
    #[must_use]
    pub fn is_down(&self) -> bool {
        self.down.load(Ordering::Acquire)
    }

    /// Write raw bytes to the connection and report write-side failures to the manager.
    ///
    /// This is a transport lifecycle seam only; message encoding/framing remains owned by B-117.
    pub async fn write_raw(self: &Arc<Self>, bytes: &[u8]) -> io::Result<()> {
        let result = {
            let mut writer = self.writer.lock().await;
            writer.write_all(bytes).await
        };
        if result.is_err() {
            self.mark_down(ConnectionDownReason::WriteError);
        }
        result
    }

    fn mark_down(self: &Arc<Self>, reason: ConnectionDownReason) {
        if self.down.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(manager) = self.manager.upgrade() {
            manager.connection_down(self.node, self, reason);
        }
    }
}

/// Handle for a running inbound accept loop.
pub struct AcceptHandle {
    local_addr: SocketAddr,
    shutdown: Arc<Notify>,
    task: JoinHandle<()>,
}

impl AcceptHandle {
    /// The address actually bound by the TCP listener.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Ask the accept loop to stop. The task exits asynchronously.
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Return true if the accept task has completed.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for AcceptHandle {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        self.task.abort();
    }
}

struct ConnectionManagerInner {
    connections: DashMap<Atom, Arc<DistConnection>>,
    atom_table: Arc<AtomTable>,
    resolver: Arc<dyn NodeResolver + Send + Sync>,
    connect_timeout: Duration,
    connection_down_hook: ConnectionDownHook,
    inbound_identifier: RwLock<Option<Arc<InboundIdentifier>>>,
}

impl ConnectionManagerInner {
    fn connection_down(
        &self,
        node: Atom,
        connection: &Arc<DistConnection>,
        reason: ConnectionDownReason,
    ) {
        let removed = self
            .connections
            .remove_if(&node, |_, current| Arc::ptr_eq(current, connection))
            .is_some();
        if removed {
            self.connection_down_hook
                .invoke(ConnectionDownEvent { node, reason });
        }
    }
}

/// Distribution TCP connection manager and active connection table.
#[derive(Clone)]
pub struct ConnectionManager {
    inner: Arc<ConnectionManagerInner>,
}

impl ConnectionManager {
    /// Create a connection manager with the default five-second connect timeout.
    #[must_use]
    pub fn new(atom_table: Arc<AtomTable>, resolver: Arc<dyn NodeResolver + Send + Sync>) -> Self {
        Self::with_connect_timeout(atom_table, resolver, DEFAULT_CONNECT_TIMEOUT)
    }

    /// Create a connection manager with a caller-specified connect timeout.
    #[must_use]
    pub fn with_connect_timeout(
        atom_table: Arc<AtomTable>,
        resolver: Arc<dyn NodeResolver + Send + Sync>,
        connect_timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(ConnectionManagerInner {
                connections: DashMap::new(),
                atom_table,
                resolver,
                connect_timeout,
                connection_down_hook: ConnectionDownHook::new(),
                inbound_identifier: RwLock::new(None),
            }),
        }
    }

    /// Return the configured outbound TCP connection timeout.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        self.inner.connect_timeout
    }

    /// Return a clone of the connection-down callback slot.
    #[must_use]
    pub fn connection_down_hook(&self) -> ConnectionDownHook {
        self.inner.connection_down_hook.clone()
    }

    /// Register or replace the connection-down callback.
    pub fn register_connection_down<F>(&self, callback: F)
    where
        F: Fn(ConnectionDownEvent) + Send + Sync + 'static,
    {
        self.inner.connection_down_hook.register(callback);
    }

    /// Register a temporary inbound identification seam.
    ///
    /// B-115 replaces this with the distribution handshake. Until then, accepted streams remain
    /// pending unless this seam identifies the peer address as a node atom.
    pub fn register_inbound_identifier<F>(&self, identifier: F)
    where
        F: Fn(SocketAddr) -> Option<Atom> + Send + Sync + 'static,
    {
        let mut slot = self
            .inner
            .inbound_identifier
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(Arc::new(identifier));
    }

    /// Remove the temporary inbound identification seam.
    pub fn unregister_inbound_identifier(&self) {
        let mut slot = self
            .inner
            .inbound_identifier
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *slot = None;
    }

    /// Number of active, identified distribution connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.inner.connections.len()
    }

    /// Look up an active distribution connection by node-name atom.
    #[must_use]
    pub fn get_connection(&self, node: Atom) -> Option<Arc<DistConnection>> {
        self.inner
            .connections
            .get(&node)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Create a manager and start a dedicated asynchronous TCP accept loop.
    pub async fn start(
        listen_addr: SocketAddr,
        resolver: Arc<dyn NodeResolver + Send + Sync>,
    ) -> io::Result<(Self, AcceptHandle)> {
        let manager = Self::new(Arc::new(AtomTable::with_common_atoms()), resolver);
        let handle = manager.listen(listen_addr).await?;
        Ok((manager, handle))
    }

    /// Start a dedicated asynchronous TCP accept loop for this manager.
    pub async fn listen(&self, listen_addr: SocketAddr) -> io::Result<AcceptHandle> {
        let listener = TcpListener::bind(listen_addr).await?;
        let local_addr = listener.local_addr()?;
        let shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&shutdown);
        let manager = self.clone();
        let task = tokio::spawn(async move {
            manager.accept_loop(listener, task_shutdown).await;
        });
        Ok(AcceptHandle {
            local_addr,
            shutdown,
            task,
        })
    }

    /// Resolve `node_name`, open a TCP connection, and add it to the active table.
    pub async fn connect(&self, node_name: &str) -> Result<Arc<DistConnection>, ConnectError> {
        let addr = self
            .inner
            .resolver
            .resolve(node_name)
            .await
            .map_err(|_| ConnectError::ResolveFailure)?;
        let stream = match tokio::time::timeout(
            self.inner.connect_timeout,
            TcpStream::connect(addr),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {
                return Err(ConnectError::ConnectionRefused);
            }
            Ok(Err(error)) => return Err(ConnectError::Io(error.to_string())),
            Err(_) => return Err(ConnectError::Timeout),
        };
        let node = self.inner.atom_table.intern(node_name);
        let peer_addr = stream.peer_addr().unwrap_or(addr);
        Ok(self.register_connection(node, peer_addr, stream))
    }

    fn register_connection(
        &self,
        node: Atom,
        peer_addr: SocketAddr,
        stream: TcpStream,
    ) -> Arc<DistConnection> {
        let (read_half, write_half) = stream.into_split();
        let connection = Arc::new(DistConnection::new(
            node,
            peer_addr,
            write_half,
            Arc::downgrade(&self.inner),
        ));
        self.inner.connections.insert(node, Arc::clone(&connection));
        self.spawn_read_lifecycle(Arc::clone(&connection), read_half);
        connection
    }

    fn spawn_read_lifecycle(&self, connection: Arc<DistConnection>, mut read_half: OwnedReadHalf) {
        tokio::spawn(async move {
            let mut buffer = [0_u8; 1];
            loop {
                match read_half.read(&mut buffer).await {
                    Ok(0) => {
                        connection.mark_down(ConnectionDownReason::PeerClosed);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        connection.mark_down(ConnectionDownReason::ReadError);
                        break;
                    }
                }
            }
        });
    }

    async fn accept_loop(&self, listener: TcpListener, shutdown: Arc<Notify>) {
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    break;
                }
                accepted = listener.accept() => {
                    let Ok((stream, peer_addr)) = accepted else {
                        continue;
                    };
                    self.handle_accepted(stream, peer_addr);
                }
            }
        }
    }

    fn handle_accepted(&self, stream: TcpStream, peer_addr: SocketAddr) {
        let identifier = self
            .inner
            .inbound_identifier
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(node) = identifier.and_then(|identifier| identifier(peer_addr)) {
            self.register_connection(node, peer_addr, stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::net::TcpListener;

    use super::*;
    use crate::distribution::resolver::StaticResolver;

    fn manager_with_resolver(resolver: Arc<StaticResolver>) -> ConnectionManager {
        ConnectionManager::new(Arc::new(AtomTable::with_common_atoms()), resolver)
    }

    #[tokio::test]
    async fn empty_manager_has_no_connections() {
        let manager = manager_with_resolver(Arc::new(StaticResolver::new()));
        let node = manager.inner.atom_table.intern("missing@127.0.0.1");

        assert_eq!(manager.connection_count(), 0);
        assert!(manager.get_connection(node).is_none());
    }

    #[tokio::test]
    async fn outbound_connect_inserts_table_entry() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| {
                panic!("failed to bind local listener: {error}");
            });
        let addr = listener.local_addr().unwrap_or_else(|error| {
            panic!("failed to inspect local listener: {error}");
        });
        tokio::spawn(async move {
            let _accepted = listener.accept().await;
        });

        let resolver = Arc::new(StaticResolver::new());
        resolver.insert("remote@127.0.0.1", addr);
        let manager = manager_with_resolver(resolver);
        let connection = manager
            .connect("remote@127.0.0.1")
            .await
            .unwrap_or_else(|error| panic!("connect failed: {error}"));
        let node = manager.inner.atom_table.intern("remote@127.0.0.1");

        assert!(Arc::ptr_eq(
            &connection,
            &manager
                .get_connection(node)
                .expect("connection should be present"),
        ));
    }

    #[tokio::test]
    async fn inbound_connection_waits_for_identification_seam() {
        let resolver = Arc::new(StaticResolver::new());
        let manager = manager_with_resolver(resolver);
        let accept = manager
            .listen("127.0.0.1:0".parse().unwrap_or_else(|error| {
                panic!("failed to parse listen address: {error}");
            }))
            .await
            .unwrap_or_else(|error| panic!("failed to start accept loop: {error}"));

        let pending_stream = TcpStream::connect(accept.local_addr())
            .await
            .unwrap_or_else(|error| panic!("failed to open pending inbound stream: {error}"));
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(manager.connection_count(), 0);
        drop(pending_stream);

        let node = manager.inner.atom_table.intern("client@127.0.0.1");
        manager.register_inbound_identifier(move |_| Some(node));
        let identified_stream = TcpStream::connect(accept.local_addr())
            .await
            .unwrap_or_else(|error| panic!("failed to open identified inbound stream: {error}"));
        tokio::time::sleep(Duration::from_millis(25)).await;

        assert!(manager.get_connection(node).is_some());
        drop(identified_stream);
    }

    #[tokio::test]
    async fn dropping_peer_removes_connection_and_notifies_once() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| {
                panic!("failed to bind local listener: {error}");
            });
        let addr = listener.local_addr().unwrap_or_else(|error| {
            panic!("failed to inspect local listener: {error}");
        });
        let accepted = tokio::spawn(async move { listener.accept().await });

        let resolver = Arc::new(StaticResolver::new());
        resolver.insert("remote@127.0.0.1", addr);
        let manager = manager_with_resolver(resolver);
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_hook = Arc::clone(&callback_count);
        manager.register_connection_down(move |_| {
            callback_count_for_hook.fetch_add(1, Ordering::SeqCst);
        });
        let node = manager.inner.atom_table.intern("remote@127.0.0.1");
        let _connection = manager
            .connect("remote@127.0.0.1")
            .await
            .unwrap_or_else(|error| panic!("connect failed: {error}"));

        let Ok(Ok((remote_stream, _))) = accepted.await else {
            panic!("listener did not accept test connection");
        };
        drop(remote_stream);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(manager.get_connection(node).is_none());
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);
    }
}
