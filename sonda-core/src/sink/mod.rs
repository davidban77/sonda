//! Sinks deliver encoded byte buffers to their destination.
//!
//! All sinks implement the `Sink` trait.

pub mod channel;
pub mod file;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "http")]
pub mod loki;
pub mod memory;
#[cfg(feature = "otlp")]
pub mod otlp_grpc;
#[cfg(feature = "remote-write")]
pub mod remote_write;
pub mod retry;
pub mod stdout;
pub mod tcp;
pub mod udp;

use std::collections::HashMap;
use std::path::Path;

use crate::SondaError;

/// A sink consumes encoded bytes and delivers them to a destination.
pub trait Sink: Send + Sync {
    /// Write encoded event data to the sink.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError>;

    /// Flush any buffered data to the destination.
    fn flush(&mut self) -> Result<(), SondaError>;
}

/// TLS configuration for Kafka broker connections.
///
/// When `enabled` is `true`, the Kafka sink connects to brokers over TLS.
/// An optional `ca_cert` path can be provided to trust a custom or self-signed
/// CA certificate. When `ca_cert` is omitted, Mozilla's bundled root
/// certificates are used via [`webpki_roots`].
#[cfg(feature = "kafka")]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub struct KafkaTlsConfig {
    /// Enable TLS for broker connections. Default: `false`.
    #[cfg_attr(feature = "config", serde(default))]
    pub enabled: bool,
    /// Optional path to a PEM-encoded CA certificate file for custom or
    /// self-signed CAs. When omitted, Mozilla's bundled root certificates are used.
    #[cfg_attr(feature = "config", serde(default))]
    pub ca_cert: Option<String>,
}

/// SASL authentication configuration for Kafka broker connections.
///
/// Supported mechanisms are `PLAIN`, `SCRAM-SHA-256`, and `SCRAM-SHA-512`.
#[cfg(feature = "kafka")]
#[derive(Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub struct KafkaSaslConfig {
    /// SASL mechanism: `"PLAIN"`, `"SCRAM-SHA-256"`, or `"SCRAM-SHA-512"`.
    pub mechanism: String,
    /// SASL username.
    pub username: String,
    /// SASL password.
    pub password: String,
}

#[cfg(feature = "kafka")]
impl std::fmt::Debug for KafkaSaslConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaSaslConfig")
            .field("mechanism", &self.mechanism)
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

/// Configuration selecting which sink to use for a scenario.
///
/// This enum is serde-deserializable from YAML scenario files.
/// The `type` field selects the variant: `stdout`, `file`, `tcp`, `udp`,
/// `http_push`, `remote_write`, `kafka`, `loki`, or `otlp_grpc`.
///
/// Feature-gated sinks (`http_push`, `loki`, `remote_write`, `kafka`,
/// `otlp_grpc`) have companion `*Disabled` variants that are compiled in
/// when their feature is absent. These accept the YAML tag so that
/// deserialization succeeds with a descriptive error from [`create_sink`]
/// instead of a generic "unknown variant" error from serde.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(tag = "type"))]
pub enum SinkConfig {
    /// Write encoded events to stdout, buffered via [`BufWriter`](std::io::BufWriter).
    #[cfg_attr(feature = "config", serde(rename = "stdout"))]
    Stdout,

    /// Write encoded events to a file at the given path.
    ///
    /// Parent directories are created automatically if they do not exist.
    #[cfg_attr(feature = "config", serde(rename = "file"))]
    File {
        /// Filesystem path to write encoded events to.
        path: String,
    },

    /// Write encoded events over a persistent TCP connection.
    ///
    /// The sink connects on construction and buffers writes via [`BufWriter`](std::io::BufWriter).
    #[cfg_attr(feature = "config", serde(rename = "tcp"))]
    Tcp {
        /// Remote address to connect to, e.g. `"127.0.0.1:9999"`.
        address: String,

        /// Optional retry policy for transient failures.
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,
    },

