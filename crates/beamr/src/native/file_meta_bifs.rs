//! File metadata BIFs.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use crate::atom::{Atom, AtomTable};
use crate::io::{IoOp, IoResult, StatxData, errno_to_atom};
use crate::native::{
    BifRegistryImpl, Capability, FileIoCompletion, FileIoContinuation, NativeRegistrationError,
    ProcessContext,
};
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;

/// Registers Erlang file metadata BIFs.
pub fn register_file_meta_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for (name, arity, function) in [
        ("file_info", 1, file_info as crate::native::NativeFn),
        ("list_dir", 1, list_dir as crate::native::NativeFn),
        ("make_dir", 1, make_dir as crate::native::NativeFn),
        ("del_file", 1, del_file as crate::native::NativeFn),
        ("del_dir", 1, del_dir as crate::native::NativeFn),
        ("rename", 2, rename as crate::native::NativeFn),
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

/// erlang:file_info/1.
pub fn file_info(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_file_info(completion, context);
    }

    let [filename] = args else {
        return Err(badarg());
    };
    let path = filename_path(*filename)?;
    context.submit_file_io(
        IoOp::Statx {
            dir_fd: libc::AT_FDCWD,
            path,
            flags: libc::AT_SYMLINK_NOFOLLOW,
            mask: statx_basic_stats_mask(),
        },
        FileIoContinuation::FileInfo,
    )?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:list_dir/1.
pub fn list_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_list_dir(completion, context);
    }

    let [dirname] = args else {
        return Err(badarg());
    };
    context.submit_file_io(
        IoOp::ListDir {
            path: filename_path(*dirname)?,
        },
        FileIoContinuation::ListDir,
    )?;
    Ok(Term::atom(Atom::OK))
}

/// erlang:make_dir/1.
pub fn make_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    submit_unary_metadata(args, context, FileIoContinuation::MakeDir, |path| {
        IoOp::MakeDir { path }
    })
}

/// erlang:del_file/1.
pub fn del_file(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    submit_unary_metadata(args, context, FileIoContinuation::DelFile, |path| {
        IoOp::DelFile { path }
    })
}

/// erlang:del_dir/1.
pub fn del_dir(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    submit_unary_metadata(args, context, FileIoContinuation::DelDir, |path| {
        IoOp::DelDir { path }
    })
}

/// erlang:rename/2.
pub fn rename(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if let Some(completion) = context.take_file_io_completion() {
        return finish_ok_metadata(completion, context);
    }

    let [source, destination] = args else {
        return Err(badarg());
    };
    context.submit_file_io(
        IoOp::Rename {
            source: filename_path(*source)?,
            destination: filename_path(*destination)?,
        },
        FileIoContinuation::Rename,
    )?;
    Ok(Term::atom(Atom::OK))
}

fn submit_unary_metadata<F>(
    args: &[Term],
    context: &mut ProcessContext,
    continuation: FileIoContinuation,
    op: F,
) -> Result<Term, Term>
where
    F: FnOnce(PathBuf) -> IoOp,
{
    if let Some(completion) = context.take_file_io_completion() {
        return finish_ok_metadata(completion, context);
    }

    let [filename] = args else {
        return Err(badarg());
    };
    context.submit_file_io(op(filename_path(*filename)?), continuation)?;
    Ok(Term::atom(Atom::OK))
}

