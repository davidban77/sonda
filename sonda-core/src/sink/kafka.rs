//! Kafka sink — batches encoded telemetry and delivers it as Kafka records.
//!
//! Uses [`rskafka`] (pure Rust, no C dependencies) to produce records to a
//! configured topic and partition. Async operations are driven by a dedicated
//! single-threaded [`tokio::runtime::Runtime`] stored in the struct, keeping
//! the public [`Sink`] interface fully synchronous.
//!
//! Encoded bytes are accumulated in an internal buffer. When the buffer
//! reaches [`KAFKA_BUFFER_SIZE`] bytes the buffer is automatically flushed as
//! a single Kafka record. Call [`KafkaSink::flush`] explicitly at shutdown to
//! send any remaining buffered data.

use std::collections::BTreeMap;

use chrono::Utc;
use rskafka::{
    client::{
        partition::{Compression, UnknownTopicHandling},
        ClientBuilder,
    },
    record::Record,
};
use tokio::runtime::Runtime;

use crate::{sink::Sink, SondaError};

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
/// Async operations are driven by a private single-threaded [`Runtime`],
/// keeping the [`Sink`] trait interface fully synchronous.
pub struct KafkaSink {
    /// The Kafka topic to produce records to.
    topic: String,
    /// The broker address string (stored for error messages).
    brokers: String,
    /// Async client for the target topic partition.
    client: rskafka::client::partition::PartitionClient,
    /// Encoded bytes waiting to be published.
    buffer: Vec<u8>,
    /// Tokio runtime used to drive async rskafka calls synchronously.
    runtime: Runtime,
}

impl KafkaSink {
    /// Create a new `KafkaSink` connected to the given Kafka broker(s).
    ///
    /// # Arguments
    ///
    /// - `brokers` — comma-separated list of `host:port` broker addresses,
    ///   e.g. `"127.0.0.1:9092"` or `"broker1:9092,broker2:9092"`.
    /// - `topic` — the Kafka topic name to produce records to.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if:
    /// - The broker addresses cannot be parsed.
    /// - A TCP connection to a broker cannot be established.
    /// - The topic cannot be resolved (metadata lookup fails).
    pub fn new(brokers: &str, topic: &str) -> Result<Self, SondaError> {
        // Build a minimal single-threaded tokio runtime. This drives all
        // async rskafka calls without making the Sink trait async.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                std::io::Error::other(format!(
                    "kafka sink: failed to build tokio runtime for broker '{}': {}",
                    brokers, e
                ))
            })?;

        // Parse broker list: split on commas and trim whitespace.
        let bootstrap_brokers: Vec<String> = brokers
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        if bootstrap_brokers.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("kafka sink: no valid broker addresses in '{}'", brokers),
            )
            .into());
        }

        let topic_str = topic.to_owned();
        let brokers_str = brokers.to_owned();

        // Build the rskafka client and partition client inside the runtime.
        let client = runtime.block_on(async {
            let kafka_client = ClientBuilder::new(bootstrap_brokers)
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
                })?;

            kafka_client
                .partition_client(
                    topic_str.clone(),
                    0, // partition 0
                    UnknownTopicHandling::Error,
                )
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
        })?;

        Ok(Self {
            topic: topic.to_owned(),
            brokers: brokers.to_owned(),
            client,
            buffer: Vec::with_capacity(KAFKA_BUFFER_SIZE),
            runtime,
        })
    }

    /// Publish the internal buffer as a single Kafka record and clear it.
    ///
    /// Returns immediately without making a network call if the buffer is
    /// empty (idempotent).
    fn publish_buffer(&mut self) -> Result<(), SondaError> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // Take ownership of the buffer contents and replace with empty vec,
        // restoring the pre-allocated capacity on success.
        let payload = std::mem::take(&mut self.buffer);
        self.buffer.reserve(KAFKA_BUFFER_SIZE);

        let record = Record {
            key: None,
            value: Some(payload),
            headers: BTreeMap::new(),
            timestamp: Utc::now(),
        };

        let topic = self.topic.clone();
        let brokers = self.brokers.clone();

        self.runtime
            .block_on(async {
                self.client
                    .produce(vec![record], Compression::NoCompression)
                    .await
            })
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!(
                        "kafka sink: failed to produce record to topic '{}' at broker(s) '{}': {}",
                        topic, brokers, e
                    ),
                )
            })?;

        Ok(())
    }
}

