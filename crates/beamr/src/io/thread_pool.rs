//! Blocking thread-pool completion ring used as a non-Linux development fallback.
//!
//! This backend preserves completion semantics for development and tests on
//! platforms without io_uring. It is not the production async I/O path.

#![cfg(not(target_os = "linux"))]

use std::ffi::CString;
use std::io;
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::io::ring::{CompletionRing, IoCompletion, IoOp, IoResult, StatxData};

const DEFAULT_POOL_SIZE: usize = 4;

enum WorkerMessage {
    Run { op_id: u64, op: IoOp },
    Shutdown,
}

/// Completion ring backed by blocking worker threads on non-Linux platforms.
pub struct ThreadPoolRing {
    next_op_id: AtomicU64,
    pending: std::sync::Arc<AtomicUsize>,
    shutdown: AtomicBool,
    job_sender: Sender<WorkerMessage>,
    completion_receiver: Receiver<IoCompletion>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl ThreadPoolRing {
    /// Construct a fallback ring with `pool_size` workers, or four workers when zero is requested.
    #[must_use]
    pub fn new(pool_size: usize) -> Self {
        let worker_count = if pool_size == 0 {
            DEFAULT_POOL_SIZE
        } else {
            pool_size
        };
        let (job_sender, job_receiver) = crossbeam_channel::unbounded();
        let (completion_sender, completion_receiver) = crossbeam_channel::unbounded();
        let pending = std::sync::Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let jobs = job_receiver.clone();
            let completions = completion_sender.clone();
            let pending = std::sync::Arc::clone(&pending);
            match thread::Builder::new()
                .name(format!("beamr-io-thread-pool-{worker_index}"))
                .spawn(move || worker_loop(jobs, completions, pending))
            {
                Ok(handle) => workers.push(handle),
                Err(_spawn_error) => break,
            }
        }

        Self {
            next_op_id: AtomicU64::new(1),
            pending,
            shutdown: AtomicBool::new(false),
            job_sender,
            completion_receiver,
            workers: Mutex::new(workers),
        }
    }
}

impl CompletionRing for ThreadPoolRing {
    fn submit(&self, op: IoOp) -> u64 {
        let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
        if self.shutdown.load(Ordering::Acquire) {
            return op_id;
        }

        self.pending.fetch_add(1, Ordering::AcqRel);
        if self
            .job_sender
            .send(WorkerMessage::Run { op_id, op })
            .is_err()
        {
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
        op_id
    }

    fn poll_completions(&self, timeout: Duration) -> Vec<IoCompletion> {
        let mut completions = Vec::new();
        match self.completion_receiver.recv_timeout(timeout) {
            Ok(completion) => completions.push(completion),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return completions,
        }

        while let Ok(completion) = self.completion_receiver.try_recv() {
            completions.push(completion);
        }
        completions
    }

    fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }

        let worker_count = self
            .workers
            .lock()
            .map(|workers| workers.len())
            .unwrap_or(0);
        for _ in 0..worker_count {
            let _sent = self.job_sender.send(WorkerMessage::Shutdown);
        }

        if let Ok(mut workers) = self.workers.lock() {
            while let Some(handle) = workers.pop() {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
            }
        }
    }
}

impl Drop for ThreadPoolRing {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    job_receiver: Receiver<WorkerMessage>,
    completion_sender: Sender<IoCompletion>,
    pending: std::sync::Arc<AtomicUsize>,
) {
    while let Ok(message) = job_receiver.recv() {
        match message {
            WorkerMessage::Run { op_id, op } => {
                let result = execute_op(op);
                pending.fetch_sub(1, Ordering::AcqRel);
                if completion_sender
                    .send(IoCompletion { op_id, result })
                    .is_err()
                {
                    break;
                }
            }
            WorkerMessage::Shutdown => break,
        }
    }
}