fn finish_file_info(
    completion: FileIoCompletion,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    if !matches!(completion.continuation, FileIoContinuation::FileInfo) {
        return error_tuple(context, Atom::UNKNOWN_ERROR);
    }

    match completion.completion.result {
        Ok(IoResult::StatResult(data)) => file_info_tuple(context, &data),
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn finish_list_dir(
    completion: FileIoCompletion,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    if !matches!(completion.continuation, FileIoContinuation::ListDir) {
        return error_tuple(context, Atom::UNKNOWN_ERROR);
    }

    match completion.completion.result {
        Ok(IoResult::DirList(entries)) => {
            let mut terms = Vec::with_capacity(entries.len());
            for entry in entries {
                terms.push(context.alloc_binary(&entry)?);
            }
            let list = context.alloc_list(&terms)?;
            ok_tuple(context, list)
        }
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn finish_ok_metadata(
    completion: FileIoCompletion,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    if !is_ok_metadata_continuation(&completion.continuation) {
        return error_tuple(context, Atom::UNKNOWN_ERROR);
    }

    match completion.completion.result {
        Ok(IoResult::Completed) => Ok(Term::atom(Atom::OK)),
        Ok(_) => error_tuple(context, Atom::UNKNOWN_ERROR),
        Err(error) => error_tuple(context, error_reason(error)),
    }
}

fn is_ok_metadata_continuation(continuation: &FileIoContinuation) -> bool {
    matches!(
        continuation,
        FileIoContinuation::MakeDir
            | FileIoContinuation::DelFile
            | FileIoContinuation::DelDir
            | FileIoContinuation::Rename
    )
}

fn statx_basic_stats_mask() -> u32 {
    #[cfg(target_os = "linux")]
    {
        libc::STATX_TYPE
            | libc::STATX_MODE
            | libc::STATX_NLINK
            | libc::STATX_UID
            | libc::STATX_GID
            | libc::STATX_ATIME
            | libc::STATX_MTIME
            | libc::STATX_CTIME
            | libc::STATX_INO
            | libc::STATX_SIZE
            | libc::STATX_BLOCKS
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

fn file_info_tuple(context: &mut ProcessContext, data: &StatxData) -> Result<Term, Term> {
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let fields = [
        Term::atom(atom_table.intern("file_info")),
        unsigned_term(data.size)?,
        Term::atom(file_type_atom(atom_table, data.mode)),
        Term::atom(access_atom(atom_table, data.mode)),
        signed_term(data.atime_sec)?,
        signed_term(data.mtime_sec)?,
        signed_term(data.ctime_sec)?,
        unsigned_term(u64::from(data.mode))?,
        unsigned_term(data.nlink)?,
        unsigned_term(u64::from(data.dev_major))?,
        unsigned_term(u64::from(data.dev_minor))?,
        unsigned_term(data.inode)?,
        unsigned_term(u64::from(data.uid))?,
        unsigned_term(u64::from(data.gid))?,
    ];
    context.alloc_tuple(&fields)
}

fn file_type_atom(atom_table: &AtomTable, mode: u32) -> Atom {
    match mode & libc::S_IFMT as u32 {
        value if value == libc::S_IFREG as u32 => atom_table.intern("regular"),
        value if value == libc::S_IFDIR as u32 => atom_table.intern("directory"),
        value if value == libc::S_IFLNK as u32 => atom_table.intern("symlink"),
        value if value == libc::S_IFBLK as u32 || value == libc::S_IFCHR as u32 => {
            atom_table.intern("device")
        }
        _ => atom_table.intern("other"),
    }
}

fn access_atom(atom_table: &AtomTable, mode: u32) -> Atom {
    let read_bits = (libc::S_IRUSR | libc::S_IRGRP | libc::S_IROTH) as u32;
    let write_bits = (libc::S_IWUSR | libc::S_IWGRP | libc::S_IWOTH) as u32;
    let readable = mode & read_bits != 0;
    let writable = mode & write_bits != 0;
    match (readable, writable) {
        (true, true) => atom_table.intern("read_write"),
        (true, false) => Atom::READ,
        (false, true) => Atom::WRITE,
        (false, false) => atom_table.intern("none"),
    }
}

fn unsigned_term(value: u64) -> Result<Term, Term> {
    i64::try_from(value)
        .ok()
        .and_then(Term::try_small_int)
        .ok_or_else(badarg)
}

fn signed_term(value: i64) -> Result<Term, Term> {
    Term::try_small_int(value).ok_or_else(badarg)
}

fn filename_path(term: Term) -> Result<PathBuf, Term> {
    let bytes = BinaryRef::new(term).ok_or_else(badarg)?.as_bytes();
    let filename = std::str::from_utf8(bytes).map_err(|_| badarg())?;
    Ok(PathBuf::from(filename))
}

/// Return the byte-oriented filename component for directory entries.
pub(crate) fn os_filename_bytes(name: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        name.to_string_lossy().as_bytes().to_vec()
    }
}

/// Blocking directory listing helper used by completion-ring worker backends.
pub(crate) fn read_dir_entries(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        entries.push(os_filename_bytes(&entry.file_name()));
    }
    Ok(entries)
}

fn ok_tuple(context: &mut ProcessContext, value: Term) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::OK), value])
}

fn error_tuple(context: &mut ProcessContext, reason: Atom) -> Result<Term, Term> {
    context.alloc_tuple(&[Term::atom(Atom::ERROR), Term::atom(reason)])
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
#[path = "file_meta_bifs_tests.rs"]
mod tests;
