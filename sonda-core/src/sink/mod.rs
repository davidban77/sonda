//! Sinks deliver encoded byte buffers to their destination.
//!
//! All sinks implement the `Sink` trait.

pub mod channel;
pub mod file;
pub mod http;
#[cfg(feature = "kafka")]
pub mod kafka;
pub mod loki;
pub mod memory;
#[cfg(feature = "remote-write")]
pub mod remote_write;
pub mod stdout;
pub mod tcp;
pub mod udp;

use std::collections::HashMap;
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

    /// Batch encoded events and deliver them via HTTP POST.
    ///
    /// Bytes are accumulated in a buffer until `batch_size` bytes are reached,
    /// then flushed as a single POST request. The `flush()` method sends any
    /// remaining buffered data.
    #[serde(rename = "http_push")]
    HttpPush {
        /// Target URL for HTTP POST requests, e.g. `"http://localhost:9090/api/v1/write"`.
        url: String,

        /// Optional `Content-Type` header value. Defaults to
        /// `"application/octet-stream"` if not specified.
        content_type: Option<String>,

        /// Optional flush threshold in bytes. Defaults to 64 KiB if not specified.
        batch_size: Option<usize>,

        /// Optional extra HTTP headers to send with every POST request.
        ///
        /// When provided, these headers are sent in addition to the `Content-Type`
        /// header. Useful for protocols that require specific headers, such as
        /// Prometheus remote write (`Content-Encoding: snappy`,
        /// `X-Prometheus-Remote-Write-Version: 0.1.0`).
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },

    /// Batch TimeSeries and deliver them as Prometheus remote write requests.
    ///
    /// This sink is designed to be paired with the `remote_write` encoder, which
    /// produces length-prefixed protobuf `TimeSeries` bytes. The sink accumulates
    /// TimeSeries entries and, on flush or when `batch_size` is reached, wraps them
    /// in a single `WriteRequest`, prost-encodes, snappy-compresses, and HTTP POSTs
    /// with the correct remote write protocol headers.
    ///
    /// Requires the `remote-write` Cargo feature to be enabled.
    #[cfg(feature = "remote-write")]
    #[serde(rename = "remote_write")]
    RemoteWrite {
        /// Target URL for the remote write endpoint, e.g.
        /// `"http://localhost:8428/api/v1/write"`.
        url: String,

        /// Flush threshold in number of TimeSeries entries. Defaults to 100 if
        /// not specified.
        #[serde(default)]
        batch_size: Option<usize>,
    },

    /// Batch encoded events and deliver them to a Kafka topic.
    ///
    /// Uses [`rskafka`](https://crates.io/crates/rskafka) — a pure-Rust Kafka
    /// client with no C dependencies — for musl-compatible static linking.
    ///
    /// Bytes are accumulated in an internal buffer. When the buffer reaches
    /// 64 KiB, or when `flush()` is called explicitly, the buffer is published
    /// as a single Kafka record to partition 0 of the configured topic.
    ///
    /// Requires the `kafka` Cargo feature to be enabled.
    #[cfg(feature = "kafka")]
    #[serde(rename = "kafka")]
    Kafka {
        /// Comma-separated list of broker `host:port` addresses,
        /// e.g. `"127.0.0.1:9092"` or `"broker1:9092,broker2:9092"`.
        brokers: String,

        /// The Kafka topic name to produce records to.
        topic: String,
    },

    /// Batch encoded log lines and deliver them to Grafana Loki via HTTP POST.
    ///
    /// Each call to `write()` appends one log line to the batch. When the batch
    /// reaches `batch_size` entries, it is automatically flushed as a single POST
    /// to `{url}/loki/api/v1/push`. Call `flush()` at shutdown to send any
    /// remaining buffered entries.
    ///
    /// Stream labels are sourced from the scenario-level `labels` configuration
    /// and passed to [`create_sink()`] via the `labels` parameter, keeping label
    /// config consistent with all other signal types.
    #[serde(rename = "loki")]
    Loki {
        /// Base URL of the Loki instance, e.g. `"http://localhost:3100"`.
        url: String,

        /// Flush threshold in log entries. Defaults to `100` if not specified.
        #[serde(default)]
        batch_size: Option<usize>,
    },
}