fn execute_op(op: IoOp) -> io::Result<IoResult> {
    match op {
        IoOp::Read {
            fd,
            buf_len,
            offset,
        } => read_fd(fd, buf_len, offset),
        IoOp::Write { fd, data, offset } => write_fd(fd, &data, offset),
        IoOp::Accept { listener_fd } => accept_fd(listener_fd),
        IoOp::Connect { fd, addr } => connect_fd(fd, addr),
        IoOp::Close { fd } => close_fd(fd),
        IoOp::Fsync { fd } => fsync_fd(fd),
        IoOp::Openat {
            dir_fd,
            path,
            flags,
            mode,
        } => openat_fd(dir_fd, &path, flags, mode),
        IoOp::Statx {
            dir_fd,
            path,
            flags,
            mask,
        } => statx_fd(dir_fd, &path, flags, mask),
        IoOp::ListDir { path } => {
            crate::native::file_meta_bifs::read_dir_entries(&path).map(IoResult::DirList)
        }
        IoOp::MakeDir { path } => std::fs::create_dir(path).map(|()| IoResult::Completed),
        IoOp::DelFile { path } => std::fs::remove_file(path).map(|()| IoResult::Completed),
        IoOp::DelDir { path } => std::fs::remove_dir(path).map(|()| IoResult::Completed),
        IoOp::Rename {
            source,
            destination,
        } => std::fs::rename(source, destination).map(|()| IoResult::Completed),
        IoOp::Nop => Ok(IoResult::Completed),
    }
}

fn read_fd(fd: RawFd, buf_len: usize, offset: u64) -> io::Result<IoResult> {
    let mut buffer = vec![0_u8; buf_len];
    let rc = if offset == u64::MAX {
        // SAFETY: `buffer` is valid writable memory for `buf_len` bytes and `fd` ownership remains with caller.
        unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) }
    } else {
        // SAFETY: `buffer` is valid writable memory for `buf_len` bytes and `fd` ownership remains with caller.
        unsafe {
            libc::pread(
                fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                offset as libc::off_t,
            )
        }
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        let bytes_read = rc as usize;
        buffer.truncate(bytes_read);
        Ok(IoResult::BytesRead(bytes_read, buffer))
    }
}

fn write_fd(fd: RawFd, data: &[u8], offset: u64) -> io::Result<IoResult> {
    let rc = if offset == u64::MAX {
        // SAFETY: `data` is valid readable memory for its length and `fd` ownership remains with caller.
        unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) }
    } else {
        // SAFETY: `data` is valid readable memory for its length and `fd` ownership remains with caller.
        unsafe { libc::pwrite(fd, data.as_ptr().cast(), data.len(), offset as libc::off_t) }
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::BytesWritten(rc as usize))
    }
}

fn close_fd(fd: RawFd) -> io::Result<IoResult> {
    // SAFETY: completion ring owns this requested close operation for the raw descriptor.
    let rc = unsafe { libc::close(fd) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::Closed)
    }
}

fn fsync_fd(fd: RawFd) -> io::Result<IoResult> {
    // SAFETY: `fd` is passed directly to libc; ownership remains with caller.
    let rc = unsafe { libc::fsync(fd) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::Synced)
    }
}

fn openat_fd(dir_fd: RawFd, path: &Path, flags: i32, mode: u32) -> io::Result<IoResult> {
    let c_path = path_to_cstring(path)?;
    // SAFETY: `c_path` is NUL-terminated and alive for the duration of the call.
    let fd = unsafe { libc::openat(dir_fd, c_path.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::Opened(fd))
    }
}

fn statx_fd(dir_fd: RawFd, path: &Path, flags: i32, mask: u32) -> io::Result<IoResult> {
    let c_path = path_to_cstring(path)?;
    // SAFETY: zeroed stat buffer is immediately initialized by fstatat on success.
    let mut stat: libc::stat = unsafe { mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `stat` points to writable memory.
    let rc = unsafe { libc::fstatat(dir_fd, c_path.as_ptr(), &mut stat, flags) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::StatResult(stat_to_data(stat, mask)))
    }
}

