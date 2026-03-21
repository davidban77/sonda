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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::sink::Sink;

    // -----------------------------------------------------------------------
    // Happy path: write and receive
    // -----------------------------------------------------------------------

    #[test]
    fn write_sends_exact_bytes_to_receiver() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"hello\n").unwrap();
        let received = rx.recv().expect("receiver should get data");
        assert_eq!(received, b"hello\n");
    }

    #[test]
    fn write_empty_slice_sends_empty_vec() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"").unwrap();
        let received = rx.recv().expect("receiver should get empty vec");
        assert!(received.is_empty());
    }

    #[test]
    fn multiple_writes_send_in_order() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"first\n").unwrap();
        sink.write(b"second\n").unwrap();
        sink.write(b"third\n").unwrap();

        assert_eq!(rx.recv().unwrap(), b"first\n");
        assert_eq!(rx.recv().unwrap(), b"second\n");
        assert_eq!(rx.recv().unwrap(), b"third\n");
    }

    #[test]
    fn flush_always_returns_ok() {
        let (tx, _rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn flush_does_not_affect_channel_contents() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"data").unwrap();
        sink.flush().unwrap();
        let received = rx.recv().unwrap();
        assert_eq!(received, b"data");
    }

    // -----------------------------------------------------------------------
    // Error case: disconnected receiver returns Err
    // -----------------------------------------------------------------------

    #[test]
    fn write_after_receiver_dropped_returns_err() {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(10);
        let mut sink = ChannelSink::new(tx);
        // Drop the receiver — channel is now disconnected.
        drop(rx);
        let result = sink.write(b"orphaned");
        assert!(
            result.is_err(),
            "write to disconnected channel should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // Backpressure: bounded(10), fast writes block once channel is full
    // -----------------------------------------------------------------------

    #[test]
    fn bounded_channel_provides_backpressure_without_oom() {
        // Channel capacity = 10. We write 20 items. The receiver drains slowly.
        // This verifies that the producer blocks (backpressure) rather than
        // allocating unbounded memory.
        let capacity = 10usize;
        let total_writes = 20usize;
        let (tx, rx) = mpsc::sync_channel(capacity);
        let mut sink = ChannelSink::new(tx);

        // Spawn a slow receiver that drains one item every 5ms.
        let receiver_handle = thread::spawn(move || {
            let mut count = 0usize;
            while count < total_writes {
                if rx.recv_timeout(Duration::from_secs(5)).is_ok() {
                    count += 1;
                    thread::sleep(Duration::from_millis(5));
                }
            }
            count
        });

        // Write all 20 items. The 11th write will block until the receiver
        // drains a slot. This must not panic or OOM — it should just block.
        for i in 0..total_writes {
            let data = format!("item-{i}\n");
            sink.write(data.as_bytes()).expect("write should succeed");
        }

        let received_count = receiver_handle
            .join()
            .expect("receiver thread should not panic");
        assert_eq!(
            received_count, total_writes,
            "receiver should get all {total_writes} items"
        );
    }

    #[test]
    fn channel_sink_write_count_matches_receive_count() {
        let (tx, rx) = mpsc::sync_channel(100);
        let mut sink = ChannelSink::new(tx);

        let n = 50usize;
        for i in 0..n {
            sink.write(format!("line {i}").as_bytes()).unwrap();
        }
        // Drop the sink to close the channel so the iterator terminates.
        drop(sink);

        let count = rx.into_iter().count();
        assert_eq!(count, n, "should receive exactly {n} items");
    }

    // -----------------------------------------------------------------------
    // Contract: Send + Sync
    // -----------------------------------------------------------------------

    #[test]
    fn channel_sink_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ChannelSink>();
    }

    #[test]
    fn channel_sink_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ChannelSink>();
    }

    #[test]
    fn channel_sink_usable_as_boxed_sink_trait_object() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink: Box<dyn Sink> = Box::new(ChannelSink::new(tx));
        sink.write(b"trait object test").unwrap();
        sink.flush().unwrap();
        let data = rx.recv().unwrap();
        assert_eq!(data, b"trait object test");
    }
}
