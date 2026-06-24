use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beamr::atom::{Atom, AtomTable};
use beamr::distribution::resolver::{NodeResolver, ResolveError, ResolveFuture};
use beamr::distribution::{DEFAULT_COOKIE, DistributionConfig};
use beamr::module::ModuleRegistry;
use beamr::native::BifRegistryImpl;
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::term::Term;
use beamr::term::boxed::write_external_pid;

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

#[tokio::test]
async fn loopback_cross_node_pid_send_round_trip() {
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

    // Identity is now established by the OTP handshake during connect/accept —
    // no address-trust seam. The connection table is keyed by each peer's
    // advertised handshake name.
    let a_node_atom = node_a.local_node().name;
    let b_node_atom = node_b.local_node().name;

    let pid_a = node_a.spawn_test_process(false);
    let pid_b = node_b.spawn_test_process(false);
    let mut heap = [0_u64; 8];
    let remote_b = write_external_pid(&mut heap[..4], b_node_atom, pid_b, 0).expect("remote B pid");
    let remote_a = write_external_pid(&mut heap[4..], a_node_atom, pid_a, 0).expect("remote A pid");

    let frame_to_b = beamr::distribution::control::encode_send_frame(
        Term::atom(Atom::OK),
        remote_b,
        Term::atom(Atom::OK),
        &atom_table,
    )
    .expect("frame to B encodes");
    let conn_to_b = node_a
        .distribution_connections()
        .connect("b@127.0.0.1")
        .await
        .expect("A connects to B");
    conn_to_b
        .write_raw(&frame_to_b)
        .await
        .expect("A writes to B");

    for _ in 0..20 {
        if node_b.has_message(pid_b, Term::atom(Atom::OK)) == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(node_b.has_message(pid_b, Term::atom(Atom::OK)), Some(true));

    let frame_to_a = beamr::distribution::control::encode_send_frame(
        Term::atom(Atom::OK),
        remote_a,
        Term::atom(Atom::OK),
        &atom_table,
    )
    .expect("frame to A encodes");
    let conn_to_a = node_b
        .distribution_connections()
        .connect("a@127.0.0.1")
        .await
        .expect("B connects to A");
    conn_to_a
        .write_raw(&frame_to_a)
        .await
        .expect("B writes to A");

    for _ in 0..20 {
        if node_a.has_message(pid_a, Term::atom(Atom::OK)) == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(node_a.has_message(pid_a, Term::atom(Atom::OK)), Some(true));

    listen_a.shutdown();
    listen_b.shutdown();
    node_a.shutdown();
    node_b.shutdown();
}
