//! Channel-based sink for forwarding encoded data to a shared receiver.
//!
//! `ChannelSink` sends encoded byte buffers over a bounded [`SyncSender`]
//! channel. A separate receiver thread owns the actual destination sink and
//! drains the channel. The bounded capacity provides backpressure: when the
//! receiver cannot keep up, `write()` blocks rather than growing memory
//! without bound.

use std::sync::mpsc::SyncSender;

use async_trait::async_trait;

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
/// ```rust,no_run
/// use std::sync::mpsc;
/// use sonda_core::sink::{Sink, channel::ChannelSink};
///
/// # async fn doc() {
/// let (tx, rx) = mpsc::sync_channel(10);
/// let mut sink = ChannelSink::new(tx);
/// sink.write(b"hello\n").await.unwrap();
/// let data = rx.recv().unwrap();
/// assert_eq!(data, b"hello\n");
/// # }
/// ```
pub struct ChannelSink {
    tx: SyncSender<Vec<u8>>,
}

impl ChannelSink {
    pub fn new(tx: SyncSender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Sink for ChannelSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.tx
            .send(data.to_vec())
            .map_err(|e| SondaError::Sink(std::io::Error::other(e.to_string())))
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
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

    #[tokio::test]
    async fn write_sends_exact_bytes_to_receiver() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"hello\n").await.unwrap();
        let received = rx.recv().expect("receiver should get data");
        assert_eq!(received, b"hello\n");
    }

    #[tokio::test]
    async fn write_empty_slice_sends_empty_vec() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"").await.unwrap();
        let received = rx.recv().expect("receiver should get empty vec");
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn multiple_writes_send_in_order() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"first\n").await.unwrap();
        sink.write(b"second\n").await.unwrap();
        sink.write(b"third\n").await.unwrap();

        assert_eq!(rx.recv().unwrap(), b"first\n");
        assert_eq!(rx.recv().unwrap(), b"second\n");
        assert_eq!(rx.recv().unwrap(), b"third\n");
    }

    #[tokio::test]
    async fn flush_always_returns_ok() {
        let (tx, _rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        assert!(sink.flush().await.is_ok());
    }

    #[tokio::test]
    async fn flush_does_not_affect_channel_contents() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink = ChannelSink::new(tx);
        sink.write(b"data").await.unwrap();
        sink.flush().await.unwrap();
        let received = rx.recv().unwrap();
        assert_eq!(received, b"data");
    }

    #[tokio::test]
    async fn write_after_receiver_dropped_returns_err() {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(10);
        let mut sink = ChannelSink::new(tx);
        drop(rx);
        let result = sink.write(b"orphaned").await;
        assert!(
            result.is_err(),
            "write to disconnected channel should return Err"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bounded_channel_provides_backpressure_without_oom() {
        let capacity = 10usize;
        let total_writes = 20usize;
        let (tx, rx) = mpsc::sync_channel(capacity);
        let mut sink = ChannelSink::new(tx);

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

        for i in 0..total_writes {
            let data = format!("item-{i}\n");
            sink.write(data.as_bytes())
                .await
                .expect("write should succeed");
        }

        let received_count = receiver_handle
            .join()
            .expect("receiver thread should not panic");
        assert_eq!(
            received_count, total_writes,
            "receiver should get all {total_writes} items"
        );
    }

    #[tokio::test]
    async fn channel_sink_write_count_matches_receive_count() {
        let (tx, rx) = mpsc::sync_channel(100);
        let mut sink = ChannelSink::new(tx);

        let n = 50usize;
        for i in 0..n {
            sink.write(format!("line {i}").as_bytes()).await.unwrap();
        }
        drop(sink);

        let count = rx.into_iter().count();
        assert_eq!(count, n, "should receive exactly {n} items");
    }

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

    #[tokio::test]
    async fn channel_sink_usable_as_boxed_sink_trait_object() {
        let (tx, rx) = mpsc::sync_channel(10);
        let mut sink: Box<dyn Sink> = Box::new(ChannelSink::new(tx));
        sink.write(b"trait object test").await.unwrap();
        sink.flush().await.unwrap();
        let data = rx.recv().unwrap();
        assert_eq!(data, b"trait object test");
    }
}
