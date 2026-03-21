//! TCP sink — delivers encoded telemetry over a persistent TCP connection.

use std::io::{BufWriter, Write};
use std::net::TcpStream;

use crate::sink::Sink;
use crate::SondaError;

/// Delivers encoded telemetry data over a TCP connection.
///
/// The underlying [`TcpStream`] is wrapped in a [`BufWriter`] to batch
/// writes and reduce syscall overhead.
pub struct TcpSink {
    writer: BufWriter<TcpStream>,
    /// Target address kept for error messages.
    addr: String,
}

impl TcpSink {
    /// Connect to `addr` and create a new [`TcpSink`].
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the connection cannot be established
    /// (e.g., connection refused, invalid address).
    pub fn new(addr: &str) -> Result<Self, SondaError> {
        let stream = TcpStream::connect(addr)
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP connect to {addr}: {e}")))?;
        Ok(Self {
            writer: BufWriter::new(stream),
            addr: addr.to_owned(),
        })
    }
}

impl Sink for TcpSink {
    /// Write `data` to the buffered TCP stream.
    ///
    /// The buffer is flushed automatically by the OS or on an explicit
    /// call to [`flush`](TcpSink::flush).
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer.write_all(data).map_err(|e| {
            std::io::Error::new(e.kind(), format!("TCP write to {}: {e}", self.addr))
        })?;
        Ok(())
    }

    /// Flush buffered data to the TCP stream.
    ///
    /// Should be called at shutdown or after each logical batch to ensure
    /// in-flight data is delivered.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.writer.flush().map_err(|e| {
            std::io::Error::new(e.kind(), format!("TCP flush to {}: {e}", self.addr))
        })?;
        Ok(())
    }
}
