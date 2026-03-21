//! Sinks deliver encoded byte buffers to their destination.
//!
//! All sinks implement the `Sink` trait.

pub mod file;
pub mod memory;
pub mod stdout;
pub mod tcp;
pub mod udp;

use std::path::Path;

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

    /// Write encoded events to a file at the given path.
    ///
    /// Parent directories are created automatically if they do not exist.
    #[serde(rename = "file")]
    File {
        /// Filesystem path to write encoded events to.
        path: String,
    },

    /// Write encoded events over a persistent TCP connection.
    ///
    /// The sink connects on construction and buffers writes via [`BufWriter`](std::io::BufWriter).
    #[serde(rename = "tcp")]
    Tcp {
        /// Remote address to connect to, e.g. `"127.0.0.1:9999"`.
        address: String,
    },

    /// Send each encoded event as a single UDP datagram.
    ///
    /// No connection is established; an ephemeral local port is bound and each
    /// call to `write` sends one `send_to` datagram.
    #[serde(rename = "udp")]
    Udp {
        /// Remote address to send datagrams to, e.g. `"127.0.0.1:9999"`.
        address: String,
    },
}

/// Create a boxed [`Sink`] from the given [`SinkConfig`].
pub fn create_sink(config: &SinkConfig) -> Result<Box<dyn Sink>, SondaError> {
    match config {
        SinkConfig::Stdout => Ok(Box::new(stdout::StdoutSink::new())),
        SinkConfig::File { path } => Ok(Box::new(file::FileSink::new(Path::new(path))?)),
        SinkConfig::Tcp { address } => Ok(Box::new(tcp::TcpSink::new(address)?)),
        SinkConfig::Udp { address } => Ok(Box::new(udp::UdpSink::new(address)?)),
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
