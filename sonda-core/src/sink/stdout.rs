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
