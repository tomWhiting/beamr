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
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Tuple};

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
        ("file_seek", 3, file_seek as crate::native::NativeFn),
        ("pread", 3, pread as crate::native::NativeFn),
        ("pwrite", 3, pwrite as crate::native::NativeFn),
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
    context.request_await_suspend(None);
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
    let count = parse_count(*count_term)?;
    let inner = resource.inner();
    submit_read(
        context,
        resource,
        count,
        inner.current_offset(),
        Some(inner),
    )
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
    let data = binary_bytes(*data_term)?;
    let expected_len = data.len();
    let inner = resource.inner();
    context.submit_file_io(
        IoOp::Write {
            fd: inner.fd(),
            data,
            offset: inner.current_offset(),
        },
        FileIoContinuation::Write {
            fd: Some(inner),
            expected_len,
        },
    )?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:file_seek/3.
pub fn file_seek(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_seek(completion, context);
    }

    let [fd_term, offset_term, whence_term] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    let offset = offset_term.as_small_int().ok_or_else(badarg)?;
    let whence = parse_whence(*whence_term)?;
    let inner = resource.inner();
    match whence {
        SeekWhence::Bof => set_seek_position(context, &inner, i128::from(offset)),
        SeekWhence::Cur => set_seek_position(
            context,
            &inner,
            i128::from(inner.current_offset()) + i128::from(offset),
        ),
        SeekWhence::Eof => {
            context.submit_file_io(
                IoOp::Statx {
                    dir_fd: inner.fd(),
                    path: PathBuf::new(),
                    flags: statx_empty_path_flag(),
                    mask: statx_size_mask(),
                },
                FileIoContinuation::SeekEof { fd: inner, offset },
            )?;
            Ok(Term::atom(Atom::OK))
        }
    }
}

/// erlang:pread/3.
pub fn pread(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_read(completion, context);
    }
    let [fd_term, offset_term, count_term] = args else {
        return Err(badarg());
    };
    submit_read(
        context,
        FdResource::new(*fd_term).ok_or_else(badarg)?,
        parse_count(*count_term)?,
        parse_non_negative_offset(*offset_term)?,
        None,
    )
}

fn submit_read(
    context: &mut ProcessContext,
    resource: FdResource,
    count: usize,
    offset: u64,
    fd: Option<Arc<FdInner>>,
) -> Result<Term, Term> {
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    context.submit_file_io(
        IoOp::Read {
            fd: resource.fd(),
            buf_len: count,
            offset,
        },
        FileIoContinuation::Read { fd },
    )?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:pwrite/3.
pub fn pwrite(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_write(completion, context);
    }

    let [fd_term, offset_term, data_term] = args else {
        return Err(badarg());
    };
    let resource = FdResource::new(*fd_term).ok_or_else(badarg)?;
    if resource.state() != FdState::Open {
        return error_tuple(context, Atom::CLOSED);
    }
    let offset = parse_non_negative_offset(*offset_term)?;
    let data = binary_bytes(*data_term)?;
    let expected_len = data.len();
    context.submit_file_io(
        IoOp::Write {
            fd: resource.fd(),
            data,
            offset,
        },
        FileIoContinuation::Write {
            fd: None,
            expected_len,
        },
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
            if let FileIoContinuation::Read { fd: Some(fd) } = completion.continuation {
                fd.advance_current_offset(bytes_read);
            }
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
        FileIoContinuation::Write { expected_len, .. } => expected_len,
        _ => return error_tuple(context, Atom::UNKNOWN_ERROR),
    };
    match completion.completion.result {
        Ok(IoResult::BytesWritten(bytes_written)) if bytes_written == expected_len => {
            advance_write_offset(&completion.continuation, bytes_written);
            Ok(Term::atom(Atom::OK))
        }
        Ok(IoResult::BytesWritten(bytes_written)) => {
            advance_write_offset(&completion.continuation, bytes_written);
            incomplete_tuple(context, bytes_written)
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn finish_seek(completion: FileIoCompletion, context: &mut ProcessContext) -> Result<Term, Term> {
    let (fd, offset) = match completion.continuation {
        FileIoContinuation::SeekEof { fd, offset } => (fd, offset),
        _ => return error_tuple(context, Atom::UNKNOWN_ERROR),
    };
    match completion.completion.result {
        Ok(IoResult::StatResult(data)) => {
            set_seek_position(context, &fd, i128::from(data.size) + i128::from(offset))
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn filename_path(term: Term) -> Result<PathBuf, Term> {
    let bytes = BinaryRef::new(term).ok_or_else(badarg)?.as_bytes();
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

enum SeekWhence {
    Bof,
    Cur,
    Eof,
}

fn parse_whence(term: Term) -> Result<SeekWhence, Term> {
    if term == Term::atom(Atom::BOF) {
        Ok(SeekWhence::Bof)
    } else if term == Term::atom(Atom::CUR) {
        Ok(SeekWhence::Cur)
    } else if term == Term::atom(Atom::EOF) {
        Ok(SeekWhence::Eof)
    } else {
        Err(badarg())
    }
}

fn parse_non_negative_offset(term: Term) -> Result<u64, Term> {
    term.as_small_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(badarg)
}

fn parse_count(term: Term) -> Result<usize, Term> {
    term.as_small_int()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(badarg)
}

fn binary_bytes(term: Term) -> Result<Vec<u8>, Term> {
    Ok(BinaryRef::new(term).ok_or_else(badarg)?.as_bytes().to_vec())
}

fn set_seek_position(
    context: &mut ProcessContext,
    fd: &FdInner,
    position: i128,
) -> Result<Term, Term> {
    let Ok(position) = u64::try_from(position) else {
        return error_tuple(context, Atom::EINVAL);
    };
    let Some(term) = i64::try_from(position).ok().and_then(Term::try_small_int) else {
        return error_tuple(context, Atom::EINVAL);
    };
    fd.set_current_offset(position);
    ok_tuple(context, term)
}

fn advance_write_offset(continuation: &FileIoContinuation, bytes_written: usize) {
    if let FileIoContinuation::Write { fd: Some(fd), .. } = continuation {
        fd.advance_current_offset(bytes_written);
    }
}

fn statx_size_mask() -> u32 {
    #[cfg(target_os = "linux")]
    {
        libc::STATX_SIZE
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

fn statx_empty_path_flag() -> i32 {
    #[cfg(target_os = "linux")]
    {
        libc::AT_EMPTY_PATH
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
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
