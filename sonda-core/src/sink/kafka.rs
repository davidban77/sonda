//! Kafka sink — batches encoded telemetry and delivers it as Kafka records.
//!
//! Uses [`rskafka`] (pure Rust, no C dependencies) to produce records to a
//! configured topic and partition.
//!
//! Encoded bytes are accumulated in an internal buffer. When the buffer
//! reaches [`KAFKA_BUFFER_SIZE`] bytes the buffer is automatically flushed as
//! a single Kafka record. Call [`KafkaSink::flush`] explicitly at shutdown to
//! send any remaining buffered data.
//!
//! ## TLS
//!
//! When [`KafkaTlsConfig`](super::KafkaTlsConfig) is provided with `enabled:
//! true`, the sink connects over TLS using `rustls`. A custom CA certificate
//! can be specified via `ca_cert`; otherwise Mozilla's bundled root
//! certificates are used.
//!
//! ## SASL
//!
//! When [`KafkaSaslConfig`](super::KafkaSaslConfig) is provided, the sink
//! authenticates using the specified SASL mechanism (`PLAIN`,
//! `SCRAM-SHA-256`, or `SCRAM-SHA-512`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use rskafka::{
    client::{
        partition::{Compression, UnknownTopicHandling},
        ClientBuilder, Credentials, SaslConfig,
    },
    record::Record,
};
use rustls_pki_types::pem::PemObject;

use crate::sink::retry::RetryPolicy;
use crate::sink::{KafkaSaslConfig, KafkaTlsConfig, Sink};
use crate::{ConfigError, SondaError};

/// Default buffer size in bytes before an automatic flush is triggered (64 KiB).
pub const KAFKA_BUFFER_SIZE: usize = 64 * 1024;

/// Delivers encoded telemetry to a Kafka topic as Kafka records.
///
/// Bytes are accumulated in an internal buffer. When the buffer reaches
/// [`KAFKA_BUFFER_SIZE`], the buffer is automatically published as a single
/// Kafka record. Call [`flush`](KafkaSink::flush) at shutdown to send any
/// remaining buffered data.
///
/// The sink uses [`rskafka`], a pure-Rust Kafka client with no C dependencies.
///
/// TLS and SASL authentication are supported for connecting to managed Kafka
/// services (Confluent Cloud, AWS MSK, Aiven, etc.).
pub struct KafkaSink {
    topic: String,
    brokers: String,
    client: rskafka::client::partition::PartitionClient,
    buffer: Vec<u8>,
    retry_policy: Option<RetryPolicy>,
    /// `Duration::ZERO` disables time-based flushing.
    max_buffer_age: Duration,
    last_flush_at: Instant,
    last_write_delivered: bool,
}

/// Build a `rustls::ClientConfig` for TLS connections to Kafka brokers.
///
/// If `ca_cert` is `Some`, the PEM file at that path is read and its
/// certificates are used as trust anchors. Otherwise, Mozilla's bundled
/// root certificates from [`webpki_roots`] are used.
///
/// # Errors
///
/// Returns [`SondaError::Sink`] if the CA certificate file cannot be read
/// or contains no valid certificates.
fn build_rustls_config(ca_cert: Option<&str>) -> Result<rustls::ClientConfig, SondaError> {
    // Install the ring crypto provider. `ok()` because another thread may
    // have already installed it.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let root_store = match ca_cert {
        Some(path) => {
            let pem_data = std::fs::read(path).map_err(|e| {
                SondaError::Sink(std::io::Error::new(
                    e.kind(),
                    format!("kafka sink: failed to read CA cert file '{}': {}", path, e),
                ))
            })?;

            let certs: Vec<_> = rustls_pki_types::CertificateDer::pem_slice_iter(&pem_data)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    SondaError::Sink(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "kafka sink: failed to parse certificate in CA cert file '{}': {}",
                            path, e
                        ),
                    ))
                })?;

            if certs.is_empty() {
                return Err(SondaError::Sink(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "kafka sink: no valid certificates found in CA cert file '{}'",
                        path
                    ),
                )));
            }

            let mut store = rustls::RootCertStore::empty();
            let (added, _ignored) = store.add_parsable_certificates(certs);
            if added == 0 {
                return Err(SondaError::Sink(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "kafka sink: no parsable trust anchors in CA cert file '{}'",
                        path
                    ),
                )));
            }
            store
        }
        None => {
            let mut store = rustls::RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            store
        }
    };

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(config)
}

