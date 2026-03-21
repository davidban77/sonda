//! Channel-based sink for forwarding encoded data to a shared receiver.
//!
//! `ChannelSink` sends encoded byte buffers over a bounded [`SyncSender`]
//! channel. A separate receiver thread owns the actual destination sink and
//! drains the channel. The bounded capacity provides backpressure: when the
//! receiver cannot keep up, `write()` blocks rather than growing memory
//! without bound.

use std::sync::mpsc::SyncSender;

use crate::sink::Sink;
use crate::SondaError;

/// A sink that forwards encoded data through a bounded `mpsc` channel.
///
/// Each call to [`write`](ChannelSink::write) sends a copy of `data` over the
/// channel as a `Vec<u8>`. The receiver on the other end is responsible for
/// writing the data to the actual destination.
///
/// # Backpressure
///
/// The underlying channel is created with a fixed capacity. When the channel
/// is full, `write()` blocks until the receiver drains at least one slot. This
/// prevents unbounded memory growth when the consumer cannot keep up with the
/// producer.
///
/// # Example
///
/// ```rust
/// use std::sync::mpsc;
/// use sonda_core::sink::{Sink, channel::ChannelSink};
///
/// let (tx, rx) = mpsc::sync_channel(10);
/// let mut sink = ChannelSink::new(tx);
/// sink.write(b"hello\n").unwrap();
/// let data = rx.recv().unwrap();
/// assert_eq!(data, b"hello\n");
/// ```
pub struct ChannelSink {
    tx: SyncSender<Vec<u8>>,
}

impl ChannelSink {
    /// Create a new `ChannelSink` that sends encoded data over `tx`.
    ///
    /// The backing channel must be created with [`mpsc::sync_channel`](std::sync::mpsc::sync_channel)
    /// to enforce bounded capacity.
    pub fn new(tx: SyncSender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

impl Sink for ChannelSink {
    /// Send a copy of `data` over the channel.
    ///
    /// Blocks when the channel is at capacity until the receiver drains a slot.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the receiver has been dropped and the
    /// channel is disconnected.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.tx
            .send(data.to_vec())
            .map_err(|e| SondaError::Sink(std::io::Error::other(e.to_string())))
    }

    /// No-op flush: channel delivery is synchronous per `send` call.
    ///
    /// Returns `Ok(())` unconditionally because the channel has no internal
    /// buffer beyond what the receiver has not yet consumed.
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}
