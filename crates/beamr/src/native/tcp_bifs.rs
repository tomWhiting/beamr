//! Completion-ring backed TCP listener BIFs.

use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::io::resource::{FdInner, FdResource, FdState};
use crate::io::{IoOp, IoResult, errno_to_atom};
use crate::native::{
    BifRegistryImpl, Capability, FileIoCompletion, FileIoContinuation, NativeRegistrationError,
    ProcessContext,
};
use crate::term::Term;
use crate::term::boxed::{Cons, Tuple};

const DEFAULT_BACKLOG: i32 = 128;

/// Registers Erlang TCP listener BIFs.
pub fn register_tcp_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for (name, arity, function) in [
        ("tcp_listen", 2, tcp_listen as crate::native::NativeFn),
        ("tcp_accept", 1, tcp_accept as crate::native::NativeFn),
        ("tcp_accept", 2, tcp_accept as crate::native::NativeFn),
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

/// erlang:tcp_listen/2.
pub fn tcp_listen(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [port_term, options_term] = args else {
        return Err(badarg());
    };

    let port = parse_port(*port_term)?;
    let options = ListenOptions::parse(*options_term, context.atom_table().ok_or_else(badarg)?)?;

    let fd = match create_listener_socket(port, options) {
        Ok(fd) => fd,
        Err(reason) => return error_tuple(context, reason),
    };
    let owner_pid = context.pid().ok_or_else(badarg)?;
    alloc_fd_resource_or_close(context, fd, owner_pid)
}

/// erlang:tcp_accept/1 and erlang:tcp_accept/2.
pub fn tcp_accept(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_accept(completion, context);
    }
    if context.receive_timeout_expired() {
        context.cancel_pending_file_io_for_current_process();
        context.clear_receive_timeout();
        return error_tuple(context, Atom::TIMEOUT);
    }

    let (fd_term, timeout_ms) = match args {
        [fd_term] => (*fd_term, None),
        [fd_term, timeout_term] => (*fd_term, Some(parse_timeout(*timeout_term)?)),
        _ => return Err(badarg()),
    };

    let resource = FdResource::new(fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    if let Some(0) = timeout_ms {
        return error_tuple(context, Atom::TIMEOUT);
    }

    context.submit_file_io_with_timeout(
        IoOp::Accept {
            listener_fd: resource.fd(),
        },
        FileIoContinuation::Accept,
        timeout_ms,
    )?;
    Ok(Term::atom(Atom::OK))
}

#[derive(Copy, Clone, Debug)]
struct ListenOptions {
    ip: Ipv4Addr,
    backlog: i32,
    reuseaddr: bool,
}

impl ListenOptions {
    fn parse(term: Term, atom_table: &AtomTable) -> Result<Self, Term> {
        let mut options = Self {
            ip: Ipv4Addr::UNSPECIFIED,
            backlog: DEFAULT_BACKLOG,
            reuseaddr: true,
        };
        let mut tail = term;
        while tail != Term::NIL {
            let cons = Cons::new(tail).ok_or_else(badarg)?;
            options.apply_option(cons.head(), atom_table)?;
            tail = cons.tail();
        }
        Ok(options)
    }

    fn apply_option(&mut self, term: Term, atom_table: &AtomTable) -> Result<(), Term> {
        let tuple = Tuple::new(term).ok_or_else(badarg)?;
        if tuple.arity() != 2 {
            return Err(badarg());
        }
        let key = tuple.get(0).ok_or_else(badarg)?;
        let value = tuple.get(1).ok_or_else(badarg)?;
        if key == dynamic_atom(atom_table, "ip") {
            self.ip = parse_ipv4(value)?;
        } else if key == dynamic_atom(atom_table, "backlog") {
            self.backlog = parse_backlog(value)?;
        } else if key == dynamic_atom(atom_table, "reuseaddr") {
            self.reuseaddr = parse_bool(value)?;
        } else {
            return Err(badarg());
        }
        Ok(())
    }
}

fn create_listener_socket(port: u16, options: ListenOptions) -> Result<RawFd, Atom> {
    // SAFETY: `socket` is called with a valid address family, socket type, and
    // protocol. The returned fd is checked for failure before use.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(error_reason(io::Error::last_os_error()));
    }

    if let Err(reason) = configure_and_listen(fd, port, options) {
        close_raw_fd(fd);
        return Err(reason);
    }
    Ok(fd)
}