fn accept_fd(listener_fd: RawFd) -> io::Result<IoResult> {
    // SAFETY: zeroed storage is filled by accept on success before being read.
    let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: `storage` and `len` are valid output pointers for accept.
    let fd = unsafe {
        libc::accept(
            listener_fd,
            (&mut storage as *mut libc::sockaddr_storage).cast(),
            &mut len,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        sockaddr_to_addr(&storage).map(|addr| IoResult::Accepted(fd, addr))
    }
}

fn connect_fd(fd: RawFd, addr: SocketAddr) -> io::Result<IoResult> {
    let (storage, len) = socket_addr_to_raw(addr);
    // SAFETY: `storage` contains a sockaddr matching `len` and is alive for the call.
    let rc = unsafe { libc::connect(fd, (&storage as *const libc::sockaddr_storage).cast(), len) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(IoResult::Connected)
    }
}

fn path_to_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL byte",
        )
    })
}

fn stat_to_data(stat: libc::stat, mask: u32) -> StatxData {
    StatxData {
        mask,
        mode: u32::from(stat.st_mode),
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        dev_major: 0,
        dev_minor: 0,
        inode: stat.st_ino,
        nlink: stat.st_nlink as u64,
        uid: stat.st_uid,
        gid: stat.st_gid,
        atime_sec: stat.st_atime,
        mtime_sec: stat.st_mtime,
        ctime_sec: stat.st_ctime,
    }
}

fn socket_addr_to_raw(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: zeroed sockaddr_storage is written with a concrete sockaddr value below.
    let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
    match addr {
        SocketAddr::V4(addr) => {
            let raw = libc::sockaddr_in {
                sin_len: mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: addr.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(addr.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: storage is large enough and properly aligned for sockaddr_in.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), raw) };
            (
                storage,
                mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(addr) => {
            let raw = libc::sockaddr_in6 {
                sin6_len: mem::size_of::<libc::sockaddr_in6>() as u8,
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: addr.port().to_be(),
                sin6_flowinfo: addr.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: addr.ip().octets(),
                },
                sin6_scope_id: addr.scope_id(),
            };
            // SAFETY: storage is large enough and properly aligned for sockaddr_in6.
            unsafe { std::ptr::write((&mut storage as *mut libc::sockaddr_storage).cast(), raw) };
            (
                storage,
                mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

fn sockaddr_to_addr(storage: &libc::sockaddr_storage) -> io::Result<SocketAddr> {
    match storage.ss_family as i32 {
        libc::AF_INET => {
            // SAFETY: family indicates storage contains sockaddr_in.
            let raw =
                unsafe { *(storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>() };
            let ip = Ipv4Addr::from(u32::from_be(raw.sin_addr.s_addr).to_ne_bytes());
            Ok(SocketAddr::V4(SocketAddrV4::new(
                ip,
                u16::from_be(raw.sin_port),
            )))
        }
        libc::AF_INET6 => {
            // SAFETY: family indicates storage contains sockaddr_in6.
            let raw =
                unsafe { *(storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in6>() };
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(raw.sin6_addr.s6_addr),
                u16::from_be(raw.sin6_port),
                raw.sin6_flowinfo,
                raw.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported socket address family",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn read_and_write_temp_file_complete() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("beamr-thread-pool-ring-{unique}.tmp"));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("temp file should open");
        let ring = ThreadPoolRing::new(2);

        let write_id = ring.submit(IoOp::Write {
            fd: file.as_raw_fd(),
            data: b"beamr".to_vec(),
            offset: 0,
        });
        let completions = ring.poll_completions(Duration::from_secs(2));
        assert!(completions.iter().any(|completion| {
            completion.op_id == write_id
                && matches!(completion.result, Ok(IoResult::BytesWritten(5)))
        }));

        let read_id = ring.submit(IoOp::Read {
            fd: file.as_raw_fd(),
            buf_len: 5,
            offset: 0,
        });
        let completions = ring.poll_completions(Duration::from_secs(2));
        assert!(completions.iter().any(|completion| {
            completion.op_id == read_id
                && matches!(&completion.result, Ok(IoResult::BytesRead(5, bytes)) if bytes == b"beamr")
        }));

        ring.shutdown();
        drop(file);
        let _removed = fs::remove_file(path);
    }
}
