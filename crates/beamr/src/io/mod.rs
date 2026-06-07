//! Configurable output sinks and resource lifecycle support used by I/O BIFs.

pub mod resource;
pub mod ring;

use std::io::Write;

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
