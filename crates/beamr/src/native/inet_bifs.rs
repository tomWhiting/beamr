//! Socket-oriented inet BIFs for FdResource-backed TCP/UDP descriptors.

use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::{Mutex, OnceLock};

use rustix::net::SocketAddrV4;
use rustix::net::sockopt;

use crate::atom::{Atom, AtomTable};
use crate::io::resource::{FdResource, FdState};
use crate::io::{IoResult, errno_to_atom};
use crate::native::{
    BifRegistryImpl, Capability, FileIoCompletion, FileIoContinuation, NativeRegistrationError,
    ProcessContext,
};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

/// Registers Erlang inet BIFs.
pub fn register_inet_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for (name, arity, function) in [
        ("inet_setopts", 2, inet_setopts as crate::native::NativeFn),
        ("inet_getopts", 2, inet_getopts as crate::native::NativeFn),
        ("inet_peername", 1, inet_peername as crate::native::NativeFn),
        ("inet_sockname", 1, inet_sockname as crate::native::NativeFn),
        ("inet_port", 1, inet_port as crate::native::NativeFn),
        ("inet_close", 1, inet_close as crate::native::NativeFn),
    ] {
        registry.register(
            erlang,
            atom_table.intern(name),
            arity,
            function,
            Capability::ExternalIo,
        )?;
    }

    Ok(())
}

/// erlang:inet_setopts/2.
pub fn inet_setopts(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [socket, options] = args else {
        return Err(badarg());
    };
    let Some(resource) = open_resource(*socket)? else {
        return error_tuple(context, Atom::CLOSED);
    };
    let socket_atoms = SocketAtoms::from_context(context)?;

    let mut tail = *options;
    while tail != Term::NIL {
        let cons = Cons::new(tail).ok_or_else(badarg)?;
        let option = SetOption::parse(cons.head(), &socket_atoms)?;
        if let Some(reason) = apply_set_option(resource.fd(), option, &socket_atoms) {
            return error_tuple(context, reason);
        }
        tail = cons.tail();
    }

    Ok(Term::atom(Atom::OK))
}

/// erlang:inet_getopts/2.
pub fn inet_getopts(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [socket, options] = args else {
        return Err(badarg());
    };
    let Some(resource) = open_resource(*socket)? else {
        return error_tuple(context, Atom::CLOSED);
    };
    let socket_atoms = SocketAtoms::from_context(context)?;

    let mut values = Vec::new();
    let mut tail = *options;
    while tail != Term::NIL {
        let cons = Cons::new(tail).ok_or_else(badarg)?;
        let option = GetOption::parse(cons.head(), &socket_atoms)?;
        let value = match read_get_option(resource.fd(), option, &socket_atoms, context)? {
            Ok(value) => value,
            Err(reason) => return error_tuple(context, reason),
        };
        values.push(value);
        tail = cons.tail();
    }

    let list = context.alloc_list(&values)?;
    ok_tuple(context, list)
}

/// erlang:inet_peername/1.
pub fn inet_peername(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [socket] = args else {
        return Err(badarg());
    };
    let Some(resource) = open_resource(*socket)? else {
        return error_tuple(context, Atom::CLOSED);
    };
    match socket_peername(resource.fd()) {
        Ok(addr) => ok_socket_addr_tuple(context, addr),
        Err(reason) => error_tuple(context, reason),
    }
}

/// erlang:inet_sockname/1.
pub fn inet_sockname(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [socket] = args else {
        return Err(badarg());
    };
    let Some(resource) = open_resource(*socket)? else {
        return error_tuple(context, Atom::CLOSED);
    };
    match socket_sockname(resource.fd()) {
        Ok(addr) => ok_socket_addr_tuple(context, addr),
        Err(reason) => error_tuple(context, reason),
    }
}

/// erlang:inet_port/1.
pub fn inet_port(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [socket] = args else {
        return Err(badarg());
    };
    let Some(resource) = open_resource(*socket)? else {
        return error_tuple(context, Atom::CLOSED);
    };
    match socket_sockname(resource.fd()) {
        Ok(addr) => {
            let port = small_int(i64::from(addr.port()))?;
            ok_tuple(context, port)
        }
        Err(reason) => error_tuple(context, reason),
    }
}

