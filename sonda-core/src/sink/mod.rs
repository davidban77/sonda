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
/// The `type` field selects the variant: `stdout`, `file`, `tcp`, or `udp`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
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
        let yaml = "type: stdout";
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

    // ---------------------------------------------------------------------------
    // SinkConfig: internally-tagged deserialization for all variants (`type:` field)
    // ---------------------------------------------------------------------------

    #[test]
    fn sink_config_file_deserializes_with_type_field() {
        let yaml = "type: file\npath: /tmp/sonda-mod-test.txt";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::File { ref path } if path == "/tmp/sonda-mod-test.txt")
        );
    }

    #[test]
    fn sink_config_tcp_deserializes_with_type_field() {
        let yaml = "type: tcp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, SinkConfig::Tcp { ref address } if address == "127.0.0.1:9999"));
    }

    #[test]
    fn sink_config_udp_deserializes_with_type_field() {
        let yaml = "type: udp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, SinkConfig::Udp { ref address } if address == "127.0.0.1:9999"));
    }

    #[test]
    fn sink_config_unknown_type_returns_error() {
        let yaml = "type: no_such_sink";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown type tag should fail deserialization"
        );
    }

    #[test]
    fn sink_config_missing_type_field_returns_error() {
        // Without the `type` field the internally-tagged enum cannot identify the variant.
        let yaml = "stdout";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "missing type field should fail deserialization"
        );
    }

    #[test]
    fn sink_config_old_external_tag_format_is_rejected() {
        // The old externally-tagged format (`!stdout`) must no longer be accepted.
        let yaml = "!stdout";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "externally-tagged YAML format must be rejected in favour of internally-tagged"
        );
    }

    #[test]
    fn sink_config_file_requires_path_field() {
        // `type: file` without a `path` field must fail.
        let yaml = "type: file";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "file variant without path should fail deserialization"
        );
    }

    #[test]
    fn sink_config_tcp_requires_address_field() {
        let yaml = "type: tcp";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "tcp variant without address should fail deserialization"
        );
    }

    #[test]
    fn sink_config_udp_requires_address_field() {
        let yaml = "type: udp";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "udp variant without address should fail deserialization"
        );
    }

    // ---------------------------------------------------------------------------
    // SinkConfig: Send + Sync contract
    // ---------------------------------------------------------------------------

    #[test]
    fn sink_config_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SinkConfig>();
    }

    // ---------------------------------------------------------------------------
    // SinkConfig: Clone + Debug for all variants
    // ---------------------------------------------------------------------------

    #[test]
    fn sink_config_file_is_cloneable_and_debuggable() {
        let config = SinkConfig::File {
            path: "/tmp/test.txt".to_string(),
        };
        let cloned = config.clone();
        assert!(matches!(cloned, SinkConfig::File { ref path } if path == "/tmp/test.txt"));
        let s = format!("{config:?}");
        assert!(s.contains("File"));
    }

    #[test]
    fn sink_config_tcp_is_cloneable_and_debuggable() {
        let config = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
        };
        let cloned = config.clone();
        assert!(matches!(cloned, SinkConfig::Tcp { ref address } if address == "127.0.0.1:9999"));
        let s = format!("{config:?}");
        assert!(s.contains("Tcp"));
    }

    #[test]
    fn sink_config_udp_is_cloneable_and_debuggable() {
        let config = SinkConfig::Udp {
            address: "127.0.0.1:9999".to_string(),
        };
        let cloned = config.clone();
        assert!(matches!(cloned, SinkConfig::Udp { ref address } if address == "127.0.0.1:9999"));
        let s = format!("{config:?}");
        assert!(s.contains("Udp"));
    }

    // ---------------------------------------------------------------------------
    // Full scenario YAML using internally-tagged EncoderConfig and SinkConfig
    // ---------------------------------------------------------------------------

    #[test]
    fn scenario_yaml_with_tcp_sink_deserializes_correctly() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: test_metric
rate: 100.0
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: tcp
  address: "127.0.0.1:4321"
"#;
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "test_metric");
        assert!(matches!(
            config.encoder,
            crate::encoder::EncoderConfig::PrometheusText
        ));
        assert!(
            matches!(config.sink, SinkConfig::Tcp { ref address } if address == "127.0.0.1:4321")
        );
    }

    #[test]
    fn scenario_yaml_with_file_sink_and_json_encoder_deserializes_correctly() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: file_json_test
rate: 10.0
generator:
  type: constant
  value: 42.0
encoder:
  type: json_lines
sink:
  type: file
  path: /tmp/sonda-file-json-test.txt
"#;
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config.encoder,
            crate::encoder::EncoderConfig::JsonLines
        ));
        assert!(
            matches!(config.sink, SinkConfig::File { ref path } if path == "/tmp/sonda-file-json-test.txt")
        );
    }

    #[test]
    fn scenario_yaml_with_udp_sink_and_influx_encoder_deserializes_correctly() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: udp_influx_test
rate: 50.0
generator:
  type: constant
  value: 0.0
encoder:
  type: influx_lp
  field_key: "bytes"
sink:
  type: udp
  address: "127.0.0.1:5555"
"#;
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config.encoder,
            crate::encoder::EncoderConfig::InfluxLineProtocol { field_key: Some(ref k) } if k == "bytes"
        ));
        assert!(
            matches!(config.sink, SinkConfig::Udp { ref address } if address == "127.0.0.1:5555")
        );
    }
}