/// Create a boxed [`Sink`] from the given [`SinkConfig`].
///
/// The optional `labels` parameter is used only by the Loki sink to set stream
/// labels. For all other sink types, pass `None`. Log scenarios pass the
/// scenario-level labels here so that Loki stream labels are configured at the
/// same level as every other signal type.
pub fn create_sink(
    config: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<Box<dyn Sink>, SondaError> {
    match config {
        SinkConfig::Stdout => Ok(Box::new(stdout::StdoutSink::new())),
        SinkConfig::File { path } => Ok(Box::new(file::FileSink::new(Path::new(path))?)),
        SinkConfig::Tcp { address } => Ok(Box::new(tcp::TcpSink::new(address)?)),
        SinkConfig::Udp { address } => Ok(Box::new(udp::UdpSink::new(address)?)),
        SinkConfig::HttpPush {
            url,
            content_type,
            batch_size,
            headers,
        } => {
            let ct = content_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let bs = batch_size.unwrap_or(http::DEFAULT_BATCH_SIZE);
            let h = headers.clone().unwrap_or_default();
            Ok(Box::new(http::HttpPushSink::new(url, ct, bs, h)?))
        }
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, batch_size } => {
            let bs = batch_size.unwrap_or(remote_write::DEFAULT_BATCH_SIZE);
            Ok(Box::new(remote_write::RemoteWriteSink::new(url, bs)?))
        }
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { brokers, topic } => {
            Ok(Box::new(kafka::KafkaSink::new(brokers, topic)?))
        }
        SinkConfig::Loki { url, batch_size } => {
            let bs = batch_size.unwrap_or(100);
            let loki_labels = labels.cloned().unwrap_or_default();
            Ok(Box::new(loki::LokiSink::new(url.clone(), loki_labels, bs)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_sink_stdout_returns_ok() {
        let result = create_sink(&SinkConfig::Stdout, None);
        assert!(result.is_ok());
    }

    #[test]
    fn create_sink_stdout_write_and_flush_succeed() {
        let mut sink = create_sink(&SinkConfig::Stdout, None).unwrap();
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
        assert!(create_sink(&config, None).is_ok());
        assert!(create_sink(&cloned, None).is_ok());
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
            crate::encoder::EncoderConfig::PrometheusText { .. }
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
            crate::encoder::EncoderConfig::JsonLines { .. }
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
            crate::encoder::EncoderConfig::InfluxLineProtocol { field_key: Some(ref k), .. } if k == "bytes"
        ));
        assert!(
            matches!(config.sink, SinkConfig::Udp { ref address } if address == "127.0.0.1:5555")
        );
    }

    // -----------------------------------------------------------------------
    // SinkConfig::Kafka deserialization and factory wiring
    // -----------------------------------------------------------------------

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_config_kafka_deserializes_with_type_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::Kafka { ref brokers, ref topic }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_config_kafka_requires_brokers_field() {
        let yaml = "type: kafka\ntopic: sonda-test";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without brokers should fail deserialization"
        );
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_config_kafka_requires_topic_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without topic should fail deserialization"
        );
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_config_kafka_is_cloneable_and_debuggable() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
        };
        let cloned = config.clone();
        assert!(
            matches!(cloned, SinkConfig::Kafka { ref brokers, ref topic }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
        let s = format!("{config:?}");
        assert!(s.contains("Kafka"));
    }

    /// create_sink with an unreachable broker returns Err (not a panic).
    /// This verifies the factory arm for Kafka is wired correctly and that
    /// construction failures surface as SondaError rather than unwrap panics.
    ///
    /// Ignored by default because rskafka may wait for a long TCP timeout
    /// before returning an error. Run with `cargo test -- --ignored` when the
    /// test environment can tolerate network delays.
    #[cfg(feature = "kafka")]
    #[test]
    #[ignore = "requires network timeout which is slow; run with --ignored when desired"]
    fn create_sink_kafka_with_unreachable_broker_returns_err() {
        // Port 1 is privileged and will always refuse connections.
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:1".to_string(),
            topic: "sonda-test".to_string(),
        };
        let result = create_sink(&config, None);
        assert!(
            result.is_err(),
            "create_sink should propagate the broker connection failure"
        );
    }

    /// create_sink with an empty broker string returns Err immediately.
    #[cfg(feature = "kafka")]
    #[test]
    fn create_sink_kafka_with_empty_broker_returns_err() {
        let config = SinkConfig::Kafka {
            brokers: String::new(),
            topic: "sonda-test".to_string(),
        };
        let result = create_sink(&config, None);
        assert!(
            result.is_err(),
            "create_sink should reject an empty broker string"
        );
    }

    // ---------------------------------------------------------------------------
    // SinkConfig::HttpPush with custom headers deserialization
    // ---------------------------------------------------------------------------

    #[test]
    fn sink_config_http_push_with_headers_deserializes() {
        let yaml = r#"
type: http_push
url: "http://localhost:8428/api/v1/write"
headers:
  Content-Type: "application/x-protobuf"
  Content-Encoding: "snappy"
  X-Prometheus-Remote-Write-Version: "0.1.0"
"#;
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush { url, headers, .. } => {
                assert_eq!(url, "http://localhost:8428/api/v1/write");
                let hdr = headers.expect("headers should be Some");
                assert_eq!(
                    hdr.get("Content-Type").map(String::as_str),
                    Some("application/x-protobuf")
                );
                assert_eq!(
                    hdr.get("Content-Encoding").map(String::as_str),
                    Some("snappy")
                );
                assert_eq!(
                    hdr.get("X-Prometheus-Remote-Write-Version")
                        .map(String::as_str),
                    Some("0.1.0")
                );
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_http_push_without_headers_is_backward_compatible() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
content_type: "text/plain"
"#;
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush {
                url,
                headers,
                content_type,
                ..
            } => {
                assert_eq!(url, "http://localhost:9090/push");
                assert_eq!(content_type.as_deref(), Some("text/plain"));
                assert!(
                    headers.is_none(),
                    "headers should default to None when not specified"
                );
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_http_push_with_empty_headers_map_deserializes() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
headers: {}
"#;
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush { headers, .. } => {
                let hdr = headers.expect("headers should be Some even when empty");
                assert!(
                    hdr.is_empty(),
                    "empty headers map should deserialize as empty HashMap"
                );
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_http_push_with_headers_is_cloneable_and_debuggable() {
        let mut hdr = HashMap::new();
        hdr.insert("X-Custom".to_string(), "val".to_string());
        let config = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            headers: Some(hdr),
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("HttpPush"));
        assert!(debug_str.contains("X-Custom"));
    }
}
