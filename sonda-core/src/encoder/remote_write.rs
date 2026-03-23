//! Prometheus remote write protobuf encoder.
//!
//! Encodes [`MetricEvent`]s into the Prometheus remote write wire format:
//! `WriteRequest` -> `TimeSeries` -> (`Label`s + `Sample`s). The output is
//! Snappy-compressed protobuf, ready for POSTing to any remote write endpoint
//! (Prometheus, Thanos, Cortex, Mimir, VictoriaMetrics/vmagent, Grafana Cloud).
//!
//! Requires the `remote-write` feature flag.
//!
//! # Wire format
//!
//! The Prometheus remote write protocol sends HTTP POST requests containing a
//! Snappy-compressed protobuf body. The protobuf schema is defined in
//! [`prometheus/prometheus/prompb/remote.proto`](https://github.com/prometheus/prometheus/blob/main/prompb/remote.proto).
//! This module hand-writes the required message types with `prost` derive macros
//! to avoid a `protoc` build dependency.

use std::time::UNIX_EPOCH;

use prost::Message;

use crate::model::metric::MetricEvent;
use crate::SondaError;

use super::Encoder;

// ---------------------------------------------------------------------------
// Protobuf message types (hand-written prost structs)
// ---------------------------------------------------------------------------

/// A Prometheus remote write request containing one or more time series.
///
/// Corresponds to `prometheus.WriteRequest` from the remote write proto definition.
#[derive(Clone, PartialEq, prost::Message)]
pub struct WriteRequest {
    /// The time series to write.
    #[prost(message, repeated, tag = "1")]
    pub timeseries: Vec<TimeSeries>,
}

/// A single time series with labels and samples.
///
/// Corresponds to `prometheus.TimeSeries`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct TimeSeries {
    /// The label set identifying this time series.
    #[prost(message, repeated, tag = "1")]
    pub labels: Vec<Label>,
    /// The samples (timestamp + value pairs) for this time series.
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
}

/// A label name-value pair.
///
/// Corresponds to `prometheus.Label`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Label {
    /// The label name.
    #[prost(string, tag = "1")]
    pub name: String,
    /// The label value.
    #[prost(string, tag = "2")]
    pub value: String,
}

/// A single sample (timestamp + value) within a time series.
///
/// Corresponds to `prometheus.Sample`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Sample {
    /// The sample value.
    #[prost(double, tag = "1")]
    pub value: f64,
    /// The sample timestamp in milliseconds since the Unix epoch.
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// Encoder implementation
// ---------------------------------------------------------------------------

/// Encodes [`MetricEvent`]s into the Prometheus remote write protobuf format.
///
/// Each call to [`encode_metric`](Encoder::encode_metric) produces one
/// Snappy-compressed `WriteRequest` containing a single `TimeSeries` with one
/// `Sample`. The `__name__` label is set to `event.name`, and all event labels
/// are included sorted alphabetically by name.
///
/// The HTTP push sink should be configured with the following headers when using
/// this encoder:
///
/// - `Content-Type: application/x-protobuf`
/// - `Content-Encoding: snappy`
/// - `X-Prometheus-Remote-Write-Version: 0.1.0`
pub struct RemoteWriteEncoder;

impl RemoteWriteEncoder {
    /// Create a new `RemoteWriteEncoder`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RemoteWriteEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for RemoteWriteEncoder {
    /// Encode a metric event into Snappy-compressed protobuf (remote write format).
    ///
    /// Builds a `WriteRequest` with one `TimeSeries` containing:
    /// - A `__name__` label set to `event.name`
    /// - All labels from `event.labels`, sorted alphabetically by name
    /// - One `Sample` with `event.value` and `event.timestamp` (milliseconds)
    ///
    /// The serialized protobuf is then Snappy-compressed (raw/block format, not
    /// framed/streaming) and appended to `buf`.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        // Build the label set: __name__ first, then all event labels sorted by key.
        // The Prometheus convention requires __name__ to be present and labels to be
        // sorted by name.
        let mut labels = Vec::with_capacity(event.labels.len() + 1);

        // __name__ label sorts before any other label starting with a letter
        // (underscore sorts before letters in ASCII), so it naturally goes first
        // when sorted alphabetically. We insert it and then add the rest.
        labels.push(Label {
            name: "__name__".to_string(),
            value: event.name.clone(),
        });

        for (key, value) in event.labels.iter() {
            labels.push(Label {
                name: key.clone(),
                value: value.clone(),
            });
        }

        // Labels must be sorted by name per the remote write spec.
        // __name__ starts with '_' which sorts before ASCII letters, so it will
        // naturally appear first. Event labels from BTreeMap are already sorted.
        // We sort the full set to guarantee correctness regardless of input order.
        labels.sort_by(|a, b| a.name.cmp(&b.name));

        // Compute the timestamp in milliseconds since the Unix epoch.
        let timestamp_ms = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?
            .as_millis() as i64;

        let write_request = WriteRequest {
            timeseries: vec![TimeSeries {
                labels,
                samples: vec![Sample {
                    value: event.value,
                    timestamp: timestamp_ms,
                }],
            }],
        };

        // Serialize to protobuf. A fresh buffer is allocated sized to the encoded
        // length. The Encoder trait takes &self, so we cannot reuse a mutable buffer
        // across calls.
        let encoded_len = write_request.encoded_len();
        let mut proto_bytes = Vec::with_capacity(encoded_len);
        write_request
            .encode(&mut proto_bytes)
            .map_err(|e| SondaError::Encoder(format!("protobuf encode error: {e}")))?;

        // Snappy-compress using raw (block) format, not framed (streaming) format.
        let mut snappy_encoder = snap::raw::Encoder::new();
        let compressed = snappy_encoder
            .compress_vec(&proto_bytes)
            .map_err(|e| SondaError::Encoder(format!("snappy compression error: {e}")))?;

        buf.extend_from_slice(&compressed);

        Ok(())
    }
}