fn configure_and_listen(fd: RawFd, port: u16, options: ListenOptions) -> Result<(), Atom> {
    if options.reuseaddr {
        let value: libc::c_int = 1;
        let optlen = mem::size_of_val(&value) as libc::socklen_t;
        // SAFETY: `fd` is an open socket; `value` points to a valid c_int for the
        // duration of the call and `optlen` matches that value's size.
        let result = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                (&value as *const libc::c_int).cast(),
                optlen,
            )
        };
        if result < 0 {
            return Err(error_reason(io::Error::last_os_error()));
        }
    }

    let mut addr = sockaddr_in(options.ip, port);
    let addr_len = mem::size_of_val(&addr) as libc::socklen_t;
    // SAFETY: `addr` is a fully initialized IPv4 sockaddr for `fd`'s address
    // family and `addr_len` matches its size.
    let bind_result = unsafe {
        libc::bind(
            fd,
            (&mut addr as *mut libc::sockaddr_in).cast::<libc::sockaddr>(),
            addr_len,
        )
    };
    if bind_result < 0 {
        return Err(error_reason(io::Error::last_os_error()));
    }

    // SAFETY: `fd` is a bound stream socket and `options.backlog` is a validated
    // non-negative backlog accepted by the OS listen call.
    let listen_result = unsafe { libc::listen(fd, options.backlog) };
    if listen_result < 0 {
        return Err(error_reason(io::Error::last_os_error()));
    }
    Ok(())
}

fn finish_accept(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    context.clear_receive_timeout();
    if !matches!(completion.continuation, FileIoContinuation::Accept) {
        return error_tuple(context, Atom::UNKNOWN_ERROR);
    }
    match completion.completion.result {
        Ok(IoResult::Accepted(fd, _peer)) => {
            let owner_pid = match context.pid() {
                Some(pid) => pid,
                None => {
                    close_raw_fd(fd);
                    return Err(badarg());
                }
            };
            let fd_term = alloc_fd_resource_or_close(context, fd, owner_pid)?;
            ok_tuple(context, fd_term)
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn sockaddr_in(ip: Ipv4Addr, port: u16) -> libc::sockaddr_in {
    libc::sockaddr_in {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
        sin_len: mem::size_of::<libc::sockaddr_in>() as u8,
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from(ip).to_be(),
        },
        sin_zero: [0; 8],
    }
}

fn parse_port(term: Term) -> Result<u16, Term> {
    term.as_small_int()
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(badarg)
}

fn parse_timeout(term: Term) -> Result<u64, Term> {
    term.as_small_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(badarg)
}

fn parse_backlog(term: Term) -> Result<i32, Term> {
    term.as_small_int()
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value >= 0)
        .ok_or_else(badarg)
}

fn parse_ipv4(term: Term) -> Result<Ipv4Addr, Term> {
    let tuple = Tuple::new(term).ok_or_else(badarg)?;
    if tuple.arity() != 4 {
        return Err(badarg());
    }
    Ok(Ipv4Addr::new(
        parse_octet(tuple.get(0).ok_or_else(badarg)?)?,
        parse_octet(tuple.get(1).ok_or_else(badarg)?)?,
        parse_octet(tuple.get(2).ok_or_else(badarg)?)?,
        parse_octet(tuple.get(3).ok_or_else(badarg)?)?,
    ))
}

fn parse_octet(term: Term) -> Result<u8, Term> {
    term.as_small_int()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(badarg)
}

fn parse_bool(term: Term) -> Result<bool, Term> {
    match term {
        value if value == Term::atom(Atom::TRUE) => Ok(true),
        value if value == Term::atom(Atom::FALSE) => Ok(false),
        _ => Err(badarg()),
    }
}

fn dynamic_atom(atom_table: &AtomTable, name: &str) -> Term {
    Term::atom(atom_table.intern(name))
}

fn ok_tuple(context: &mut ProcessContext, value: Term) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::OK), value])
}

fn error_tuple(context: &mut ProcessContext, reason: Atom) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(reason)])
}

fn alloc_fd_resource_or_close(
    context: &mut ProcessContext,
    fd: RawFd,
    owner_pid: u64,
) -> Result<Term, Term> {
    match context.alloc_fd_resource(Arc::new(FdInner::new(fd, owner_pid))) {
        Ok(resource) => Ok(resource),
        Err(reason) => {
            close_raw_fd(fd);
            Err(reason)
        }
    }
}

fn error_reason(error: io::Error) -> Atom {
    error
        .raw_os_error()
        .map(errno_to_atom)
        .unwrap_or(Atom::UNKNOWN_ERROR)
}

fn close_raw_fd(fd: RawFd) {
    // SAFETY: this function is only called on setup-failure paths before the fd
    // is wrapped in FdResource ownership, so no other owner will close it.
    let _result = unsafe { libc::close(fd) };
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "tcp_bifs_tests.rs"]
mod tests;
