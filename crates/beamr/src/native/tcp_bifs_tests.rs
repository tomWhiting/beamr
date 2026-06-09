use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::{IntoRawFd, RawFd};
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
use crate::term::binary::{Binary, packed_word_count, write_binary};
use crate::term::boxed::{Tuple, write_cons, write_tuple};

use super::{register_tcp_bifs, tcp_accept, tcp_connect, tcp_listen, tcp_recv, tcp_send};

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

fn binary_term(bytes: &[u8]) -> Term {
    let words = 2 + packed_word_count(bytes.len());
    let heap = Box::leak(vec![0_u64; words].into_boxed_slice());
    write_binary(heap, bytes).expect("binary")
}

fn fd_is_closed(fd: RawFd) -> bool {
    // SAFETY: F_GETFD only observes descriptor table state for the supplied raw fd.
    let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
}

#[test]
fn register_tcp_bifs_registers_listener_accept_connect_send_recv_mfas() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();

    register_tcp_bifs(&registry, &atom_table).expect("tcp registration");

    let erlang = atom_table.intern("erlang");
    for (name, arity) in [
        ("tcp_listen", 2),
        ("tcp_accept", 1),
        ("tcp_accept", 2),
        ("tcp_connect", 3),
        ("tcp_send", 2),
        ("tcp_recv", 2),
        ("tcp_recv", 3),
    ] {
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
fn tcp_listen_in_use_port_returns_error_tuple() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind occupied port");
    let port = occupied.local_addr().expect("occupied addr").port();
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
    let options = list1(option_tuple(ip_key, ip_value));
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, None);

    let result = tcp_listen(&[Term::small_int(i64::from(port)), options], &mut context)
        .expect("listen error tuple");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
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
fn tcp_accept_timeout_does_not_leak_fd() {
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
    process.set_receive_timeout(Some(ReceiveTimeout {
        timeout_position: CodePosition {
            module: Atom::OK,
            instruction_pointer: 0,
        },
        milliseconds: 1,
    }));
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(listener_fd, PID)))
        .expect("fd resource");

    let result = tcp_accept(&[resource, Term::small_int(1)], &mut context).expect("timeout");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::TIMEOUT)));
    assert!(fd_is_closed(accepted_fd));
    assert!(facility.tracked().is_empty());
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

#[test]
fn tcp_connect_submits_connect_and_suspends() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let host = binary_term(b"127.0.0.1");

    let result =
        tcp_connect(&[host, Term::small_int(9), Term::NIL], &mut context).expect("connect submit");

    assert_eq!(result, Term::atom(Atom::OK));
    assert!(matches!(
        facility.submitted().as_slice(),
        [IoOp::Connect { fd, addr }]
            if *fd >= 0 && *addr == SocketAddr::from((Ipv4Addr::LOCALHOST, 9))
    ));
    assert!(matches!(
        facility.tracked().as_slice(),
        [(PID, 0, FileIoContinuation::Connect { .. })]
    ));
    assert_eq!(context.take_suspend().expect("suspend").timeout_ms, None);
}

#[test]
fn tcp_connect_completion_returns_ok_fd_resource() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let inner = Arc::new(FdInner::new(fd, PID));
    facility.push_completion(
        FileIoContinuation::Connect {
            fd: Arc::clone(&inner),
        },
        Ok(IoResult::Connected),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(facility));

    let result = tcp_connect(
        &[binary_term(b"127.0.0.1"), Term::small_int(9), Term::NIL],
        &mut context,
    )
    .expect("connect completion");

    let tuple = Tuple::new(result).expect("ok tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    let resource = FdResource::new(tuple.get(1).expect("fd resource")).expect("fd resource");
    assert_eq!(resource.fd(), fd);
    assert_eq!(resource.owner_pid(), PID);
}

#[test]
fn tcp_connect_refused_returns_econnrefused() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let inner = Arc::new(FdInner::new(fd, PID));
    facility.push_completion(
        FileIoContinuation::Connect { fd: inner },
        Err(io::Error::from_raw_os_error(libc::ECONNREFUSED)),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(facility));

    let result = tcp_connect(
        &[binary_term(b"127.0.0.1"), Term::small_int(9), Term::NIL],
        &mut context,
    )
    .expect("connect refused");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::ECONNREFUSED)));
}

#[test]
fn tcp_send_submits_stream_write() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_send(&[resource, binary_term(b"hello")], &mut context).expect("send submit");

    assert_eq!(result, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Write {
            fd,
            data: b"hello".to_vec(),
            offset: u64::MAX,
        }]
    );
    assert!(matches!(
        facility.tracked().as_slice(),
        [(PID, 0, FileIoContinuation::TcpWrite { remaining, .. })] if remaining == b"hello"
    ));
}

