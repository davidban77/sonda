//! OTLP protobuf encoder for metrics and logs.
//!
//! Encodes [`MetricEvent`]s and [`LogEvent`]s into length-prefixed OTLP protobuf
//! messages. The `OtlpGrpcSink` (see `sink::otlp_grpc`) accumulates these
//! messages, wraps them in full `ExportMetricsServiceRequest` or
//! `ExportLogsServiceRequest` envelopes, and sends them via gRPC.
//!
//! This two-stage design (encoder writes individual data points, sink batches
//! and sends) mirrors the `remote_write` encoder/sink pattern.
//!
//! Requires the `otlp` feature flag.
//!
//! # Wire format
//!
//! The OTLP protocol uses protobuf messages defined in the
//! [opentelemetry-proto](https://github.com/open-telemetry/opentelemetry-proto)
//! repository. This module hand-writes the required message types with `prost`
//! derive macros to avoid a `protoc` build dependency.

use std::time::UNIX_EPOCH;

use prost::Message;

use crate::model::log::{LogEvent, Severity};
use crate::model::metric::MetricEvent;
use crate::{EncoderError, SondaError};

use super::Encoder;

// ---------------------------------------------------------------------------
// Protobuf message types — shared
// ---------------------------------------------------------------------------

/// A key-value pair used for resource attributes, data point attributes, and
/// log record attributes.
///
/// Corresponds to `opentelemetry.proto.common.v1.KeyValue`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyValue {
    /// The attribute key.
    #[prost(string, tag = "1")]
    pub key: String,
    /// The attribute value.
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

/// A polymorphic attribute value.
///
/// Corresponds to `opentelemetry.proto.common.v1.AnyValue`. Only the subset
/// of value types needed by Sonda is implemented (string, bool, int, double).
#[derive(Clone, PartialEq, prost::Message)]
pub struct AnyValue {
    /// The value payload, represented as a oneof.
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4")]
    pub value: Option<any_value::Value>,
}

/// Inner oneof variants for [`AnyValue`].
pub mod any_value {
    /// The value payload variants.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        /// A string value.
        #[prost(string, tag = "1")]
        StringValue(String),
        /// A boolean value.
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        /// A 64-bit signed integer value.
        #[prost(int64, tag = "3")]
        IntValue(i64),
        /// A 64-bit floating-point value.
        #[prost(double, tag = "4")]
        DoubleValue(f64),
    }
}

/// Describes the source of telemetry data.
///
/// Corresponds to `opentelemetry.proto.resource.v1.Resource`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Resource {
    /// Key-value pairs describing the resource.
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

/// Identifies the instrumentation library that produced the telemetry.
///
/// Corresponds to `opentelemetry.proto.common.v1.InstrumentationScope`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct InstrumentationScope {
    /// The instrumentation library name.
    #[prost(string, tag = "1")]
    pub name: String,
    /// The instrumentation library version.
    #[prost(string, tag = "2")]
    pub version: String,
}

// ---------------------------------------------------------------------------
// Protobuf message types — metrics
// ---------------------------------------------------------------------------

/// Top-level request for exporting metrics.
///
/// Corresponds to `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportMetricsServiceRequest {
    /// The resource metrics to export.
    #[prost(message, repeated, tag = "1")]
    pub resource_metrics: Vec<ResourceMetrics>,
}

/// Response from the metrics export service.
///
/// Corresponds to `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceResponse`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportMetricsServiceResponse {}

/// Metrics data associated with a resource.
///
/// Corresponds to `opentelemetry.proto.metrics.v1.ResourceMetrics`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceMetrics {
    /// The resource that produced these metrics.
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    /// Metrics grouped by instrumentation scope.
    #[prost(message, repeated, tag = "2")]
    pub scope_metrics: Vec<ScopeMetrics>,
}