/// erlang:inet_close/1.
pub fn inet_close(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_close(completion, context);
    }

    let [socket] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*socket).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    active_modes()
        .lock()
        .map_err(|_| badarg())?
        .remove(&resource.fd());
    let inner = resource.inner();
    let ring = context.file_completion_ring().ok_or_else(badarg)?;
    let Some(op_id) = inner.explicit_close_with_op_id(ring) else {
        return error_tuple(context, Atom::CLOSED);
    };
    context.track_submitted_file_io(op_id, FileIoContinuation::Close { fd: inner })?;
    if let Some(completion) = context.take_file_io_completion() {
        return finish_close(completion, context);
    }
    context.request_await_suspend(None);
    Ok(Term::atom(Atom::OK))
}

#[derive(Copy, Clone)]
struct SocketAtoms {
    nodelay: Atom,
    keepalive: Atom,
    reuseaddr: Atom,
    sndbuf: Atom,
    recbuf: Atom,
    active: Atom,
    once: Atom,
}

impl SocketAtoms {
    fn from_context(context: &mut ProcessContext) -> Result<Self, Term> {
        let atom_table = context.atom_table().ok_or_else(badarg)?;
        Ok(Self {
            nodelay: atom_table.intern("nodelay"),
            keepalive: atom_table.intern("keepalive"),
            reuseaddr: atom_table.intern("reuseaddr"),
            sndbuf: atom_table.intern("sndbuf"),
            recbuf: atom_table.intern("recbuf"),
            active: atom_table.intern("active"),
            once: atom_table.intern("once"),
        })
    }
}

#[derive(Copy, Clone)]
enum SetOption {
    NoDelay(bool),
    KeepAlive(bool),
    ReuseAddr(bool),
    SndBuf(usize),
    RecBuf(usize),
    Active(ActiveMode),
}

impl SetOption {
    fn parse(term: Term, atoms: &SocketAtoms) -> Result<Self, Term> {
        let tuple = Tuple::new(term).ok_or_else(badarg)?;
        if tuple.arity() != 2 {
            return Err(badarg());
        }
        let key = tuple.get(0).ok_or_else(badarg)?;
        let value = tuple.get(1).ok_or_else(badarg)?;
        if key == Term::atom(atoms.nodelay) {
            Ok(Self::NoDelay(bool_value(value)?))
        } else if key == Term::atom(atoms.keepalive) {
            Ok(Self::KeepAlive(bool_value(value)?))
        } else if key == Term::atom(atoms.reuseaddr) {
            Ok(Self::ReuseAddr(bool_value(value)?))
        } else if key == Term::atom(atoms.sndbuf) {
            Ok(Self::SndBuf(buffer_size(value)?))
        } else if key == Term::atom(atoms.recbuf) {
            Ok(Self::RecBuf(buffer_size(value)?))
        } else if key == Term::atom(atoms.active) {
            Ok(Self::Active(ActiveMode::parse(value, atoms)?))
        } else {
            Err(badarg())
        }
    }
}

#[derive(Copy, Clone)]
enum GetOption {
    NoDelay,
    KeepAlive,
    ReuseAddr,
    SndBuf,
    RecBuf,
    Active,
}