impl Sink for KafkaSink {
    /// Append encoded event data to the internal buffer.
    ///
    /// When the buffer reaches [`KAFKA_BUFFER_SIZE`] bytes, the buffer is
    /// automatically published as a single Kafka record. Returns an error
    /// only if the automatic flush fails.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() >= KAFKA_BUFFER_SIZE {
            self.publish_buffer()?;
        }
        Ok(())
    }

    /// Flush any remaining buffered data as a Kafka record.
    ///
    /// Safe to call multiple times. Returns `Ok(())` immediately if the
    /// buffer is empty.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.publish_buffer()
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

    #[test]
    fn sink_config_kafka_deserializes_with_brokers_and_topic() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"\ntopic: sonda-test";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            SinkConfig::Kafka { brokers, topic } => {
                assert_eq!(brokers, "127.0.0.1:9092");
                assert_eq!(topic, "sonda-test");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_kafka_deserializes_with_multiple_brokers() {
        let yaml = "type: kafka\nbrokers: \"broker1:9092,broker2:9092\"\ntopic: my-topic";
        let config: SinkConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            matches!(config, SinkConfig::Kafka { ref brokers, ref topic }
                if brokers == "broker1:9092,broker2:9092" && topic == "my-topic")
        );
    }

    #[test]
    fn sink_config_kafka_requires_brokers_field() {
        let yaml = "type: kafka\ntopic: sonda-test";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without brokers should fail deserialization"
        );
    }

    #[test]
    fn sink_config_kafka_requires_topic_field() {
        let yaml = "type: kafka\nbrokers: \"127.0.0.1:9092\"";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "kafka variant without topic should fail deserialization"
        );
    }

    #[test]
    fn sink_config_kafka_is_cloneable() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
        };
        let cloned = config.clone();
        assert!(
            matches!(cloned, SinkConfig::Kafka { ref brokers, ref topic }
                if brokers == "127.0.0.1:9092" && topic == "sonda-test")
        );
    }

    #[test]
    fn sink_config_kafka_is_debuggable() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
        };
        let s = format!("{config:?}");
        assert!(s.contains("Kafka"));
        assert!(s.contains("9092"));
        assert!(s.contains("sonda-test"));
    }

    // -----------------------------------------------------------------------
    // Construction failure: unreachable broker
    // -----------------------------------------------------------------------

    /// Connecting to a port where no Kafka broker is listening must return a
    /// SondaError::Sink containing the broker address in the error message.
    ///
    /// Ignored by default because rskafka may wait for a long TCP timeout
    /// before returning an error. Run with `cargo test -- --ignored` when a
    /// local Kafka broker is available and the test environment can tolerate
    /// network delays.
    #[test]
    #[ignore = "requires network timeout which is slow; run with --ignored when desired"]
    fn new_with_unreachable_broker_returns_sink_error() {
        // Port 1 is privileged and will always refuse connections.
        let result = KafkaSink::new("127.0.0.1:1", "sonda-test");
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

    /// An empty broker string (after trimming) should return an error before
    /// attempting any network connection.
    #[test]
    fn new_with_empty_broker_string_returns_error() {
        let result = KafkaSink::new("", "sonda-test");
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

    /// A broker string composed only of commas and whitespace has no valid
    /// entries; this must be caught before any network call.
    #[test]
    fn new_with_whitespace_only_broker_string_returns_error() {
        let result = KafkaSink::new("  ,  ,  ", "sonda-test");
        assert!(
            result.is_err(),
            "broker string with only separators must be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // Full scenario YAML: kafka sink variant
    // -----------------------------------------------------------------------

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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "kafka_test");
        assert!(
            matches!(config.sink, SinkConfig::Kafka { ref brokers, ref topic }
                if brokers == "127.0.0.1:9092" && topic == "sonda-metrics")
        );
    }
}