    /// Send each encoded event as a single UDP datagram.
    ///
    /// No connection is established; an ephemeral local port is bound and each
    /// call to `write` sends one `send_to` datagram.
    #[cfg_attr(feature = "config", serde(rename = "udp"))]
    Udp {
        /// Remote address to send datagrams to, e.g. `"127.0.0.1:9999"`.
        address: String,
    },

    /// Batch encoded events and deliver them via HTTP POST.
    ///
    /// Bytes are accumulated in a buffer until `batch_size` bytes are reached,
    /// then flushed as a single POST request. The `flush()` method sends any
    /// remaining buffered data.
    ///
    /// Requires the `http` Cargo feature to be enabled.
    #[cfg(feature = "http")]
    #[cfg_attr(feature = "config", serde(rename = "http_push"))]
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
        #[cfg_attr(feature = "config", serde(default))]
        headers: Option<HashMap<String, String>>,

        /// Optional retry policy for transient failures.
        ///
        /// When absent, the sink fails immediately on errors (no retry).
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,
    },

    /// Placeholder variant when the `http` feature is not compiled in.
    ///
    /// Deserializes the `http_push` YAML tag so that the error message can
    /// point the user at the missing feature flag instead of producing a
    /// generic "unknown variant" error from serde.
    #[cfg(not(feature = "http"))]
    #[cfg_attr(feature = "config", serde(rename = "http_push"))]
    HttpPushDisabled {},

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
    #[cfg_attr(feature = "config", serde(rename = "remote_write"))]
    RemoteWrite {
        /// Target URL for the remote write endpoint, e.g.
        /// `"http://localhost:8428/api/v1/write"`.
        url: String,

        /// Flush threshold in number of TimeSeries entries. Defaults to 100 if
        /// not specified.
        #[cfg_attr(feature = "config", serde(default))]
        batch_size: Option<usize>,

        /// Optional retry policy for transient failures.
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,
    },

    /// Placeholder variant when the `remote-write` feature is not compiled in.
    ///
    /// Deserializes the `remote_write` YAML tag so that the error message can
    /// point the user at the missing feature flag instead of producing a
    /// generic "unknown variant" error from serde.
    #[cfg(not(feature = "remote-write"))]
    #[cfg_attr(feature = "config", serde(rename = "remote_write"))]
    RemoteWriteDisabled {},

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
    #[cfg_attr(feature = "config", serde(rename = "kafka"))]
    Kafka {
        /// Comma-separated list of broker `host:port` addresses,
        /// e.g. `"127.0.0.1:9092"` or `"broker1:9092,broker2:9092"`.
        brokers: String,

        /// The Kafka topic name to produce records to.
        topic: String,

        /// Optional retry policy for transient failures.
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,

        /// Optional TLS configuration for encrypted broker connections.
        #[cfg_attr(feature = "config", serde(default))]
        tls: Option<KafkaTlsConfig>,

        /// Optional SASL authentication configuration.
        #[cfg_attr(feature = "config", serde(default))]
        sasl: Option<KafkaSaslConfig>,
    },

    /// Placeholder variant when the `kafka` feature is not compiled in.
    ///
    /// Deserializes the `kafka` YAML tag so that the error message can
    /// point the user at the missing feature flag instead of producing a
    /// generic "unknown variant" error from serde.
    #[cfg(not(feature = "kafka"))]
    #[cfg_attr(feature = "config", serde(rename = "kafka"))]
    KafkaDisabled {},

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
    ///
    /// Requires the `http` Cargo feature to be enabled.
    #[cfg(feature = "http")]
    #[cfg_attr(feature = "config", serde(rename = "loki"))]
    Loki {
        /// Base URL of the Loki instance, e.g. `"http://localhost:3100"`.
        url: String,

        /// Flush threshold in log entries. Defaults to `100` if not specified.
        #[cfg_attr(feature = "config", serde(default))]
        batch_size: Option<usize>,

        /// Optional retry policy for transient failures.
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,
    },

    /// Placeholder variant when the `http` feature is not compiled in (Loki).
    ///
    /// Deserializes the `loki` YAML tag so that the error message can
    /// point the user at the missing feature flag instead of producing a
    /// generic "unknown variant" error from serde.
    #[cfg(not(feature = "http"))]
    #[cfg_attr(feature = "config", serde(rename = "loki"))]
    LokiDisabled {},

    /// Batch OTLP protobuf data and deliver via gRPC to an OpenTelemetry Collector.
    ///
    /// This sink is designed to be paired with the `otlp` encoder, which produces
    /// length-prefixed protobuf `Metric` or `LogRecord` bytes. The sink accumulates
    /// entries and, on flush or when `batch_size` is reached, wraps them in the
    /// appropriate OTLP export request and sends via gRPC.
    ///
    /// Requires the `otlp` Cargo feature to be enabled.
    #[cfg(feature = "otlp")]
    #[cfg_attr(feature = "config", serde(rename = "otlp_grpc"))]
    OtlpGrpc {
        /// gRPC endpoint URL, e.g. `"http://localhost:4317"`.
        endpoint: String,

        /// Whether to send metrics or logs.
        signal_type: otlp_grpc::OtlpSignalType,

        /// Flush threshold in number of data points / log records.
        /// Defaults to 100 if not specified.
        #[cfg_attr(feature = "config", serde(default))]
        batch_size: Option<usize>,

        /// Optional retry policy for transient failures.
        #[cfg_attr(feature = "config", serde(default))]
        retry: Option<retry::RetryConfig>,
    },

    /// Placeholder variant when the `otlp` feature is not compiled in.
    ///
    /// Deserializes the `otlp_grpc` YAML tag so that the error message can
    /// point the user at the missing feature flag instead of producing a
    /// generic "unknown variant" error from serde.
    #[cfg(not(feature = "otlp"))]
    #[cfg_attr(feature = "config", serde(rename = "otlp_grpc"))]
    OtlpGrpcDisabled {},
}

