//! CLI output sinks used by run and record modes.

use std::sync::{Arc, Mutex};

use beamr::io::IoSink;

#[derive(Clone, Default)]
pub struct CaptureSink {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CaptureSink {
    pub fn into_string(self) -> String {
        let bytes = match self.bytes.lock() {
            Ok(guard) => guard.clone(),
            Err(error) => error.into_inner().clone(),
        };
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl IoSink for CaptureSink {
    fn write(&self, bytes: &[u8]) {
        match self.bytes.lock() {
            Ok(mut guard) => guard.extend_from_slice(bytes),
            Err(error) => error.into_inner().extend_from_slice(bytes),
        }
    }
}

#[derive(Debug, Default)]
pub struct ConsoleSink;

impl IoSink for ConsoleSink {
    fn write(&self, bytes: &[u8]) {
        let mut stdout = std::io::stdout().lock();
        let _ = std::io::Write::write_all(&mut stdout, bytes);
        let _ = std::io::Write::flush(&mut stdout);
    }
}
