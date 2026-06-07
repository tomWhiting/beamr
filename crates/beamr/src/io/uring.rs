//! Linux io_uring-backed completion ring.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
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

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use io_uring::{IoUring, opcode, types};

use crate::io::ring::{CompletionRing, IoCompletion, IoOp, IoResult, StatxData};

const DEFAULT_RING_DEPTH: u32 = 256;
const RING_TICK: Duration = Duration::from_millis(10);

enum RingMessage {
    Submit { op_id: u64, op: IoOp },
    Shutdown,
}

enum InFlightOp {
    Read {
        buffer: Vec<u8>,
    },
    Write {
        data: Vec<u8>,
    },
    Accept {
        storage: Box<libc::sockaddr_storage>,
        len: Box<libc::socklen_t>,
    },
    Connect {
        storage: Box<libc::sockaddr_storage>,
    },
    Close,
    Fsync,
    Openat {
        path: CString,
    },
    Statx {
        path: CString,
        stat: Box<libc::statx>,
    },
    Nop,
}

/// Completion ring backed by Linux io_uring.
pub struct IoUringRing {
    next_op_id: AtomicU64,
    pending: std::sync::Arc<AtomicUsize>,
    shutdown: std::sync::Arc<AtomicBool>,
    op_sender: Sender<RingMessage>,
    completion_receiver: Receiver<IoCompletion>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl IoUringRing {
    /// Construct an io_uring ring with `ring_depth`, or 256 entries when zero is requested.
    pub fn new(ring_depth: u32) -> io::Result<Self> {
        let depth = if ring_depth == 0 {
            DEFAULT_RING_DEPTH
        } else {
            ring_depth
        };
        let (op_sender, op_receiver) = crossbeam_channel::unbounded();
        let (completion_sender, completion_receiver) = crossbeam_channel::unbounded();
        let (init_sender, init_receiver) = crossbeam_channel::bounded(1);
        let pending = std::sync::Arc::new(AtomicUsize::new(0));
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let thread_pending = std::sync::Arc::clone(&pending);
        let thread_shutdown = std::sync::Arc::clone(&shutdown);

        let handle = thread::Builder::new()
            .name("beamr-io-uring".to_string())
            .spawn(move || {
                let ring = IoUring::new(depth);
                match ring {
                    Ok(ring) => {
                        let _sent = init_sender.send(Ok(()));
                        ring_loop(
                            ring,
                            op_receiver,
                            completion_sender,
                            thread_pending,
                            thread_shutdown,
                        );
                    }
                    Err(error) => {
                        let _sent = init_sender.send(Err(error));
                    }
                }
            })?;

        match init_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                next_op_id: AtomicU64::new(1),
                pending,
                shutdown,
                op_sender,
                completion_receiver,
                thread: Mutex::new(Some(handle)),
            }),
            Ok(Err(error)) => {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
                Err(error)
            }
            Err(_disconnected) => {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "io_uring thread exited during initialization",
                ))
            }
        }
    }
}

