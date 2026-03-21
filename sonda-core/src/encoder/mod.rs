//! Encoders serialize telemetry events into wire format bytes.
//!
//! All encoders implement the `Encoder` trait. They write into a caller-provided
//! `Vec<u8>` to avoid per-event allocations.

pub mod influx;
pub mod json;
pub mod prometheus;

use serde::Deserialize;

use crate::model::metric::MetricEvent;

/// Encodes telemetry events into a specific wire format.
///
/// Implementations should pre-build any invariant content (label prefixes,
/// metric name validation) at construction time.
pub trait Encoder: Send + Sync {
    /// Encode a metric event into the provided buffer.
    fn encode_metric(
        &self,
        event: &MetricEvent,
        buf: &mut Vec<u8>,
    ) -> Result<(), crate::SondaError>;
}

/// Configuration selecting which encoder to use for a scenario.
///
/// This enum is serde-deserializable from YAML scenario files.
#[derive(Debug, Clone, Deserialize)]
pub enum EncoderConfig {
    /// Prometheus text exposition format (version 0.0.4).
    #[serde(rename = "prometheus_text")]
    PrometheusText,
    /// InfluxDB line protocol.
    ///
    /// `field_key` sets the field key used for the metric value. Defaults to `"value"`.
    #[serde(rename = "influx_lp")]
    InfluxLineProtocol {
        /// The InfluxDB field key for the metric value. Defaults to `"value"` if absent.
        field_key: Option<String>,
    },
    /// JSON Lines (NDJSON) format.
    ///
    /// Each event is serialized as one JSON object per line. Compatible with Elasticsearch,
    /// Loki, and generic HTTP ingest endpoints.
    #[serde(rename = "json_lines")]
    JsonLines,
}

/// Create a boxed [`Encoder`] from the given [`EncoderConfig`].
pub fn create_encoder(config: &EncoderConfig) -> Box<dyn Encoder> {
    match config {
        EncoderConfig::PrometheusText => Box::new(prometheus::PrometheusText::new()),
        EncoderConfig::InfluxLineProtocol { field_key } => {
            Box::new(influx::InfluxLineProtocol::new(field_key.clone()))
        }
        EncoderConfig::JsonLines => Box::new(json::JsonLines::new()),
    }
}
