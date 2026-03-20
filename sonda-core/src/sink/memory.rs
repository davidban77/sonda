//! In-memory sink for testing.

use super::Sink;
use crate::SondaError;

/// An in-memory sink that accumulates all written bytes in a `Vec<u8>`.
///
/// This sink is intended for use in tests across the project. It allows
/// callers to inspect the exact bytes that would have been delivered to a
/// real destination (file, socket, stdout) without any I/O.
pub struct MemorySink {
    /// All bytes written to this sink, in order.
    pub buffer: Vec<u8>,
}

impl MemorySink {
    /// Create a new, empty `MemorySink`.
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for MemorySink {
    /// Append `data` to the internal buffer.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    /// No-op flush — all data is already in memory.
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}