/// Metrics produced by a single instrumentation scope.
///
/// Corresponds to `opentelemetry.proto.metrics.v1.ScopeMetrics`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ScopeMetrics {
    /// The instrumentation scope that produced these metrics.
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    /// The metrics in this scope.
    #[prost(message, repeated, tag = "2")]
    pub metrics: Vec<Metric>,
}

/// A single metric with its name and data.
///
/// Corresponds to `opentelemetry.proto.metrics.v1.Metric`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Metric {
    /// The metric name.
    #[prost(string, tag = "1")]
    pub name: String,
    /// A description of the metric.
    #[prost(string, tag = "2")]
    pub description: String,
    /// The unit of the metric (e.g., "1", "ms", "By").
    #[prost(string, tag = "3")]
    pub unit: String,
    /// The metric data, represented as a oneof.
    #[prost(oneof = "metric::Data", tags = "5")]
    pub data: Option<metric::Data>,
}

/// Inner oneof variants for [`Metric`] data.
pub mod metric {
    /// The metric data variants.
    ///
    /// Only `Gauge` is implemented — Sonda generates instantaneous point-in-time
    /// values, which map to OTLP Gauge semantics.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Data {
        /// Gauge data points.
        #[prost(message, tag = "5")]
        Gauge(super::Gauge),
    }
}

/// A collection of gauge data points.
///
/// Corresponds to `opentelemetry.proto.metrics.v1.Gauge`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Gauge {
    /// The data points in this gauge.
    #[prost(message, repeated, tag = "1")]
    pub data_points: Vec<NumberDataPoint>,
}

/// A single numeric data point.
///
/// Corresponds to `opentelemetry.proto.metrics.v1.NumberDataPoint`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NumberDataPoint {
    /// Key-value pairs describing this data point.
    #[prost(message, repeated, tag = "7")]
    pub attributes: Vec<KeyValue>,
    /// The timestamp in nanoseconds since the Unix epoch.
    #[prost(fixed64, tag = "3")]
    pub time_unix_nano: u64,
    /// The data point value, represented as a oneof.
    #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
    pub value: Option<number_data_point::Value>,
}

/// Inner oneof variants for [`NumberDataPoint`] value.
pub mod number_data_point {
    /// The value variants for a numeric data point.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        /// A double-precision floating-point value.
        #[prost(double, tag = "4")]
        AsDouble(f64),
        /// A 64-bit signed integer value.
        #[prost(sfixed64, tag = "6")]
        AsInt(i64),
    }
}

// ---------------------------------------------------------------------------
// Protobuf message types — logs
// ---------------------------------------------------------------------------

/// Top-level request for exporting logs.
///
/// Corresponds to `opentelemetry.proto.collector.logs.v1.ExportLogsServiceRequest`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogsServiceRequest {
    /// The resource logs to export.
    #[prost(message, repeated, tag = "1")]
    pub resource_logs: Vec<ResourceLogs>,
}

/// Response from the logs export service.
///
/// Corresponds to `opentelemetry.proto.collector.logs.v1.ExportLogsServiceResponse`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogsServiceResponse {}

/// Log records associated with a resource.
///
/// Corresponds to `opentelemetry.proto.logs.v1.ResourceLogs`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceLogs {
    /// The resource that produced these logs.
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    /// Logs grouped by instrumentation scope.
    #[prost(message, repeated, tag = "2")]
    pub scope_logs: Vec<ScopeLogs>,
}

/// Log records produced by a single instrumentation scope.
///
/// Corresponds to `opentelemetry.proto.logs.v1.ScopeLogs`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ScopeLogs {
    /// The instrumentation scope that produced these logs.
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    /// The log records in this scope.
    #[prost(message, repeated, tag = "2")]
    pub log_records: Vec<LogRecord>,
}