impl CompletionRing for IoUringRing {
    fn submit(&self, op: IoOp) -> u64 {
        let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
        if self.shutdown.load(Ordering::Acquire) {
            return op_id;
        }

        self.pending.fetch_add(1, Ordering::AcqRel);
        if self
            .op_sender
            .send(RingMessage::Submit { op_id, op })
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

        loop {
            match self.completion_receiver.try_recv() {
                Ok(completion) => completions.push(completion),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
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
        let _sent = self.op_sender.send(RingMessage::Shutdown);
        if let Ok(mut handle_slot) = self.thread.lock() {
            if let Some(handle) = handle_slot.take() {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
            }
        }
    }
}

impl Drop for IoUringRing {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn ring_loop(
    mut ring: IoUring,
    op_receiver: Receiver<RingMessage>,
    completion_sender: Sender<IoCompletion>,
    pending: std::sync::Arc<AtomicUsize>,
    shutdown: std::sync::Arc<AtomicBool>,
) {
    let mut in_flight = HashMap::new();
    while !shutdown.load(Ordering::Acquire) || !in_flight.is_empty() {
        drain_messages(
            &mut ring,
            &op_receiver,
            &completion_sender,
            &pending,
            &mut in_flight,
            shutdown.load(Ordering::Acquire),
        );
        let _submitted = ring.submitter().submit();
        drain_cqes(&mut ring, &completion_sender, &pending, &mut in_flight);
        if in_flight.is_empty() && shutdown.load(Ordering::Acquire) {
            break;
        }
        match op_receiver.recv_timeout(RING_TICK) {
            Ok(message) => handle_message(
                &mut ring,
                message,
                &completion_sender,
                &pending,
                &mut in_flight,
            ),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => shutdown.store(true, Ordering::Release),
        }
    }
}

fn drain_messages(
    ring: &mut IoUring,
    op_receiver: &Receiver<RingMessage>,
    completion_sender: &Sender<IoCompletion>,
    pending: &AtomicUsize,
    in_flight: &mut HashMap<u64, InFlightOp>,
    shutting_down: bool,
) {
    if shutting_down {
        return;
    }
    loop {
        match op_receiver.try_recv() {
            Ok(message) => handle_message(ring, message, completion_sender, pending, in_flight),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn handle_message(
    ring: &mut IoUring,
    message: RingMessage,
    completion_sender: &Sender<IoCompletion>,
    pending: &AtomicUsize,
    in_flight: &mut HashMap<u64, InFlightOp>,
) {
    match message {
        RingMessage::Submit { op_id, op } => match build_entry(op_id, op) {
            Ok((entry, in_flight_op)) => {
                let mut sq = ring.submission();
                // SAFETY: any pointers referenced by the SQE are owned by `in_flight_op`,
                // which is inserted before submission and kept alive until its CQE arrives.
                let push_result = unsafe { sq.push(&entry) };
                drop(sq);
                match push_result {
                    Ok(()) => {
                        in_flight.insert(op_id, in_flight_op);
                    }
                    Err(_entry) => complete_error(
                        completion_sender,
                        pending,
                        op_id,
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "io_uring submission queue is full",
                        ),
                    ),
                }
            }
            Err(error) => complete_error(completion_sender, pending, op_id, error),
        },
        RingMessage::Shutdown => {}
    }
}

fn build_entry(op_id: u64, op: IoOp) -> io::Result<(io_uring::squeue::Entry, InFlightOp)> {
    match op {
        IoOp::Read {
            fd,
            buf_len,
            offset,
        } => {
            let mut buffer = vec![0_u8; buf_len];
            let entry = opcode::Read::new(types::Fd(fd), buffer.as_mut_ptr(), buffer.len() as u32)
                .offset(offset)
                .build()
                .user_data(op_id);
            Ok((entry, InFlightOp::Read { buffer }))
        }
        IoOp::Write { fd, data, offset } => {
            let entry = opcode::Write::new(types::Fd(fd), data.as_ptr(), data.len() as u32)
                .offset(offset)
                .build()
                .user_data(op_id);
            Ok((entry, InFlightOp::Write { data }))
        }
        IoOp::Accept { listener_fd } => {
            // SAFETY: zeroed storage is filled by accept on success before being read.
            let mut storage = Box::new(unsafe { mem::zeroed::<libc::sockaddr_storage>() });
            let mut len = Box::new(mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t);
            let entry = opcode::Accept::new(
                types::Fd(listener_fd),
                (&mut *storage as *mut libc::sockaddr_storage).cast(),
                &mut *len,
            )
            .build()
            .user_data(op_id);
            Ok((entry, InFlightOp::Accept { storage, len }))
        }
        IoOp::Connect { fd, addr } => {
            let (storage, len) = socket_addr_to_raw(addr);
            let storage = Box::new(storage);
            let entry = opcode::Connect::new(
                types::Fd(fd),
                (&*storage as *const libc::sockaddr_storage).cast(),
                len,
            )
            .build()
            .user_data(op_id);
            Ok((entry, InFlightOp::Connect { storage }))
        }
        IoOp::Close { fd } => Ok((
            opcode::Close::new(types::Fd(fd)).build().user_data(op_id),
            InFlightOp::Close,
        )),
        IoOp::Fsync { fd } => Ok((
            opcode::Fsync::new(types::Fd(fd)).build().user_data(op_id),
            InFlightOp::Fsync,
        )),
        IoOp::Openat {
            dir_fd,
            path,
            flags,
            mode,
        } => {
            let path = path_to_cstring(&path)?;
            let entry = opcode::OpenAt::new(types::Fd(dir_fd), path.as_ptr())
                .flags(flags)
                .mode(mode)
                .build()
                .user_data(op_id);
            Ok((entry, InFlightOp::Openat { path }))
        }
        IoOp::Statx {
            dir_fd,
            path,
            flags,
            mask,
        } => {
            let path = path_to_cstring(&path)?;
            // SAFETY: zeroed statx buffer is initialized by the kernel on successful completion.
            let mut stat = Box::new(unsafe { mem::zeroed::<libc::statx>() });
            let entry =
                opcode::Statx::new(types::Fd(dir_fd), path.as_ptr(), flags, mask, &mut *stat)
                    .build()
                    .user_data(op_id);
            Ok((entry, InFlightOp::Statx { path, stat }))
        }
        IoOp::Nop => Ok((opcode::Nop::new().build().user_data(op_id), InFlightOp::Nop)),
    }
}

fn drain_cqes(
    ring: &mut IoUring,
    completion_sender: &Sender<IoCompletion>,
    pending: &AtomicUsize,
    in_flight: &mut HashMap<u64, InFlightOp>,
) {
    let mut cq = ring.completion();
    for cqe in &mut cq {
        let op_id = cqe.user_data();
        let result = cqe.result();
        let Some(op) = in_flight.remove(&op_id) else {
            continue;
        };
        pending.fetch_sub(1, Ordering::AcqRel);
        let completion = decode_completion(op_id, result, op);
        if completion_sender.send(completion).is_err() {
            break;
        }
    }
}

fn decode_completion(op_id: u64, result: i32, op: InFlightOp) -> IoCompletion {
    if result < 0 {
        return IoCompletion {
            op_id,
            result: Err(io::Error::from_raw_os_error(-result)),
        };
    }

    let io_result = match op {
        InFlightOp::Read { mut buffer } => {
            let bytes_read = result as usize;
            buffer.truncate(bytes_read);
            Ok(IoResult::BytesRead(bytes_read, buffer))
        }
        InFlightOp::Write { data: _data } => Ok(IoResult::BytesWritten(result as usize)),
        InFlightOp::Accept { storage, len: _len } => {
            sockaddr_to_addr(&storage).map(|addr| IoResult::Accepted(result, addr))
        }
        InFlightOp::Connect { storage: _storage } => Ok(IoResult::Connected),
        InFlightOp::Close => Ok(IoResult::Closed),
        InFlightOp::Fsync => Ok(IoResult::Synced),
        InFlightOp::Openat { path: _path } => Ok(IoResult::Opened(result)),
        InFlightOp::Statx { path: _path, stat } => Ok(IoResult::StatResult(statx_to_data(&stat))),
        InFlightOp::Nop => Ok(IoResult::Completed),
    };
    IoCompletion {
        op_id,
        result: io_result,
    }
}

fn complete_error(
    completion_sender: &Sender<IoCompletion>,
    pending: &AtomicUsize,
    op_id: u64,
    error: io::Error,
) {
    pending.fetch_sub(1, Ordering::AcqRel);
    let _sent = completion_sender.send(IoCompletion {
        op_id,
        result: Err(error),
    });
}

fn path_to_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL byte",
        )
    })
}

fn statx_to_data(stat: &libc::statx) -> StatxData {
    StatxData {
        mask: stat.stx_mask,
        mode: stat.stx_mode as u32,
        size: stat.stx_size,
        blocks: stat.stx_blocks,
        dev_major: stat.stx_dev_major,
        dev_minor: stat.stx_dev_minor,
        inode: stat.stx_ino,
        nlink: stat.stx_nlink as u64,
        uid: stat.stx_uid,
        gid: stat.stx_gid,
        atime_sec: stat.stx_atime.tv_sec,
        mtime_sec: stat.stx_mtime.tv_sec,
        ctime_sec: stat.stx_ctime.tv_sec,
    }
}

fn socket_addr_to_raw(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: zeroed sockaddr_storage is written with a concrete sockaddr value below.
    let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
    match addr {
        SocketAddr::V4(addr) => {
            let raw = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: addr.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(addr.ip().octets()).to_be(),
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
    use std::thread;

    #[test]
    fn nop_completes_successfully() {
        let ring = IoUringRing::new(8).expect("io_uring should initialize");
        let op_id = ring.submit(IoOp::Nop);
        let completions = ring.poll_completions(Duration::from_secs(2));
        assert!(completions.iter().any(|completion| {
            completion.op_id == op_id && matches!(completion.result, Ok(IoResult::Completed))
        }));
        ring.shutdown();
    }

    #[test]
    fn read_pipe_completes_after_writer_thread_writes() {
        let mut fds = [0; 2];
        // SAFETY: `fds` points to two valid RawFd slots for libc to initialize.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        let read_fd = fds[0];
        let write_fd = fds[1];
        let ring = IoUringRing::new(8).expect("io_uring should initialize");
        let op_id = ring.submit(IoOp::Read {
            fd: read_fd,
            buf_len: 5,
            offset: u64::MAX,
        });
        let writer = thread::spawn(move || {
            let bytes = b"beamr";
            // SAFETY: `bytes` is a valid readable buffer and `write_fd` is the pipe write end.
            let _written = unsafe { libc::write(write_fd, bytes.as_ptr().cast(), bytes.len()) };
            // SAFETY: this thread owns the pipe write end after it is moved into the closure.
            let _closed = unsafe { libc::close(write_fd) };
        });

        let completions = ring.poll_completions(Duration::from_secs(2));
        assert!(completions.iter().any(|completion| {
            completion.op_id == op_id
                && matches!(&completion.result, Ok(IoResult::BytesRead(5, bytes)) if bytes == b"beamr")
        }));
        ring.shutdown();
        if let Err(payload) = writer.join() {
            std::panic::resume_unwind(payload);
        }
        // SAFETY: read end remains open after the read completion and is owned by this test.
        let _closed = unsafe { libc::close(read_fd) };
    }
}
