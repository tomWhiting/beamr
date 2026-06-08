//! Completion-ring backed file BIFs.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::io::resource::{FdInner, FdResource, FdState};
use crate::io::{IoOp, IoResult, errno_to_atom};
use crate::native::{
    BifRegistryImpl, Capability, FileIoCompletion, FileIoContinuation, NativeRegistrationError,
    ProcessContext,
};
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Tuple};

const CURRENT_POSITION: u64 = u64::MAX;
const DEFAULT_FILE_PERMISSIONS: u32 = 0o644;

/// Registers Erlang file BIFs.
pub fn register_file_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for (name, arity, function) in [
        ("open_file", 2, file_open as crate::native::NativeFn),
        ("close_file", 1, file_close as crate::native::NativeFn),
        ("read_file", 2, file_read as crate::native::NativeFn),
        ("write_file", 2, file_write as crate::native::NativeFn),
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

/// erlang:open_file/2.
pub fn file_open(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_open(completion, context);
    }

    let [filename, modes] = args else {
        return Err(badarg());
    };
    let path = filename_path(*filename)?;
    let flags = parse_modes(*modes)?;
    let op = IoOp::Openat {
        dir_fd: libc::AT_FDCWD,
        path,
        flags,
        mode: DEFAULT_FILE_PERMISSIONS,
    };
    context.submit_file_io(op, FileIoContinuation::Open)?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:close_file/1.
pub fn file_close(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_close(completion, context);
    }

    let [fd_term] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    let inner = resource.inner();
    let ring = context.file_completion_ring().ok_or_else(badarg)?;
    let Some(op_id) = inner.explicit_close_with_op_id(ring) else {
        return error_tuple(context, Atom::CLOSED);
    };
    context.track_submitted_file_io(op_id, FileIoContinuation::Close { fd: inner })?;
    if let Some(completion) = context.take_file_io_completion() {
        return finish_close(completion, context);
    }
    context.request_suspend(None);
    Ok(Term::atom(Atom::OK))
}

/// erlang:read_file/2.
pub fn file_read(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_read(completion, context);
    }

    let [fd_term, count_term] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    let count = count_term
        .as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)?;
    context.submit_file_io(
        IoOp::Read {
            fd: resource.fd(),
            buf_len: count,
            offset: CURRENT_POSITION,
        },
        FileIoContinuation::Read,
    )?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:write_file/2.
pub fn file_write(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_write(completion, context);
    }

    let [fd_term, data_term] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    let data = Binary::new(*data_term)
        .ok_or_else(badarg)?
        .as_bytes()
        .to_vec();
    let expected_len = data.len();
    context.submit_file_io(
        IoOp::Write {
            fd: resource.fd(),
            data,
            offset: CURRENT_POSITION,
        },
        FileIoContinuation::Write { expected_len },
    )?;
    Ok(Term::atom(Atom::OK))
}

fn finish_open(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    match completion.completion.result {
        Ok(IoResult::Opened(fd)) => {
            let owner_pid = context.pid().ok_or_else(badarg)?;
            context.alloc_fd_resource(Arc::new(FdInner::new(fd, owner_pid)))
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
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

fn finish_read(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    match completion.completion.result {
        Ok(IoResult::BytesRead(bytes_read, bytes)) => {
            let read_bytes = bytes.get(..bytes_read).ok_or_else(badarg)?;
            let binary = context.alloc_binary(read_bytes)?;
            ok_tuple(context, binary)
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn finish_write(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    let expected_len = match completion.continuation {
        FileIoContinuation::Write { expected_len } => expected_len,
        _ => return error_tuple(context, Atom::UNKNOWN_ERROR),
    };
    match completion.completion.result {
        Ok(IoResult::BytesWritten(bytes_written)) if bytes_written == expected_len => {
            Ok(Term::atom(Atom::OK))
        }
        Ok(IoResult::BytesWritten(bytes_written)) => incomplete_tuple(context, bytes_written),
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn filename_path(term: Term) -> Result<PathBuf, Term> {
    let bytes = Binary::new(term).ok_or_else(badarg)?.as_bytes();
    let filename = std::str::from_utf8(bytes).map_err(|_| badarg())?;
    Ok(PathBuf::from(filename))
}

fn parse_modes(modes: Term) -> Result<i32, Term> {
    let mut read = false;
    let mut write = false;
    let mut create = false;
    let mut truncate = false;
    let mut append = false;
    let mut tail = modes;

    while tail != Term::NIL {
        let cons = Cons::new(tail).ok_or_else(badarg)?;
        match mode_item(cons.head())? {
            ModeItem::Read => read = true,
            ModeItem::Write => write = true,
            ModeItem::Append => {
                write = true;
                append = true;
            }
            ModeItem::Create(true) => create = true,
            ModeItem::Create(false) => {}
            ModeItem::Truncate(true) => {
                write = true;
                truncate = true;
            }
            ModeItem::Truncate(false) => {}
        }
        tail = cons.tail();
    }

    let mut flags = match (read, write) {
        (true, true) => libc::O_RDWR,
        (false, true) => libc::O_WRONLY,
        _ => libc::O_RDONLY,
    };
    if create {
        flags |= libc::O_CREAT;
    }
    if truncate {
        flags |= libc::O_TRUNC;
    }
    if append {
        flags |= libc::O_APPEND;
    }
    Ok(flags)
}

enum ModeItem {
    Read,
    Write,
    Append,
    Create(bool),
    Truncate(bool),
}

fn mode_item(term: Term) -> Result<ModeItem, Term> {
    match term {
        value if value == Term::atom(Atom::READ) => Ok(ModeItem::Read),
        value if value == Term::atom(Atom::WRITE) => Ok(ModeItem::Write),
        value if value == Term::atom(Atom::APPEND) => Ok(ModeItem::Append),
        _ => mode_tuple(term),
    }
}

fn mode_tuple(term: Term) -> Result<ModeItem, Term> {
    let tuple = Tuple::new(term).ok_or_else(badarg)?;
    let key = tuple.get(0).ok_or_else(badarg)?;
    let value = tuple.get(1).ok_or_else(badarg)?;
    if tuple.arity() != 2 {
        return Err(badarg());
    }
    let bool_value = match value {
        atom if atom == Term::atom(Atom::TRUE) => true,
        atom if atom == Term::atom(Atom::FALSE) => false,
        _ => return Err(badarg()),
    };
    if key == Term::atom(Atom::CREATE) {
        Ok(ModeItem::Create(bool_value))
    } else if key == Term::atom(Atom::TRUNCATE) {
        Ok(ModeItem::Truncate(bool_value))
    } else {
        Err(badarg())
    }
}

fn ok_tuple(context: &mut ProcessContext, value: Term) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::OK), value])
}

fn error_tuple(context: &mut ProcessContext, reason: Atom) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(reason)])
}

fn incomplete_tuple(context: &mut ProcessContext, bytes_written: usize) -> Result<Term, Term> {
    let count = i64::try_from(bytes_written)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)?;
    let reason = context.alloc_tuple(&[Term::atom(Atom::INCOMPLETE), count])?;
    context.alloc_tuple(&[Term::atom(Atom::ERROR), reason])
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
#[path = "file_bifs_tests.rs"]
mod tests;
