//! Backend-neutral completion ring types shared by I/O lifecycle code.

#[cfg(target_os = "linux")]
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// I/O operation accepted by a completion ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IoOp {
    /// Read up to `buf_len` bytes from `fd` at `offset`.
    Read {
        fd: RawFd,
        buf_len: usize,
        offset: u64,
    },
    /// Write `data` to `fd` at `offset`.
    Write {
        fd: RawFd,
        data: Vec<u8>,
        offset: u64,
    },
    /// Accept a connection from a listening socket.
    Accept { listener_fd: RawFd },
    /// Connect a socket to `addr`.
    Connect { fd: RawFd, addr: SocketAddr },
    /// Close a raw file descriptor asynchronously.
    Close { fd: RawFd },
    /// Synchronize file contents to stable storage.
    Fsync { fd: RawFd },
    /// Open a path relative to `dir_fd`.
    Openat {
        dir_fd: RawFd,
        path: PathBuf,
        flags: i32,
        mode: u32,
    },
    /// Query file metadata relative to `dir_fd`.
    Statx {
        dir_fd: RawFd,
        path: PathBuf,
        flags: i32,
        mask: u32,
    },
    /// Complete without performing I/O.
    Nop,
}

/// Portable subset of statx-style metadata returned by completion rings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatxData {
    /// Mask of fields populated by the backend.
    pub mask: u32,
    /// File type and permissions.
    pub mode: u32,
    /// File size in bytes.
    pub size: u64,
    /// Number of allocated 512-byte blocks.
    pub blocks: u64,
    /// Device major number for the inode owner device, when available.
    pub dev_major: u32,
    /// Device minor number for the inode owner device, when available.
    pub dev_minor: u32,
    /// Inode number.
    pub inode: u64,
    /// Link count.
    pub nlink: u64,
    /// Owning user id.
    pub uid: u32,
    /// Owning group id.
    pub gid: u32,
    /// Last access time, seconds since the Unix epoch.
    pub atime_sec: i64,
    /// Last modification time, seconds since the Unix epoch.
    pub mtime_sec: i64,
    /// Last status-change time, seconds since the Unix epoch.
    pub ctime_sec: i64,
}

/// Successful operation result produced by a completion ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IoResult {
    /// Bytes read and the buffer containing exactly those bytes.
    BytesRead(usize, Vec<u8>),
    /// Number of bytes written.
    BytesWritten(usize),
    /// Accepted file descriptor and peer address.
    Accepted(RawFd, SocketAddr),
    /// Connect completed successfully.
    Connected,
    /// File descriptor closed.
    Closed,
    /// Fsync completed successfully.
    Synced,
    /// Opened file descriptor.
    Opened(RawFd),
    /// Stat result.
    StatResult(StatxData),
    /// Generic successful completion.
    Completed,
}

/// Completion emitted by a ring for a submitted operation.
#[derive(Debug)]
pub struct IoCompletion {
    /// Operation id returned by [`CompletionRing::submit`].
    pub op_id: u64,
    /// Backend-decoded result.
    pub result: io::Result<IoResult>,
}

/// Completion-ring interface used by platform-specific I/O backends.
pub trait CompletionRing: Send + Sync {
    /// Submit an operation and return its ring-assigned monotonically increasing operation id.
    fn submit(&self, op: IoOp) -> u64;

    /// Poll for completions, waiting up to `timeout` for the first completion.
    fn poll_completions(&self, timeout: Duration) -> Vec<IoCompletion>;

    /// Return the number of operations submitted but not yet completed.
    fn pending_count(&self) -> usize;

    /// Stop accepting work and cleanly shut down backend workers.
    fn shutdown(&self);
}

/// Ring used only when a requested platform backend cannot be constructed.
#[cfg(target_os = "linux")]
pub(crate) struct FailedRing {
    next_op_id: AtomicU64,
    error_kind: io::ErrorKind,
    error_message: String,
    completions: Mutex<VecDeque<IoCompletion>>,
}

#[cfg(target_os = "linux")]
impl FailedRing {
    pub(crate) fn new(error: io::Error) -> Self {
        Self {
            next_op_id: AtomicU64::new(1),
            error_kind: error.kind(),
            error_message: error.to_string(),
            completions: Mutex::new(VecDeque::new()),
        }
    }
}

#[cfg(target_os = "linux")]
impl CompletionRing for FailedRing {
    fn submit(&self, _op: IoOp) -> u64 {
        let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
        let completion = IoCompletion {
            op_id,
            result: Err(io::Error::new(
                self.error_kind,
                format!(
                    "completion ring backend unavailable: {}",
                    self.error_message
                ),
            )),
        };
        if let Ok(mut completions) = self.completions.lock() {
            completions.push_back(completion);
        }
        op_id
    }

    fn poll_completions(&self, _timeout: Duration) -> Vec<IoCompletion> {
        self.completions
            .lock()
            .map(|mut completions| completions.drain(..).collect())
            .unwrap_or_default()
    }

    fn pending_count(&self) -> usize {
        0
    }

    fn shutdown(&self) {}
}
