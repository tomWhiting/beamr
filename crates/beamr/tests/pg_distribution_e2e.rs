//! Cross-node process-group propagation end-to-end tests.
//!
//! Exercises the PIECE 2 distribution wiring: a local `pg` join/leave on one
//! node is encoded as a `PG_UPDATE` control frame, transmitted over a real
//! loopback TCP distribution link, decoded on the peer, and applied to the
//! peer's `PgRegistry` remote-member view. Also verifies the connection-down
//! hook purges a lost node's remote members.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beamr::atom::AtomTable;
use beamr::distribution::pg::RemoteMember;
use beamr::distribution::resolver::{NodeResolver, ResolveError, ResolveFuture};
use beamr::distribution::{DEFAULT_COOKIE, DistributionConfig};
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::{Actor, ActorContext, ActorMessage, NativeContext, spawn_actor};

#[derive(Default)]
struct DynamicResolver {
    nodes: Mutex<HashMap<String, SocketAddr>>,
}

impl DynamicResolver {
    fn insert(&self, name: &str, addr: SocketAddr) {
        self.nodes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(name.to_owned(), addr);
    }
}

impl NodeResolver for DynamicResolver {
    fn resolve<'a>(&'a self, name: &'a str) -> ResolveFuture<'a> {
        let result = self
            .nodes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(name)
            .copied()
            .ok_or(ResolveError::NotFound);
        Box::pin(async move { result })
    }
}

fn scheduler(
    node_name: &str,
    resolver: Arc<DynamicResolver>,
    atom_table: Arc<AtomTable>,
) -> Scheduler {
    let bif_registry = Arc::new(BifRegistryImpl::new());
    let module_registry = Arc::new(ModuleRegistry::new());
    Scheduler::with_code_server_and_policy(
        SchedulerConfig {
            thread_count: Some(1),
            node_name: Some(node_name.to_owned()),
            distribution: Some(DistributionConfig {
                resolver,
                cookie: DEFAULT_COOKIE.to_owned(),
            }),
            ..SchedulerConfig::default()
        },
        module_registry,
        atom_table,
        bif_registry,
        Arc::new(beamr::native::AllCapabilitiesPolicy),
    )
    .expect("scheduler starts")
}

fn scheduler_arc(
    node_name: &str,
    resolver: Arc<DynamicResolver>,
    atom_table: Arc<AtomTable>,
) -> Arc<Scheduler> {
    Arc::new(scheduler(node_name, resolver, atom_table))
}

/// A minimal actor that never exits on its own. Used as a real
/// scheduler-supervised process whose lifetime the test controls via
/// `exit_signal`, so its exit deterministically drives `cleanup_exited_process`.
struct Idle;

/// Trivial unit message carried by the idle actor (never sent in these tests).
#[derive(Clone)]
struct Ping;

impl ActorMessage for Ping {
    fn encode(&self, _ctx: &mut NativeContext<'_>) -> Option<Term> {
        Some(Term::NIL)
    }
    fn decode(_term: Term) -> Option<Self> {
        Some(Self)
    }
}

impl Actor for Idle {
    type Call = Ping;
    type Reply = Ping;
    type Cast = Ping;

    fn handle_call(&mut self, _request: Ping, _ctx: &mut ActorContext<'_, '_>) -> Ping {
        Ping
    }
    fn handle_cast(&mut self, _request: Ping, _ctx: &mut ActorContext<'_, '_>) {}
}