impl GetOption {
    fn parse(term: Term, atoms: &SocketAtoms) -> Result<Self, Term> {
        if term == Term::atom(atoms.nodelay) {
            Ok(Self::NoDelay)
        } else if term == Term::atom(atoms.keepalive) {
            Ok(Self::KeepAlive)
        } else if term == Term::atom(atoms.reuseaddr) {
            Ok(Self::ReuseAddr)
        } else if term == Term::atom(atoms.sndbuf) {
            Ok(Self::SndBuf)
        } else if term == Term::atom(atoms.recbuf) {
            Ok(Self::RecBuf)
        } else if term == Term::atom(atoms.active) {
            Ok(Self::Active)
        } else {
            Err(badarg())
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ActiveMode {
    Passive,
    Active,
    Once,
}

impl ActiveMode {
    fn parse(term: Term, atoms: &SocketAtoms) -> Result<Self, Term> {
        if term == Term::atom(Atom::FALSE) {
            Ok(Self::Passive)
        } else if term == Term::atom(Atom::TRUE) {
            Ok(Self::Active)
        } else if term == Term::atom(atoms.once) {
            Ok(Self::Once)
        } else {
            Err(badarg())
        }
    }

    fn term(self, atoms: &SocketAtoms) -> Term {
        match self {
            Self::Passive => Term::atom(Atom::FALSE),
            Self::Active => Term::atom(Atom::TRUE),
            Self::Once => Term::atom(atoms.once),
        }
    }
}

fn apply_set_option(fd: RawFd, option: SetOption, atoms: &SocketAtoms) -> Option<Atom> {
    let socket = socket_handle(fd);
    match option {
        SetOption::NoDelay(value) => sockopt::set_tcp_nodelay(socket, value)
            .err()
            .map(rustix_error_reason),
        SetOption::KeepAlive(value) => sockopt::set_socket_keepalive(socket, value)
            .err()
            .map(rustix_error_reason),
        SetOption::ReuseAddr(value) => sockopt::set_socket_reuseaddr(socket, value)
            .err()
            .map(rustix_error_reason),
        SetOption::SndBuf(value) => sockopt::set_socket_send_buffer_size(socket, value)
            .err()
            .map(rustix_error_reason),
        SetOption::RecBuf(value) => sockopt::set_socket_recv_buffer_size(socket, value)
            .err()
            .map(rustix_error_reason),
        SetOption::Active(mode) => set_active_mode(fd, mode, atoms),
    }
}

fn read_get_option(
    fd: RawFd,
    option: GetOption,
    atoms: &SocketAtoms,
    context: &mut ProcessContext,
) -> Result<Result<Term, Atom>, Term> {
    let socket = socket_handle(fd);
    match option {
        GetOption::NoDelay => option_tuple_result(
            context,
            atoms.nodelay,
            sockopt::tcp_nodelay(socket)
                .map(bool_term)
                .map_err(rustix_error_reason),
        ),
        GetOption::KeepAlive => option_tuple_result(
            context,
            atoms.keepalive,
            sockopt::socket_keepalive(socket)
                .map(bool_term)
                .map_err(rustix_error_reason),
        ),
        GetOption::ReuseAddr => option_tuple_result(
            context,
            atoms.reuseaddr,
            sockopt::socket_reuseaddr(socket)
                .map(bool_term)
                .map_err(rustix_error_reason),
        ),
        GetOption::SndBuf => option_tuple_result(
            context,
            atoms.sndbuf,
            sockopt::socket_send_buffer_size(socket)
                .map_err(rustix_error_reason)
                .and_then(usize_term),
        ),
        GetOption::RecBuf => option_tuple_result(
            context,
            atoms.recbuf,
            sockopt::socket_recv_buffer_size(socket)
                .map_err(rustix_error_reason)
                .and_then(usize_term),
        ),
        GetOption::Active => option_tuple_result(
            context,
            atoms.active,
            Ok(active_mode(fd, atoms).term(atoms)),
        ),
    }
}

fn socket_handle(fd: RawFd) -> rustix::fd::BorrowedFd<'static> {
    // SAFETY: callers validate that `fd` comes from an open `FdResource` before
    // invoking synchronous socket operations, and the borrowed handle is used
    // only for the duration of the syscall wrapper rather than stored.
    unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) }
}

fn set_active_mode(fd: i32, mode: ActiveMode, _atoms: &SocketAtoms) -> Option<Atom> {
    let Ok(mut modes) = active_modes().lock() else {
        return Some(Atom::UNKNOWN_ERROR);
    };
    if mode == ActiveMode::Passive {
        modes.remove(&fd);
    } else {
        modes.insert(fd, mode);
    }
    None
}

fn active_mode(fd: i32, _atoms: &SocketAtoms) -> ActiveMode {
    let Ok(modes) = active_modes().lock() else {
        return ActiveMode::Passive;
    };
    modes.get(&fd).copied().unwrap_or(ActiveMode::Passive)
}

fn active_modes() -> &'static Mutex<HashMap<i32, ActiveMode>> {
    static ACTIVE_MODES: OnceLock<Mutex<HashMap<i32, ActiveMode>>> = OnceLock::new();
    ACTIVE_MODES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn option_tuple_result(
    context: &mut ProcessContext,
    option: Atom,
    value: Result<Term, Atom>,
) -> Result<Result<Term, Atom>, Term> {
    match value {
        Ok(term) => Ok(Ok(context.alloc_tuple(&[Term::atom(option), term])?)),
        Err(reason) => Ok(Err(reason)),
    }
}

