use super::*;
use crate::atom::{Atom, AtomTable};
use crate::distribution::connection::ConnectionManager;
use crate::distribution::control::{DistributionSendError, DistributionSendFacility};
use crate::distribution::resolver::StaticResolver;
use crate::distribution::{DEFAULT_NODE_NAME, NetKernel, Node};
use crate::native::spawn::{
    SpawnError, SpawnFacility, SpawnMonitorResult, SpawnOptions, SpawnOptionsResult, SpawnRecord,
};
use crate::native::supervision::{
    MonitorResult, SupervisionError, SupervisionFacility, SupervisionRecord,
};
use crate::native::{BifRegistryImpl, Capability, ProcessContext};
use crate::process::ExitReason;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary;
use crate::term::boxed::{
    BigInt, Float, write_closure, write_cons, write_external_pid, write_external_reference,
    write_float, write_tuple,
};
use crate::term::reference_ref::ReferenceRef;

use std::sync::{Arc, Mutex};

struct NoConnectionDistributionSend;

impl DistributionSendFacility for NoConnectionDistributionSend {
    fn send_remote(&self, _target: Term, _message: Term) -> Result<(), DistributionSendError> {
        Err(DistributionSendError::NoConnection)
    }
}

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn context_with_atom_table(process: &mut Process) -> (Arc<AtomTable>, ProcessContext<'_>) {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = context(process);
    ctx.set_atom_table(Some(table.clone()));
    (table, ctx)
}

fn atom(table: &AtomTable, name: &str) -> Term {
    Term::atom(table.intern(name))
}

fn list2(head: Term, tail_head: Term) -> Term {
    let tail_heap = Box::leak(Box::new([0u64; 2]));
    let head_heap = Box::leak(Box::new([0u64; 2]));
    let tail = write_cons(tail_heap, tail_head, Term::NIL).expect("tail cons");
    write_cons(head_heap, head, tail).expect("head cons")
}

fn float(value: f64) -> Term {
    let heap = Box::leak(Box::new([0u64; 2]));
    write_float(heap, value).expect("float")
}

// ---- erlang:element/2 ----

#[test]
fn element_returns_first_element() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let elements = [
        Term::small_int(10),
        Term::small_int(20),
        Term::small_int(30),
    ];
    let mut heap = [0u64; 4];
    let tuple = write_tuple(&mut heap, &elements).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(1), tuple], &mut ctx),
        Ok(Term::small_int(10))
    );
}

#[test]
fn element_returns_last_element() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let elements = [
        Term::small_int(10),
        Term::small_int(20),
        Term::small_int(30),
    ];
    let mut heap = [0u64; 4];
    let tuple = write_tuple(&mut heap, &elements).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(3), tuple], &mut ctx),
        Ok(Term::small_int(30))
    );
}

#[test]
fn element_badarg_index_zero() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(0), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_index_out_of_range() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(2), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_negative_index() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::small_int(-1), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_non_tuple() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_element(&[Term::small_int(1), Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_non_integer_index() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 2];
    let tuple = write_tuple(&mut heap, &[Term::small_int(1)]).expect("tuple");
    assert_eq!(
        bif_element(&[Term::atom(Atom::OK), tuple], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn element_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_element(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:send/2 ----

#[test]
fn send_returns_message() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let message = Term::atom(Atom::OK);
    assert_eq!(bif_send(&[Term::pid(1), message], &mut ctx), Ok(message));
}

#[test]
fn send_badarg_non_pid() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_send(&[Term::small_int(1), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn send_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_send(&[Term::pid(1)], &mut ctx), Err(badarg()));
}

#[test]
fn send_remote_pid_without_connection_returns_noconnection_atom() {
    let mut process = Process::new(1, 128);
    let (atom_table, mut ctx) = context_with_atom_table(&mut process);
    ctx.set_distribution_send_facility(Some(Arc::new(NoConnectionDistributionSend)));
    let remote_node = atom_table.intern("remote@127.0.0.1");
    let mut heap = [0_u64; 4];
    let remote = write_external_pid(&mut heap, remote_node, 99, 0).expect("remote pid");
    let noconnection = atom(&atom_table, "noconnection");

    assert_eq!(
        bif_send(&[remote, Term::atom(Atom::OK)], &mut ctx),
        Err(noconnection)
    );
}

// ---- erlang:tuple_size/1 ----

#[test]
fn tuple_size_returns_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 4];
    let tuple = write_tuple(
        &mut heap,
        &[Term::small_int(1), Term::small_int(2), Term::small_int(3)],
    )
    .expect("tuple");
    assert_eq!(bif_tuple_size(&[tuple], &mut ctx), Ok(Term::small_int(3)));
}

#[test]
fn tuple_size_empty_tuple() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 1];
    let tuple = write_tuple(&mut heap, &[]).expect("empty tuple");
    assert_eq!(bif_tuple_size(&[tuple], &mut ctx), Ok(Term::small_int(0)));
}