/// A single log record.
///
/// Corresponds to `opentelemetry.proto.logs.v1.LogRecord`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct LogRecord {
    /// The timestamp in nanoseconds since the Unix epoch.
    #[prost(fixed64, tag = "1")]
    pub time_unix_nano: u64,
    /// The severity number (maps to OTLP SeverityNumber enum values).
    #[prost(int32, tag = "2")]
    pub severity_number: i32,
    /// The severity text (human-readable, e.g. "INFO").
    #[prost(string, tag = "3")]
    pub severity_text: String,
    /// The log message body.
    #[prost(message, optional, tag = "5")]
    pub body: Option<AnyValue>,
    /// Key-value pairs describing this log record.
    #[prost(message, repeated, tag = "6")]
    pub attributes: Vec<KeyValue>,
}

/// OTLP severity number values.
///
/// Maps from Sonda's [`Severity`] to the OTLP severity number enum.
/// See the [OTLP spec](https://opentelemetry.io/docs/specs/otel/logs/data-model/#severity-fields).
pub fn severity_to_number(severity: Severity) -> i32 {
    match severity {
        Severity::Trace => 1,  // SEVERITY_NUMBER_TRACE
        Severity::Debug => 5,  // SEVERITY_NUMBER_DEBUG
        Severity::Info => 9,   // SEVERITY_NUMBER_INFO
        Severity::Warn => 13,  // SEVERITY_NUMBER_WARN
        Severity::Error => 17, // SEVERITY_NUMBER_ERROR
        Severity::Fatal => 21, // SEVERITY_NUMBER_FATAL
    }
}

/// Maps a Sonda [`Severity`] to its OTLP severity text.
fn severity_to_text(severity: Severity) -> &'static str {
    match severity {
        Severity::Trace => "TRACE",
        Severity::Debug => "DEBUG",
        Severity::Info => "INFO",
        Severity::Warn => "WARN",
        Severity::Error => "ERROR",
        Severity::Fatal => "FATAL",
    }
}

// ---------------------------------------------------------------------------
// Helper: convert SystemTime to nanoseconds since Unix epoch
// ---------------------------------------------------------------------------

/// Convert a [`std::time::SystemTime`] to nanoseconds since the Unix epoch.
///
/// # Errors
///
/// Returns [`SondaError::Encoder`] if the timestamp predates the Unix epoch.
fn timestamp_nanos(ts: std::time::SystemTime) -> Result<u64, SondaError> {
    let duration = ts
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SondaError::Encoder(EncoderError::TimestampBeforeEpoch(e)))?;
    Ok(duration.as_secs() * 1_000_000_000 + u64::from(duration.subsec_nanos()))
}

/// Build a [`KeyValue`] with a string value.
fn string_kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

// ---------------------------------------------------------------------------
// Encoder implementation
// ---------------------------------------------------------------------------

/// Encodes [`MetricEvent`]s and [`LogEvent`]s into length-prefixed OTLP
/// protobuf messages.
///
/// **Metrics**: Each call to [`encode_metric`](Encoder::encode_metric) produces
/// one prost-encoded [`Metric`] message (containing a single [`Gauge`] with one
/// [`NumberDataPoint`]), prefixed with a 4-byte little-endian u32 length. Event
/// labels become data point attributes.
///
/// **Logs**: Each call to [`encode_log`](Encoder::encode_log) produces one
/// prost-encoded [`LogRecord`] message, prefixed with a 4-byte little-endian
/// u32 length. Event labels and fields become log record attributes.
///
/// These length-prefixed messages are designed to be consumed by
/// [`OtlpGrpcSink`](crate::sink::otlp_grpc::OtlpGrpcSink), which batches them
/// into full `ExportMetricsServiceRequest` or `ExportLogsServiceRequest`
/// envelopes for gRPC delivery.
pub struct OtlpEncoder;

