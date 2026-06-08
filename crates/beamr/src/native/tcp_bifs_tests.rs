use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::IntoRawFd;
use std::sync::{Arc, Mutex};

use crate::atom::{Atom, AtomTable};
use crate::io::resource::{FdInner, FdResource};
use crate::io::{CompletionRing, IoCompletion, IoOp, IoResult};
use crate::native::{
    BifRegistryImpl, Capability, FileIoCompletion, FileIoContinuation, FileIoFacility,
    ProcessContext,
};
use crate::process::{CodePosition, Process, ReceiveTimeout};
use crate::term::Term;
use crate::term::boxed::{Tuple, write_cons, write_tuple};

use super::{register_tcp_bifs, tcp_accept, tcp_listen};

const PID: u64 = 77;

#[derive(Default)]
struct MockRing {
    next_op_id: Mutex<u64>,
    submitted: Mutex<Vec<IoOp>>,
}

impl MockRing {
    fn submitted(&self) -> Vec<IoOp> {
        self.submitted.lock().expect("submitted lock").clone()
    }
}

impl CompletionRing for MockRing {
    fn submit(&self, op: IoOp) -> u64 {
        self.submitted.lock().expect("submitted lock").push(op);
        let mut next = self.next_op_id.lock().expect("next op id lock");
        let op_id = *next;
        *next += 1;
        op_id
    }

    fn poll_completions(&self, _timeout: std::time::Duration) -> Vec<IoCompletion> {
        Vec::new()
    }

    fn pending_count(&self) -> usize {
        self.submitted.lock().map(|ops| ops.len()).unwrap_or(0)
    }

    fn shutdown(&self) {}
}

#[derive(Default)]
struct MockFileIoFacility {
    ring: MockRing,
    pending: Mutex<Vec<(u64, u64, FileIoContinuation)>>,
    completions: Mutex<VecDeque<FileIoCompletion>>,
}

impl MockFileIoFacility {
    fn push_completion(&self, continuation: FileIoContinuation, result: io::Result<IoResult>) {
        self.completions
            .lock()
            .expect("completions lock")
            .push_back(FileIoCompletion {
                op_id: 1,
                continuation,
                completion: IoCompletion { op_id: 1, result },
            });
    }

    fn submitted(&self) -> Vec<IoOp> {
        self.ring.submitted()
    }

    fn tracked(&self) -> Vec<(u64, u64, FileIoContinuation)> {
        self.pending.lock().expect("pending lock").clone()
    }
}

impl FileIoFacility for MockFileIoFacility {
    fn submit_file_io(&self, pid: u64, op: IoOp, continuation: FileIoContinuation) -> u64 {
        let op_id = self.ring.submit(op);
        self.track_submitted_file_io(pid, op_id, continuation);
        op_id
    }

    fn track_submitted_file_io(&self, pid: u64, op_id: u64, continuation: FileIoContinuation) {
        self.pending
            .lock()
            .expect("pending lock")
            .push((pid, op_id, continuation));
    }

    fn take_file_io_completion(&self, _pid: u64) -> Option<FileIoCompletion> {
        self.completions
            .lock()
            .expect("completions lock")
            .pop_front()
    }

    fn cancel_pending_file_io_for_pid(&self, pid: u64) {
        self.pending
            .lock()
            .expect("pending lock")
            .retain(|(pending_pid, _, _)| *pending_pid != pid);
    }

    fn ring(&self) -> &dyn CompletionRing {
        &self.ring
    }
}

fn context<'a>(
    process: &'a mut Process,
    atom_table: Arc<AtomTable>,
    facility: Option<Arc<MockFileIoFacility>>,
) -> ProcessContext<'a> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(atom_table));
    if let Some(facility) = facility {
        context.set_file_io_facility(Some(facility));
    }
    context.attach_process(process, 0);
    context
}

fn list1(head: Term) -> Term {
    let cell = Box::leak(Box::new([0_u64; 2]));
    write_cons(cell, head, Term::NIL).expect("cons")
}

fn option_tuple(key: Term, value: Term) -> Term {
    let tuple = Box::leak(Box::new([0_u64; 3]));
    write_tuple(tuple, &[key, value]).expect("option tuple")
}

#[test]
fn register_tcp_bifs_registers_listener_and_accept_mfas() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();

    register_tcp_bifs(&registry, &atom_table).expect("tcp registration");

    let erlang = atom_table.intern("erlang");
    for (name, arity) in [("tcp_listen", 2), ("tcp_accept", 1), ("tcp_accept", 2)] {
        let function = atom_table.intern(name);
        let entry = registry
            .lookup(erlang, function, arity)
            .expect("registered TCP BIF");
        assert_eq!(entry.capability, Capability::ExternalIo);
    }
}

#[test]
fn tcp_listen_returns_fd_resource_for_random_port() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, None);

    let result = tcp_listen(&[Term::small_int(0), Term::NIL], &mut context).expect("listen");

    let resource = FdResource::new(result).expect("fd resource");
    assert!(resource.fd() >= 0);
    assert_eq!(resource.owner_pid(), PID);
}

#[test]
fn tcp_listen_parses_ipv4_backlog_and_reuseaddr_options() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let ip_key = Term::atom(atom_table.intern("ip"));
    let ip_tuple = Box::leak(Box::new([0_u64; 5]));
    let ip_value = write_tuple(
        ip_tuple,
        &[
            Term::small_int(127),
            Term::small_int(0),
            Term::small_int(0),
            Term::small_int(1),
        ],
    )
    .expect("ip tuple");
    let option = option_tuple(ip_key, ip_value);
    let options = list1(option);
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, None);

    let result = tcp_listen(&[Term::small_int(0), options], &mut context).expect("listen");

    assert!(FdResource::new(result).is_some());
}

#[test]
fn tcp_accept_submits_accept_and_suspends() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    assert_eq!(
        tcp_accept(&[resource], &mut context),
        Ok(Term::atom(Atom::OK))
    );

    assert_eq!(facility.submitted(), vec![IoOp::Accept { listener_fd: fd }]);
    assert!(matches!(
        facility.tracked().as_slice(),
        [(PID, 0, FileIoContinuation::Accept)]
    ));
    assert_eq!(context.take_suspend().expect("suspend").timeout_ms, None);
}

#[test]
fn tcp_accept_completion_returns_ok_fd_resource() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
    let listener_fd = listener.into_raw_fd();
    let accepted = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("accepted fd stand-in");
    let accepted_fd = accepted.into_raw_fd();
    facility.push_completion(
        FileIoContinuation::Accept,
        Ok(IoResult::Accepted(
            accepted_fd,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 12345)),
        )),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(listener_fd, PID)))
        .expect("fd resource");

    let result = tcp_accept(&[resource], &mut context).expect("accept result");

    let tuple = Tuple::new(result).expect("ok tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    let conn = FdResource::new(tuple.get(1).expect("conn fd")).expect("conn resource");
    assert_eq!(conn.fd(), accepted_fd);
    assert_eq!(conn.owner_pid(), PID);
}

#[test]
fn tcp_accept_timeout_zero_returns_timeout_without_submit() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_accept(&[resource, Term::small_int(0)], &mut context).expect("timeout");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::TIMEOUT)));
    assert!(facility.submitted().is_empty());
}

#[test]
fn tcp_accept_timeout_reentry_returns_timeout() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    process.set_receive_timeout(Some(ReceiveTimeout {
        timeout_position: CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        },
        milliseconds: 1,
    }));
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_accept(&[resource, Term::small_int(1)], &mut context).expect("timeout");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::TIMEOUT)));
    assert!(facility.submitted().is_empty());
}
