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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_sink_stdout_returns_ok() {
        let result = create_sink(&SinkConfig::Stdout);
        assert!(result.is_ok());
    }

    #[test]
    fn create_sink_stdout_write_and_flush_succeed() {
        let mut sink = create_sink(&SinkConfig::Stdout).unwrap();
        assert!(sink.write(b"").is_ok());
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn sink_config_stdout_deserializes_from_yaml() {
        let yaml = "stdout";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, SinkConfig::Stdout));
    }

    #[test]
    fn sink_config_is_cloneable() {
        let config = SinkConfig::Stdout;
        let cloned = config.clone();
        // Both variants should produce valid sinks
        assert!(create_sink(&config).is_ok());
        assert!(create_sink(&cloned).is_ok());
    }

    #[test]
    fn sink_config_is_debuggable() {
        let config = SinkConfig::Stdout;
        let s = format!("{config:?}");
        assert!(s.contains("Stdout"));
    }
}