/// Poll `predicate` for up to ~1s, sleeping between attempts.
async fn eventually(mut predicate: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

/// A join on node A is transmitted to node B and reflected in B's remote
/// members as an external PID carrying A's node name; disconnecting A from B
/// then purges A's remote members via the connection-down hook.
#[tokio::test]
async fn pg_join_visible_on_peer_and_purged_on_node_down() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("listen address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("listen address parses"))
        .await
        .expect("node B listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());

    // Identity is established by the OTP handshake during connect/accept: B
    // attributes A's inbound stream to A's advertised node name, so the
    // PG_UPDATE applies under the correct node and the down-hook keys on it.
    let a_node_atom = node_a.local_node().name;

    // Establish A -> B so A's broadcast has a connected node to transmit to.
    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");

    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("workers");
    let member_pid = 4242_u64;

    // Local join on A: broadcasts a PG_UPDATE join frame to every connected node.
    node_a.pg_registry().join(scope, group, member_pid);

    let expected = RemoteMember {
        node: a_node_atom,
        pid_number: member_pid,
        serial: 0,
    };
    let registry_b = node_b.pg_registry();
    let saw_join = eventually(|| registry_b.remote_members(scope, group).contains(&expected)).await;
    assert!(
        saw_join,
        "B should observe A's remote pg member after a join"
    );

    // Node-down: disconnect A from B's view. B's connection-down hook must purge
    // every remote member that belonged to A.
    assert!(
        node_b
            .distribution_connections()
            .disconnect_node(a_node_atom)
    );
    let purged = eventually(|| registry_b.remote_members(scope, group).is_empty()).await;
    assert!(purged, "B should purge A's remote members on node-down");

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}

/// The async sender is CONNECTED-ONLY: it never triggers an inline reconnect.
///
/// A joins a member while it has NO connection to B. The join returns promptly
/// and B never observes that member over the full poll window (the update is
/// dropped, not buffered). After A then connects to B and joins a *second*
/// member, B sees ONLY the second member — proving the first was never replayed
/// from a buffer.
#[tokio::test]
async fn pg_join_is_connected_only_and_not_replayed() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node B listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());

    // Identity established by the handshake on accept; no address-trust seam.
    let a_node_atom = node_a.local_node().name;

    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("connected_only");
    let first_pid = 11_u64;
    let second_pid = 22_u64;

    // A has NO connection to B yet. This join must not reach B and must return
    // promptly (no inline connect from the send path).
    node_a.pg_registry().join(scope, group, first_pid);

    let first_member = RemoteMember {
        node: a_node_atom,
        pid_number: first_pid,
        serial: 0,
    };
    let registry_b = node_b.pg_registry();
    // Over the full window B must never see the first member.
    let leaked = eventually(|| {
        registry_b
            .remote_members(scope, group)
            .contains(&first_member)
    })
    .await;
    assert!(
        !leaked,
        "a join with no connection must not reach B (connected-only, no inline reconnect)"
    );

    // Now connect A -> B and join a second member.
    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");
    node_a.pg_registry().join(scope, group, second_pid);

    let second_member = RemoteMember {
        node: a_node_atom,
        pid_number: second_pid,
        serial: 0,
    };
    let saw_second = eventually(|| {
        registry_b
            .remote_members(scope, group)
            .contains(&second_member)
    })
    .await;
    assert!(saw_second, "B should observe the second (connected) join");

    // The first member must NOT have been buffered and replayed on connect.
    assert!(
        !registry_b
            .remote_members(scope, group)
            .contains(&first_member),
        "the pre-connection join must not be replayed from a buffer"
    );

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}

/// A real scheduler-supervised process that is a pg member, when terminated,
/// drives `cleanup_exited_process`: A's local membership empties (sync local
/// purge) and the leave is propagated so B drops the remote member (async).
#[tokio::test]
async fn process_exit_purges_local_and_propagates_leave() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler_arc(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node B listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());

    // Identity established by the handshake on accept; no address-trust seam.
    let a_node_atom = node_a.local_node().name;

    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");

    // Spawn a REAL process on A and make it a pg member.
    let actor = spawn_actor(&node_a, || Idle).expect("spawn idle actor");
    let member_pid = actor.pid;
    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("supervised");
    node_a.pg_registry().join(scope, group, member_pid);

    let expected = RemoteMember {
        node: a_node_atom,
        pid_number: member_pid,
        serial: 0,
    };
    let registry_a = node_a.pg_registry();
    let registry_b = node_b.pg_registry();
    assert!(
        eventually(|| registry_b.remote_members(scope, group).contains(&expected)).await,
        "B should observe the live process as a remote member"
    );

    // Terminate the process. The exit path runs the synchronous local purge and
    // propagates the leave asynchronously through the installed sender.
    node_a
        .exit_signal(0, member_pid, ExitReason::Kill)
        .expect("exit signal delivered");

    assert!(
        eventually(|| registry_a.local_members(scope, group).is_empty()).await,
        "A's local membership should empty after the process exits"
    );
    assert!(
        eventually(|| !registry_b.remote_members(scope, group).contains(&expected)).await,
        "B should drop the remote member after the process exits on A"
    );

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}

/// A join broadcast reaches every connected node (one PG_UPDATE frame per peer):
/// with A connected to both B and C, a single join on A is reflected on both.
#[tokio::test]
async fn pg_join_broadcasts_to_every_connected_node() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_c = scheduler(
        "c@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node B listens");
    let listen_c = node_c
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node C listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());
    resolver.insert("c@127.0.0.1", listen_c.local_addr());

    // Identity established by the handshake on accept; no address-trust seam.
    let a_node_atom = node_a.local_node().name;

    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");
    node_a
        .distribution_connections()
        .connect("c@127.0.0.1")
        .await
        .expect("A connects to C");

    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("fanout");
    let member_pid = 7_u64;
    node_a.pg_registry().join(scope, group, member_pid);

    let expected = RemoteMember {
        node: a_node_atom,
        pid_number: member_pid,
        serial: 0,
    };
    let registry_b = node_b.pg_registry();
    let registry_c = node_c.pg_registry();
    let on_b = eventually(|| registry_b.remote_members(scope, group).contains(&expected)).await;
    let on_c = eventually(|| registry_c.remote_members(scope, group).contains(&expected)).await;
    assert!(on_b, "B should observe the join");
    assert!(on_c, "C should observe the join");

    listen_a.shutdown();
    listen_b.shutdown();
    listen_c.shutdown();
    node_a.shutdown();
    node_b.shutdown();
    node_c.shutdown();
}

