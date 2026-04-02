//! Prometheus remote write protobuf encoder.
//!
//! Encodes [`MetricEvent`]s into individual `TimeSeries` protobuf messages,
//! length-prefixed with a 4-byte little-endian u32. The `RemoteWriteSink`
//! (see `sink::remote_write`) accumulates these TimeSeries messages, wraps
//! them in a single `WriteRequest`, prost-encodes, snappy-compresses, and
//! HTTP POSTs the result to the remote write endpoint.
//!
//! This two-stage design (encoder writes raw TimeSeries, sink batches and
//! compresses) solves the batching problem: concatenating individually
//! snappy-compressed protobuf blobs produces corrupt input. By deferring
//! compression to flush time, the sink can build one valid WriteRequest
//! per HTTP POST.
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
use crate::{EncoderError, SondaError};

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

/// Encodes [`MetricEvent`]s into length-prefixed protobuf `TimeSeries` messages.
///
/// Each call to [`encode_metric`](Encoder::encode_metric) produces one
/// prost-encoded `TimeSeries` (NOT a full `WriteRequest`, NOT snappy-compressed),
/// prefixed with a 4-byte little-endian u32 length. This encoding is designed to
/// be consumed by `RemoteWriteSink`,
/// which batches multiple TimeSeries into a single `WriteRequest`, snappy-compresses,
/// and HTTP POSTs the result.
///
/// The `__name__` label is set to `event.name`, and all event labels are included
/// sorted alphabetically by name.
///
/// **Important:** This encoder must be paired with the `remote_write` sink type,
/// not the generic `http_push` sink. The `remote_write` sink handles WriteRequest
/// framing and snappy compression at flush time.
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
    /// Encode a metric event as a length-prefixed protobuf `TimeSeries`.
    ///
    /// Builds a single `TimeSeries` containing:
    /// - A `__name__` label set to `event.name`
    /// - All labels from `event.labels`, sorted alphabetically by name
    /// - One `Sample` with `event.value` and `event.timestamp` (milliseconds)
    ///
    /// The serialized protobuf bytes are prefixed with a 4-byte little-endian
    /// u32 length and appended to `buf`. No snappy compression is applied here;
    /// that is the responsibility of the `RemoteWriteSink` at flush time.
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
            value: event.name.to_string(),
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
            .map_err(|e| SondaError::Encoder(EncoderError::TimestampBeforeEpoch(e)))?
            .as_millis() as i64;

        let timeseries = TimeSeries {
            labels,
            samples: vec![Sample {
                value: event.value,
                timestamp: timestamp_ms,
            }],
        };

        // Serialize the TimeSeries to protobuf bytes.
        let encoded_len = timeseries.encoded_len();
        let mut proto_bytes = Vec::with_capacity(encoded_len);
        timeseries.encode(&mut proto_bytes).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf encode error: {e}")))
        })?;

        // Write a 4-byte little-endian length prefix followed by the protobuf bytes.
        // The RemoteWriteSink uses this prefix to split the buffer into individual
        // TimeSeries messages for batching.
        let len = proto_bytes.len() as u32;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&proto_bytes);

        Ok(())
    }
}

