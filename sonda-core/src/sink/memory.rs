//! In-memory sink for testing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use super::Sink;
use crate::SondaError;

/// Ring of most-recent captured `(Instant, Vec<u8>)` events with a fixed cap.
#[derive(Debug)]
pub struct CapturedRing {
    events: VecDeque<(Instant, Vec<u8>)>,
    max: usize,
}

impl CapturedRing {
    pub fn new(max: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(max),
            max: max.max(1),
        }
    }

    pub fn push(&mut self, event: (Instant, Vec<u8>)) {
        if self.events.len() == self.max {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn events(&self) -> &VecDeque<(Instant, Vec<u8>)> {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.max
    }
}

/// An in-memory sink that accumulates all written bytes in a `Vec<u8>`.
///
/// Intended for tests across the project: callers can inspect the exact bytes
/// that would have been delivered to a real destination without any I/O.
pub struct MemorySink {
    pub buffer: Vec<u8>,
    captured: Option<Arc<Mutex<CapturedRing>>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            captured: None,
        }
    }

    /// Record `(Instant, bytes)` on every write, keeping the most recent
    /// `max_events` entries.
    pub fn with_capture(max_events: usize) -> Self {
        Self {
            buffer: Vec::new(),
            captured: Some(Arc::new(Mutex::new(CapturedRing::new(max_events)))),
        }
    }

    /// Mirror writes into an externally-owned capture ring.
    pub fn with_shared_capture(handle: Arc<Mutex<CapturedRing>>) -> Self {
        Self {
            buffer: Vec::new(),
            captured: Some(handle),
        }
    }

    pub fn capture_handle(&self) -> Option<Arc<Mutex<CapturedRing>>> {
        self.captured.as_ref().map(Arc::clone)
    }

    pub fn captured(&self) -> Option<Vec<(Instant, Vec<u8>)>> {
        let handle = self.captured.as_ref()?;
        let guard = handle.lock().ok()?;
        Some(guard.events().iter().cloned().collect())
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Sink for MemorySink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buffer.extend_from_slice(data);
        if let Some(handle) = self.captured.as_ref() {
            if let Ok(mut ring) = handle.lock() {
                ring.push((Instant::now(), data.to_vec()));
            }
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_stores_exact_bytes_in_buffer() {
        let mut sink = MemorySink::new();
        let data = b"hello, world\n";
        sink.write(data).await.unwrap();
        assert_eq!(sink.buffer, data);
    }

    #[tokio::test]
    async fn write_empty_slice_appends_nothing() {
        let mut sink = MemorySink::new();
        sink.write(b"").await.unwrap();
        assert!(sink.buffer.is_empty());
    }

    #[tokio::test]
    async fn multiple_writes_accumulate_in_order() {
        let mut sink = MemorySink::new();
        sink.write(b"foo").await.unwrap();
        sink.write(b"bar").await.unwrap();
        sink.write(b"baz").await.unwrap();
        assert_eq!(&sink.buffer, b"foobarbaz");
    }

    #[tokio::test]
    async fn flush_is_noop_and_returns_ok() {
        let mut sink = MemorySink::new();
        sink.write(b"data").await.unwrap();
        let result = sink.flush().await;
        assert!(result.is_ok());
        assert_eq!(&sink.buffer, b"data");
    }

    #[tokio::test]
    async fn flush_on_empty_sink_returns_ok() {
        let mut sink = MemorySink::new();
        assert!(sink.flush().await.is_ok());
    }

    #[test]
    fn default_creates_empty_sink() {
        let sink = MemorySink::default();
        assert!(sink.buffer.is_empty());
    }

    #[tokio::test]
    async fn buffer_field_is_publicly_accessible() {
        let mut sink = MemorySink::new();
        sink.write(b"inspect me").await.unwrap();
        assert_eq!(sink.buffer.len(), 10);
    }

    #[test]
    fn memory_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemorySink>();
    }

    #[tokio::test]
    async fn memory_sink_usable_as_boxed_sink_trait_object() {
        let mut sink: Box<dyn Sink> = Box::new(MemorySink::new());
        sink.write(b"trait object write").await.unwrap();
        sink.flush().await.unwrap();
    }

    #[tokio::test]
    async fn new_disables_capture() {
        let mut sink = MemorySink::new();
        sink.write(b"x").await.unwrap();
        assert!(sink.captured().is_none());
        assert!(sink.capture_handle().is_none());
    }

    #[tokio::test]
    async fn with_capture_records_timestamped_writes() {
        let mut sink = MemorySink::with_capture(8);
        sink.write(b"first").await.unwrap();
        sink.write(b"second").await.unwrap();
        let snap = sink.captured().expect("capture enabled");
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].1, b"first");
        assert_eq!(snap[1].1, b"second");
        assert!(snap[1].0 >= snap[0].0);
    }

    #[tokio::test]
    async fn capture_ring_evicts_oldest_when_full() {
        let mut sink = MemorySink::with_capture(3);
        for i in 0..5 {
            sink.write(format!("e{i}").as_bytes()).await.unwrap();
        }
        let snap = sink.captured().expect("capture enabled");
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].1, b"e2");
        assert_eq!(snap[1].1, b"e3");
        assert_eq!(snap[2].1, b"e4");
    }

    #[tokio::test]
    async fn capture_handle_observes_writes_across_clones() {
        let sink = MemorySink::with_capture(4);
        let handle = sink.capture_handle().expect("capture enabled");
        let mut sink = sink;
        sink.write(b"a").await.unwrap();
        sink.write(b"b").await.unwrap();
        let guard = handle.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard.events()[0].1, b"a");
        assert_eq!(guard.events()[1].1, b"b");
    }

    #[tokio::test]
    async fn with_shared_capture_writes_through_external_handle() {
        let handle = Arc::new(Mutex::new(CapturedRing::new(4)));
        let mut sink = MemorySink::with_shared_capture(Arc::clone(&handle));
        sink.write(b"shared").await.unwrap();
        let guard = handle.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard.events()[0].1, b"shared");
    }

    #[tokio::test]
    async fn buffer_remains_populated_alongside_capture() {
        let mut sink = MemorySink::with_capture(4);
        sink.write(b"hello").await.unwrap();
        assert_eq!(&sink.buffer, b"hello");
        assert_eq!(sink.captured().unwrap().len(), 1);
    }

    #[test]
    fn captured_ring_capacity_minimum_is_one() {
        let ring = CapturedRing::new(0);
        assert_eq!(ring.capacity(), 1);
        assert!(ring.is_empty());
    }

    #[test]
    fn captured_ring_len_and_is_empty_track_state() {
        let mut ring = CapturedRing::new(2);
        assert!(ring.is_empty());
        ring.push((Instant::now(), vec![1]));
        assert_eq!(ring.len(), 1);
        assert!(!ring.is_empty());
    }
}