fn socket_peername(fd: RawFd) -> Result<SocketAddrV4, Atom> {
    let socket = socket_handle(fd);
    let Some(addr) = rustix::net::getpeername(socket).map_err(rustix_error_reason)? else {
        return Err(Atom::ENOTCONN);
    };
    socket_addr_v4(addr)
}

fn socket_sockname(fd: RawFd) -> Result<SocketAddrV4, Atom> {
    let socket = socket_handle(fd);
    let addr = rustix::net::getsockname(socket).map_err(rustix_error_reason)?;
    socket_addr_v4(addr)
}

fn socket_addr_v4(addr: rustix::net::SocketAddrAny) -> Result<SocketAddrV4, Atom> {
    SocketAddrV4::try_from(addr).map_err(rustix_error_reason)
}

fn ok_socket_addr_tuple(context: &mut ProcessContext, addr: SocketAddrV4) -> Result<Term, Term> {
    let ip = addr.ip().octets();
    let address = context.alloc_tuple(&[
        small_int(i64::from(ip[0]))?,
        small_int(i64::from(ip[1]))?,
        small_int(i64::from(ip[2]))?,
        small_int(i64::from(ip[3]))?,
    ])?;
    let port = small_int(i64::from(addr.port()))?;
    let pair = context.alloc_tuple(&[address, port])?;
    ok_tuple(context, pair)
}

fn open_resource(socket: Term) -> Result<Option<FdResource>, Term> {
    let resource = FdResource::new(socket).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return Ok(None);
    }
    Ok(Some(resource))
}

fn finish_close(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    let fd = match completion.continuation {
        FileIoContinuation::Close { fd } => fd,
        _ => return error_tuple(context, Atom::UNKNOWN_ERROR),
    };
    match completion.completion.result {
        Ok(IoResult::Closed) => {
            fd.mark_closed();
            Ok(Term::atom(Atom::OK))
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn bool_value(term: Term) -> Result<bool, Term> {
    if term == Term::atom(Atom::TRUE) {
        Ok(true)
    } else if term == Term::atom(Atom::FALSE) {
        Ok(false)
    } else {
        Err(badarg())
    }
}

fn buffer_size(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(badarg)
}

fn bool_term(value: bool) -> Term {
    Term::atom(if value { Atom::TRUE } else { Atom::FALSE })
}

fn usize_term(value: usize) -> Result<Term, Atom> {
    i64::try_from(value)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or(Atom::UNKNOWN_ERROR)
}

fn small_int(value: i64) -> Result<Term, Term> {
    Term::try_small_int(value).ok_or_else(badarg)
}

fn ok_tuple(context: &mut ProcessContext, value: Term) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::OK), value])
}

fn error_tuple(context: &mut ProcessContext, reason: Atom) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(reason)])
}

fn rustix_error_reason(error: rustix::io::Errno) -> Atom {
    errno_to_atom(error.raw_os_error())
}