#[test]
fn tuple_size_badarg_non_tuple() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_tuple_size(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn tuple_size_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_tuple_size(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:make_ref/0 ----

#[test]
fn make_ref_returns_local_reference() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let result = bif_make_ref(&[], &mut ctx).expect("make_ref");
    assert!(ReferenceRef::new(result).is_some());
}

#[test]
fn make_ref_returns_unique_values() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let ref1 = bif_make_ref(&[], &mut ctx).expect("make_ref 1");
    let ref2 = bif_make_ref(&[], &mut ctx).expect("make_ref 2");
    assert_ne!(ref1, ref2);
}

#[test]
fn make_ref_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_make_ref(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

// ---- erlang:node/0,1 ----

fn context_with_local_node<'a>(
    process: &'a mut Process,
    atom_table: &AtomTable,
) -> ProcessContext<'a> {
    let local_name = atom_table.intern("nonode@nohost");
    let mut ctx = context(process);
    ctx.set_local_node(Some(Node::new(local_name, 0)));
    ctx
}

#[test]
fn node_0_returns_local_node_name() {
    let atom_table = AtomTable::new();
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);
    let local_name = atom_table.intern("nonode@nohost");

    assert_eq!(bif_node_0(&[], &mut ctx), Ok(Term::atom(local_name)));
}

#[test]
fn node_1_returns_local_node_for_local_pid() {
    let atom_table = AtomTable::new();
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);
    let local_name = atom_table.intern("nonode@nohost");

    assert_eq!(
        bif_node_1(&[Term::pid(1)], &mut ctx),
        Ok(Term::atom(local_name))
    );
}

#[test]
fn node_1_returns_remote_node_for_remote_pid() {
    let atom_table = AtomTable::new();
    let remote_name = atom_table.intern("remote@example.test");
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);
    let mut heap = [0_u64; 4];
    let pid = write_external_pid(&mut heap, remote_name, 42, 3).expect("remote pid");

    assert_eq!(bif_node_1(&[pid], &mut ctx), Ok(Term::atom(remote_name)));
}

#[test]
fn node_1_returns_local_node_for_local_reference() {
    let atom_table = AtomTable::new();
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);
    let local_name = atom_table.intern("nonode@nohost");
    let reference = bif_make_ref(&[], &mut ctx).expect("make_ref");

    assert_eq!(
        bif_node_1(&[reference], &mut ctx),
        Ok(Term::atom(local_name))
    );
}

#[test]
fn node_1_returns_remote_node_for_remote_reference() {
    let atom_table = AtomTable::new();
    let remote_name = atom_table.intern("remote@example.test");
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);
    let mut heap = [0_u64; 3];
    let reference = write_external_reference(&mut heap, remote_name, 99).expect("remote ref");

    assert_eq!(
        bif_node_1(&[reference], &mut ctx),
        Ok(Term::atom(remote_name))
    );
}

#[test]
fn node_1_badarg_for_non_pid_non_reference() {
    let atom_table = AtomTable::new();
    let mut process = Process::new(1, 128);
    let mut ctx = context_with_local_node(&mut process, &atom_table);

    assert_eq!(bif_node_1(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}

#[test]
fn is_alive_false_without_distribution_node() {
    let mut ctx = ProcessContext::new();

    assert_eq!(bif_is_alive_0(&[], &mut ctx), Ok(Term::atom(Atom::FALSE)));
}

#[test]
fn is_alive_false_for_default_node_name() {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(Arc::clone(&table)));
    ctx.set_local_node(Some(Node::new(table.intern(DEFAULT_NODE_NAME), 0)));

    assert_eq!(bif_is_alive_0(&[], &mut ctx), Ok(Term::atom(Atom::FALSE)));
}

#[test]
fn is_alive_true_for_real_node_name() {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(Arc::clone(&table)));
    ctx.set_local_node(Some(Node::new(table.intern("beamr@test"), 0)));

    assert_eq!(bif_is_alive_0(&[], &mut ctx), Ok(Term::atom(Atom::TRUE)));
}

