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
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::atom::{Atom, AtomTable};
use crate::distribution::handshake::{
    HandshakeError, HandshakeNode, initiate_handshake_async, respond_handshake_async,
};
use crate::distribution::resolver::NodeResolver;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default whole-handshake deadline. Mirrors [`DEFAULT_CONNECT_TIMEOUT`]: any
/// finite value removes the deadlock; 5s tolerates a loaded peer without wedging
/// a cluster (DISTRIBUTION-HANDSHAKE-DESIGN.md D3).
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// A write exceeded its deadline (peer connected but not reading; kernel
    /// send buffer full). Treated as a terminal write failure by the outbound
    /// sender so a wedged peer cannot stall the shared drain.
    WriteTimeout,
    /// The local node explicitly closed the connection.
    ManualDisconnect,
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
type ControlFrameHandler = dyn Fn(&[u8], &[u8]) + Send + Sync + 'static;

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

    /// Mark this connection down because a write exceeded its deadline.
    ///
    /// The outbound sender's drain bounds each `write_raw` with a timeout so a
    /// wedged peer cannot stall propagation for the whole cluster. On timeout the
    /// write future is dropped without `write_raw` observing a failure, so the
    /// drain calls this to drive the same connection-down path (hook + remote
    /// purge) a genuine write error would. Idempotent via the inner `mark_down`.
    pub fn mark_down_write_timeout(self: &Arc<Self>) {
        self.mark_down(ConnectionDownReason::WriteTimeout);
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
    /// Whole-handshake deadline applied around the OTP exchange on both the
    /// outbound `connect` and the inbound accept-side responder. Bounds a stalled
    /// or malicious peer so `connect` always returns and no responder task parks
    /// forever (DISTRIBUTION-HANDSHAKE-DESIGN.md HS-1, D3).
    handshake_timeout: Duration,
    connection_down_hook: ConnectionDownHook,
    control_frame_handler: RwLock<Option<Arc<ControlFrameHandler>>>,
    /// Shared handshake secret. Both peers must agree on this value or the OTP
    /// challenge/response is rejected and the connection is dropped.
    cookie: String,
    /// This node's advertised distribution name, sent in the handshake name
    /// packet so the peer keys its connection table by our identity.
    local_node_name: String,
    /// This node's creation value, sent alongside the name in the handshake.
    local_creation: u32,
    /// Runtime handle that drives the read/accept tasks. In production the
    /// scheduler binds the [`DistSender`](crate::distribution::sender::DistSender)
    /// runtime here so the receive side is driven even though no ambient runtime
    /// exists. When unset (e.g. `#[tokio::test]`), the tasks fall back to the
    /// ambient runtime via bare `tokio::spawn`.
    runtime_handle: RwLock<Option<Handle>>,
}