#[test]
fn tcp_send_resubmits_partial_write_then_returns_ok() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let inner = Arc::new(FdInner::new(fd, PID));
    facility.push_completion(
        FileIoContinuation::TcpWrite {
            fd: Arc::clone(&inner),
            remaining: b"abcdef".to_vec(),
            bytes_written: 0,
        },
        Ok(IoResult::BytesWritten(2)),
    );
    facility.push_completion(
        FileIoContinuation::TcpWrite {
            fd: inner,
            remaining: b"cdef".to_vec(),
            bytes_written: 2,
        },
        Ok(IoResult::BytesWritten(4)),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));

    let first = tcp_send(&[Term::NIL, Term::NIL], &mut context).expect("partial completion");
    assert_eq!(first, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Write {
            fd,
            data: b"cdef".to_vec(),
            offset: u64::MAX,
        }]
    );

    let second = tcp_send(&[Term::NIL, Term::NIL], &mut context).expect("final completion");
    assert_eq!(second, Term::atom(Atom::OK));
}

#[test]
fn tcp_send_connection_reset_returns_closed() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    facility.push_completion(
        FileIoContinuation::TcpWrite {
            fd: Arc::new(FdInner::new(fd, PID)),
            remaining: b"x".to_vec(),
            bytes_written: 0,
        },
        Err(io::Error::from_raw_os_error(libc::ECONNRESET)),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(facility));

    let result = tcp_send(&[Term::NIL, Term::NIL], &mut context).expect("closed");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::CLOSED)));
}

#[test]
fn tcp_recv_submits_stream_read_with_timeout() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_recv(
        &[resource, Term::small_int(5), Term::small_int(100)],
        &mut context,
    )
    .expect("recv submit");

    assert_eq!(result, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Read {
            fd,
            buf_len: 5,
            offset: u64::MAX,
        }]
    );
    assert_eq!(
        context.take_suspend().expect("suspend").timeout_ms,
        Some(100)
    );
}

#[test]
fn tcp_recv_zero_length_uses_default_buffer() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_recv(&[resource, Term::small_int(0)], &mut context).expect("recv submit");

    assert_eq!(result, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Read {
            fd,
            buf_len: 64 * 1024,
            offset: u64::MAX,
        }]
    );
}

#[test]
fn tcp_recv_large_exact_length_reads_in_bounded_chunks() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));
    let resource = context
        .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
        .expect("fd resource");

    let result = tcp_recv(
        &[resource, Term::small_int((64 * 1024 + 1) as i64)],
        &mut context,
    )
    .expect("large recv submit");

    assert_eq!(result, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Read {
            fd,
            buf_len: 64 * 1024,
            offset: u64::MAX,
        }]
    );
}

#[test]
fn tcp_recv_exact_length_accumulates_multiple_reads() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    let inner = Arc::new(FdInner::new(fd, PID));
    facility.push_completion(
        FileIoContinuation::TcpRead {
            fd: Arc::clone(&inner),
            requested_len: 5,
            accumulated: Vec::new(),
            timeout_ms: None,
        },
        Ok(IoResult::BytesRead(2, b"he".to_vec())),
    );
    facility.push_completion(
        FileIoContinuation::TcpRead {
            fd: inner,
            requested_len: 5,
            accumulated: b"he".to_vec(),
            timeout_ms: None,
        },
        Ok(IoResult::BytesRead(3, b"llo".to_vec())),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(Arc::clone(&facility)));

    let first = tcp_recv(&[Term::NIL, Term::NIL], &mut context).expect("partial recv");
    assert_eq!(first, Term::atom(Atom::OK));
    assert_eq!(
        facility.submitted(),
        vec![IoOp::Read {
            fd,
            buf_len: 3,
            offset: u64::MAX,
        }]
    );

    let second = tcp_recv(&[Term::NIL, Term::NIL], &mut context).expect("complete recv");
    let tuple = Tuple::new(second).expect("ok tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    let binary = Binary::new(tuple.get(1).expect("binary")).expect("binary");
    assert_eq!(binary.as_bytes(), b"hello");
}

#[test]
fn tcp_recv_zero_byte_close_returns_closed() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
    let fd = listener.into_raw_fd();
    facility.push_completion(
        FileIoContinuation::TcpRead {
            fd: Arc::new(FdInner::new(fd, PID)),
            requested_len: 1,
            accumulated: Vec::new(),
            timeout_ms: None,
        },
        Ok(IoResult::BytesRead(0, Vec::new())),
    );
    let mut process = Process::new(PID, 128);
    let mut context = context(&mut process, atom_table, Some(facility));

    let result = tcp_recv(&[Term::NIL, Term::NIL], &mut context).expect("closed");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::CLOSED)));
}

#[test]
fn tcp_recv_timeout_returns_timeout() {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let facility = Arc::new(MockFileIoFacility::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("fd stand-in");
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

    let result = tcp_recv(
        &[resource, Term::small_int(1), Term::small_int(1)],
        &mut context,
    )
    .expect("timeout");

    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(Atom::TIMEOUT)));
    assert!(facility.submitted().is_empty());
}

// ── tcp_setopts + tcp_controlling_process tests ─────────────────────────────

use crate::io::resource::{FD_RESOURCE_WORDS, FdMode, write_fd_resource};
use crate::native::TcpIoFacility;

#[derive(Default)]
struct MockTcpIoFacility {
    submissions: Mutex<Vec<(Arc<FdInner>, usize)>>,
}