/// Create a boxed [`Sink`] from the given [`SinkConfig`].
///
/// The optional `labels` parameter is used only by the Loki sink (feature
/// `http`) to set stream labels. For all other sink types, pass `None`. Log
/// scenarios pass the scenario-level labels here so that Loki stream labels
/// are configured at the same level as every other signal type.
pub fn create_sink(
    config: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<Box<dyn Sink>, SondaError> {
    // `labels` is only consumed by the Loki arm (feature = "http"). Suppress
    // the unused-variable warning when that feature is disabled.
    let _ = &labels;
    match config {
        SinkConfig::Stdout => Ok(Box::new(stdout::StdoutSink::new())),
        SinkConfig::File { path } => Ok(Box::new(file::FileSink::new(Path::new(path))?)),
        SinkConfig::Tcp {
            address,
            retry: retry_cfg,
        } => {
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(tcp::TcpSink::new(address, rp)?))
        }
        SinkConfig::Udp { address } => Ok(Box::new(udp::UdpSink::new(address)?)),
        #[cfg(feature = "http")]
        SinkConfig::HttpPush {
            url,
            content_type,
            batch_size,
            headers,
            retry: retry_cfg,
        } => {
            let ct = content_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let bs = batch_size.unwrap_or(http::DEFAULT_BATCH_SIZE);
            let h = headers.clone().unwrap_or_default();
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(http::HttpPushSink::new(url, ct, bs, h, rp)?))
        }
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite {
            url,
            batch_size,
            retry: retry_cfg,
        } => {
            let bs = batch_size.unwrap_or(remote_write::DEFAULT_BATCH_SIZE);
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(remote_write::RemoteWriteSink::new(url, bs, rp)?))
        }
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka {
            brokers,
            topic,
            retry: retry_cfg,
            tls,
            sasl,
        } => {
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(kafka::KafkaSink::new(
                brokers,
                topic,
                rp,
                tls.as_ref(),
                sasl.as_ref(),
            )?))
        }
        #[cfg(feature = "http")]
        SinkConfig::Loki {
            url,
            batch_size,
            retry: retry_cfg,
        } => {
            let bs = batch_size.unwrap_or(100);
            let loki_labels = labels.cloned().unwrap_or_default();
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(loki::LokiSink::new(
                url.clone(),
                loki_labels,
                bs,
                rp,
            )?))
        }
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc {
            endpoint,
            signal_type,
            batch_size,
            retry: retry_cfg,
        } => {
            let bs = batch_size.unwrap_or(otlp_grpc::DEFAULT_BATCH_SIZE);
            // Convert scenario labels to OTLP Resource attributes.
            let resource_attrs: Vec<crate::encoder::otlp::KeyValue> = labels
                .map(|l| {
                    l.iter()
                        .map(|(k, v)| crate::encoder::otlp::KeyValue {
                            key: k.clone(),
                            value: Some(crate::encoder::otlp::AnyValue {
                                value: Some(crate::encoder::otlp::any_value::Value::StringValue(
                                    v.clone(),
                                )),
                            }),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let rp = retry_cfg
                .as_ref()
                .map(retry::RetryPolicy::from_config)
                .transpose()?;
            Ok(Box::new(otlp_grpc::OtlpGrpcSink::new(
                endpoint,
                *signal_type,
                bs,
                resource_attrs,
                rp,
            )?))
        }
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled { .. } => {
            Err(SondaError::Config(crate::ConfigError::invalid(
                "sink type 'http_push' requires the 'http' feature: cargo build -F http",
            )))
        }
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled { .. } => {
            Err(SondaError::Config(crate::ConfigError::invalid(
                "sink type 'remote_write' requires the 'remote-write' feature: \
                 cargo build -F remote-write",
            )))
        }
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled { .. } => Err(SondaError::Config(crate::ConfigError::invalid(
            "sink type 'kafka' requires the 'kafka' feature: cargo build -F kafka",
        ))),
        #[cfg(not(feature = "http"))]
        SinkConfig::LokiDisabled { .. } => Err(SondaError::Config(crate::ConfigError::invalid(
            "sink type 'loki' requires the 'http' feature: cargo build -F http",
        ))),
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled { .. } => {
            Err(SondaError::Config(crate::ConfigError::invalid(
                "sink type 'otlp_grpc' requires the 'otlp' feature: cargo build -F otlp",
            )))
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

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_stdout_deserializes_from_yaml() {
        let yaml = "type: stdout";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
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

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_file_deserializes_with_type_field() {
        let yaml = "type: file\npath: /tmp/sonda-mod-test.txt";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::File { ref path } if path == "/tmp/sonda-mod-test.txt")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_tcp_deserializes_with_type_field() {
        let yaml = "type: tcp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::Tcp { ref address, .. } if address == "127.0.0.1:9999")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_udp_deserializes_with_type_field() {
        let yaml = "type: udp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(config, SinkConfig::Udp { ref address } if address == "127.0.0.1:9999"));
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_unknown_type_returns_error() {
        let yaml = "type: no_such_sink";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown type tag should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_missing_type_field_returns_error() {
        // Without the `type` field the internally-tagged enum cannot identify the variant.
        let yaml = "stdout";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "missing type field should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_old_external_tag_format_is_rejected() {
        // The old externally-tagged format (`!stdout`) must no longer be accepted.
        let yaml = "!stdout";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "externally-tagged YAML format must be rejected in favour of internally-tagged"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_file_requires_path_field() {
        // `type: file` without a `path` field must fail.
        let yaml = "type: file";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "file variant without path should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_tcp_requires_address_field() {
        let yaml = "type: tcp";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "tcp variant without address should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_udp_requires_address_field() {
        let yaml = "type: udp";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
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
            retry: None,
        };
        let cloned = config.clone();
        assert!(
            matches!(cloned, SinkConfig::Tcp { ref address, .. } if address == "127.0.0.1:9999")
        );
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

    #[cfg(feature = "config")]
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
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "test_metric");
        assert!(matches!(
            config.encoder,
            crate::encoder::EncoderConfig::PrometheusText { .. }
        ));
        assert!(
            matches!(config.sink, SinkConfig::Tcp { ref address, .. } if address == "127.0.0.1:4321")
        );
    }

    #[cfg(feature = "config")]
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
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(
            config.encoder,
            crate::encoder::EncoderConfig::JsonLines { .. }
        ));
        assert!(
            matches!(config.sink, SinkConfig::File { ref path } if path == "/tmp/sonda-file-json-test.txt")
        );
    }

    #[cfg(feature = "config")]
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
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
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

    #[cfg(all(feature = "kafka", feature = "config"))]
    #[test]
    fn sink_config_kafka_deserializes_with_type_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::Kafka { ref brokers, ref topic, .. }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
    }

    #[cfg(all(feature = "kafka", feature = "config"))]
    #[test]
    fn sink_config_kafka_requires_brokers_field() {
        let yaml = "type: kafka\ntopic: sonda-test";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without brokers should fail deserialization"
        );
    }

    #[cfg(all(feature = "kafka", feature = "config"))]
    #[test]
    fn sink_config_kafka_requires_topic_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
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
            retry: None,
            tls: None,
            sasl: None,
        };
        let cloned = config.clone();
        assert!(
            matches!(cloned, SinkConfig::Kafka { ref brokers, ref topic, .. }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
        let s = format!("{config:?}");
        assert!(s.contains("Kafka"));
    }

    /// KafkaSaslConfig Debug output must redact the password field to prevent
    /// accidental credential exposure in logs or error messages.
    #[cfg(feature = "kafka")]
    #[test]
    fn kafka_sasl_config_debug_redacts_password() {
        let sasl = KafkaSaslConfig {
            mechanism: "PLAIN".to_string(),
            username: "alice".to_string(),
            password: "super-secret-password".to_string(),
        };
        let debug_output = format!("{sasl:?}");
        assert!(
            debug_output.contains("alice"),
            "Debug output should contain the username, got: {debug_output}"
        );
        assert!(
            !debug_output.contains("super-secret-password"),
            "Debug output must NOT contain the password in plaintext, got: {debug_output}"
        );
        assert!(
            debug_output.contains("***"),
            "Debug output should show a redacted password placeholder, got: {debug_output}"
        );
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
            retry: None,
            tls: None,
            sasl: None,
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
            retry: None,
            tls: None,
            sasl: None,
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

    #[cfg(all(feature = "http", feature = "config"))]
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
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
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

    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn sink_config_http_push_without_headers_is_backward_compatible() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
content_type: "text/plain"
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
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

    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn sink_config_http_push_with_empty_headers_map_deserializes() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
headers: {}
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
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

    #[cfg(feature = "http")]
    #[test]
    fn sink_config_http_push_with_headers_is_cloneable_and_debuggable() {
        let mut hdr = HashMap::new();
        hdr.insert("X-Custom".to_string(), "val".to_string());
        let config = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            headers: Some(hdr),
            retry: None,
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("HttpPush"));
        assert!(debug_str.contains("X-Custom"));
    }

    // ---------------------------------------------------------------------------
    // Feature gate: `http` feature controls HttpPush and Loki availability
    // ---------------------------------------------------------------------------

    /// When the `http` feature is enabled, `SinkConfig::HttpPush` must be
    /// constructible and the factory must produce a valid sink.
    #[cfg(feature = "http")]
    #[test]
    fn http_feature_enables_http_push_variant() {
        let config = SinkConfig::HttpPush {
            url: "http://127.0.0.1:19999/push".to_string(),
            content_type: None,
            batch_size: None,
            headers: None,
            retry: None,
        };
        let result = create_sink(&config, None);
        assert!(
            result.is_ok(),
            "HttpPush variant must be available when http feature is enabled"
        );
    }

    /// When the `http` feature is enabled, `SinkConfig::Loki` must be
    /// constructible and the factory must produce a valid sink.
    #[cfg(feature = "http")]
    #[test]
    fn http_feature_enables_loki_variant() {
        let config = SinkConfig::Loki {
            url: "http://127.0.0.1:19999".to_string(),
            batch_size: None,
            retry: None,
        };
        let result = create_sink(&config, None);
        assert!(
            result.is_ok(),
            "Loki variant must be available when http feature is enabled"
        );
    }

    /// When the `http` feature is enabled, `type: http_push` YAML must
    /// deserialize into the `HttpPush` variant.
    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn http_feature_enables_http_push_deserialization() {
        let yaml = "type: http_push\nurl: \"http://localhost:9090/push\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        assert!(matches!(config, SinkConfig::HttpPush { .. }));
    }

    /// When the `http` feature is enabled, `type: loki` YAML must
    /// deserialize into the `Loki` variant.
    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn http_feature_enables_loki_deserialization() {
        let yaml = "type: loki\nurl: \"http://localhost:3100\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        assert!(matches!(config, SinkConfig::Loki { .. }));
    }

    /// Non-HTTP sinks (stdout, file, tcp, udp) must remain available
    /// regardless of the `http` feature flag.
    #[test]
    fn non_http_sinks_available_without_http_feature() {
        // This test compiles and runs with or without the `http` feature.
        assert!(create_sink(&SinkConfig::Stdout, None).is_ok());
    }

    // ---------------------------------------------------------------------------
    // SinkConfig: retry field deserialization
    // ---------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_tcp_with_retry_deserializes() {
        let yaml = r#"
type: tcp
address: "127.0.0.1:9999"
retry:
  max_attempts: 3
  initial_backoff: 100ms
  max_backoff: 5s
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Tcp { address, retry } => {
                assert_eq!(address, "127.0.0.1:9999");
                let r = retry.expect("retry should be Some");
                assert_eq!(r.max_attempts, 3);
                assert_eq!(r.initial_backoff, "100ms");
                assert_eq!(r.max_backoff, "5s");
            }
            other => panic!("expected SinkConfig::Tcp, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_tcp_without_retry_has_none() {
        let yaml = "type: tcp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Tcp { retry, .. } => {
                assert!(retry.is_none(), "retry should default to None");
            }
            other => panic!("expected SinkConfig::Tcp, got {other:?}"),
        }
    }

    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn sink_config_http_push_with_retry_deserializes() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
retry:
  max_attempts: 5
  initial_backoff: 200ms
  max_backoff: 10s
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush { retry, .. } => {
                let r = retry.expect("retry should be Some");
                assert_eq!(r.max_attempts, 5);
                assert_eq!(r.initial_backoff, "200ms");
                assert_eq!(r.max_backoff, "10s");
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[cfg(all(feature = "http", feature = "config"))]
    #[test]
    fn sink_config_http_push_without_retry_is_backward_compatible() {
        let yaml = "type: http_push\nurl: \"http://localhost:9090/push\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush { retry, .. } => {
                assert!(retry.is_none(), "retry should default to None");
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Disabled feature variants: YAML deserialization succeeds and create_sink
    // returns a helpful error instead of a generic "unknown variant" error.
    // These tests only compile when the corresponding feature is disabled.
    // ---------------------------------------------------------------------------

    #[cfg(all(not(feature = "kafka"), feature = "config"))]
    #[test]
    fn kafka_yaml_deserializes_into_disabled_variant_when_feature_is_off() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml)
            .expect("type: kafka must deserialize even without the kafka feature");
        assert!(matches!(config, SinkConfig::KafkaDisabled { .. }));
    }

    #[cfg(not(feature = "kafka"))]
    #[test]
    fn create_sink_kafka_disabled_returns_feature_hint_error() {
        let config = SinkConfig::KafkaDisabled {};
        let err = create_sink(&config, None)
            .err()
            .expect("must return Err for disabled variant");
        let msg = err.to_string();
        assert!(
            msg.contains("kafka"),
            "error must mention the sink type, got: {msg}"
        );
        assert!(
            msg.contains("cargo build -F kafka"),
            "error must tell the user how to enable the feature, got: {msg}"
        );
    }

    #[cfg(all(not(feature = "http"), feature = "config"))]
    #[test]
    fn http_push_yaml_deserializes_into_disabled_variant_when_feature_is_off() {
        let yaml = "type: http_push\nurl: \"http://localhost:9090/push\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml)
            .expect("type: http_push must deserialize even without the http feature");
        assert!(matches!(config, SinkConfig::HttpPushDisabled { .. }));
    }

    #[cfg(not(feature = "http"))]
    #[test]
    fn create_sink_http_push_disabled_returns_feature_hint_error() {
        let config = SinkConfig::HttpPushDisabled {};
        let err = create_sink(&config, None)
            .err()
            .expect("must return Err for disabled variant");
        let msg = err.to_string();
        assert!(
            msg.contains("http_push"),
            "error must mention the sink type, got: {msg}"
        );
        assert!(
            msg.contains("cargo build -F http"),
            "error must tell the user how to enable the feature, got: {msg}"
        );
    }

    #[cfg(all(not(feature = "http"), feature = "config"))]
    #[test]
    fn loki_yaml_deserializes_into_disabled_variant_when_feature_is_off() {
        let yaml = "type: loki\nurl: \"http://localhost:3100\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml)
            .expect("type: loki must deserialize even without the http feature");
        assert!(matches!(config, SinkConfig::LokiDisabled { .. }));
    }

    #[cfg(not(feature = "http"))]
    #[test]
    fn create_sink_loki_disabled_returns_feature_hint_error() {
        let config = SinkConfig::LokiDisabled {};
        let err = create_sink(&config, None)
            .err()
            .expect("must return Err for disabled variant");
        let msg = err.to_string();
        assert!(
            msg.contains("loki"),
            "error must mention the sink type, got: {msg}"
        );
        assert!(
            msg.contains("cargo build -F http"),
            "error must tell the user how to enable the feature, got: {msg}"
        );
    }

    #[cfg(all(not(feature = "remote-write"), feature = "config"))]
    #[test]
    fn remote_write_yaml_deserializes_into_disabled_variant_when_feature_is_off() {
        let yaml = "type: remote_write\nurl: \"http://localhost:8428/api/v1/write\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml)
            .expect("type: remote_write must deserialize even without the remote-write feature");
        assert!(matches!(config, SinkConfig::RemoteWriteDisabled { .. }));
    }

    #[cfg(not(feature = "remote-write"))]
    #[test]
    fn create_sink_remote_write_disabled_returns_feature_hint_error() {
        let config = SinkConfig::RemoteWriteDisabled {};
        let err = create_sink(&config, None)
            .err()
            .expect("must return Err for disabled variant");
        let msg = err.to_string();
        assert!(
            msg.contains("remote_write"),
            "error must mention the sink type, got: {msg}"
        );
        assert!(
            msg.contains("cargo build -F remote-write"),
            "error must tell the user how to enable the feature, got: {msg}"
        );
    }

    #[cfg(all(not(feature = "otlp"), feature = "config"))]
    #[test]
    fn otlp_grpc_yaml_deserializes_into_disabled_variant_when_feature_is_off() {
        let yaml = "type: otlp_grpc\nendpoint: \"http://localhost:4317\"\nsignal_type: metrics";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml)
            .expect("type: otlp_grpc must deserialize even without the otlp feature");
        assert!(matches!(config, SinkConfig::OtlpGrpcDisabled { .. }));
    }

    #[cfg(not(feature = "otlp"))]
    #[test]
    fn create_sink_otlp_grpc_disabled_returns_feature_hint_error() {
        let config = SinkConfig::OtlpGrpcDisabled {};
        let err = create_sink(&config, None)
            .err()
            .expect("must return Err for disabled variant");
        let msg = err.to_string();
        assert!(
            msg.contains("otlp_grpc"),
            "error must mention the sink type, got: {msg}"
        );
        assert!(
            msg.contains("cargo build -F otlp"),
            "error must tell the user how to enable the feature, got: {msg}"
        );
    }
}