/// Map a [`KafkaSaslConfig`] to an [`rskafka::client::SaslConfig`].
///
/// # Errors
///
/// Returns [`SondaError::Config`] if the mechanism string is not one of
/// `PLAIN`, `SCRAM-SHA-256`, or `SCRAM-SHA-512`.
fn map_sasl_config(sasl: &KafkaSaslConfig) -> Result<SaslConfig, SondaError> {
    let creds = Credentials::new(sasl.username.clone(), sasl.password.clone());
    match sasl.mechanism.as_str() {
        "PLAIN" => Ok(SaslConfig::Plain(creds)),
        "SCRAM-SHA-256" => Ok(SaslConfig::ScramSha256(creds)),
        "SCRAM-SHA-512" => Ok(SaslConfig::ScramSha512(creds)),
        other => Err(SondaError::Config(ConfigError::invalid(format!(
            "unsupported SASL mechanism: '{}' (expected PLAIN, SCRAM-SHA-256, or SCRAM-SHA-512)",
            other
        )))),
    }
}

impl KafkaSink {
    /// Create a new `KafkaSink` connected to the given Kafka broker(s).
    ///
    /// # Arguments
    ///
    /// - `brokers` — comma-separated list of `host:port` broker addresses,
    ///   e.g. `"127.0.0.1:9092"` or `"broker1:9092,broker2:9092"`.
    /// - `topic` — the Kafka topic name to produce records to.
    /// - `retry_policy` — optional retry policy for transient produce failures.
    /// - `tls_config` — optional TLS configuration for encrypted connections.
    /// - `sasl_config` — optional SASL authentication configuration.
    /// - `max_buffer_age` — maximum age a non-empty buffer may reach before a
    ///   time-based flush. `Duration::ZERO` disables time-based flushing.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if:
    /// - The broker addresses cannot be parsed.
    /// - A TCP connection to a broker cannot be established.
    /// - The topic metadata lookup fails after retries.
    /// - The TLS CA certificate file cannot be read or is invalid.
    ///
    /// Returns [`SondaError::Config`] if the SASL mechanism is unsupported.
    ///
    /// # Note
    ///
    /// The constructor retries metadata lookups for the target topic, so
    /// broker-side auto-topic-creation (`auto.create.topics.enable=true`)
    /// works out of the box. This may cause the constructor to briefly block
    /// while the broker creates the topic.
    pub async fn new(
        brokers: &str,
        topic: &str,
        retry_policy: Option<RetryPolicy>,
        tls_config: Option<&KafkaTlsConfig>,
        sasl_config: Option<&KafkaSaslConfig>,
        max_buffer_age: Duration,
    ) -> Result<Self, SondaError> {
        let bootstrap_brokers: Vec<String> = brokers
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        if bootstrap_brokers.is_empty() {
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("kafka sink: no valid broker addresses in '{}'", brokers),
            )));
        }

        let tls_rustls = match tls_config {
            Some(tls) if tls.enabled => {
                let cfg = build_rustls_config(tls.ca_cert.as_deref())?;
                Some(Arc::new(cfg))
            }
            _ => None,
        };

        let sasl = sasl_config.map(map_sasl_config).transpose()?;

        if sasl.is_some() && tls_rustls.is_none() {
            eprintln!(
                "WARNING: kafka sink: SASL authentication is configured without TLS — \
                 credentials will be sent in plaintext over the network"
            );
        }

        let topic_str = topic.to_owned();
        let brokers_str = brokers.to_owned();

        let mut builder = ClientBuilder::new(bootstrap_brokers);
        if let Some(tls) = tls_rustls {
            builder = builder.tls_config(tls);
        }
        if let Some(sasl) = sasl {
            builder = builder.sasl_config(sasl);
        }

        let kafka_client = builder
            .build()
            .await
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!(
                        "kafka sink: failed to connect to broker(s) '{}': {}",
                        brokers_str, e
                    ),
                )
            })
            .map_err(SondaError::Sink)?;

        let client = kafka_client
            .partition_client(topic_str.clone(), 0, UnknownTopicHandling::Retry)
            .await
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "kafka sink: failed to get partition client for topic '{}' at broker(s) '{}': {}",
                        topic_str, brokers_str, e
                    ),
                )
            })
            .map_err(SondaError::Sink)?;

        Ok(Self {
            topic: topic.to_owned(),
            brokers: brokers.to_owned(),
            client,
            buffer: Vec::with_capacity(KAFKA_BUFFER_SIZE),
            retry_policy,
            max_buffer_age,
            last_flush_at: Instant::now(),
            last_write_delivered: false,
        })
    }

    async fn publish_buffer(&mut self) -> Result<(), SondaError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.last_flush_at = Instant::now();
        let payload = std::mem::replace(&mut self.buffer, Vec::with_capacity(KAFKA_BUFFER_SIZE));

        let send_once = || async {
            let record = Record {
                key: None,
                value: Some(payload.clone()),
                headers: BTreeMap::new(),
                timestamp: Utc::now(),
            };
            self.client
                .produce(vec![record], Compression::NoCompression)
                .await
                .map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!(
                            "kafka sink: failed to produce record to topic '{}' at broker(s) '{}': {}",
                            self.topic, self.brokers, e
                        ),
                    )
                })
                .map_err(SondaError::Sink)?;
            Ok(())
        };

        if let Some(policy) = self.retry_policy.clone() {
            run_with_retry(&policy, send_once).await
        } else {
            send_once().await
        }
    }
}

