//! In-memory sink for testing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::Sink;
use crate::SondaError;

/// Ring of most-recent captured `(Instant, Vec<u8>)` events with a fixed cap.
#[derive(Debug)]
pub struct CapturedRing {
    events: VecDeque<(Instant, Vec<u8>)>,
    max: usize,
}

impl CapturedRing {
    /// Create an empty ring that retains at most `max` events.
    pub fn new(max: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(max),
            max: max.max(1),
        }
    }

    /// Append `event`, evicting the oldest entry once the cap is reached.
    pub fn push(&mut self, event: (Instant, Vec<u8>)) {
        if self.events.len() == self.max {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Borrow the retained events in insertion order.
    pub fn events(&self) -> &VecDeque<(Instant, Vec<u8>)> {
        &self.events
    }

    /// Number of retained events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the ring currently holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Configured cap on retained events.
    pub fn capacity(&self) -> usize {
        self.max
    }
}

/// An in-memory sink that accumulates all written bytes in a `Vec<u8>`.
///
/// This sink is intended for use in tests across the project. It allows
/// callers to inspect the exact bytes that would have been delivered to a
/// real destination (file, socket, stdout) without any I/O.
pub struct MemorySink {
    /// All bytes written to this sink, in order.
    pub buffer: Vec<u8>,
    captured: Option<Arc<Mutex<CapturedRing>>>,
}

impl MemorySink {
    /// Create a new, empty `MemorySink` with no per-write timestamp capture.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            captured: None,
        }
    }

    /// Create a `MemorySink` that records `(Instant, bytes)` on every write,
    /// keeping at most `max_events` of the most recent entries.
    pub fn with_capture(max_events: usize) -> Self {
        Self {
            buffer: Vec::new(),
            captured: Some(Arc::new(Mutex::new(CapturedRing::new(max_events)))),
        }
    }

    /// Create a `MemorySink` whose timestamped writes land in `handle`.
    pub fn with_shared_capture(handle: Arc<Mutex<CapturedRing>>) -> Self {
        Self {
            buffer: Vec::new(),
            captured: Some(handle),
        }
    }

    /// Borrow a clone of the shared capture handle, if capture is enabled.
    pub fn capture_handle(&self) -> Option<Arc<Mutex<CapturedRing>>> {
        self.captured.as_ref().map(Arc::clone)
    }

    /// Snapshot the currently retained capture ring, if capture is enabled.
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

impl Sink for MemorySink {
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buffer.extend_from_slice(data);
        if let Some(handle) = self.captured.as_ref() {
            if let Ok(mut ring) = handle.lock() {
                ring.push((Instant::now(), data.to_vec()));
            }
        }
        Ok(())
    }

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

    #[test]
    fn new_disables_capture() {
        let mut sink = MemorySink::new();
        sink.write(b"x").unwrap();
        assert!(sink.captured().is_none());
        assert!(sink.capture_handle().is_none());
    }

    #[test]
    fn with_capture_records_timestamped_writes() {
        let mut sink = MemorySink::with_capture(8);
        sink.write(b"first").unwrap();
        sink.write(b"second").unwrap();
        let snap = sink.captured().expect("capture enabled");
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].1, b"first");
        assert_eq!(snap[1].1, b"second");
        assert!(snap[1].0 >= snap[0].0);
    }

    #[test]
    fn capture_ring_evicts_oldest_when_full() {
        let mut sink = MemorySink::with_capture(3);
        for i in 0..5 {
            sink.write(format!("e{i}").as_bytes()).unwrap();
        }
        let snap = sink.captured().expect("capture enabled");
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].1, b"e2");
        assert_eq!(snap[1].1, b"e3");
        assert_eq!(snap[2].1, b"e4");
    }

    #[test]
    fn capture_handle_observes_writes_across_clones() {
        let sink = MemorySink::with_capture(4);
        let handle = sink.capture_handle().expect("capture enabled");
        let mut sink = sink;
        sink.write(b"a").unwrap();
        sink.write(b"b").unwrap();
        let guard = handle.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard.events()[0].1, b"a");
        assert_eq!(guard.events()[1].1, b"b");
    }

    #[test]
    fn with_shared_capture_writes_through_external_handle() {
        let handle = Arc::new(Mutex::new(CapturedRing::new(4)));
        let mut sink = MemorySink::with_shared_capture(Arc::clone(&handle));
        sink.write(b"shared").unwrap();
        let guard = handle.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard.events()[0].1, b"shared");
    }

    #[test]
    fn buffer_remains_populated_alongside_capture() {
        let mut sink = MemorySink::with_capture(4);
        sink.write(b"hello").unwrap();
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
