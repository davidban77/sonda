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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_stores_exact_bytes_in_buffer() {
        let mut sink = MemorySink::new();
        let data = b"hello, world\n";
        sink.write(data).unwrap();
        assert_eq!(sink.buffer, data);
    }

    #[test]
    fn write_empty_slice_appends_nothing() {
        let mut sink = MemorySink::new();
        sink.write(b"").unwrap();
        assert!(sink.buffer.is_empty());
    }

    #[test]
    fn multiple_writes_accumulate_in_order() {
        let mut sink = MemorySink::new();
        sink.write(b"foo").unwrap();
        sink.write(b"bar").unwrap();
        sink.write(b"baz").unwrap();
        assert_eq!(&sink.buffer, b"foobarbaz");
    }

    #[test]
    fn flush_is_noop_and_returns_ok() {
        let mut sink = MemorySink::new();
        sink.write(b"data").unwrap();
        let result = sink.flush();
        assert!(result.is_ok());
        // Buffer unchanged after flush
        assert_eq!(&sink.buffer, b"data");
    }

    #[test]
    fn flush_on_empty_sink_returns_ok() {
        let mut sink = MemorySink::new();
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn default_creates_empty_sink() {
        let sink = MemorySink::default();
        assert!(sink.buffer.is_empty());
    }

    #[test]
    fn buffer_field_is_publicly_accessible() {
        let mut sink = MemorySink::new();
        sink.write(b"inspect me").unwrap();
        // Direct field access — confirms pub visibility
        assert_eq!(sink.buffer.len(), 10);
    }

    /// Compile-time proof that MemorySink satisfies Send + Sync.
    #[test]
    fn memory_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemorySink>();
    }

    /// Verify that a boxed Sink trait object compiles correctly with MemorySink.
    #[test]
    fn memory_sink_usable_as_boxed_sink_trait_object() {
        let mut sink: Box<dyn Sink> = Box::new(MemorySink::new());
        sink.write(b"trait object write").unwrap();
        sink.flush().unwrap();
    }
}
