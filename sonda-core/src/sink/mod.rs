//! Sinks deliver encoded byte buffers to their destination.
//!
//! All sinks implement the `Sink` trait.

// pub mod stdout;  // TODO: Phase 0 MVP

/// A sink consumes encoded bytes and delivers them to a destination.
pub trait Sink: Send + Sync {
    /// Write encoded event data to the sink.
    fn write(&mut self, data: &[u8]) -> Result<(), crate::SondaError>;

    /// Flush any buffered data to the destination.
    fn flush(&mut self) -> Result<(), crate::SondaError>;
}