/// Parse all length-prefixed `TimeSeries` messages from a byte buffer.
///
/// The encoder writes each TimeSeries as a 4-byte little-endian u32 length
/// prefix followed by that many bytes of prost-encoded protobuf. This function
/// reads all such messages from `data` and returns the decoded `TimeSeries` structs.
///
/// Used by `RemoteWriteSink` to
/// accumulate TimeSeries for batching into a single `WriteRequest`.
///
/// # Errors
///
/// Returns [`SondaError::Encoder`] if the buffer is truncated or contains
/// invalid protobuf data.
pub fn parse_length_prefixed_timeseries(data: &[u8]) -> Result<Vec<TimeSeries>, SondaError> {
    let mut result = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        if offset + 4 > data.len() {
            return Err(SondaError::Encoder(EncoderError::Other(
                "truncated length prefix in TimeSeries buffer".into(),
            )));
        }

        let len = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + len > data.len() {
            return Err(SondaError::Encoder(EncoderError::Other(format!(
                "truncated TimeSeries protobuf: expected {} bytes, got {}",
                len,
                data.len() - offset
            ))));
        }

        let ts = TimeSeries::decode(&data[offset..offset + len]).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf decode error: {e}")))
        })?;
        result.push(ts);
        offset += len;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::metric::{Labels, MetricEvent};
    use std::time::{Duration, UNIX_EPOCH};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    /// Build a MetricEvent with a deterministic timestamp for testing.
    fn make_event(
        name: &str,
        value: f64,
        label_pairs: &[(&str, &str)],
        timestamp_ms: u64,
    ) -> MetricEvent {
        let labels = Labels::from_pairs(label_pairs).expect("valid labels");
        let ts = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
        MetricEvent::with_timestamp(name.to_string(), value, labels, ts).expect("valid metric name")
    }

    /// Decode a length-prefixed TimeSeries from a buffer.
    /// Returns the first TimeSeries found (reads 4-byte LE length prefix, then protobuf).
    fn decode_timeseries(buf: &[u8]) -> TimeSeries {
        assert!(buf.len() >= 4, "buffer must contain at least length prefix");
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let proto_bytes = &buf[4..4 + len];
        TimeSeries::decode(proto_bytes).expect("protobuf decode")
    }

    /// Decode all length-prefixed TimeSeries from a buffer.
    fn decode_all_timeseries(buf: &[u8]) -> Vec<TimeSeries> {
        let mut result = Vec::new();
        let mut offset = 0;
        while offset + 4 <= buf.len() {
            let len = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]) as usize;
            offset += 4;
            let proto_bytes = &buf[offset..offset + len];
            result.push(TimeSeries::decode(proto_bytes).expect("protobuf decode"));
            offset += len;
        }
        result
    }

    // -------------------------------------------------------------------------
    // Happy path: encode produces valid length-prefixed protobuf
    // -------------------------------------------------------------------------

    #[test]
    fn encode_metric_produces_nonempty_bytes() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("cpu_usage", 42.5, &[("host", "server1")], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");
        // At minimum: 4-byte length prefix + at least 1 byte of protobuf
        assert!(
            buf.len() > 4,
            "encoded output must contain length prefix + protobuf"
        );
    }

    #[test]
    fn length_prefix_matches_protobuf_length() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("test_metric", 99.9, &[("env", "prod")], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(
            buf.len(),
            4 + len,
            "total buffer length must equal 4 (prefix) + declared protobuf length"
        );

        // Decode the protobuf to verify it is valid
        let ts = TimeSeries::decode(&buf[4..]).expect("protobuf decode should succeed");
        assert_eq!(ts.samples.len(), 1, "TimeSeries should contain one sample");
    }

    // -------------------------------------------------------------------------
    // __name__ label is correctly set to the metric name
    // -------------------------------------------------------------------------

    #[test]
    fn name_label_is_set_to_metric_name() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("http_requests_total", 100.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);

        let name_label = ts
            .labels
            .iter()
            .find(|l| l.name == "__name__")
            .expect("__name__ label must be present");

        assert_eq!(
            name_label.value, "http_requests_total",
            "__name__ label value must match the metric name"
        );
    }

    // -------------------------------------------------------------------------
    // Labels are sorted alphabetically
    // -------------------------------------------------------------------------

    #[test]
    fn labels_are_sorted_alphabetically() {
        let encoder = RemoteWriteEncoder::new();
        // Labels provided in non-alphabetical order
        let event = make_event(
            "my_metric",
            1.0,
            &[("zone", "eu1"), ("env", "prod"), ("host", "server1")],
            1_700_000_000_000,
        );
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);
        let label_names: Vec<&str> = ts.labels.iter().map(|l| l.name.as_str()).collect();

        // __name__ starts with underscore which sorts before ascii letters
        assert_eq!(
            label_names,
            vec!["__name__", "env", "host", "zone"],
            "labels must be sorted alphabetically with __name__ first"
        );
    }

    // -------------------------------------------------------------------------
    // Sample has correct value and timestamp
    // -------------------------------------------------------------------------

    #[test]
    fn sample_has_correct_value_and_timestamp() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("gauge_metric", 3.14, &[], 1_700_000_000_500);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);
        assert_eq!(ts.samples.len(), 1, "must contain exactly one sample");

        let sample = &ts.samples[0];
        assert!(
            (sample.value - 3.14).abs() < f64::EPSILON,
            "sample value must be 3.14, got {}",
            sample.value
        );
        assert_eq!(
            sample.timestamp, 1_700_000_000_500i64,
            "timestamp must be in milliseconds since epoch"
        );
    }

    // -------------------------------------------------------------------------
    // Multiple labels are included
    // -------------------------------------------------------------------------

    #[test]
    fn multiple_labels_are_included_in_output() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event(
            "up",
            1.0,
            &[
                ("instance", "server-01"),
                ("job", "sonda"),
                ("env", "staging"),
            ],
            1_700_000_000_000,
        );
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);

        // 3 user labels + 1 __name__ = 4 total
        assert_eq!(
            ts.labels.len(),
            4,
            "must have 3 user labels + 1 __name__ label"
        );

        // Verify each user label is present
        let label_map: std::collections::HashMap<&str, &str> = ts
            .labels
            .iter()
            .map(|l| (l.name.as_str(), l.value.as_str()))
            .collect();

        assert_eq!(label_map.get("instance"), Some(&"server-01"));
        assert_eq!(label_map.get("job"), Some(&"sonda"));
        assert_eq!(label_map.get("env"), Some(&"staging"));
        assert_eq!(label_map.get("__name__"), Some(&"up"));
    }

    // -------------------------------------------------------------------------
    // No labels (empty) case works
    // -------------------------------------------------------------------------

    #[test]
    fn empty_labels_produces_only_name_label() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("bare_metric", 0.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);

        assert_eq!(
            ts.labels.len(),
            1,
            "with no user labels, only __name__ should be present"
        );
        assert_eq!(ts.labels[0].name, "__name__");
        assert_eq!(ts.labels[0].value, "bare_metric");
    }

    // -------------------------------------------------------------------------
    // Encoder is Send + Sync
    // -------------------------------------------------------------------------

    #[test]
    fn remote_write_encoder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RemoteWriteEncoder>();
    }

    // -------------------------------------------------------------------------
    // Default impl works
    // -------------------------------------------------------------------------

    #[test]
    fn default_creates_valid_encoder() {
        let encoder = RemoteWriteEncoder::default();
        let event = make_event("test", 1.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");
        assert!(!buf.is_empty());
    }

    // -------------------------------------------------------------------------
    // Protobuf types are correct (hand-written prost structs)
    // -------------------------------------------------------------------------

    #[test]
    fn write_request_roundtrips_through_protobuf() {
        let wr = WriteRequest {
            timeseries: vec![TimeSeries {
                labels: vec![
                    Label {
                        name: "__name__".to_string(),
                        value: "test".to_string(),
                    },
                    Label {
                        name: "env".to_string(),
                        value: "prod".to_string(),
                    },
                ],
                samples: vec![Sample {
                    value: 42.0,
                    timestamp: 1_700_000_000_000,
                }],
            }],
        };

        let mut encoded = Vec::new();
        wr.encode(&mut encoded).expect("encode should succeed");
        let decoded = WriteRequest::decode(encoded.as_slice()).expect("decode should succeed");
        assert_eq!(wr, decoded, "roundtripped WriteRequest must match original");
    }

    // -------------------------------------------------------------------------
    // Multiple events: buffer accumulates correctly with length prefixes
    // -------------------------------------------------------------------------

    #[test]
    fn multiple_encode_calls_append_to_buffer() {
        let encoder = RemoteWriteEncoder::new();
        let event1 = make_event("metric_a", 1.0, &[], 1_700_000_000_000);
        let event2 = make_event("metric_b", 2.0, &[], 1_700_000_001_000);

        let mut buf = Vec::new();
        encoder.encode_metric(&event1, &mut buf).expect("encode 1");
        let len_after_first = buf.len();
        assert!(len_after_first > 0, "first encode should produce bytes");

        encoder.encode_metric(&event2, &mut buf).expect("encode 2");
        assert!(
            buf.len() > len_after_first,
            "second encode should append more bytes"
        );

        // Both should be independently decodeable
        let all_ts = decode_all_timeseries(&buf);
        assert_eq!(all_ts.len(), 2, "should have two TimeSeries in buffer");
    }

    // -------------------------------------------------------------------------
    // Timestamp at epoch zero works
    // -------------------------------------------------------------------------

    #[test]
    fn timestamp_at_epoch_zero_produces_zero_ms() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("epoch_test", 1.0, &[], 0);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);
        let sample = &ts.samples[0];
        assert_eq!(sample.timestamp, 0, "timestamp at epoch should be 0 ms");
    }

    // -------------------------------------------------------------------------
    // Large value and negative zero edge cases
    // -------------------------------------------------------------------------

    #[test]
    fn large_float_value_is_preserved() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("big_metric", f64::MAX, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);
        let sample = &ts.samples[0];
        assert_eq!(sample.value, f64::MAX, "f64::MAX must be preserved");
    }

    #[test]
    fn zero_value_is_preserved() {
        let encoder = RemoteWriteEncoder::new();
        let event = make_event("zero_metric", 0.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let ts = decode_timeseries(&buf);
        let sample = &ts.samples[0];
        assert!(
            sample.value == 0.0,
            "zero value must be preserved, got {}",
            sample.value
        );
    }

    // -------------------------------------------------------------------------
    // encode_log returns not supported error
    // -------------------------------------------------------------------------

    #[test]
    fn encode_log_returns_not_supported_error() {
        use crate::model::log::LogEvent;
        use std::collections::BTreeMap;

        let encoder = RemoteWriteEncoder::new();
        let log_event = LogEvent::new(
            crate::model::log::Severity::Info,
            "test message".to_string(),
            BTreeMap::new(),
        );
        let mut buf = Vec::new();
        let result = encoder.encode_log(&log_event, &mut buf);
        assert!(
            result.is_err(),
            "remote write encoder must not support log encoding"
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not supported"),
            "error message should contain 'not supported', got: {msg}"
        );
    }
}
