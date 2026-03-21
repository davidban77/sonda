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
