//! Sinks deliver encoded byte buffers to their destination.
//!
//! All sinks implement the `Sink` trait.

pub mod memory;
pub mod stdout;

use serde::Deserialize;

use crate::SondaError;

/// A sink consumes encoded bytes and delivers them to a destination.
pub trait Sink: Send + Sync {
    /// Write encoded event data to the sink.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError>;

    /// Flush any buffered data to the destination.
    fn flush(&mut self) -> Result<(), SondaError>;
}

/// Configuration selecting which sink to use for a scenario.
///
/// This enum is serde-deserializable from YAML scenario files.
#[derive(Debug, Clone, Deserialize)]
pub enum SinkConfig {
    /// Write encoded events to stdout, buffered via [`BufWriter`](std::io::BufWriter).
    #[serde(rename = "stdout")]
    Stdout,
}

/// Create a boxed [`Sink`] from the given [`SinkConfig`].
pub fn create_sink(config: &SinkConfig) -> Result<Box<dyn Sink>, SondaError> {
    match config {
        SinkConfig::Stdout => Ok(Box::new(stdout::StdoutSink::new())),
    }
}
