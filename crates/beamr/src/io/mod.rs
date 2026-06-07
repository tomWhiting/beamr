//! Configurable output sinks and resource lifecycle support used by I/O BIFs.

pub mod resource;
pub mod ring;
#[cfg(not(target_os = "linux"))]
pub mod thread_pool;
#[cfg(target_os = "linux")]
pub mod uring;

use std::io::Write;

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
            Err(error) => Box::new(ring::FailedRing::new(error.kind())),
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