impl OtlpEncoder {
    /// Create a new `OtlpEncoder`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OtlpEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for OtlpEncoder {
    /// Encode a metric event as a length-prefixed OTLP protobuf [`Metric`].
    ///
    /// Builds a single `Metric` containing one `Gauge` with one
    /// `NumberDataPoint`. The metric name is set from `event.name`, event labels
    /// become data point attributes, the value is stored as `as_double`, and the
    /// timestamp is converted to nanoseconds since the Unix epoch.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let time_unix_nano = timestamp_nanos(event.timestamp)?;

        // Build attributes from event labels.
        let attributes: Vec<KeyValue> = event.labels.iter().map(|(k, v)| string_kv(k, v)).collect();

        let data_point = NumberDataPoint {
            attributes,
            time_unix_nano,
            value: Some(number_data_point::Value::AsDouble(event.value)),
        };

        let metric = Metric {
            name: event.name.to_string(),
            description: String::new(),
            unit: String::new(),
            data: Some(metric::Data::Gauge(Gauge {
                data_points: vec![data_point],
            })),
        };

        // Serialize the Metric to protobuf bytes.
        let encoded_len = metric.encoded_len();
        let mut proto_bytes = Vec::with_capacity(encoded_len);
        metric.encode(&mut proto_bytes).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf encode error: {e}")))
        })?;

        // Write a 4-byte little-endian length prefix followed by the protobuf bytes.
        let len = proto_bytes.len() as u32;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&proto_bytes);

        Ok(())
    }

    /// Encode a log event as a length-prefixed OTLP protobuf [`LogRecord`].
    ///
    /// Builds a single `LogRecord` with the message body, severity, timestamp,
    /// and attributes from both scenario labels and event fields.
    fn encode_log(&self, event: &LogEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let time_unix_nano = timestamp_nanos(event.timestamp)?;

        // Build attributes from labels (scenario-level) and fields (event-level).
        let mut attributes: Vec<KeyValue> =
            Vec::with_capacity(event.labels.len() + event.fields.len());
        for (k, v) in event.labels.iter() {
            attributes.push(string_kv(k, v));
        }
        for (k, v) in &event.fields {
            attributes.push(string_kv(k, v));
        }

        let log_record = LogRecord {
            time_unix_nano,
            severity_number: severity_to_number(event.severity),
            severity_text: severity_to_text(event.severity).to_string(),
            body: Some(AnyValue {
                value: Some(any_value::Value::StringValue(event.message.clone())),
            }),
            attributes,
        };

        // Serialize the LogRecord to protobuf bytes.
        let encoded_len = log_record.encoded_len();
        let mut proto_bytes = Vec::with_capacity(encoded_len);
        log_record.encode(&mut proto_bytes).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf encode error: {e}")))
        })?;

        // Write a 4-byte little-endian length prefix followed by the protobuf bytes.
        let len = proto_bytes.len() as u32;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&proto_bytes);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Parser helpers — consumed by the OtlpGrpcSink
// ---------------------------------------------------------------------------

/// Parse all length-prefixed [`Metric`] messages from a byte buffer.
///
/// The encoder writes each `Metric` as a 4-byte little-endian u32 length
/// prefix followed by that many bytes of prost-encoded protobuf. This function
/// reads all such messages from `data` and returns the decoded `Metric` structs.
///
/// Used by [`OtlpGrpcSink`](crate::sink::otlp_grpc::OtlpGrpcSink) to
/// accumulate metrics for batching.
///
/// # Errors
///
/// Returns [`SondaError::Encoder`] if the buffer is truncated or contains
/// invalid protobuf data.
pub fn parse_length_prefixed_metrics(data: &[u8]) -> Result<Vec<Metric>, SondaError> {
    parse_length_prefixed(data, "Metric")
}

/// Parse all length-prefixed [`LogRecord`] messages from a byte buffer.
///
/// The encoder writes each `LogRecord` as a 4-byte little-endian u32 length
/// prefix followed by that many bytes of prost-encoded protobuf. This function
/// reads all such messages from `data` and returns the decoded `LogRecord` structs.
///
/// Used by [`OtlpGrpcSink`](crate::sink::otlp_grpc::OtlpGrpcSink) to
/// accumulate log records for batching.
///
/// # Errors
///
/// Returns [`SondaError::Encoder`] if the buffer is truncated or contains
/// invalid protobuf data.
pub fn parse_length_prefixed_log_records(data: &[u8]) -> Result<Vec<LogRecord>, SondaError> {
    parse_length_prefixed(data, "LogRecord")
}