#[test]
fn nodes_returns_connected_node_atoms() {
    let table = Arc::new(AtomTable::with_common_atoms());
    let node = table.intern("remote@test");
    let manager = ConnectionManager::new(
        Arc::clone(&table),
        Arc::new(StaticResolver::new(std::collections::HashMap::new())),
    );
    let net_kernel = Arc::new(NetKernel::new(manager.clone()));
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    ctx.set_net_kernel(Some(net_kernel));

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("failed to bind listener: {error}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("failed to read listener addr: {error}"));
    let client = std::net::TcpStream::connect(addr)
        .unwrap_or_else(|error| panic!("failed to connect client: {error}"));
    let (server, peer_addr) = listener
        .accept()
        .unwrap_or_else(|error| panic!("failed to accept client: {error}"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("failed to build test runtime: {error}"));
    runtime.block_on(async {
        manager
            .register_test_connection(node, peer_addr, server)
            .unwrap_or_else(|error| panic!("failed to register test connection: {error}"));
    });
    drop(client);

    let list = bif_nodes_0(&[], &mut ctx).expect("nodes/0 result");
    let cons = Cons::new(list).expect("single-node list");
    assert_eq!(cons.head(), Term::atom(node));
    assert_eq!(cons.tail(), Term::NIL);
}

#[test]
fn disconnect_node_removes_node_from_nodes() {
    let table = Arc::new(AtomTable::with_common_atoms());
    let node = table.intern("remote@test");
    let manager = ConnectionManager::new(
        Arc::clone(&table),
        Arc::new(StaticResolver::new(std::collections::HashMap::new())),
    );
    let net_kernel = Arc::new(NetKernel::new(manager.clone()));
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    ctx.set_net_kernel(Some(net_kernel));

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("failed to bind listener: {error}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("failed to read listener addr: {error}"));
    let client = std::net::TcpStream::connect(addr)
        .unwrap_or_else(|error| panic!("failed to connect client: {error}"));
    let (server, peer_addr) = listener
        .accept()
        .unwrap_or_else(|error| panic!("failed to accept client: {error}"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("failed to build test runtime: {error}"));
    runtime.block_on(async {
        manager
            .register_test_connection(node, peer_addr, server)
            .unwrap_or_else(|error| panic!("failed to register test connection: {error}"));
    });
    drop(client);

    assert_eq!(
        bif_disconnect_node_1(&[Term::atom(node)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(bif_nodes_0(&[], &mut ctx), Ok(Term::NIL));
}

// ---- erlang:is_process_alive/1 ----

#[test]
fn is_process_alive_self_is_true() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(5));
    assert_eq!(
        bif_is_process_alive(&[Term::pid(5)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn is_process_alive_false_without_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_is_process_alive(&[Term::pid(99)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
}

#[test]
fn is_process_alive_true_with_facility() {
    let (_, mut ctx) = sup_ctx(100, 1, true);
    assert_eq!(
        bif_is_process_alive(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn is_process_alive_false_dead_process() {
    let (_, mut ctx) = sup_ctx(100, 1, false);
    assert_eq!(
        bif_is_process_alive(&[Term::pid(2)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );
}

#[test]
fn is_process_alive_badarg_non_pid() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    assert_eq!(
        bif_is_process_alive(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn is_process_alive_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_is_process_alive(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn/1 ----

#[test]
fn spawn_1_with_zero_arity_closure() {
    let (f, mut ctx) = spawn_ctx(42, 1);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Ok(Term::pid(42)));
    let records = f.lambda_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].caller_pid, 1);
    assert_eq!(records[0].link_to, None);
}

#[test]
fn spawn_1_badarg_non_zero_arity() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 2, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_with_captures() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    let free_vars = [Term::small_int(1)];
    let mut heap = [0u64; 8];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &free_vars).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_non_closure() {
    let (_, mut ctx) = spawn_ctx(42, 1);
    assert_eq!(bif_spawn_1(&[Term::small_int(42)], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_no_facility() {
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(1));
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_1(&[fun], &mut ctx), Err(badarg()));
}

#[test]
fn spawn_1_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(bif_spawn_1(&[], &mut ctx), Err(badarg()));
}

// ---- erlang:spawn_link/1 ----

#[test]
fn spawn_link_1_sets_link_to_parent() {
    let (f, mut ctx) = spawn_ctx(42, 3);
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Ok(Term::pid(42)));
    let records = f.lambda_records();
    assert_eq!(records[0].caller_pid, 3);
    assert_eq!(records[0].link_to, Some(3));
}

#[test]
fn spawn_link_1_badarg_without_pid() {
    let f: Arc<dyn SpawnFacility> = Arc::new(MockSpawnFacility::new(42));
    let mut ctx = ProcessContext::new();
    ctx.set_spawn_facility(Some(f));
    let mut heap = [0u64; 7];
    let fun = write_closure(&mut heap, Atom::OK, 0, 0, 1, 0, &[]).expect("closure");
    assert_eq!(bif_spawn_link_1(&[fun], &mut ctx), Err(badarg()));
}

// ---- erlang:byte_size/1 and erlang:iolist_size/1 ----

#[test]
fn byte_size_returns_binary_length() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").expect("binary");
    assert_eq!(bif_byte_size(&[bin], &mut ctx), Ok(Term::small_int(5)));
}

#[test]
fn byte_size_rejects_non_binary() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_byte_size(&[Term::small_int(5)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn iolist_size_returns_binary_length() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").expect("binary");
    assert_eq!(bif_iolist_size(&[bin], &mut ctx), Ok(Term::small_int(5)));
}

#[test]
fn iolist_size_rejects_complex_iolist_stub() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let mut cell = [0u64; 2];
    let list =
        crate::term::boxed::write_cons(&mut cell, Term::small_int(65), Term::NIL).expect("list");
    assert_eq!(bif_iolist_size(&[list], &mut ctx), Err(badarg()));
}

// ---- erlang:phash2/1,2 ----

#[test]
fn phash2_is_deterministic_and_range_bounded() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let term = Term::small_int(42);
    let hash1 = bif_phash2_1(&[term], &mut ctx).expect("phash2/1");
    let hash2 = bif_phash2_1(&[term], &mut ctx).expect("phash2/1 again");
    assert_eq!(hash1, hash2);
    let hash = hash1.as_small_int().expect("small int hash");
    assert!((0..(1_i64 << 27)).contains(&hash));

    let ranged = bif_phash2_2(&[term, Term::small_int(10)], &mut ctx).expect("phash2/2");
    let ranged = ranged.as_small_int().expect("small int ranged hash");
    assert!((0..10).contains(&ranged));
}

#[test]
fn phash2_rejects_invalid_range() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_phash2_2(&[Term::small_int(1), Term::small_int(0)], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_phash2_2(&[Term::small_int(1), Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_phash2_2(&[Term::small_int(1), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:monotonic_time/system_time/time_offset ----

#[test]
fn monotonic_time_is_nondecreasing() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let first = bif_monotonic_time_0(&[], &mut ctx)
        .expect("monotonic 1")
        .as_small_int()
        .expect("small int");
    let second = bif_monotonic_time_0(&[], &mut ctx)
        .expect("monotonic 2")
        .as_small_int()
        .expect("small int");
    assert!(second >= first);
}

#[test]
fn time_unit_millisecond_converts_native_time() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    let native = bif_monotonic_time_0(&[], &mut ctx)
        .expect("native")
        .as_small_int()
        .expect("native small int");
    let millis = bif_monotonic_time_1(&[atom(&table, "millisecond")], &mut ctx)
        .expect("millisecond")
        .as_small_int()
        .expect("millisecond small int");
    assert!(millis <= native / 1_000 + 1);
}

#[test]
fn time_unit_rejects_unknown_or_non_atom_units() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    assert_eq!(
        bif_monotonic_time_1(&[atom(&table, "fortnight")], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_system_time_1(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn system_time_and_time_offset_return_small_ints() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert!(
        bif_system_time_0(&[], &mut ctx)
            .expect("system time")
            .as_small_int()
            .is_some()
    );
    assert!(
        bif_time_offset_0(&[], &mut ctx)
            .expect("time offset")
            .as_small_int()
            .is_some()
    );
}

// ---- erlang:unique_integer/0,1 ----

#[test]
fn unique_integer_returns_unique_positive_monotonic_values() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    let first = bif_unique_integer_0(&[], &mut ctx)
        .expect("unique 1")
        .as_small_int()
        .expect("small int");
    let second = bif_unique_integer_0(&[], &mut ctx)
        .expect("unique 2")
        .as_small_int()
        .expect("small int");
    assert_ne!(first, second);
    assert!(first > 0);
    assert!(second > first);
}

#[test]
fn unique_integer_accepts_positive_and_monotonic_options() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    let options = list2(atom(&table, "positive"), atom(&table, "monotonic"));
    let value = bif_unique_integer_1(&[options], &mut ctx)
        .expect("unique options")
        .as_small_int()
        .expect("small int");
    assert!(value > 0);
}

#[test]
fn unique_integer_rejects_unknown_option() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    let options = list2(atom(&table, "positive"), atom(&table, "unknown"));
    assert_eq!(bif_unique_integer_1(&[options], &mut ctx), Err(badarg()));
}

#[test]
fn unique_integer_rejects_improper_and_non_atom_options() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    let mut improper_heap = [0u64; 2];
    let improper = write_cons(
        &mut improper_heap,
        atom(&table, "positive"),
        Term::small_int(1),
    )
    .expect("improper options");
    let mut non_atom_heap = [0u64; 2];
    let non_atom =
        write_cons(&mut non_atom_heap, Term::small_int(1), Term::NIL).expect("non-atom options");
    assert_eq!(bif_unique_integer_1(&[improper], &mut ctx), Err(badarg()));
    assert_eq!(bif_unique_integer_1(&[non_atom], &mut ctx), Err(badarg()));
}

// ---- erlang:min/2, max/2, abs/1 ----

#[test]
fn min_and_max_use_beam_term_order_with_atom_names() {
    let mut process = Process::new(1, 128);
    let (table, mut ctx) = context_with_atom_table(&mut process);
    assert_eq!(
        bif_min_2(&[Term::small_int(1), Term::small_int(2)], &mut ctx),
        Ok(Term::small_int(1))
    );
    assert_eq!(
        bif_max_2(&[Term::small_int(1), Term::small_int(2)], &mut ctx),
        Ok(Term::small_int(2))
    );
    let z_atom = atom(&table, "z");
    let a_atom = atom(&table, "a");
    assert_eq!(bif_min_2(&[z_atom, a_atom], &mut ctx), Ok(a_atom));
    assert_eq!(bif_max_2(&[a_atom, z_atom], &mut ctx), Ok(z_atom));
}

#[test]
fn abs_handles_numbers_and_rejects_non_numbers() {
    let mut process = Process::new(1, 128);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_abs_1(&[Term::small_int(-5)], &mut ctx),
        Ok(Term::small_int(5))
    );
    assert_eq!(
        bif_abs_1(&[Term::small_int(5)], &mut ctx),
        Ok(Term::small_int(5))
    );
    let float_result = bif_abs_1(&[float(-2.5)], &mut ctx).expect("float abs");
    assert_eq!(Float::new(float_result).expect("float").value(), 2.5);
    assert_eq!(bif_abs_1(&[Term::atom(Atom::OK)], &mut ctx), Err(badarg()));
}

#[test]
fn abs_promotes_small_overflow_and_accepts_bignums() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);

    // abs(SMALL_INT_MIN) leaves the small range and promotes to a bignum.
    let promoted = bif_abs_1(&[Term::small_int(Term::SMALL_INT_MIN)], &mut ctx).expect("promotes");
    let bigint = BigInt::new(promoted).expect("bignum box");
    assert!(!bigint.is_negative());
    assert_eq!(bigint.limbs(), [Term::SMALL_INT_MIN.unsigned_abs()]);

    // abs(-(10^20)) -> 100000000000000000000 keeps the magnitude, drops the sign.
    let magnitude = 100_000_000_000_000_000_000_u128;
    let limbs = [magnitude as u64, (magnitude >> 64) as u64];
    let negative = ctx.alloc_bigint(true, &limbs).expect("bignum");
    let absolute = bif_abs_1(&[negative], &mut ctx).expect("abs");
    let bigint = BigInt::new(absolute).expect("bignum box");
    assert!(!bigint.is_negative());
    assert_eq!(bigint.limbs(), limbs);

    // A non-canonical bignum holding -5 demotes to the small immediate 5.
    let small_magnitude = ctx.alloc_bigint(true, &[5]).expect("bignum");
    assert_eq!(
        bif_abs_1(&[small_magnitude], &mut ctx),
        Ok(Term::small_int(5))
    );
}

// ---- Registration ----

#[test]
fn register_gate3_bifs_registers_all() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate3_bifs(&reg, &at).expect("gate 3 registration");
    let erlang = at.intern("erlang");
    for (name, arity) in [
        ("element", 2),
        ("send", 2),
        ("tuple_size", 1),
        ("make_ref", 0),
        ("node", 0),
        ("node", 1),
        ("is_process_alive", 1),
        ("spawn", 1),
        ("spawn_link", 1),
        // Type conversion BIFs (R1)
        ("list_to_atom", 1),
        ("atom_to_list", 1),
        ("list_to_existing_atom", 1),
        ("list_to_integer", 1),
        ("list_to_float", 1),
        ("float_to_list", 1),
        ("float_to_binary", 2),
        ("binary_to_atom", 1),
        ("atom_to_binary", 1),
        ("atom_to_binary", 2),
        ("binary_to_existing_atom", 1),
        ("binary_to_existing_atom", 2),
        ("binary_to_list", 1),
        ("list_to_binary", 1),
        ("map_get", 2),
        // Process registry BIFs (R2)
        ("register", 2),
        ("unregister", 1),
        ("whereis", 1),
        // demonitor/2 (R3)
        ("demonitor", 2),
        // Gleam stdlib support (B-033)
        ("byte_size", 1),
        ("iolist_size", 1),
        // Additional erlang BIFs (B-037)
        ("round", 1),
        ("trunc", 1),
        ("is_bitstring", 1),
        ("is_map_key", 2),
        ("map_size", 1),
        ("binary_part", 3),
        ("bit_size", 1),
        ("-", 1),
        // B-129 hashing, time, unique values, and misc utilities.
        ("phash2", 1),
        ("phash2", 2),
        ("monotonic_time", 0),
        ("system_time", 0),
        ("monotonic_time", 1),
        ("system_time", 1),
        ("time_offset", 0),
        ("unique_integer", 0),
        ("unique_integer", 1),
        ("min", 2),
        ("max", 2),
        ("abs", 1),
    ] {
        assert!(
            reg.lookup(erlang, at.intern(name), arity).is_some(),
            "missing erlang:{name}/{arity}"
        );
    }

    for (name, arity) in [("spawn", 1), ("spawn_link", 1)] {
        let entry = reg
            .lookup(erlang, at.intern(name), arity)
            .unwrap_or_else(|| panic!("missing erlang:{name}/{arity}"));
        assert_eq!(entry.capability, Capability::Spawn);
    }
}

#[test]
fn register_gate3_bifs_fails_twice() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    register_gate3_bifs(&reg, &at).expect("first");
    assert!(register_gate3_bifs(&reg, &at).is_err());
}

#[test]
fn all_three_gates_coexist() {
    let at = AtomTable::new();
    let reg = BifRegistryImpl::new();
    crate::native::bifs::register_gate1_bifs(&reg, &at).expect("gate 1");
    crate::native::process_bifs::register_gate2_bifs(&reg, &at).expect("gate 2");
    register_gate3_bifs(&reg, &at).expect("gate 3");
    let erlang = at.intern("erlang");
    // Gate 1
    assert!(reg.lookup(erlang, at.intern("+"), 2).is_some());
    // Gate 2
    assert!(reg.lookup(erlang, at.intern("self"), 0).is_some());
    // Gate 3
    assert!(reg.lookup(erlang, at.intern("element"), 2).is_some());
    assert!(reg.lookup(erlang, at.intern("make_ref"), 0).is_some());
    assert!(reg.lookup(erlang, at.intern("term_to_binary"), 1).is_some());
    assert!(reg.lookup(erlang, at.intern("binary_to_term"), 1).is_some());
}

// ---- Helpers ----

fn spawn_ctx(next_pid: u64, caller_pid: u64) -> (Arc<MockSpawnFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockSpawnFacility::new(next_pid));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_spawn_facility(Some(f.clone()));
    (f, ctx)
}

fn sup_ctx(
    next_ref: u64,
    caller_pid: u64,
    alive: bool,
) -> (Arc<MockSupervisionFacility>, ProcessContext<'static>) {
    let f = Arc::new(MockSupervisionFacility::new(next_ref, alive));
    let mut ctx = ProcessContext::new();
    ctx.set_pid(Some(caller_pid));
    ctx.set_supervision_facility(Some(f.clone()));
    (f, ctx)
}

// ---- Mock spawn facility ----

struct LambdaSpawnRecord {
    caller_pid: u64,
    link_to: Option<u64>,
}

struct MockSpawnFacility {
    next_pid: u64,
    records: Mutex<Vec<SpawnRecord>>,
    lambda_records: Mutex<Vec<LambdaSpawnRecord>>,
}

impl MockSpawnFacility {
    fn new(next_pid: u64) -> Self {
        Self {
            next_pid,
            records: Mutex::new(Vec::new()),
            lambda_records: Mutex::new(Vec::new()),
        }
    }
    fn lambda_records(&self) -> Vec<LambdaSpawnRecord> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

impl SpawnFacility for MockSpawnFacility {
    fn spawn(
        &self,
        caller_pid: u64,
        module: Atom,
        function: Atom,
        args: Vec<Term>,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SpawnRecord {
                caller_pid,
                module,
                function,
                args,
                link_to,
            });
        Ok(self.next_pid)
    }

    fn spawn_monitor(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _function: Atom,
        _args: Vec<Term>,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        Ok(SpawnMonitorResult {
            pid: self.next_pid,
            reference: 0,
        })
    }

    fn spawn_lambda(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        link_to: Option<u64>,
    ) -> Result<u64, SpawnError> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaSpawnRecord {
                caller_pid,
                link_to,
            });
        Ok(self.next_pid)
    }

    fn spawn_lambda_monitor(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
    ) -> Result<SpawnMonitorResult, SpawnError> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaSpawnRecord {
                caller_pid,
                link_to: None,
            });
        Ok(SpawnMonitorResult {
            pid: self.next_pid,
            reference: 0,
        })
    }

    fn spawn_with_options(
        &self,
        _caller_pid: u64,
        _module: Atom,
        _function: Atom,
        _args: Vec<Term>,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        Ok(SpawnOptionsResult {
            pid: self.next_pid,
            reference: options.monitor.then_some(0),
        })
    }

    fn spawn_lambda_with_options(
        &self,
        caller_pid: u64,
        _module: Atom,
        _lambda_index: u32,
        options: SpawnOptions,
    ) -> Result<SpawnOptionsResult, SpawnError> {
        self.lambda_records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LambdaSpawnRecord {
                caller_pid,
                link_to: options.link.then_some(caller_pid),
            });
        Ok(SpawnOptionsResult {
            pid: self.next_pid,
            reference: options.monitor.then_some(0),
        })
    }
}

// ---- Mock supervision facility ----

struct MockSupervisionFacility {
    next_reference: u64,
    target_alive: bool,
    records: Mutex<Vec<SupervisionRecord>>,
}

impl MockSupervisionFacility {
    fn new(next_reference: u64, target_alive: bool) -> Self {
        Self {
            next_reference,
            target_alive,
            records: Mutex::new(Vec::new()),
        }
    }
}

impl SupervisionFacility for MockSupervisionFacility {
    fn monitor(&self, caller_pid: u64, target_pid: u64) -> Result<MonitorResult, SupervisionError> {
        if !self.target_alive {
            return Err(SupervisionError::NoProc);
        }
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Monitor {
                caller_pid,
                target_pid,
            });
        Ok(MonitorResult {
            reference: self.next_reference,
            immediate_down: false,
        })
    }

    fn demonitor(&self, caller_pid: u64, reference: u64) -> Result<(), SupervisionError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SupervisionRecord::Demonitor {
                caller_pid,
                reference,
            });
        Ok(())
    }

    fn exit_signal(
        &self,
        caller_pid: u64,
        target_pid: u64,
        reason: ExitReason,
    ) -> Result<(), SupervisionError> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(
            SupervisionRecord::ExitSignal {
                caller_pid,
                target_pid,
                reason,
            },
        );
        Ok(())
    }
}
