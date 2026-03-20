//! Buffered stdout sink.

use std::io::{BufWriter, Stdout, Write};

use super::Sink;
use crate::SondaError;

/// A sink that writes encoded event data to stdout using a buffered writer.
///
/// Wraps `std::io::stdout()` in a [`BufWriter`] to avoid issuing a syscall for
/// every event. Call [`flush`](StdoutSink::flush) to force any buffered data to
/// be written before the program exits.
pub struct StdoutSink {
    writer: BufWriter<Stdout>,
}

impl StdoutSink {
    /// Create a new `StdoutSink` backed by buffered stdout.
    pub fn new() -> Self {
        Self {
            writer: BufWriter::new(std::io::stdout()),
        }
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for StdoutSink {
    /// Write `data` to the buffered stdout writer.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer.write_all(data)?;
        Ok(())
    }

    /// Flush any buffered bytes to stdout.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_sink_constructs_without_panicking() {
        let _sink = StdoutSink::new();
    }

    #[test]
    fn stdout_sink_default_constructs_without_panicking() {
        let _sink = StdoutSink::default();
    }

    #[test]
    fn write_and_flush_do_not_error() {
        let mut sink = StdoutSink::new();
        // Writing empty bytes is a valid no-op that still exercises the code path.
        let write_result = sink.write(b"");
        assert!(write_result.is_ok());
        let flush_result = sink.flush();
        assert!(flush_result.is_ok());
    }

    #[test]
    fn write_non_empty_data_does_not_error() {
        let mut sink = StdoutSink::new();
        let result = sink.write(b"up{} 1 1700000000000\n");
        assert!(result.is_ok());
        // Flush to ensure buffered data is pushed through
        assert!(sink.flush().is_ok());
    }

    /// Compile-time proof that StdoutSink satisfies Send + Sync.
    #[test]
    fn stdout_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StdoutSink>();
    }
}