/// Down -> up -> rejoin reconnection contract: A and B share pg membership over
/// a live link; B loses A (connection-down purges A's remote members); A then
/// re-dials B and joins afresh. The post-reconnect view must match a FRESH join
/// — the newly joined member is visible, and the pre-down member is NOT silently
/// resurrected from stale state.
#[tokio::test]
async fn reconnection_down_up_rejoin_reestablishes_membership_without_stale_resurrection() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node B listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());

    let a_node_atom = node_a.local_node().name;
    let b_node_atom = node_b.local_node().name;

    // Phase 1 — UP: A connects to B and joins a member; B observes it.
    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B (initial)");

    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("rejoin");
    let pre_down_pid = 100_u64;
    node_a.pg_registry().join(scope, group, pre_down_pid);

    let pre_down_member = RemoteMember {
        node: a_node_atom,
        pid_number: pre_down_pid,
        serial: 0,
    };
    let registry_b = node_b.pg_registry();
    assert!(
        eventually(|| registry_b
            .remote_members(scope, group)
            .contains(&pre_down_member))
        .await,
        "B observes A's member while the link is up"
    );

    // Phase 2 — DOWN: B drops A. The connection-down hook purges A's remote
    // members and B no longer lists A as connected.
    assert!(
        node_b
            .distribution_connections()
            .disconnect_node(a_node_atom),
        "B disconnects A"
    );
    assert!(
        eventually(|| registry_b.remote_members(scope, group).is_empty()).await,
        "B purges A's remote members on node-down"
    );
    assert!(
        eventually(|| !node_b
            .distribution_connections()
            .connected_nodes()
            .contains(&a_node_atom))
        .await,
        "B no longer lists A as connected after the drop"
    );

    // Phase 3 — REJOIN: A re-dials B and joins a NEW member. The fresh link must
    // re-establish membership exactly as a first-time join would.
    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A re-dials B after the drop");
    // The re-dial reinstates the link in both directions' connection tables.
    assert!(
        eventually(|| node_a
            .distribution_connections()
            .connected_nodes()
            .contains(&b_node_atom))
        .await,
        "A lists B as connected again after the re-dial"
    );

    let rejoin_pid = 200_u64;
    node_a.pg_registry().join(scope, group, rejoin_pid);
    let rejoin_member = RemoteMember {
        node: a_node_atom,
        pid_number: rejoin_pid,
        serial: 0,
    };
    assert!(
        eventually(|| registry_b
            .remote_members(scope, group)
            .contains(&rejoin_member))
        .await,
        "B observes the post-reconnect join (membership re-established like a fresh join)"
    );

    // The pre-down member must NOT have been resurrected from stale state: the
    // purge was real and the rejoin carried only the new member.
    assert!(
        !registry_b
            .remote_members(scope, group)
            .contains(&pre_down_member),
        "the pre-down member must stay purged, not silently resurrected on reconnect"
    );

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}

/// A local leave (e.g. from `remove_pid_from_all_scopes` on process exit) is
/// transmitted to the peer and removes the corresponding remote member there.
#[tokio::test]
async fn pg_local_leave_transmits_and_removes_remote_member() {
    let resolver = Arc::new(DynamicResolver::default());
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let node_a = scheduler(
        "a@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );
    let node_b = scheduler(
        "b@127.0.0.1",
        Arc::clone(&resolver),
        Arc::clone(&atom_table),
    );

    let listen_a = node_a
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node A listens");
    let listen_b = node_b
        .distribution_connections()
        .listen("127.0.0.1:0".parse().expect("address parses"))
        .await
        .expect("node B listens");
    resolver.insert("a@127.0.0.1", listen_a.local_addr());
    resolver.insert("b@127.0.0.1", listen_b.local_addr());

    // Identity established by the handshake on accept; no address-trust seam.
    let a_node_atom = node_a.local_node().name;

    node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");

    let scope = node_a.pg_registry().default_scope();
    let group = atom_table.intern("ephemeral");
    let member_pid = 99_u64;

    node_a.pg_registry().join(scope, group, member_pid);
    let expected = RemoteMember {
        node: a_node_atom,
        pid_number: member_pid,
        serial: 0,
    };
    let registry_b = node_b.pg_registry();
    assert!(
        eventually(|| registry_b.remote_members(scope, group).contains(&expected)).await,
        "B should observe the join before the leave"
    );

    // Local exit path: removing the pid from all scopes broadcasts a leave,
    // which now transmits to B and drops the remote member there.
    node_a.pg_registry().remove_pid_from_all_scopes(member_pid);
    assert!(
        eventually(|| !registry_b.remote_members(scope, group).contains(&expected)).await,
        "B should drop the remote member after A's leave"
    );

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}