impl ConnectionManagerInner {
    /// Spawn `future` on the bound runtime handle when one is set, else on the
    /// ambient runtime. Used for the read/accept lifecycle tasks.
    fn spawn_lifecycle<F>(&self, future: F) -> JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let handle = self
            .runtime_handle
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        match handle {
            Some(handle) => handle.spawn(future),
            None => tokio::spawn(future),
        }
    }

    /// Build the local handshake descriptor advertised to peers.
    fn handshake_node(&self) -> Result<HandshakeNode, ConnectError> {
        HandshakeNode::with_default_flags(self.local_node_name.clone(), self.local_creation)
            .map_err(|error| ConnectError::Io(error.to_string()))
    }

    /// Produce a per-handshake challenge value. The challenge is drawn from a
    /// cryptographically secure random source, so it is unpredictable per
    /// session. This is the canonical OTP behavior: the shared cookie still
    /// provides authentication, while an unpredictable challenge adds
    /// defense-in-depth against replay (an attacker cannot precompute the
    /// digest for a challenge they cannot guess).
    fn gen_challenge(&self) -> u32 {
        rand::random::<u32>()
    }
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
    ///
    /// `cookie`, `local_node_name`, and `local_creation` are the local node's
    /// OTP handshake identity: the cookie authenticates peers, while the name and
    /// creation are advertised so a peer keys its connection table by this node.
    #[must_use]
    pub fn new(
        atom_table: Arc<AtomTable>,
        resolver: Arc<dyn NodeResolver + Send + Sync>,
        cookie: impl Into<String>,
        local_node_name: impl Into<String>,
        local_creation: u32,
    ) -> Self {
        Self::with_connect_timeout(
            atom_table,
            resolver,
            cookie,
            local_node_name,
            local_creation,
            DEFAULT_CONNECT_TIMEOUT,
        )
    }

    /// Create a connection manager with a caller-specified connect timeout.
    #[must_use]
    pub fn with_connect_timeout(
        atom_table: Arc<AtomTable>,
        resolver: Arc<dyn NodeResolver + Send + Sync>,
        cookie: impl Into<String>,
        local_node_name: impl Into<String>,
        local_creation: u32,
        connect_timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(ConnectionManagerInner {
                connections: DashMap::new(),
                atom_table,
                resolver,
                connect_timeout,
                handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
                connection_down_hook: ConnectionDownHook::new(),
                control_frame_handler: RwLock::new(None),
                cookie: cookie.into(),
                local_node_name: local_node_name.into(),
                local_creation,
                runtime_handle: RwLock::new(None),
            }),
        }
    }

    /// Bind a tokio runtime handle for the read/accept lifecycle tasks.
    ///
    /// The scheduler calls this with the owned `DistSender` runtime handle so the
    /// receive side is driven in production (where no ambient runtime exists).
    /// Must be called before any connection is established; existing tasks keep
    /// the runtime they were spawned on.
    pub fn set_runtime_handle(&self, handle: Handle) {
        *self
            .inner
            .runtime_handle
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
    }

    /// Return the configured outbound TCP connection timeout.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        self.inner.connect_timeout
    }

    /// Return the configured whole-handshake deadline.
    #[must_use]
    pub fn handshake_timeout(&self) -> Duration {
        self.inner.handshake_timeout
    }

    /// Override the whole-handshake deadline on a freshly-built manager.
    ///
    /// Builder-style: must be called before the manager is cloned or any
    /// connection is started, while its `inner` is still uniquely owned. Returns
    /// `self` unchanged if the manager has already been shared (a clone exists),
    /// since the deadline is read by in-flight handshakes and cannot be mutated
    /// race-free afterward.
    #[must_use]
    pub fn with_handshake_timeout(mut self, handshake_timeout: Duration) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.handshake_timeout = handshake_timeout;
        }
        self
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

    /// Register a handler for framed distribution control messages read from active links.
    pub fn register_control_frame_handler<F>(&self, handler: F)
    where
        F: Fn(&[u8], &[u8]) + Send + Sync + 'static,
    {
        let mut slot = self
            .inner
            .control_frame_handler
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(Arc::new(handler));
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

    /// Return the node-name atoms for all active distribution connections.
    #[must_use]
    pub fn connected_nodes(&self) -> Vec<Atom> {
        let mut nodes: Vec<_> = self
            .inner
            .connections
            .iter()
            .map(|entry| *entry.key())
            .collect();
        nodes.sort_unstable_by_key(|node| node.index());
        nodes
    }

    /// Idempotently connect to a node-name atom, returning `false` for transport failures.
    pub async fn connect_node(&self, node: Atom) -> bool {
        if self.get_connection(node).is_some() {
            return true;
        }
        let Some(node_name) = self.inner.atom_table.resolve(node).map(str::to_owned) else {
            return false;
        };
        self.connect(&node_name).await.is_ok()
    }

    /// Manually disconnect an active node and emit the connection-down hook once.
    pub fn disconnect_node(&self, node: Atom) -> bool {
        let Some(connection) = self.get_connection(node) else {
            return true;
        };
        connection.mark_down(ConnectionDownReason::ManualDisconnect);
        true
    }

    /// Create a manager and start a dedicated asynchronous TCP accept loop.
    pub async fn start(
        listen_addr: SocketAddr,
        resolver: Arc<dyn NodeResolver + Send + Sync>,
        cookie: impl Into<String>,
        local_node_name: impl Into<String>,
        local_creation: u32,
    ) -> io::Result<(Self, AcceptHandle)> {
        let manager = Self::new(
            Arc::new(AtomTable::with_common_atoms()),
            resolver,
            cookie,
            local_node_name,
            local_creation,
        );
        let handle = manager.listen(listen_addr).await?;
        Ok((manager, handle))
    }

    /// Start a dedicated asynchronous TCP accept loop for this manager.
    pub async fn listen(&self, listen_addr: SocketAddr) -> io::Result<AcceptHandle> {
        let listener = TcpListener::bind(listen_addr).await?;
        Ok(self.listen_with(listener))
    }

    /// Start a dedicated asynchronous TCP accept loop on a pre-bound listener.
    ///
    /// Separated from [`listen`](Self::listen) so callers that must bind the
    /// listener before the manager exists (e.g. to publish the chosen port into a
    /// resolver) can reuse the same accept-loop spawn. The accept loop runs on the
    /// bound runtime handle via [`ConnectionManagerInner::spawn_lifecycle`].
    #[must_use]
    pub fn listen_with(&self, listener: TcpListener) -> AcceptHandle {
        let local_addr = listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&shutdown);
        let manager = self.clone();
        let task = self.inner.spawn_lifecycle(async move {
            manager.accept_loop(listener, task_shutdown).await;
        });
        AcceptHandle {
            local_addr,
            shutdown,
            task,
        }
    }

    /// Resolve `node_name`, open a TCP connection, run the OTP distribution
    /// handshake, and add the authenticated link to the active table.
    ///
    /// The connection is keyed by the name the peer advertises in the handshake
    /// — not by `node_name`/the resolver key — so identity is established by the
    /// authenticated handshake rather than by trusting the dialed address. On any
    /// handshake failure the stream is dropped (closing the TCP connection) and a
    /// [`ConnectError::Io`] is returned.
    pub async fn connect(&self, node_name: &str) -> Result<Arc<DistConnection>, ConnectError> {
        let addr = self
            .inner
            .resolver
            .resolve(node_name)
            .await
            .map_err(|_| ConnectError::ResolveFailure)?;
        let mut stream = match tokio::time::timeout(
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
        let peer_addr = stream.peer_addr().unwrap_or(addr);

        let local = self.inner.handshake_node()?;
        // Bound the whole handshake so a stalled or malicious peer can never park
        // this call forever; `connect` is now guaranteed to return within
        // handshake_timeout (HS-1). On elapse the stream is dropped, closing the
        // TCP connection.
        let result = match tokio::time::timeout(
            self.inner.handshake_timeout,
            initiate_handshake_async(
                &mut stream,
                &local,
                &self.inner.cookie,
                self.inner.gen_challenge(),
            ),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => return Err(ConnectError::Io(error.to_string())),
            Err(_) => return Err(ConnectError::Io(HandshakeError::Timeout.to_string())),
        };
        // Dropping the stream on the error paths above closes the TCP connection;
        // on success the authenticated remote name becomes the connection-table
        // key.
        let node = self.inner.atom_table.intern(result.remote_name());
        Ok(self.register_connection(node, peer_addr, stream))
    }

    /// Install an authenticated link, deduplicating against an existing `Up`
    /// connection for the same peer name (HS-2).
    ///
    /// Two simultaneous handshakes (one inbound, one outbound) for the same pair
    /// can both reach this point. A blind `insert` would clobber the first link's
    /// `Arc<DistConnection>` in the table while leaving its read task running on an
    /// orphaned socket. Instead this uses the entry API to atomically check for a
    /// live existing link: if one is present, the newcomer is the loser — its
    /// stream is dropped (closing the TCP connection) and its read task is never
    /// spawned, and the existing survivor is returned. A stale entry whose
    /// connection has already gone down is replaced (the reconnect path). This
    /// guarantees the invariant: at most one live `Up` connection per peer name.
    fn register_connection(
        &self,
        node: Atom,
        peer_addr: SocketAddr,
        stream: TcpStream,
    ) -> Arc<DistConnection> {
        use dashmap::mapref::entry::Entry;

        match self.inner.connections.entry(node) {
            Entry::Occupied(mut occupied) => {
                if !occupied.get().is_down() {
                    // A live link already won this pair. Drop the loser's stream
                    // (closing its TCP connection) and do NOT spawn its reader.
                    drop(stream);
                    return Arc::clone(occupied.get());
                }
                // The existing entry is a dead link awaiting reap; replace it.
                let (connection, read_half) = self.build_connection(node, peer_addr, stream);
                occupied.insert(Arc::clone(&connection));
                self.spawn_read_lifecycle(Arc::clone(&connection), read_half);
                connection
            }
            Entry::Vacant(vacant) => {
                let (connection, read_half) = self.build_connection(node, peer_addr, stream);
                vacant.insert(Arc::clone(&connection));
                self.spawn_read_lifecycle(Arc::clone(&connection), read_half);
                connection
            }
        }
    }

    /// Split a stream into a [`DistConnection`] and its read half, without
    /// touching the connection table. Shared by both `register_connection` arms.
    fn build_connection(
        &self,
        node: Atom,
        peer_addr: SocketAddr,
        stream: TcpStream,
    ) -> (Arc<DistConnection>, OwnedReadHalf) {
        let (read_half, write_half) = stream.into_split();
        let connection = Arc::new(DistConnection::new(
            node,
            peer_addr,
            write_half,
            Arc::downgrade(&self.inner),
        ));
        (connection, read_half)
    }

    /// Register a pre-connected standard stream for native BIF unit tests.
    #[cfg(test)]
    pub(crate) fn register_test_connection(
        &self,
        node: Atom,
        peer_addr: SocketAddr,
        stream: std::net::TcpStream,
    ) -> io::Result<Arc<DistConnection>> {
        stream.set_nonblocking(true)?;
        let stream = TcpStream::from_std(stream)?;
        Ok(self.register_connection(node, peer_addr, stream))
    }

    fn spawn_read_lifecycle(&self, connection: Arc<DistConnection>, mut read_half: OwnedReadHalf) {
        let manager = Arc::clone(&self.inner);
        self.inner.spawn_lifecycle(async move {
            loop {
                let mut header = [0_u8; 8];
                match read_half.read_exact(&mut header).await {
                    Ok(0) => {
                        connection.mark_down(ConnectionDownReason::PeerClosed);
                        break;
                    }
                    Ok(_) => {
                        let control_len =
                            u32::from_be_bytes([header[0], header[1], header[2], header[3]])
                                as usize;
                        let payload_len =
                            u32::from_be_bytes([header[4], header[5], header[6], header[7]])
                                as usize;
                        let Some(total_len) = control_len.checked_add(payload_len) else {
                            connection.mark_down(ConnectionDownReason::ReadError);
                            break;
                        };
                        let mut frame = vec![0_u8; total_len];
                        if read_half.read_exact(&mut frame).await.is_err() {
                            connection.mark_down(ConnectionDownReason::ReadError);
                            break;
                        }
                        let handler = manager
                            .control_frame_handler
                            .read()
                            .unwrap_or_else(|error| error.into_inner())
                            .clone();
                        if let Some(handler) = handler {
                            let (control, payload) = frame.split_at(control_len);
                            handler(control, payload);
                        }
                    }
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

    /// Run the inbound OTP handshake on an accepted stream, then register it.
    ///
    /// The handshake is asynchronous, so it is spawned onto the bound runtime via
    /// [`ConnectionManagerInner::spawn_lifecycle`] — the same mechanism the
    /// read/accept lifecycle uses — so it is driven even in production where no
    /// ambient tokio runtime exists on worker threads. The handshake completes on
    /// the raw stream (2-byte length-prefixed packets) before the connection is
    /// registered and its data-frame read loop starts. On success the connection
    /// is keyed by the peer's authenticated handshake name; on failure the stream
    /// is dropped, closing the TCP connection.
    fn handle_accepted(&self, mut stream: TcpStream, peer_addr: SocketAddr) {
        let manager = self.clone();
        self.inner.spawn_lifecycle(async move {
            let local = match manager.inner.handshake_node() {
                Ok(local) => local,
                Err(_) => return,
            };
            // Bound the responder so a stalled or malicious peer can never park
            // this spawned task forever; on elapse the stream is dropped, closing
            // the TCP connection (HS-1).
            let outcome = tokio::time::timeout(
                manager.inner.handshake_timeout,
                respond_handshake_async(
                    &mut stream,
                    &local,
                    &manager.inner.cookie,
                    manager.inner.gen_challenge(),
                ),
            )
            .await;
            match outcome {
                Ok(Ok(result)) => {
                    let node = manager.inner.atom_table.intern(result.remote_name());
                    manager.register_connection(node, peer_addr, stream);
                }
                Ok(Err(_)) | Err(_) => {
                    drop(stream);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Barrier, mpsc};
    use std::thread;
    use std::time::Instant;

    use tokio::net::TcpListener;
    use tokio::runtime::Builder;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::distribution::handshake::HandshakeNode;
    use crate::distribution::resolver::StaticResolver;

    const TEST_COOKIE: &str = "test-cookie";

    fn manager_with_resolver(resolver: Arc<StaticResolver>) -> ConnectionManager {
        ConnectionManager::new(
            Arc::new(AtomTable::with_common_atoms()),
            resolver,
            TEST_COOKIE,
            "local@127.0.0.1",
            1,
        )
    }

    /// Accept a single inbound stream on `listener` and respond to the OTP
    /// handshake advertising `name`, mirroring a real peer's accept side so the
    /// outbound `connect` under test can complete its handshake.
    fn spawn_responder(
        listener: TcpListener,
        name: &'static str,
        cookie: &'static str,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                return;
            };
            let local = HandshakeNode::with_default_flags(name, 7)
                .expect("responder node name should be valid");
            let _ = crate::distribution::handshake::respond_handshake_async(
                &mut stream,
                &local,
                cookie,
                99,
            )
            .await;
            // Keep the accepted stream alive so the connection is not torn down
            // while the test inspects the outbound side.
            tokio::time::sleep(Duration::from_millis(200)).await;
        })
    }

    /// Accept one inbound stream, complete the handshake advertising `name`, and
    /// hand the accepted (still-open) stream back to the caller so a test can
    /// later drop it to simulate the peer going away after a successful link.
    fn spawn_responder_handoff(
        listener: TcpListener,
        name: &'static str,
    ) -> tokio::sync::oneshot::Receiver<TcpStream> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                return;
            };
            let local = HandshakeNode::with_default_flags(name, 7)
                .expect("responder node name should be valid");
            if crate::distribution::handshake::respond_handshake_async(
                &mut stream,
                &local,
                TEST_COOKIE,
                99,
            )
            .await
            .is_ok()
            {
                let _ = sender.send(stream);
            }
        });
        receiver
    }

    #[tokio::test]
    async fn empty_manager_has_no_connections() {
        let manager = manager_with_resolver(Arc::new(StaticResolver::new(
            std::collections::HashMap::new(),
        )));
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
        let _responder = spawn_responder(listener, "remote@127.0.0.1", TEST_COOKIE);

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
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
    async fn connect_keys_table_by_remote_handshake_name_not_resolver_key() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("failed to bind local listener: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("failed to inspect local listener: {error}"));
        // The peer advertises a DIFFERENT name than the resolver key the dialer
        // used, proving identity comes from the authenticated handshake.
        let _responder = spawn_responder(listener, "advertised@127.0.0.1", TEST_COOKIE);

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "dialed@127.0.0.1".to_string(),
            addr,
        )])));
        let manager = manager_with_resolver(resolver);
        let connection = manager
            .connect("dialed@127.0.0.1")
            .await
            .unwrap_or_else(|error| panic!("connect failed: {error}"));

        let advertised = manager.inner.atom_table.intern("advertised@127.0.0.1");
        let dialed = manager.inner.atom_table.intern("dialed@127.0.0.1");
        assert_eq!(connection.node(), advertised);
        assert!(manager.get_connection(advertised).is_some());
        assert!(
            manager.get_connection(dialed).is_none(),
            "connection must not be keyed by the resolver key"
        );
    }

    #[tokio::test]
    async fn connect_rejects_wrong_cookie_and_records_no_entry() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("failed to bind local listener: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("failed to inspect local listener: {error}"));
        // Responder uses a different cookie, so the handshake digest mismatches.
        let _responder = spawn_responder(listener, "remote@127.0.0.1", "other-cookie");

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
        let manager = manager_with_resolver(resolver);
        let result = manager.connect("remote@127.0.0.1").await;

        assert!(
            matches!(result, Err(ConnectError::Io(_))),
            "connect must fail with Io on cookie mismatch"
        );
        assert_eq!(manager.connection_count(), 0);
        let remote = manager.inner.atom_table.intern("remote@127.0.0.1");
        assert!(manager.get_connection(remote).is_none());
    }

    #[tokio::test]
    async fn inbound_wrong_cookie_registers_no_entry() {
        // A listening manager authenticates with TEST_COOKIE. A peer that
        // initiates the handshake with a DIFFERENT cookie must be rejected by
        // the register-side accept loop (the `handle_accepted` Err -> drop arm)
        // and must NOT receive a connection-table entry.
        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::new()));
        let manager = manager_with_resolver(resolver);
        let accept = manager
            .listen("127.0.0.1:0".parse().unwrap_or_else(|error| {
                panic!("failed to parse listen address: {error}");
            }))
            .await
            .unwrap_or_else(|error| panic!("failed to start accept loop: {error}"));

        let mut client = TcpStream::connect(accept.local_addr())
            .await
            .unwrap_or_else(|error| panic!("failed to open inbound stream: {error}"));
        let client_node = HandshakeNode::with_default_flags("client@127.0.0.1", 5)
            .expect("client node name should be valid");
        // The client uses the WRONG cookie, so the digest mismatches and the
        // listening manager's responder rejects the handshake.
        let result = crate::distribution::handshake::initiate_handshake_async(
            &mut client,
            &client_node,
            "wrong-cookie",
            42,
        )
        .await;
        assert!(
            result.is_err(),
            "inbound handshake with wrong cookie must fail"
        );

        // The inbound handshake runs on a spawned task, so poll (rather than a
        // fixed sleep) to confirm the rejection never produces a table entry.
        let node = manager.inner.atom_table.intern("client@127.0.0.1");
        for _ in 0..40 {
            assert_eq!(
                manager.connection_count(),
                0,
                "wrong-cookie peer must never register a connection"
            );
            assert!(
                manager.get_connection(node).is_none(),
                "wrong-cookie peer must not appear in the connection table"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(client);
    }

    /// HS-1: an outbound `connect` to a peer that accepts the TCP connection but
    /// never speaks the handshake must return a handshake-timeout error within the
    /// configured handshake deadline, not hang. This is the bounded-return
    /// contract that lets the haematite-side retry above the seam make progress.
    #[tokio::test]
    async fn connect_returns_timeout_when_peer_never_handshakes() {
        // A bare listener that accepts then stays silent (no responder).
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("failed to bind local listener: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("failed to inspect local listener: {error}"));
        let _silent_accept = tokio::spawn(async move {
            // Accept and hold the stream open without ever writing a handshake byte.
            if let Ok((stream, _peer)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(stream);
            }
        });

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "silent@127.0.0.1".to_string(),
            addr,
        )])));
        let manager =
            manager_with_resolver(resolver).with_handshake_timeout(Duration::from_secs(1));

        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(15), manager.connect("silent@127.0.0.1"))
                .await;

        let outcome = result
            .expect("connect must return within the handshake deadline, not hang")
            .map(|_connection| ());
        assert!(
            matches!(outcome, Err(ConnectError::Io(_))),
            "a non-speaking peer must surface as a connect error, got {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "connect should return near the 1s handshake deadline, took {:?}",
            started.elapsed()
        );
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn connect_node_is_idempotent_and_lists_node() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("failed to bind local listener: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("failed to inspect local listener: {error}"));
        let _responder = spawn_responder(listener, "remote@127.0.0.1", TEST_COOKIE);

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
        let manager = manager_with_resolver(resolver);
        let node = manager.inner.atom_table.intern("remote@127.0.0.1");

        assert!(manager.connect_node(node).await);
        assert!(manager.connect_node(node).await);
        assert_eq!(manager.connected_nodes(), vec![node]);
        assert_eq!(manager.connection_count(), 1);
    }

    #[tokio::test]
    async fn connect_node_returns_false_for_unresolved_node() {
        let manager = manager_with_resolver(Arc::new(StaticResolver::new(
            std::collections::HashMap::new(),
        )));
        let node = manager.inner.atom_table.intern("missing@127.0.0.1");

        assert!(!manager.connect_node(node).await);
        assert!(manager.connected_nodes().is_empty());
    }

    #[tokio::test]
    async fn inbound_peer_registers_under_its_handshake_name() {
        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::new()));
        let manager = manager_with_resolver(resolver);
        let accept = manager
            .listen("127.0.0.1:0".parse().unwrap_or_else(|error| {
                panic!("failed to parse listen address: {error}");
            }))
            .await
            .unwrap_or_else(|error| panic!("failed to start accept loop: {error}"));

        // The inbound peer initiates the handshake advertising "client@127.0.0.1".
        // The manager must register it under that authenticated name with NO
        // address-identity seam.
        let mut client = TcpStream::connect(accept.local_addr())
            .await
            .unwrap_or_else(|error| panic!("failed to open inbound stream: {error}"));
        let client_node = HandshakeNode::with_default_flags("client@127.0.0.1", 5)
            .expect("client node name should be valid");
        crate::distribution::handshake::initiate_handshake_async(
            &mut client,
            &client_node,
            TEST_COOKIE,
            42,
        )
        .await
        .expect("inbound peer handshake should succeed");

        let node = manager.inner.atom_table.intern("client@127.0.0.1");
        let mut connected = false;
        for _ in 0..40 {
            if manager.get_connection(node).is_some() {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            connected,
            "inbound peer should register under its handshake name"
        );
        assert_eq!(manager.connected_nodes(), vec![node]);
        drop(client);
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
        let remote_stream = spawn_responder_handoff(listener, "remote@127.0.0.1");

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
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

        let remote_stream = remote_stream
            .await
            .expect("responder did not complete handshake");
        drop(remote_stream);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(manager.get_connection(node).is_none());
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manual_disconnect_removes_connection_and_notifies_once() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("failed to bind local listener: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("failed to inspect local listener: {error}"));
        let _responder = spawn_responder(listener, "remote@127.0.0.1", TEST_COOKIE);

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
        let manager = manager_with_resolver(resolver);
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_hook = Arc::clone(&callback_count);
        manager.register_connection_down(move |event| {
            assert_eq!(event.reason, ConnectionDownReason::ManualDisconnect);
            callback_count_for_hook.fetch_add(1, Ordering::SeqCst);
        });
        let node = manager.inner.atom_table.intern("remote@127.0.0.1");

        assert!(manager.connect_node(node).await);
        assert!(manager.disconnect_node(node));
        assert!(manager.disconnect_node(node));

        assert!(manager.get_connection(node).is_none());
        assert!(manager.connected_nodes().is_empty());
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn write_error_removes_connection_and_notifies_once() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| {
                panic!("failed to bind local listener: {error}");
            });
        let addr = listener.local_addr().unwrap_or_else(|error| {
            panic!("failed to inspect local listener: {error}");
        });
        let remote_stream = spawn_responder_handoff(listener, "remote@127.0.0.1");

        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::from([(
            "remote@127.0.0.1".to_string(),
            addr,
        )])));
        let manager = manager_with_resolver(resolver);
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_hook = Arc::clone(&callback_count);
        manager.register_connection_down(move |_| {
            callback_count_for_hook.fetch_add(1, Ordering::SeqCst);
        });
        let node = manager.inner.atom_table.intern("remote@127.0.0.1");
        let connection = manager
            .connect("remote@127.0.0.1")
            .await
            .unwrap_or_else(|error| panic!("connect failed: {error}"));

        let remote_stream = remote_stream
            .await
            .expect("responder did not complete handshake");
        drop(remote_stream);

        for _ in 0..8 {
            if connection.write_raw(b"probe").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;

        assert!(manager.get_connection(node).is_none());
        assert_eq!(callback_count.load(Ordering::SeqCst), 1);
    }

    /// HS-2: two simultaneous installs for the same peer name must leave exactly
    /// one live link, and the loser's socket must be closed (its reader is never
    /// spawned, so it cannot linger as an orphan on a half-link). The winner is
    /// the first-installed connection; the second install returns that same Arc
    /// and drops its own stream, which the loser's peer observes as EOF.
    #[tokio::test]
    async fn hs2_two_simultaneous_installs_keep_exactly_one_no_orphan_reader() {
        let resolver = Arc::new(StaticResolver::new(std::collections::HashMap::new()));
        let manager = manager_with_resolver(resolver);
        let node = manager.inner.atom_table.intern("peer@127.0.0.1");

        // Two independent connected socket pairs standing in for the inbound and
        // outbound halves of a simultaneous connect. The client ends let us
        // observe whether each server end stays open or is closed.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind helper listener");
        let addr = listener.local_addr().expect("inspect helper listener");

        let mut client_first = TcpStream::connect(addr)
            .await
            .expect("client_first connects");
        let (server_first, _) = listener.accept().await.expect("accept server_first");
        let mut client_second = TcpStream::connect(addr)
            .await
            .expect("client_second connects");
        let (server_second, _) = listener.accept().await.expect("accept server_second");

        let winner = manager.register_connection(node, addr, server_first);
        let returned = manager.register_connection(node, addr, server_second);

        // Exactly one table entry, and the second install returned the winner.
        assert_eq!(manager.connection_count(), 1);
        assert!(
            Arc::ptr_eq(&winner, &returned),
            "the second install must return the existing survivor, not a new link"
        );
        assert!(Arc::ptr_eq(
            &winner,
            &manager
                .get_connection(node)
                .expect("survivor must be in the table"),
        ));

        // The winner's socket stays open: a write reaches its peer.
        winner
            .write_raw(&[0_u8; 8])
            .await
            .expect("winner link must remain writable");
        let mut header = [0_u8; 8];
        client_first
            .read_exact(&mut header)
            .await
            .expect("winner's peer must receive the keepalive frame");

        // The loser's socket was closed (stream dropped, no reader spawned), so
        // its peer observes EOF rather than a live, orphaned half-link.
        let mut byte = [0_u8; 1];
        let eof = tokio::time::timeout(Duration::from_secs(5), client_second.read(&mut byte))
            .await
            .expect("loser's socket should close promptly, not hang")
            .expect("reading the closed loser socket should not error");
        assert_eq!(eof, 0, "the loser's socket must be closed (EOF)");
    }

    type Resolver = Arc<dyn NodeResolver + Send + Sync>;

    /// HS-0 (deterministic root-cause oracle): an inbound peer completes the TCP
    /// connect then sends nothing, so the accept-side responder's first read sits
    /// on an untimed `read_exact`. Pre-HS-1 that responder task never resolves and
    /// the silent peer's socket stays open forever — the canonical handshake hang
    /// that, multiplied across a `>=3`-node mesh of blocking dials, wedges a
    /// cluster. After HS-1 the responder hits the whole-handshake deadline, the
    /// server drops the stream, and the silent peer observes EOF.
    ///
    /// The oracle drives the REAL `ConnectionManager` accept loop (so it exercises
    /// the production timeout path, not a test-local wrapper) with a short
    /// handshake deadline, then reads the silent peer's socket under an inner
    /// bound. Pre-HS-1 the read never returns and the bound fires → failure,
    /// demonstrating the hang. Post-HS-1 the read returns EOF promptly → pass. A
    /// whole-test wall-clock watchdog guards against any hang escaping the bound.
    #[test]
    fn hs0_silent_peer_handshake_terminates_and_does_not_hang() {
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_silent_peer_scenario();
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(Duration::from_secs(45)) {
            Ok(()) => worker.join().expect("HS-0 worker thread should not panic"),
            Err(_) => panic!(
                "HS-0 DEADLOCK: a silent peer's inbound handshake never terminated \
                 (untimed read parked the responder forever)"
            ),
        }
    }

    fn run_silent_peer_scenario() {
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build handshake runtime");
        runtime.block_on(async {
            let resolver: Resolver = Arc::new(StaticResolver::new(HashMap::new()));
            // Short handshake deadline so the post-fix path resolves quickly; the
            // pre-fix path has no deadline at all and hangs regardless.
            let manager = ConnectionManager::new(
                Arc::new(AtomTable::with_common_atoms()),
                resolver,
                TEST_COOKIE,
                "server@127.0.0.1",
                1,
            )
            .with_handshake_timeout(Duration::from_secs(2));
            let accept = manager
                .listen("127.0.0.1:0".parse().expect("parse listen addr"))
                .await
                .expect("start accept loop");

            // Silent peer: connect, then never send a single byte. The accept loop
            // spawns a responder that blocks on the first handshake read.
            let mut silent = TcpStream::connect(accept.local_addr())
                .await
                .expect("silent peer connects");

            // Pre-HS-1 the responder never times out, so the server never closes
            // the socket and this read blocks forever (caught by the inner bound).
            // Post-HS-1 the responder hits the deadline, the server drops the
            // stream, and this read returns EOF (Ok(0)).
            let mut byte = [0_u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(15), silent.read(&mut byte)).await;

            let read = read.expect(
                "silent peer's socket was never closed: the inbound responder \
                 parked on an untimed handshake read (HS-1 not in effect)",
            );
            assert_eq!(
                read.expect("reading the closed socket should not error"),
                0,
                "expected EOF after the responder timed out and dropped the stream"
            );

            // No connection should have been registered for the silent peer.
            assert_eq!(manager.connection_count(), 0);
            drop(accept);
        });
    }

    /// HS-0 (convergence): a 3-node full mesh, every node dialing its two peers
    /// simultaneously (barrier-released) from synchronous threads via
    /// `runtime.block_on` — the haematite seam. Each node's accept/responder
    /// tasks share its single worker. After HS-3 exactly one link survives per
    /// pair (no last-writer-wins clobber) and that link is usable in both
    /// directions. Pre-fix this can deadlock or leave mismatched half-links;
    /// run under a hard watchdog so a hang fails the test.
    #[test]
    fn hs0_three_node_simultaneous_dial_mesh_forms_without_deadlock() {
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_three_node_mesh();
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(()) => worker.join().expect("mesh worker thread should not panic"),
            Err(_) => panic!(
                "HS-0 DEADLOCK: 3-node simultaneous-dial mesh did not converge \
                 within the watchdog window (connect never returned)"
            ),
        }
    }

    fn run_three_node_mesh() {
        let names = ["alpha@127.0.0.1", "bravo@127.0.0.1", "charlie@127.0.0.1"];
        // Bind every listener first so the shared resolver maps all names.
        let mut prepared = Vec::new();
        let mut address_map = HashMap::new();
        for name in names {
            let runtime = Arc::new(
                Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .expect("build single-worker node runtime"),
            );
            let listener = runtime
                .block_on(TcpListener::bind("127.0.0.1:0"))
                .expect("bind node listener");
            address_map.insert(name.to_string(), listener.local_addr().expect("addr"));
            prepared.push((name, runtime, listener));
        }
        let resolver: Resolver = Arc::new(StaticResolver::new(address_map));

        let mut nodes = Vec::new();
        for (name, runtime, listener) in prepared {
            let manager = ConnectionManager::new(
                Arc::new(AtomTable::with_common_atoms()),
                Arc::clone(&resolver),
                TEST_COOKIE,
                name,
                1,
            );
            manager.set_runtime_handle(runtime.handle().clone());
            // Count control frames this node's read loops actually deliver. A
            // delivered frame proves the link is whole: the socket this node holds
            // for the peer is the same one the peer reads from. The pre-HS-2/3
            // last-writer-wins clobber can orphan one socket's reader, so a frame
            // written to the surviving write half is never observed here.
            let received = Arc::new(AtomicUsize::new(0));
            let received_for_handler = Arc::clone(&received);
            manager.register_control_frame_handler(move |_control, _payload| {
                received_for_handler.fetch_add(1, Ordering::SeqCst);
            });
            let accept = runtime.block_on(async { manager.listen_with(listener) });
            nodes.push((name, manager, runtime, accept, received));
        }

        // 3 nodes x 2 peers = 6 dialing threads, released together.
        let barrier = Arc::new(Barrier::new(6));
        let mut dialers = Vec::new();
        for (name, manager, runtime, _accept, _received) in &nodes {
            for peer in names {
                if peer == *name {
                    continue;
                }
                let manager = manager.clone();
                let runtime = Arc::clone(runtime);
                let barrier = Arc::clone(&barrier);
                let peer_name = peer.to_string();
                dialers.push(thread::spawn(move || {
                    barrier.wait();
                    let _ = runtime.block_on(manager.connect(&peer_name));
                }));
            }
        }
        for dialer in dialers {
            dialer
                .join()
                .expect("dialer thread should not panic (connect must return)");
        }

        // Exactly one link per pair on every node. Poll: the losing inbound may
        // still be tearing down when the winning `connect` returns.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if nodes
                .iter()
                .all(|(_, manager, _, _, _)| manager.connection_count() == 2)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "mesh did not converge to one link per pair: counts = {:?}",
                nodes
                    .iter()
                    .map(|(_, manager, _, _, _)| manager.connection_count())
                    .collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(25));
        }

        // Every directed edge must carry a frame end-to-end. Each node writes one
        // 8-byte zero header (a zero-length control+payload frame) to each peer
        // link; each node must then OBSERVE the two frames its peers sent it. A
        // clobbered half-link silently drops the frame, so the receiver's count
        // stays below 2 and this fails — the deterministic pre-fix symptom.
        for (name, manager, runtime, _accept, _received) in &nodes {
            for peer in names {
                if peer == *name {
                    continue;
                }
                let peer_atom = manager.inner.atom_table.intern(peer);
                let connection = manager
                    .get_connection(peer_atom)
                    .unwrap_or_else(|| panic!("{name} has no link to {peer}"));
                runtime
                    .block_on(connection.write_raw(&[0_u8; 8]))
                    .unwrap_or_else(|error| {
                        panic!("{name} -> {peer} surviving link not writable: {error}")
                    });
            }
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if nodes
                .iter()
                .all(|(_, _, _, _, received)| received.load(Ordering::SeqCst) >= 2)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "mesh links are not whole bidirectionally: per-node received \
                 frame counts = {:?} (expected >= 2 each)",
                nodes
                    .iter()
                    .map(|(_, _, _, _, received)| received.load(Ordering::SeqCst))
                    .collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}