/// Generic parser for length-prefixed prost messages.
fn parse_length_prefixed<T: Message + Default>(
    data: &[u8],
    type_name: &str,
) -> Result<Vec<T>, SondaError> {
    let mut result = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        if offset + 4 > data.len() {
            return Err(SondaError::Encoder(EncoderError::Other(format!(
                "truncated length prefix in {type_name} buffer"
            ))));
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
                "truncated {type_name} protobuf: expected {len} bytes, got {}",
                data.len() - offset
            ))));
        }

        let msg = T::decode(&data[offset..offset + len]).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf decode error: {e}")))
        })?;
        result.push(msg);
        offset += len;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::log::LogEvent;
    use crate::model::metric::{Labels, MetricEvent};
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a MetricEvent with a deterministic timestamp for testing.
    fn make_metric(
        name: &str,
        value: f64,
        label_pairs: &[(&str, &str)],
        timestamp_ms: u64,
    ) -> MetricEvent {
        let labels = Labels::from_pairs(label_pairs).expect("valid labels");
        let ts = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
        MetricEvent::with_timestamp(name.to_string(), value, labels, ts).expect("valid metric name")
    }

    /// Build a LogEvent with a deterministic timestamp for testing.
    fn make_log(
        severity: Severity,
        message: &str,
        label_pairs: &[(&str, &str)],
        fields: &[(&str, &str)],
        timestamp_ms: u64,
    ) -> LogEvent {
        let labels = Labels::from_pairs(label_pairs).expect("valid labels");
        let mut field_map = BTreeMap::new();
        for (k, v) in fields {
            field_map.insert(k.to_string(), v.to_string());
        }
        let ts = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
        LogEvent::with_timestamp(ts, severity, message.to_string(), labels, field_map)
    }

    /// Decode the first length-prefixed Metric from a buffer.
    fn decode_first_metric(buf: &[u8]) -> Metric {
        assert!(buf.len() >= 4, "buffer must contain at least length prefix");
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let proto_bytes = &buf[4..4 + len];
        Metric::decode(proto_bytes).expect("protobuf decode")
    }

    /// Decode the first length-prefixed LogRecord from a buffer.
    fn decode_first_log_record(buf: &[u8]) -> LogRecord {
        assert!(buf.len() >= 4, "buffer must contain at least length prefix");
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let proto_bytes = &buf[4..4 + len];
        LogRecord::decode(proto_bytes).expect("protobuf decode")
    }

    // -----------------------------------------------------------------------
    // Metric encoding: happy path
    // -----------------------------------------------------------------------

    #[test]
    fn encode_metric_produces_nonempty_bytes() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("cpu_usage", 42.5, &[("host", "server1")], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");
        assert!(
            buf.len() > 4,
            "encoded output must contain length prefix + protobuf"
        );
    }

    #[test]
    fn length_prefix_matches_protobuf_length() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("test_metric", 99.9, &[("env", "prod")], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(
            buf.len(),
            4 + len,
            "total buffer length must equal 4 (prefix) + declared protobuf length"
        );
    }

    #[test]
    fn metric_name_is_set_correctly() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("http_requests_total", 100.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        assert_eq!(metric.name, "http_requests_total");
    }

    #[test]
    fn metric_value_is_stored_as_double() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("gauge", 3.14, &[], 1_700_000_000_500);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge data, got {other:?}"),
        };
        assert_eq!(gauge.data_points.len(), 1);
        let dp = &gauge.data_points[0];
        match &dp.value {
            Some(number_data_point::Value::AsDouble(v)) => {
                assert!(
                    (v - 3.14).abs() < f64::EPSILON,
                    "value must be 3.14, got {v}"
                );
            }
            other => panic!("expected AsDouble, got {other:?}"),
        }
    }

    #[test]
    fn metric_timestamp_is_nanoseconds() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("ts_test", 1.0, &[], 1_700_000_000_500);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        // 1_700_000_000_500 ms = 1_700_000_000_500_000_000 ns
        assert_eq!(dp.time_unix_nano, 1_700_000_000_500_000_000);
    }

    #[test]
    fn metric_labels_become_data_point_attributes() {
        let encoder = OtlpEncoder::new();
        let event = make_metric(
            "my_metric",
            1.0,
            &[("zone", "eu1"), ("env", "prod"), ("host", "server1")],
            1_700_000_000_000,
        );
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        assert_eq!(dp.attributes.len(), 3, "must have 3 attributes");

        let attr_map: std::collections::HashMap<&str, &str> = dp
            .attributes
            .iter()
            .map(|kv| {
                let val = match &kv.value {
                    Some(AnyValue {
                        value: Some(any_value::Value::StringValue(s)),
                    }) => s.as_str(),
                    _ => "",
                };
                (kv.key.as_str(), val)
            })
            .collect();

        assert_eq!(attr_map.get("env"), Some(&"prod"));
        assert_eq!(attr_map.get("host"), Some(&"server1"));
        assert_eq!(attr_map.get("zone"), Some(&"eu1"));
    }

    #[test]
    fn metric_empty_labels_produces_no_attributes() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("bare_metric", 0.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        assert!(dp.attributes.is_empty(), "no labels means no attributes");
    }

    #[test]
    fn metric_zero_value_is_preserved() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("zero", 0.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        match &dp.value {
            Some(number_data_point::Value::AsDouble(v)) => {
                assert!(*v == 0.0, "zero value must be preserved, got {v}");
            }
            other => panic!("expected AsDouble, got {other:?}"),
        }
    }

    #[test]
    fn metric_large_float_is_preserved() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("big", f64::MAX, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        match &dp.value {
            Some(number_data_point::Value::AsDouble(v)) => {
                assert_eq!(*v, f64::MAX, "f64::MAX must be preserved");
            }
            other => panic!("expected AsDouble, got {other:?}"),
        }
    }

    #[test]
    fn metric_timestamp_at_epoch_zero() {
        let encoder = OtlpEncoder::new();
        let event = make_metric("epoch_test", 1.0, &[], 0);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");

        let metric = decode_first_metric(&buf);
        let gauge = match metric.data {
            Some(metric::Data::Gauge(g)) => g,
            other => panic!("expected Gauge, got {other:?}"),
        };
        let dp = &gauge.data_points[0];
        assert_eq!(dp.time_unix_nano, 0, "timestamp at epoch should be 0 ns");
    }

    #[test]
    fn multiple_metric_encodes_append_to_buffer() {
        let encoder = OtlpEncoder::new();
        let e1 = make_metric("metric_a", 1.0, &[], 1_700_000_000_000);
        let e2 = make_metric("metric_b", 2.0, &[], 1_700_000_001_000);

        let mut buf = Vec::new();
        encoder.encode_metric(&e1, &mut buf).expect("encode 1");
        let len_after_first = buf.len();
        encoder.encode_metric(&e2, &mut buf).expect("encode 2");
        assert!(buf.len() > len_after_first, "second encode should append");

        let metrics = parse_length_prefixed_metrics(&buf).expect("parse ok");
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].name, "metric_a");
        assert_eq!(metrics[1].name, "metric_b");
    }

    // -----------------------------------------------------------------------
    // Log encoding: happy path
    // -----------------------------------------------------------------------

    #[test]
    fn encode_log_produces_nonempty_bytes() {
        let encoder = OtlpEncoder::new();
        let event = make_log(Severity::Info, "hello", &[], &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");
        assert!(buf.len() > 4, "encoded log must contain prefix + protobuf");
    }

    #[test]
    fn log_message_body_is_set() {
        let encoder = OtlpEncoder::new();
        let event = make_log(
            Severity::Info,
            "request processed",
            &[],
            &[],
            1_700_000_000_000,
        );
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");

        let rec = decode_first_log_record(&buf);
        match rec.body {
            Some(AnyValue {
                value: Some(any_value::Value::StringValue(ref s)),
            }) => assert_eq!(s, "request processed"),
            other => panic!("expected string body, got {other:?}"),
        }
    }

    #[test]
    fn log_severity_number_is_correct() {
        let encoder = OtlpEncoder::new();
        let cases = [
            (Severity::Trace, 1),
            (Severity::Debug, 5),
            (Severity::Info, 9),
            (Severity::Warn, 13),
            (Severity::Error, 17),
            (Severity::Fatal, 21),
        ];
        for (severity, expected_num) in cases {
            let event = make_log(severity, "test", &[], &[], 1_700_000_000_000);
            let mut buf = Vec::new();
            encoder.encode_log(&event, &mut buf).expect("encode ok");
            let rec = decode_first_log_record(&buf);
            assert_eq!(
                rec.severity_number, expected_num,
                "severity {:?} should map to {}",
                severity, expected_num
            );
        }
    }

    #[test]
    fn log_severity_text_is_correct() {
        let encoder = OtlpEncoder::new();
        let event = make_log(Severity::Warn, "watch out", &[], &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");

        let rec = decode_first_log_record(&buf);
        assert_eq!(rec.severity_text, "WARN");
    }

    #[test]
    fn log_timestamp_is_nanoseconds() {
        let encoder = OtlpEncoder::new();
        let event = make_log(Severity::Info, "ts", &[], &[], 1_700_000_000_500);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");

        let rec = decode_first_log_record(&buf);
        assert_eq!(rec.time_unix_nano, 1_700_000_000_500_000_000);
    }

    #[test]
    fn log_labels_and_fields_become_attributes() {
        let encoder = OtlpEncoder::new();
        let event = make_log(
            Severity::Info,
            "msg",
            &[("host", "server1")],
            &[("latency", "50ms")],
            1_700_000_000_000,
        );
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");

        let rec = decode_first_log_record(&buf);
        assert_eq!(rec.attributes.len(), 2, "1 label + 1 field = 2 attributes");

        let attr_map: std::collections::HashMap<&str, &str> = rec
            .attributes
            .iter()
            .map(|kv| {
                let val = match &kv.value {
                    Some(AnyValue {
                        value: Some(any_value::Value::StringValue(s)),
                    }) => s.as_str(),
                    _ => "",
                };
                (kv.key.as_str(), val)
            })
            .collect();

        assert_eq!(attr_map.get("host"), Some(&"server1"));
        assert_eq!(attr_map.get("latency"), Some(&"50ms"));
    }

    #[test]
    fn log_empty_labels_and_fields_produces_no_attributes() {
        let encoder = OtlpEncoder::new();
        let event = make_log(Severity::Info, "bare", &[], &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).expect("encode ok");

        let rec = decode_first_log_record(&buf);
        assert!(rec.attributes.is_empty());
    }

    #[test]
    fn multiple_log_encodes_append_to_buffer() {
        let encoder = OtlpEncoder::new();
        let e1 = make_log(Severity::Info, "first", &[], &[], 1_700_000_000_000);
        let e2 = make_log(Severity::Error, "second", &[], &[], 1_700_000_001_000);

        let mut buf = Vec::new();
        encoder.encode_log(&e1, &mut buf).expect("encode 1");
        let len_after_first = buf.len();
        encoder.encode_log(&e2, &mut buf).expect("encode 2");
        assert!(buf.len() > len_after_first, "second encode should append");

        let records = parse_length_prefixed_log_records(&buf).expect("parse ok");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].severity_text, "INFO");
        assert_eq!(records[1].severity_text, "ERROR");
    }

    // -----------------------------------------------------------------------
    // Parser helpers
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metrics_empty_buffer_returns_empty_vec() {
        let result = parse_length_prefixed_metrics(&[]).expect("empty is ok");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_log_records_empty_buffer_returns_empty_vec() {
        let result = parse_length_prefixed_log_records(&[]).expect("empty is ok");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_metrics_truncated_prefix_returns_error() {
        let result = parse_length_prefixed_metrics(&[0x01, 0x02]);
        assert!(result.is_err(), "truncated prefix should be an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("truncated"),
            "error should mention truncation: {msg}"
        );
    }

    #[test]
    fn parse_metrics_truncated_body_returns_error() {
        // Length prefix says 100 bytes, but only 2 are available.
        let mut data = vec![100, 0, 0, 0];
        data.extend_from_slice(&[0x0A, 0x0B]);
        let result = parse_length_prefixed_metrics(&data);
        assert!(result.is_err(), "truncated body should be an error");
    }

    // -----------------------------------------------------------------------
    // Protobuf roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn export_metrics_service_request_roundtrips() {
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![string_kv("service.name", "sonda")],
                }),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(InstrumentationScope {
                        name: "sonda".to_string(),
                        version: "0.4.0".to_string(),
                    }),
                    metrics: vec![Metric {
                        name: "test_gauge".to_string(),
                        description: String::new(),
                        unit: String::new(),
                        data: Some(metric::Data::Gauge(Gauge {
                            data_points: vec![NumberDataPoint {
                                attributes: vec![],
                                time_unix_nano: 1_700_000_000_000_000_000,
                                value: Some(number_data_point::Value::AsDouble(42.0)),
                            }],
                        })),
                    }],
                }],
            }],
        };

        let mut encoded = Vec::new();
        req.encode(&mut encoded).expect("encode");
        let decoded = ExportMetricsServiceRequest::decode(encoded.as_slice()).expect("decode");
        assert_eq!(req, decoded);
    }

    #[test]
    fn export_logs_service_request_roundtrips() {
        let req = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![string_kv("service.name", "sonda")],
                }),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope {
                        name: "sonda".to_string(),
                        version: "0.4.0".to_string(),
                    }),
                    log_records: vec![LogRecord {
                        time_unix_nano: 1_700_000_000_000_000_000,
                        severity_number: 9,
                        severity_text: "INFO".to_string(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("test".to_string())),
                        }),
                        attributes: vec![],
                    }],
                }],
            }],
        };

        let mut encoded = Vec::new();
        req.encode(&mut encoded).expect("encode");
        let decoded = ExportLogsServiceRequest::decode(encoded.as_slice()).expect("decode");
        assert_eq!(req, decoded);
    }

    // -----------------------------------------------------------------------
    // Severity mapping
    // -----------------------------------------------------------------------

    #[test]
    fn severity_to_number_maps_all_variants() {
        assert_eq!(severity_to_number(Severity::Trace), 1);
        assert_eq!(severity_to_number(Severity::Debug), 5);
        assert_eq!(severity_to_number(Severity::Info), 9);
        assert_eq!(severity_to_number(Severity::Warn), 13);
        assert_eq!(severity_to_number(Severity::Error), 17);
        assert_eq!(severity_to_number(Severity::Fatal), 21);
    }

    // -----------------------------------------------------------------------
    // Send + Sync contract
    // -----------------------------------------------------------------------

    #[test]
    fn otlp_encoder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OtlpEncoder>();
    }

    #[test]
    fn default_creates_valid_encoder() {
        let encoder = OtlpEncoder::default();
        let event = make_metric("test", 1.0, &[], 1_700_000_000_000);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encode ok");
        assert!(!buf.is_empty());
    }
}
