//! HS-5: cross-node 3-node full-mesh handshake convergence integration test.
//!
//! The distribution handshake deadlock (DISTRIBUTION-HANDSHAKE-DESIGN.md) was
//! first observed by an external multi-process spike: a `>=3`-node mesh, where
//! every node dials every peer, would hang because two simultaneous dials per
//! pair produced two competing TCP connections with no tie-break, and every
//! handshake read was an untimed `read_exact`. This committed test reproduces
//! that scenario inside cargo and asserts the post-HS-1/HS-3 mesh converges.
//!
//! ## What this test DOES exercise (real, not mocked)
//!
//! - Three independent [`ConnectionManager`]s, each with its OWN dedicated tokio
//!   runtime bound via `set_runtime_handle` (production binds the `DistSender`
//!   runtime the same way) and its OWN real `TcpListener` accept loop. There is
//!   no shared runtime across the three nodes.
//! - The real outbound [`ConnectionManager::connect`] path: TCP connect +
//!   bounded OTP handshake (HS-1 deadline) + simultaneous-connect tie-break
//!   (HS-3) + race-safe install (HS-2).
//! - The real inbound accept loop + responder (`handle_accepted` ->
//!   `respond_handshake_async_with` -> `decide_inbound_status`), so the
//!   name-comparison tie-break runs in production form.
//! - All `N*(N-1) = 6` directed dials fired SIMULTANEOUSLY from 6 separate OS
//!   threads released by a [`Barrier`], each driving a blocking `runtime.block_on`
//!   — the exact synchronous-thread + `block_on` shape of the haematite
//!   `DistributionEndpoint::connect` seam that deadlocked.
//! - Convergence assertions: exactly one link per pair on every node (no
//!   last-writer-wins clobber), and every directed edge carries a control frame
//!   END TO END (proving the surviving link is whole — the socket a node holds
//!   for a peer is the same one the peer reads from, so a clobbered half-link is
//!   caught).
//! - A hard wall-clock watchdog (separate thread + `recv_timeout`): pre-HS-1/HS-3
//!   the mesh hangs and the watchdog fails the test; post-fix it converges well
//!   inside the window.
//!
//! ## What this test does NOT cover (caveats)
//!
//! - **Not separate OS processes.** It is a multi-RUNTIME, multi-OS-THREAD
//!   in-process analog. A true `std::process` fork-per-node test is impractical
//!   inside a single `cargo test` binary (no stable cross-process harness, and
//!   port/lifecycle management would dominate). The in-process form still
//!   reproduces the deadlock's root cause — competing simultaneous connects with
//!   per-node isolated runtimes and blocking dials — because the deadlock was a
//!   protocol/arbitration defect, not an address-space artifact. The external
//!   spike confirmed the cross-process variant; this guards the fix in CI.
//! - **Does not drive the haematite `DistributionEndpoint` seam.** It exercises
//!   beamr's `ConnectionManager` directly (the layer that owns the handshake).
//!   The haematite-seam proof is HS-5's optional cross-repo companion.
//! - **Does not exercise the data-frame/message phase** beyond a single
//!   zero-length control frame per edge used as a liveness probe; payload
//!   encoding/decoding is covered elsewhere (`pg_distribution_e2e`).
//! - **Loopback only** (`127.0.0.1`): no real network latency, partition, or MTU
//!   effects.

#![cfg(feature = "net")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use beamr::atom::AtomTable;
use beamr::distribution::ConnectionManager;
use beamr::distribution::resolver::{NodeResolver, StaticResolver};
use tokio::net::TcpListener;
use tokio::runtime::{Builder, Runtime};

const TEST_COOKIE: &str = "mesh-handshake-cookie";

/// Globally-unique, ordered node names so the HS-3 name-comparison tie-break has
/// a deterministic winner per pair.
const NAMES: [&str; 3] = ["alpha@127.0.0.1", "bravo@127.0.0.1", "charlie@127.0.0.1"];

type SharedResolver = Arc<dyn NodeResolver + Send + Sync>;

/// One mesh node: its manager, the runtime its lifecycle tasks run on, the accept
/// handle kept alive for the test duration, and a counter of control frames its
/// read loops have actually delivered.
struct MeshNode {
    name: &'static str,
    manager: ConnectionManager,
    runtime: Arc<Runtime>,
    received: Arc<AtomicUsize>,
    _accept: beamr::distribution::connection::AcceptHandle,
}

/// HS-5: a 3-node full mesh where every node simultaneously dials both peers must
/// converge to exactly one whole link per pair, under a hard watchdog. Pre-fix
/// this hangs (untimed handshake reads + no simultaneous-connect arbitration);
/// post-HS-1/HS-3 it converges promptly.
#[test]
fn hs5_three_node_mesh_converges_cross_runtime_under_watchdog() {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        run_mesh_scenario();
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => worker
            .join()
            .expect("HS-5 mesh worker thread should not panic"),
        Err(_) => panic!(
            "HS-5 DEADLOCK: 3-node simultaneous-dial mesh did not converge within \
             the 60s watchdog (a connect never returned — the pre-HS-1/HS-3 symptom)"
        ),
    }
}

