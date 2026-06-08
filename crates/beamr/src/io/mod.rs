//! Configurable output sinks and resource lifecycle support used by I/O BIFs.

pub mod bridge;
pub mod facility;
pub mod resource;
pub mod ring;
#[cfg(not(target_os = "linux"))]
pub mod thread_pool;
#[cfg(target_os = "linux")]
pub mod uring;

use std::io::Write;

pub use bridge::{IoCompletionBridge, IoWakeTarget, PendingIo, PendingIoRegistry, ResultMode};
pub use facility::{CompletionRingIoFacility, IoError, IoFacility};

use crate::atom::Atom;

pub use ring::{CompletionRing, IoCompletion, IoOp, IoResult, StatxData};
#[cfg(not(target_os = "linux"))]
pub use thread_pool::ThreadPoolRing;
#[cfg(target_os = "linux")]
pub use uring::IoUringRing;

/// Configuration for constructing the platform completion ring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RingConfig {
    /// Linux io_uring queue depth. Defaults to 256.
    pub ring_depth: u32,
    /// Non-Linux fallback worker count. Defaults to 4.
    pub fallback_pool_size: usize,
}

#[cfg(test)]
mod tests {
    use super::errno_to_atom;
    use crate::atom::Atom;

    #[test]
    fn errno_mapping_returns_erlang_reason_atoms() {
        assert_eq!(errno_to_atom(libc::ENOENT), Atom::ENOENT);
        assert_eq!(errno_to_atom(libc::EACCES), Atom::EACCES);
        assert_eq!(errno_to_atom(libc::EEXIST), Atom::EEXIST);
        assert_eq!(errno_to_atom(libc::EISDIR), Atom::EISDIR);
        assert_eq!(errno_to_atom(libc::ENOTDIR), Atom::ENOTDIR);
        assert_eq!(errno_to_atom(libc::ENOSPC), Atom::ENOSPC);
        assert_eq!(errno_to_atom(libc::EMFILE), Atom::EMFILE);
        assert_eq!(errno_to_atom(libc::ENFILE), Atom::ENFILE);
        assert_eq!(errno_to_atom(libc::EBADF), Atom::EBADF);
        assert_eq!(errno_to_atom(libc::EPIPE), Atom::EPIPE);
        assert_eq!(errno_to_atom(libc::EAGAIN), Atom::EAGAIN);
        assert_eq!(errno_to_atom(libc::EINVAL), Atom::EINVAL);
        assert_eq!(errno_to_atom(libc::ENOTEMPTY), Atom::ENOTEMPTY);
        assert_eq!(errno_to_atom(libc::EXDEV), Atom::EXDEV);
        assert_eq!(errno_to_atom(libc::ELOOP), Atom::ELOOP);
        assert_eq!(errno_to_atom(libc::EROFS), Atom::EROFS);
        assert_eq!(errno_to_atom(libc::ENAMETOOLONG), Atom::ENAMETOOLONG);
        assert_eq!(errno_to_atom(libc::EPERM), Atom::EPERM);
        assert_eq!(errno_to_atom(libc::ECONNREFUSED), Atom::ECONNREFUSED);
        assert_eq!(errno_to_atom(libc::ECONNRESET), Atom::ECONNRESET);
        assert_eq!(errno_to_atom(libc::EINPROGRESS), Atom::EINPROGRESS);
        assert_eq!(errno_to_atom(i32::MAX), Atom::UNKNOWN_ERROR);
    }
}

/// Map OS errno values into Erlang-style file error reason atoms.
#[must_use]
pub fn errno_to_atom(errno: i32) -> Atom {
    match errno {
        libc::ENOENT => Atom::ENOENT,
        libc::EACCES => Atom::EACCES,
        libc::EEXIST => Atom::EEXIST,
        libc::EISDIR => Atom::EISDIR,
        libc::ENOTDIR => Atom::ENOTDIR,
        libc::ENOSPC => Atom::ENOSPC,
        libc::EMFILE => Atom::EMFILE,
        libc::ENFILE => Atom::ENFILE,
        libc::EBADF => Atom::EBADF,
        libc::EPIPE => Atom::EPIPE,
        libc::EAGAIN => Atom::EAGAIN,
        libc::EINVAL => Atom::EINVAL,
        libc::ENOTEMPTY => Atom::ENOTEMPTY,
        libc::EXDEV => Atom::EXDEV,
        libc::ELOOP => Atom::ELOOP,
        libc::EROFS => Atom::EROFS,
        libc::ENAMETOOLONG => Atom::ENAMETOOLONG,
        libc::EPERM => Atom::EPERM,
        libc::ECONNREFUSED => Atom::ECONNREFUSED,
        libc::ECONNRESET => Atom::ECONNRESET,
        libc::EINPROGRESS => Atom::EINPROGRESS,
        _ => Atom::UNKNOWN_ERROR,
    }
}

impl Default for RingConfig {
    fn default() -> Self {
        Self {
            ring_depth: 256,
            fallback_pool_size: 4,
        }
    }
}

/// Construct the platform-appropriate completion ring.
#[must_use]
pub fn create_ring(config: RingConfig) -> Box<dyn CompletionRing> {
    #[cfg(target_os = "linux")]
    {
        match try_create_ring(config) {
            Ok(ring) => ring,
            Err(error) => Box::new(ring::FailedRing::new(error)),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Box::new(ThreadPoolRing::new(config.fallback_pool_size))
    }
}

/// Fallible platform ring construction for callers that want backend initialization errors.
pub fn try_create_ring(config: RingConfig) -> std::io::Result<Box<dyn CompletionRing>> {
    #[cfg(target_os = "linux")]
    {
        IoUringRing::new(config.ring_depth).map(|ring| Box::new(ring) as Box<dyn CompletionRing>)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(Box::new(ThreadPoolRing::new(config.fallback_pool_size)))
    }
}

/// Output target for `io` module BIFs.
pub trait IoSink: Send + Sync {
    /// Write bytes to the sink.
    fn write(&self, bytes: &[u8]);
}

/// Default output sink that intentionally discards all bytes.
#[derive(Debug, Default)]
pub struct NullSink;

impl IoSink for NullSink {
    fn write(&self, _bytes: &[u8]) {}
}

/// Output sink that writes directly to process stdout.
#[derive(Debug, Default)]
pub struct StdoutSink;

impl IoSink for StdoutSink {
    fn write(&self, bytes: &[u8]) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(bytes);
        let _ = stdout.flush();
    }
}