impl TcpIoFacility for MockTcpIoFacility {
    fn submit_active_tcp_read(&self, socket: Arc<FdInner>, buf_len: usize) -> Option<u64> {
        let mut submissions = self
            .submissions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        submissions.push((socket, buf_len));
        Some(submissions.len() as u64)
    }
}

fn socket_term(socket: Arc<FdInner>) -> (Vec<u64>, Term) {
    let mut heap = vec![0; FD_RESOURCE_WORDS];
    let term = write_fd_resource(&mut heap, socket).expect("fd resource term");
    (heap, term)
}

fn active_option_list(atom_table: &AtomTable, value: Term) -> (Vec<u64>, Vec<u64>, Term) {
    let active = atom_table.intern("active");
    let mut tuple_heap = vec![0; 3];
    let option = write_tuple(&mut tuple_heap, &[Term::atom(active), value]).expect("option tuple");
    let mut cons_heap = vec![0; 2];
    let list = write_cons(&mut cons_heap, option, Term::NIL).expect("option list");
    (tuple_heap, cons_heap, list)
}

fn context_with_pid(pid: u64) -> (Arc<AtomTable>, ProcessContext<'static>) {
    let atom_table = Arc::new(AtomTable::with_common_atoms());
    let mut context = ProcessContext::new();
    context.set_pid(Some(pid));
    context.set_atom_table(Some(Arc::clone(&atom_table)));
    (atom_table, context)
}

#[test]
fn register_tcp_bifs_includes_setopts_and_controlling_process() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();
    register_tcp_bifs(&registry, &atom_table).expect("tcp registration");

    let erlang = atom_table.intern("erlang");
    for (name, arity) in [("tcp_setopts", 2), ("tcp_controlling_process", 2)] {
        let function = atom_table.intern(name);
        let entry = registry
            .lookup(erlang, function, arity)
            .expect("registered TCP BIF");
        assert_eq!(entry.capability, Capability::ExternalIo);
    }
}

#[test]
fn tcp_setopts_active_from_passive_starts_read_loop() {
    let (atom_table, mut context) = context_with_pid(7);
    let facility = Arc::new(MockTcpIoFacility::default());
    context.set_tcp_io_facility(Some(facility.clone()));
    let socket = Arc::new(FdInner::new(55, 7));
    let (_socket_heap, socket_term_val) = socket_term(Arc::clone(&socket));
    let (_tuple_heap, _cons_heap, options) =
        active_option_list(&atom_table, Term::atom(Atom::TRUE));

    assert_eq!(
        super::tcp_setopts(&[socket_term_val, options], &mut context),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(socket.mode(), FdMode::Active);
    let submissions = facility
        .submissions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].0.fd(), 55);
}

#[test]
fn tcp_setopts_active_once_from_active_does_not_start_duplicate_read() {
    let (atom_table, mut context) = context_with_pid(8);
    let facility = Arc::new(MockTcpIoFacility::default());
    context.set_tcp_io_facility(Some(facility.clone()));
    let socket = Arc::new(FdInner::new(56, 8));
    socket.set_mode(FdMode::Active);
    let (_socket_heap, socket_term_val) = socket_term(Arc::clone(&socket));
    let once = atom_table.intern("once");
    let (_tuple_heap, _cons_heap, options) = active_option_list(&atom_table, Term::atom(once));

    assert_eq!(
        super::tcp_setopts(&[socket_term_val, options], &mut context),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(socket.mode(), FdMode::ActiveOnce);
    assert!(
        facility
            .submissions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
}

#[test]
fn tcp_setopts_passive_stops_future_resubmits_without_facility() {
    let (atom_table, mut context) = context_with_pid(9);
    let socket = Arc::new(FdInner::new(57, 9));
    socket.set_mode(FdMode::Active);
    let (_socket_heap, socket_term_val) = socket_term(Arc::clone(&socket));
    let (_tuple_heap, _cons_heap, options) =
        active_option_list(&atom_table, Term::atom(Atom::FALSE));

    assert_eq!(
        super::tcp_setopts(&[socket_term_val, options], &mut context),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(socket.mode(), FdMode::Passive);
}

#[test]
fn tcp_controlling_process_transfers_only_from_current_controller() {
    let (atom_table, mut owner_context) = context_with_pid(10);
    let socket = Arc::new(FdInner::new(58, 10));
    let (_socket_heap, socket_term_val) = socket_term(Arc::clone(&socket));

    assert_eq!(
        super::tcp_controlling_process(&[socket_term_val, Term::pid(11)], &mut owner_context),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(socket.controlling_process(), 11);

    let mut process = Process::new(12, 128);
    let mut not_owner_context = ProcessContext::new();
    not_owner_context.set_atom_table(Some(atom_table));
    not_owner_context.attach_process(&mut process, 0);
    let not_owner = not_owner_context
        .atom_table()
        .expect("atom table")
        .intern("not_owner");
    let result =
        super::tcp_controlling_process(&[socket_term_val, Term::pid(12)], &mut not_owner_context)
            .expect("not_owner tuple");
    let tuple = Tuple::new(result).expect("error tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::atom(not_owner)));
    assert_eq!(socket.controlling_process(), 11);
}