fn run_mesh_scenario() {
    let nodes = build_nodes();
    drive_simultaneous_dials(&nodes);
    await_one_link_per_pair(&nodes);
    probe_every_directed_edge(&nodes);
    await_all_edges_whole(&nodes);
}

/// Build all three nodes: bind every listener FIRST (so the shared resolver can
/// map every name to its address before any dial fires), then construct one
/// `ConnectionManager` per node bound to its own dedicated runtime and accept
/// loop, with a control-frame counter installed.
fn build_nodes() -> Vec<MeshNode> {
    struct Prepared {
        name: &'static str,
        runtime: Arc<Runtime>,
        listener: TcpListener,
    }

    let mut prepared = Vec::new();
    let mut address_map = HashMap::new();
    for name in NAMES {
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
        prepared.push(Prepared {
            name,
            runtime,
            listener,
        });
    }
    let resolver: SharedResolver = Arc::new(StaticResolver::new(address_map));

    let mut nodes = Vec::new();
    for Prepared {
        name,
        runtime,
        listener,
    } in prepared
    {
        let manager = ConnectionManager::new(
            Arc::new(AtomTable::with_common_atoms()),
            Arc::clone(&resolver),
            TEST_COOKIE,
            name,
            1,
        );
        manager.set_runtime_handle(runtime.handle().clone());

        // A delivered control frame proves the link is whole: the socket this node
        // holds for the peer is the same one the peer reads from. A clobbered
        // half-link silently drops the frame, so the count stays below 2.
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = Arc::clone(&received);
        manager.register_control_frame_handler(move |_control, _payload| {
            received_for_handler.fetch_add(1, Ordering::SeqCst);
        });

        let accept = runtime.block_on(async { manager.listen_with(listener) });
        nodes.push(MeshNode {
            name,
            manager,
            runtime,
            received,
            _accept: accept,
        });
    }
    nodes
}

/// Fire all 6 directed dials simultaneously from separate OS threads, each
/// driving a blocking `runtime.block_on(connect)` — the haematite seam shape.
fn drive_simultaneous_dials(nodes: &[MeshNode]) {
    let barrier = Arc::new(Barrier::new(NAMES.len() * (NAMES.len() - 1)));
    let mut dialers = Vec::new();
    for node in nodes {
        for peer in NAMES {
            if peer == node.name {
                continue;
            }
            let manager = node.manager.clone();
            let runtime = Arc::clone(&node.runtime);
            let barrier = Arc::clone(&barrier);
            let peer_name = peer.to_string();
            dialers.push(thread::spawn(move || {
                barrier.wait();
                // The result is intentionally ignored: a benign simultaneous-abort
                // (`nok`) is a valid outcome for the losing side of a pair; the
                // reciprocal link is the survivor. Convergence is asserted on the
                // table state below, not on per-dial return values. `connect` must,
                // however, RETURN — a hang is caught by the outer watchdog.
                let _ = runtime.block_on(manager.connect(&peer_name));
            }));
        }
    }
    for dialer in dialers {
        dialer
            .join()
            .expect("a dialer thread panicked (connect must always return)");
    }
}

/// Poll until every node holds exactly one link per pair (count == 2). The losing
/// inbound of a simultaneous connect may still be tearing down when the winning
/// `connect` returns, so this tolerates a brief settling window.
fn await_one_link_per_pair(nodes: &[MeshNode]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if nodes
            .iter()
            .all(|node| node.manager.connection_count() == NAMES.len() - 1)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "mesh did not converge to one link per pair: counts = {:?}",
            nodes
                .iter()
                .map(|node| node.manager.connection_count())
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

/// Write one zero-length control+payload frame (8 zero header bytes) on every
/// directed edge, proving each surviving link's write half is reachable.
fn probe_every_directed_edge(nodes: &[MeshNode]) {
    for node in nodes {
        for peer in NAMES {
            if peer == node.name {
                continue;
            }
            let peer_atom = node.manager_atom(peer);
            let connection = node
                .manager
                .get_connection(peer_atom)
                .unwrap_or_else(|| panic!("{} has no link to {peer}", node.name));
            node.runtime
                .block_on(connection.write_raw(&[0_u8; 8]))
                .unwrap_or_else(|error| {
                    panic!(
                        "{} -> {peer} surviving link not writable: {error}",
                        node.name
                    )
                });
        }
    }
}

/// Poll until every node has OBSERVED both frames its peers sent it. A clobbered
/// half-link drops the frame, so the receiver's count stays below 2 and this
/// fails — the deterministic pre-fix symptom.
fn await_all_edges_whole(nodes: &[MeshNode]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if nodes
            .iter()
            .all(|node| node.received.load(Ordering::SeqCst) >= NAMES.len() - 1)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "mesh links are not whole bidirectionally: per-node received frame \
             counts = {:?} (expected >= {} each)",
            nodes
                .iter()
                .map(|node| node.received.load(Ordering::SeqCst))
                .collect::<Vec<_>>(),
            NAMES.len() - 1
        );
        thread::sleep(Duration::from_millis(25));
    }
}

impl MeshNode {
    /// Intern a peer name into this node's atom table for table lookups.
    fn manager_atom(&self, name: &str) -> beamr::atom::Atom {
        self.manager.atom_table().intern(name)
    }
}