fn error_reason(error: io::Error) -> Atom {
    error
        .raw_os_error()
        .map(errno_to_atom)
        .unwrap_or(Atom::UNKNOWN_ERROR)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::net::{TcpListener, TcpStream, UdpSocket};
    use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
    use std::sync::{Arc, Mutex};

    use crate::atom::AtomTable;
    use crate::io::resource::FdInner;
    use crate::io::{CompletionRing, IoCompletion, IoOp};
    use crate::native::{FileIoCompletion, FileIoContinuation, FileIoFacility, ProcessContext};
    use crate::process::Process;
    use crate::term::boxed::{Cons, Tuple};

    use super::*;

    const PID: u64 = 42;

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

        fn cancel_pending_file_io_for_pid(&self, _pid: u64) {}

        fn ring(&self) -> &dyn CompletionRing {
            &self.ring
        }
    }

    fn context<'a>(
        process: &'a mut Process,
        facility: Option<Arc<MockFileIoFacility>>,
    ) -> ProcessContext<'a> {
        let mut context = ProcessContext::new();
        context.attach_process(process, 0);
        context.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        if let Some(facility) = facility {
            context.set_file_io_facility(Some(facility));
        }
        context
    }

    fn list(context: &mut ProcessContext<'_>, values: &[Term]) -> Term {
        context.alloc_list(values).expect("list allocation")
    }

    fn tuple2(context: &mut ProcessContext<'_>, left: Term, right: Term) -> Term {
        context
            .alloc_tuple(&[left, right])
            .expect("tuple allocation")
    }

    fn fd_resource(context: &mut ProcessContext<'_>, fd: RawFd) -> Term {
        context
            .alloc_fd_resource(Arc::new(FdInner::new(fd, PID)))
            .expect("fd resource allocation")
    }

    fn result_tuple(term: Term) -> (Term, Term) {
        let tuple = Tuple::new(term).expect("tuple result");
        (tuple.get(0).expect("tag"), tuple.get(1).expect("value"))
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let client = TcpStream::connect(addr).expect("connect client");
        let (server, _) = listener.accept().expect("accept client");
        (client, server)
    }

    #[test]
    fn setopts_and_getopts_round_trip_nodelay() {
        let (client, _server) = tcp_pair();
        let fd = client.into_raw_fd();
        let mut process = Process::new(PID, 256);
        let mut context = context(&mut process, None);
        let nodelay = context.atom_table().expect("atom table").intern("nodelay");
        let resource = fd_resource(&mut context, fd);
        let option = tuple2(&mut context, Term::atom(nodelay), Term::atom(Atom::TRUE));
        let options = list(&mut context, &[option]);

        let result = inet_setopts(&[resource, options], &mut context).expect("setopts result");
        assert_eq!(result, Term::atom(Atom::OK));

        let query = list(&mut context, &[Term::atom(nodelay)]);
        let result = inet_getopts(&[resource, query], &mut context).expect("getopts result");
        let (tag, values) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::OK));
        let cons = Cons::new(values).expect("one option");
        let (option_name, option_value) = result_tuple(cons.head());
        assert_eq!(option_name, Term::atom(nodelay));
        assert_eq!(option_value, Term::atom(Atom::TRUE));
        assert_eq!(cons.tail(), Term::NIL);
    }

    #[test]
    fn active_mode_round_trip_does_not_disturb_socket_options() {
        let (client, _server) = tcp_pair();
        let fd = client.into_raw_fd();
        let mut process = Process::new(PID, 256);
        let mut context = context(&mut process, None);
        let atom_table = context.atom_table().expect("atom table");
        let nodelay = atom_table.intern("nodelay");
        let active = atom_table.intern("active");
        let once = atom_table.intern("once");
        let resource = fd_resource(&mut context, fd);
        let nodelay_option = tuple2(&mut context, Term::atom(nodelay), Term::atom(Atom::TRUE));
        let active_option = tuple2(&mut context, Term::atom(active), Term::atom(once));
        let set = list(&mut context, &[nodelay_option, active_option]);

        assert_eq!(
            inet_setopts(&[resource, set], &mut context).expect("setopts result"),
            Term::atom(Atom::OK)
        );

        let query = list(&mut context, &[Term::atom(active), Term::atom(nodelay)]);
        let result = inet_getopts(&[resource, query], &mut context).expect("getopts result");
        let (tag, values) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::OK));
        let active_cons = Cons::new(values).expect("active option");
        let (active_name, active_value) = result_tuple(active_cons.head());
        assert_eq!(active_name, Term::atom(active));
        assert_eq!(active_value, Term::atom(once));
        let nodelay_cons = Cons::new(active_cons.tail()).expect("nodelay option");
        let (nodelay_name, nodelay_value) = result_tuple(nodelay_cons.head());
        assert_eq!(nodelay_name, Term::atom(nodelay));
        assert_eq!(nodelay_value, Term::atom(Atom::TRUE));
        assert_eq!(nodelay_cons.tail(), Term::NIL);
    }

    #[test]
    fn peername_and_sockname_return_ipv4_address_port_tuples() {
        let (client, server) = tcp_pair();
        let expected_peer = server.local_addr().expect("server local addr");
        let expected_local = client.local_addr().expect("client local addr");
        let fd = client.into_raw_fd();
        let mut process = Process::new(PID, 256);
        let mut context = context(&mut process, None);
        let resource = fd_resource(&mut context, fd);

        let peer = inet_peername(&[resource], &mut context).expect("peername result");
        assert_socket_addr_tuple(peer, expected_peer.port());
        let sock = inet_sockname(&[resource], &mut context).expect("sockname result");
        assert_socket_addr_tuple(sock, expected_local.port());
    }

    #[test]
    fn peername_on_unconnected_udp_returns_enotconn() {
        let udp = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
        let fd = udp.into_raw_fd();
        let mut process = Process::new(PID, 128);
        let mut context = context(&mut process, None);
        let resource = fd_resource(&mut context, fd);

        let result = inet_peername(&[resource], &mut context).expect("peername result");
        let (tag, reason) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::ERROR));
        assert_eq!(reason, Term::atom(Atom::ENOTCONN));
    }

    #[test]
    fn inet_port_returns_bound_port_from_tcp_and_udp_sockname() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let expected_tcp_port = listener.local_addr().expect("listener addr").port();
        let tcp_fd = listener.into_raw_fd();
        let udp = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
        let expected_udp_port = udp.local_addr().expect("udp addr").port();
        let udp_fd = udp.into_raw_fd();
        let mut process = Process::new(PID, 128);
        let mut context = context(&mut process, None);
        let tcp_resource = fd_resource(&mut context, tcp_fd);
        let udp_resource = fd_resource(&mut context, udp_fd);

        let result = inet_port(&[tcp_resource], &mut context).expect("tcp port result");
        let (tag, port) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::OK));
        assert_eq!(port.as_small_int(), Some(i64::from(expected_tcp_port)));

        let result = inet_port(&[udp_resource], &mut context).expect("udp port result");
        let (tag, port) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::OK));
        assert_eq!(port.as_small_int(), Some(i64::from(expected_udp_port)));
    }

    #[test]
    fn closed_resource_returns_error_tuple_for_inet_queries() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mut process = Process::new(PID, 128);
        let mut context = context(&mut process, None);
        let resource = fd_resource(&mut context, listener.into_raw_fd());
        FdResource::new(resource)
            .expect("fd resource")
            .inner()
            .mark_closed();
        let nodelay = context.atom_table().expect("atom table").intern("nodelay");
        let query = list(&mut context, &[Term::atom(nodelay)]);

        let result = inet_getopts(&[resource, query], &mut context).expect("closed getopts");
        let (tag, reason) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::ERROR));
        assert_eq!(reason, Term::atom(Atom::CLOSED));
    }

    #[test]
    fn inet_close_submits_close_and_marks_closed_on_completion() {
        let facility = Arc::new(MockFileIoFacility::default());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let fd = listener.as_raw_fd();
        let mut process = Process::new(PID, 128);
        let mut context = context(&mut process, Some(Arc::clone(&facility)));
        let resource = fd_resource(&mut context, listener.into_raw_fd());

        let result = inet_close(&[resource], &mut context).expect("close placeholder");
        assert_eq!(result, Term::atom(Atom::OK));
        assert!(context.take_suspend().is_some());
        assert!(
            matches!(facility.submitted().as_slice(), [IoOp::Close { fd: submitted_fd }] if *submitted_fd == fd)
        );

        let inner = match &facility.tracked()[0].2 {
            FileIoContinuation::Close { fd } => Arc::clone(fd),
            other => panic!("expected close continuation, got {other:?}"),
        };
        facility.push_completion(
            FileIoContinuation::Close { fd: inner },
            Ok(IoResult::Closed),
        );
        let result = inet_close(&[resource], &mut context).expect("close completion");
        assert_eq!(result, Term::atom(Atom::OK));
        assert_eq!(
            FdResource::new(resource).expect("fd resource").state(),
            FdState::Closed
        );
    }

    fn assert_socket_addr_tuple(result: Term, expected_port: u16) {
        let (tag, pair) = result_tuple(result);
        assert_eq!(tag, Term::atom(Atom::OK));
        let (address, port) = result_tuple(pair);
        assert_eq!(port.as_small_int(), Some(i64::from(expected_port)));
        let tuple = Tuple::new(address).expect("address tuple");
        assert_eq!(tuple.arity(), 4);
        for index in 0..4 {
            let octet = tuple.get(index).expect("octet");
            assert!(matches!(octet.as_small_int(), Some(0..=255)));
        }
    }
}
