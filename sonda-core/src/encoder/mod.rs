//! Encoders serialize telemetry events into wire format bytes.
//!
//! All encoders implement the `Encoder` trait. They write into a caller-provided
//! `Vec<u8>` to avoid per-event allocations.

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
}

/// Create a boxed [`Encoder`] from the given [`EncoderConfig`].
pub fn create_encoder(config: &EncoderConfig) -> Box<dyn Encoder> {
    match config {
        EncoderConfig::PrometheusText => Box::new(prometheus::PrometheusText::new()),
    }
}
