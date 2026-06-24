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
use beamr::distribution::DistributionConfig;
use beamr::distribution::pg::RemoteMember;
use beamr::distribution::resolver::{NodeResolver, ResolveError, ResolveFuture};
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::scheduler::{Scheduler, SchedulerConfig};

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
            distribution: Some(DistributionConfig { resolver }),
            ..SchedulerConfig::default()
        },
        module_registry,
        atom_table,
        bif_registry,
        Arc::new(beamr::native::AllCapabilitiesPolicy),
    )
    .expect("scheduler starts")
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

    let a_node_atom = node_a.local_node().name;
    let b_node_atom = node_b.local_node().name;
    // B must attribute A's inbound stream to A's node atom so the PG_UPDATE
    // applies under the correct node and the down-hook keys on it.
    node_b
        .distribution_connections()
        .register_inbound_identifier(move |_| Some(a_node_atom));
    node_a
        .distribution_connections()
        .register_inbound_identifier(move |_| Some(b_node_atom));

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

    let a_node_atom = node_a.local_node().name;
    node_b
        .distribution_connections()
        .register_inbound_identifier(move |_| Some(a_node_atom));
    node_c
        .distribution_connections()
        .register_inbound_identifier(move |_| Some(a_node_atom));

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

    let a_node_atom = node_a.local_node().name;
    node_b
        .distribution_connections()
        .register_inbound_identifier(move |_| Some(a_node_atom));

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