async fn run_with_retry<F, Fut>(policy: &RetryPolicy, mut send_once: F) -> Result<(), SondaError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), SondaError>>,
{
    let mut last_error = match send_once().await {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    for attempt in 0..policy.max_attempts() {
        let backoff = policy.jittered_backoff(attempt);
        eprintln!(
            "sonda: retry {}/{} after {}ms (error: {})",
            attempt + 1,
            policy.max_attempts(),
            backoff.as_millis(),
            last_error,
        );
        tokio::time::sleep(backoff).await;
        match send_once().await {
            Ok(()) => return Ok(()),
            Err(e) => last_error = e,
        }
    }
    eprintln!(
        "sonda: all {} retries exhausted (last error: {})",
        policy.max_attempts(),
        last_error,
    );
    Err(last_error)
}

#[async_trait]
impl Sink for KafkaSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buffer.extend_from_slice(data);
        let size_reached = self.buffer.len() >= KAFKA_BUFFER_SIZE;
        let age_reached =
            !self.max_buffer_age.is_zero() && self.last_flush_at.elapsed() >= self.max_buffer_age;
        let should_flush = size_reached || age_reached;
        if should_flush {
            self.publish_buffer().await?;
        }
        self.last_write_delivered = should_flush;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        self.publish_buffer().await
    }

    fn last_write_delivered(&self) -> bool {
        self.last_write_delivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::SinkConfig;

    // -----------------------------------------------------------------------
    // KAFKA_BUFFER_SIZE constant
    // -----------------------------------------------------------------------

    #[test]
    fn kafka_buffer_size_is_64_kib() {
        assert_eq!(KAFKA_BUFFER_SIZE, 64 * 1024);
    }

    // -----------------------------------------------------------------------
    // Send + Sync contract (compile-time)
    // -----------------------------------------------------------------------

    /// KafkaSink must satisfy Send + Sync so it can be used behind a Mutex or
    /// sent across threads.
    #[test]
    fn kafka_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<KafkaSink>();
    }

    // -----------------------------------------------------------------------
    // SinkConfig deserialization
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_brokers_and_topic() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { brokers, topic, .. } => {
                assert_eq!(brokers, "127.0.0.1:9092");
                assert_eq!(topic, "sonda-test");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_multiple_brokers() {
        let yaml = "type: kafka\nbrokers: \"broker1:9092,broker2:9092\"\ntopic: my-topic";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::Kafka { ref brokers, ref topic, .. }
                if brokers == "broker1:9092,broker2:9092" && topic == "my-topic")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_requires_brokers_field() {
        let yaml = "type: kafka\ntopic: sonda-test";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without brokers should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_requires_topic_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without topic should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_max_buffer_age() {
        let yaml =
            "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test\nmax_buffer_age: 10s";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { max_buffer_age, .. } => {
                assert_eq!(max_buffer_age.as_deref(), Some("10s"));
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_max_buffer_age_defaults_to_none() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { max_buffer_age, .. } => {
                assert!(max_buffer_age.is_none());
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_kafka_is_cloneable() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: None,
            retry: None,
            tls: None,
            sasl: None,
        };
        let cloned = config.clone();
        assert!(
            matches!(cloned, SinkConfig::Kafka { ref brokers, ref topic, .. }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
    }

    #[test]
    fn sink_config_kafka_is_debuggable() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: None,
            retry: None,
            tls: None,
            sasl: None,
        };
        let s = format!("{config:?}");
        assert!(s.contains("Kafka"));
        assert!(s.contains("9092"));
        assert!(s.contains("sonda-test"));
    }

    // -----------------------------------------------------------------------
    // Construction failure: unreachable broker
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore = "requires network timeout which is slow; run with --ignored when desired"]
    async fn new_with_unreachable_broker_returns_sink_error() {
        let result = KafkaSink::new(
            "127.0.0.1:1",
            "sonda-test",
            None,
            None,
            None,
            Duration::ZERO,
        )
        .await;
        match result {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("127.0.0.1:1") || msg.contains("kafka"),
                    "error message should reference the broker address or 'kafka', got: {msg}"
                );
            }
            Ok(_) => panic!("construction must fail when broker is unreachable"),
        }
    }

    #[tokio::test]
    async fn new_with_empty_broker_string_returns_error() {
        let result = KafkaSink::new("", "sonda-test", None, None, None, Duration::ZERO).await;
        match result {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("kafka") || msg.contains("broker"),
                    "error should mention kafka or broker, got: {msg}"
                );
            }
            Ok(_) => panic!("empty broker string must be rejected"),
        }
    }

    #[tokio::test]
    async fn new_with_whitespace_only_broker_string_returns_error() {
        let result =
            KafkaSink::new("  ,  ,  ", "sonda-test", None, None, None, Duration::ZERO).await;
        assert!(
            result.is_err(),
            "broker string with only separators must be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // TLS config construction
    // -----------------------------------------------------------------------

    #[test]
    fn build_tls_config_with_system_roots_succeeds() {
        let config = build_rustls_config(None);
        assert!(
            config.is_ok(),
            "building TLS config with webpki roots should succeed"
        );
    }

    #[test]
    fn build_tls_config_with_invalid_ca_cert_path_returns_error() {
        let result = build_rustls_config(Some("/nonexistent/path/ca.pem"));
        match result {
            Err(SondaError::Sink(ref io_err)) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
                let msg = io_err.to_string();
                assert!(
                    msg.contains("/nonexistent/path/ca.pem"),
                    "error should reference the file path, got: {msg}"
                );
            }
            Err(ref e) => panic!("expected SondaError::Sink, got: {e:?}"),
            Ok(_) => panic!("nonexistent CA cert path must return error"),
        }
    }

    #[test]
    fn build_tls_config_with_valid_ca_cert_succeeds() {
        // Create a temporary self-signed PEM certificate for testing.
        let pem_data = include_str!("../../tests/fixtures/test-ca.pem");
        let tmpdir = std::env::temp_dir();
        let cert_path = tmpdir.join("sonda-test-ca.pem");
        std::fs::write(&cert_path, pem_data).expect("failed to write test cert");

        let result = build_rustls_config(Some(cert_path.to_str().unwrap()));
        // Clean up before asserting
        let _ = std::fs::remove_file(&cert_path);

        assert!(
            result.is_ok(),
            "building TLS config with a valid PEM cert should succeed, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn build_tls_config_with_corrupt_cert_returns_error() {
        let tmpdir = std::env::temp_dir();
        let cert_path = tmpdir.join("sonda-test-corrupt.pem");
        // A PEM file with a valid header/footer but corrupt base64 body.
        let corrupt_pem =
            "-----BEGIN CERTIFICATE-----\n!!INVALID_BASE64!!\n-----END CERTIFICATE-----\n";
        std::fs::write(&cert_path, corrupt_pem).expect("failed to write corrupt cert");

        let result = build_rustls_config(Some(cert_path.to_str().unwrap()));
        let _ = std::fs::remove_file(&cert_path);

        match result {
            Err(SondaError::Sink(ref io_err)) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
                let msg = io_err.to_string();
                assert!(
                    msg.contains("failed to parse certificate"),
                    "error should mention parse failure, got: {msg}"
                );
            }
            Err(ref e) => panic!("expected SondaError::Sink with InvalidData, got: {e:?}"),
            Ok(_) => panic!("corrupt PEM cert must return error"),
        }
    }

    #[test]
    fn build_tls_config_with_empty_pem_file_returns_error() {
        let tmpdir = std::env::temp_dir();
        let cert_path = tmpdir.join("sonda-test-empty.pem");
        std::fs::write(&cert_path, "").expect("failed to write empty file");

        let result = build_rustls_config(Some(cert_path.to_str().unwrap()));
        let _ = std::fs::remove_file(&cert_path);

        assert!(result.is_err(), "empty PEM file should return error");
        match result {
            Err(SondaError::Sink(ref io_err)) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
            }
            _ => panic!("expected SondaError::Sink with InvalidData kind"),
        }
    }

    // -----------------------------------------------------------------------
    // SASL config mapping
    // -----------------------------------------------------------------------

    #[test]
    fn map_sasl_config_plain() {
        let sasl = KafkaSaslConfig {
            mechanism: "PLAIN".to_string(),
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let result = map_sasl_config(&sasl);
        assert!(result.is_ok(), "PLAIN mechanism should map successfully");
        assert!(matches!(result.unwrap(), SaslConfig::Plain(_)));
    }

    #[test]
    fn map_sasl_config_scram_sha256() {
        let sasl = KafkaSaslConfig {
            mechanism: "SCRAM-SHA-256".to_string(),
            username: "bob".to_string(),
            password: "pw".to_string(),
        };
        let result = map_sasl_config(&sasl);
        assert!(result.is_ok(), "SCRAM-SHA-256 should map successfully");
        assert!(matches!(result.unwrap(), SaslConfig::ScramSha256(_)));
    }

    #[test]
    fn map_sasl_config_scram_sha512() {
        let sasl = KafkaSaslConfig {
            mechanism: "SCRAM-SHA-512".to_string(),
            username: "carol".to_string(),
            password: "pw".to_string(),
        };
        let result = map_sasl_config(&sasl);
        assert!(result.is_ok(), "SCRAM-SHA-512 should map successfully");
        assert!(matches!(result.unwrap(), SaslConfig::ScramSha512(_)));
    }

    #[test]
    fn map_sasl_config_unknown_mechanism_returns_error() {
        let sasl = KafkaSaslConfig {
            mechanism: "GSSAPI".to_string(),
            username: "user".to_string(),
            password: "pw".to_string(),
        };
        let result = map_sasl_config(&sasl);
        match result {
            Err(SondaError::Config(ConfigError::InvalidValue(ref msg))) => {
                assert!(
                    msg.contains("GSSAPI"),
                    "error message should reference the unsupported mechanism, got: {msg}"
                );
            }
            Err(ref e) => panic!("expected SondaError::Config(InvalidValue), got: {e:?}"),
            Ok(_) => panic!("unknown mechanism must return error"),
        }
    }

    // -----------------------------------------------------------------------
    // SinkConfig deserialization: TLS and SASL
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_tls_enabled() {
        let yaml = r#"
type: kafka
brokers: "broker.example.com:9093"
topic: test
tls:
  enabled: true
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { tls, .. } => {
                let tls = tls.expect("tls should be present");
                assert!(tls.enabled, "tls.enabled should be true");
                assert!(tls.ca_cert.is_none(), "ca_cert should be None");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_tls_and_ca_cert() {
        let yaml = r#"
type: kafka
brokers: "broker.example.com:9093"
topic: test
tls:
  enabled: true
  ca_cert: /path/to/ca.pem
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { tls, .. } => {
                let tls = tls.expect("tls should be present");
                assert!(tls.enabled);
                assert_eq!(tls.ca_cert.as_deref(), Some("/path/to/ca.pem"));
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_sasl_plain() {
        let yaml = r#"
type: kafka
brokers: "broker.example.com:9093"
topic: test
sasl:
  mechanism: PLAIN
  username: alice
  password: secret
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { sasl, .. } => {
                let sasl = sasl.expect("sasl should be present");
                assert_eq!(sasl.mechanism, "PLAIN");
                assert_eq!(sasl.username, "alice");
                assert_eq!(sasl.password, "secret");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_sasl_scram_sha256() {
        let yaml = r#"
type: kafka
brokers: "broker.example.com:9093"
topic: test
sasl:
  mechanism: SCRAM-SHA-256
  username: bob
  password: pw
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { sasl, .. } => {
                let sasl = sasl.expect("sasl should be present");
                assert_eq!(sasl.mechanism, "SCRAM-SHA-256");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_with_tls_and_sasl() {
        let yaml = r#"
type: kafka
brokers: "broker.example.com:9093"
topic: test
tls:
  enabled: true
sasl:
  mechanism: SCRAM-SHA-512
  username: carol
  password: s3cret
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { tls, sasl, .. } => {
                let tls = tls.expect("tls should be present");
                assert!(tls.enabled);
                let sasl = sasl.expect("sasl should be present");
                assert_eq!(sasl.mechanism, "SCRAM-SHA-512");
                assert_eq!(sasl.username, "carol");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_kafka_deserializes_without_tls_or_sasl() {
        let yaml = r#"
type: kafka
brokers: "127.0.0.1:9092"
topic: sonda-test
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { tls, sasl, .. } => {
                assert!(tls.is_none(), "tls should default to None");
                assert!(sasl.is_none(), "sasl should default to None");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Full scenario YAML: kafka sink variant with TLS and SASL
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn scenario_yaml_with_kafka_sink_deserializes_correctly() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: kafka_test
rate: 100.0
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: kafka
  brokers: "127.0.0.1:9092"
  topic: sonda-metrics
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "kafka_test");
        assert!(
            matches!(config.sink, SinkConfig::Kafka { ref brokers, ref topic, .. }
                if brokers == "127.0.0.1:9092" && topic == "sonda-metrics")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn scenario_yaml_with_kafka_tls_and_sasl_deserializes_correctly() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: kafka_tls_sasl
rate: 10.0
duration: 30s
generator:
  type: constant
  value: 42.0
labels:
  env: staging
encoder:
  type: prometheus_text
sink:
  type: kafka
  brokers: "broker.example.com:9093"
  topic: sonda-metrics
  tls:
    enabled: true
    ca_cert: /etc/ssl/certs/kafka-ca.pem
  sasl:
    mechanism: PLAIN
    username: sonda
    password: changeme
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "kafka_tls_sasl");
        match &config.sink {
            SinkConfig::Kafka {
                brokers,
                topic,
                tls,
                sasl,
                ..
            } => {
                assert_eq!(brokers.as_str(), "broker.example.com:9093");
                assert_eq!(topic.as_str(), "sonda-metrics");
                let tls = tls.as_ref().expect("tls should be present");
                assert!(tls.enabled);
                assert_eq!(tls.ca_cert.as_deref(), Some("/etc/ssl/certs/kafka-ca.pem"));
                let sasl = sasl.as_ref().expect("sasl should be present");
                assert_eq!(sasl.mechanism, "PLAIN");
                assert_eq!(sasl.username, "sonda");
                assert_eq!(sasl.password, "changeme");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }
}
