//! JSON Lines encoder.
//!
//! Encodes metric and log events as newline-delimited JSON (NDJSON). Each line is a
//! self-contained JSON object, making the output compatible with Elasticsearch, Loki,
//! and generic HTTP ingest endpoints.
//!
//! Metric output format:
//! ```text
//! {"name":"metric","value":1.0,"labels":{"k":"v"},"timestamp":"2026-03-20T12:00:00.000Z"}
//! ```
//!
//! Log output format:
//! ```text
//! {"timestamp":"2026-03-20T12:00:00.000Z","severity":"info","message":"Request from 10.0.0.1","labels":{"device":"wlan0"},"fields":{"ip":"10.0.0.1","endpoint":"/api"}}
//! ```
//!
//! Timestamp uses RFC 3339 / ISO 8601 format with millisecond precision. Formatted without
//! pulling in `chrono` — derived directly from [`std::time::SystemTime`] arithmetic.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::SondaError;

use super::Encoder;

/// Encodes [`MetricEvent`]s as newline-delimited JSON (JSON Lines format).
///
/// Each call to [`encode_metric`](Self::encode_metric) appends one complete JSON object
/// followed by a newline character to the caller-provided buffer.
///
/// No per-event heap allocations beyond what `serde_json::to_writer` needs internally.
/// All invariant content is pre-built at construction time — for `JsonLines` this
/// struct has no per-scenario state, so construction is essentially free.
pub struct JsonLines;

impl JsonLines {
    /// Create a new `JsonLines` encoder.
    pub fn new() -> Self {
        Self
    }
}

impl Default for JsonLines {
    fn default() -> Self {
        Self::new()
    }
}

/// Intermediate serde-serializable representation of a metric event.
///
/// Uses `BTreeMap` for labels so the JSON field order is consistent and deterministic.
#[derive(Serialize)]
struct JsonMetric<'a> {
    name: &'a str,
    value: f64,
    labels: BTreeMap<&'a str, &'a str>,
    timestamp: String,
}

/// Intermediate serde-serializable representation of a log event.
///
/// Field order matches the spec: timestamp, severity, message, labels, fields.
/// Uses `BTreeMap` for labels and fields so the JSON field order is consistent and deterministic.
#[derive(Serialize)]
struct JsonLog<'a> {
    timestamp: String,
    severity: &'a str,
    message: &'a str,
    labels: BTreeMap<&'a str, &'a str>,
    fields: BTreeMap<&'a str, &'a str>,
}

impl Encoder for JsonLines {
    /// Encode a metric event as a JSON object and append it to `buf`, followed by `\n`.
    ///
    /// Uses `serde_json::to_writer` to write directly into the caller-provided buffer,
    /// avoiding an intermediate `String` allocation.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let timestamp = super::format_rfc3339_millis(event.timestamp)?;

        let labels: BTreeMap<&str, &str> = event
            .labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let record = JsonMetric {
            name: &event.name,
            value: event.value,
            labels,
            timestamp,
        };

        serde_json::to_writer(&mut *buf, &record)
            .map_err(|e| SondaError::Encoder(format!("JSON serialization failed: {e}")))?;

        buf.push(b'\n');

        Ok(())
    }

    /// Encode a log event as a JSON object and append it to `buf`, followed by `\n`.
    ///
    /// Output format: `{"timestamp":"...","severity":"info","message":"...","fields":{...}}`
    ///
    /// Uses `serde_json::to_writer` to write directly into the caller-provided buffer.
    fn encode_log(&self, event: &LogEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let timestamp = super::format_rfc3339_millis(event.timestamp)?;

        // Serialize severity to its lowercase string representation using serde.
        let severity_str = match event.severity {
            crate::model::log::Severity::Trace => "trace",
            crate::model::log::Severity::Debug => "debug",
            crate::model::log::Severity::Info => "info",
            crate::model::log::Severity::Warn => "warn",
            crate::model::log::Severity::Error => "error",
            crate::model::log::Severity::Fatal => "fatal",
        };

        let labels: BTreeMap<&str, &str> = event
            .labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let fields: BTreeMap<&str, &str> = event
            .fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let record = JsonLog {
            timestamp,
            severity: severity_str,
            message: &event.message,
            labels,
            fields,
        };

        serde_json::to_writer(&mut *buf, &record)
            .map_err(|e| SondaError::Encoder(format!("JSON serialization failed: {e}")))?;

        buf.push(b'\n');

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::metric::{Labels, MetricEvent};
    use std::time::{Duration, UNIX_EPOCH};

    /// Build a MetricEvent with a fixed timestamp for deterministic tests.
    fn make_event(
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
        timestamp: std::time::SystemTime,
    ) -> MetricEvent {
        let labels = Labels::from_pairs(labels).unwrap();
        MetricEvent::with_timestamp(name.to_string(), value, labels, timestamp).unwrap()
    }

    // --- Happy path: valid JSON output ---

    #[test]
    fn output_is_valid_json_parseable_by_serde_json() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("cpu_usage", 0.75, &[("host", "srv1")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = String::from_utf8(buf).unwrap();
        let line = line.trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).expect("must be valid JSON");
        assert!(parsed.is_object(), "output must be a JSON object");
    }

    // --- Roundtrip: all fields survive encode → parse ---

    #[test]
    fn roundtrip_name_matches_original_event() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("http_requests", 42.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["name"], "http_requests");
    }

    #[test]
    fn roundtrip_value_matches_original_event() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("latency", 3.14, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!((parsed["value"].as_f64().unwrap() - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn roundtrip_labels_match_original_event() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("metric", 1.0, &[("env", "prod"), ("host", "srv1")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["labels"]["env"], "prod");
        assert_eq!(parsed["labels"]["host"], "srv1");
    }

    #[test]
    fn roundtrip_timestamp_matches_original_event() {
        // Unix epoch 1700000000.000 = 2023-11-14T22:13:20.000Z
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["timestamp"], "2023-11-14T22:13:20.000Z",
            "timestamp must be RFC 3339 with millisecond precision"
        );
    }

    // --- Empty labels ---

    #[test]
    fn empty_labels_produces_empty_json_object() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["labels"],
            serde_json::json!({}),
            "empty labels must be an empty JSON object"
        );
    }

    // --- Newline termination ---

    #[test]
    fn each_encoded_line_ends_with_newline() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        assert_eq!(
            *buf.last().unwrap(),
            b'\n',
            "line must terminate with newline"
        );
    }

    #[test]
    fn multiple_events_each_end_with_newline() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        for i in 0..3u64 {
            let event = make_event("up", i as f64, &[], ts + Duration::from_millis(i));
            encoder.encode_metric(&event, &mut buf).unwrap();
        }

        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "must produce exactly 3 lines");
        // Verify each line is valid JSON
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("each line must be valid JSON");
        }
    }

    // --- Buffer accumulation ---

    #[test]
    fn multiple_encodes_accumulate_in_same_buffer() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();

        let event1 = make_event("metric_a", 1.0, &[], ts);
        let event2 = make_event("metric_b", 2.0, &[], ts + Duration::from_millis(1));
        encoder.encode_metric(&event1, &mut buf).unwrap();
        encoder.encode_metric(&event2, &mut buf).unwrap();

        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.contains("metric_a"),
            "buffer must contain first metric name"
        );
        assert!(
            text.contains("metric_b"),
            "buffer must contain second metric name"
        );
    }

    // --- Timestamp format ---

    #[test]
    fn timestamp_uses_rfc3339_format_with_millisecond_precision() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        let ts_str = parsed["timestamp"].as_str().unwrap();

        // Must end with Z (UTC), have T separator, and contain milliseconds
        assert!(ts_str.ends_with('Z'), "timestamp must end with Z: {ts_str}");
        assert!(
            ts_str.contains('T'),
            "timestamp must contain T separator: {ts_str}"
        );
        // Must match pattern YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(
            ts_str.len(),
            24,
            "timestamp must be exactly 24 chars: {ts_str}"
        );
        assert!(
            ts_str.contains(".123"),
            "milliseconds must be .123: {ts_str}"
        );
    }

    #[test]
    fn timestamp_at_unix_epoch_formats_correctly() {
        let ts = UNIX_EPOCH;
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["timestamp"], "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn timestamp_with_zero_milliseconds_shows_dot_zero_zero_zero() {
        // Exactly 1 second past epoch, no sub-second component
        let ts = UNIX_EPOCH + Duration::from_secs(1);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["timestamp"], "1970-01-01T00:00:01.000Z");
    }

    // --- Regression anchor: hardcoded expected byte string ---

    #[test]
    fn regression_anchor_single_label_exact_output() {
        // Timestamp: 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_event("http_requests", 100.0, &[("endpoint", "api")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let output = String::from_utf8(buf).unwrap();
        // Verify the exact JSON structure (field order must be deterministic)
        assert_eq!(
            output,
            "{\"name\":\"http_requests\",\"value\":100.0,\"labels\":{\"endpoint\":\"api\"},\"timestamp\":\"2026-03-20T12:00:00.000Z\"}\n"
        );
    }

    #[test]
    fn regression_anchor_no_labels_exact_output() {
        // 2023-11-14T22:13:20.000Z = 1700000000 seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("up", 1.0, &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "{\"name\":\"up\",\"value\":1.0,\"labels\":{},\"timestamp\":\"2023-11-14T22:13:20.000Z\"}\n"
        );
    }

    #[test]
    fn regression_anchor_multiple_labels_sorted_in_output() {
        // Labels must appear sorted by key in the output
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event(
            "cpu",
            0.5,
            &[("zone", "eu1"), ("host", "srv1"), ("env", "prod")],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "{\"name\":\"cpu\",\"value\":0.5,\"labels\":{\"env\":\"prod\",\"host\":\"srv1\",\"zone\":\"eu1\"},\"timestamp\":\"2023-11-14T22:13:20.000Z\"}\n"
        );
    }

    // --- JSON field order consistency ---

    #[test]
    fn json_fields_appear_in_consistent_order() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("metric", 1.0, &[("k", "v")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let output = String::from_utf8(buf).unwrap();
        let line = output.trim_end_matches('\n');

        // Verify field order: name, value, labels, timestamp
        let name_pos = line.find("\"name\"").unwrap();
        let value_pos = line.find("\"value\"").unwrap();
        let labels_pos = line.find("\"labels\"").unwrap();
        let timestamp_pos = line.find("\"timestamp\"").unwrap();

        assert!(name_pos < value_pos, "name must come before value");
        assert!(value_pos < labels_pos, "value must come before labels");
        assert!(
            labels_pos < timestamp_pos,
            "labels must come before timestamp"
        );
    }

    // --- Send + Sync contract ---

    #[test]
    fn json_lines_encoder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<JsonLines>();
    }

    // --- EncoderConfig factory wiring ---

    #[test]
    fn encoder_config_json_lines_creates_encoder_via_factory() {
        use crate::encoder::{create_encoder, EncoderConfig};

        let config = EncoderConfig::JsonLines;
        let encoder = create_encoder(&config);

        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let event = make_event("up", 1.0, &[], ts);
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();

        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["name"], "up");
    }

    // --- format_rfc3339_millis direct tests ---

    #[test]
    fn format_rfc3339_millis_epoch_returns_correct_string() {
        let result = super::super::format_rfc3339_millis(UNIX_EPOCH).unwrap();
        assert_eq!(result, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_known_timestamp_2026_03_20_returns_correct_string() {
        // 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2026-03-20T12:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_preserves_milliseconds() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_456);
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert!(
            result.ends_with(".456Z"),
            "must end with .456Z but got: {result}"
        );
    }

    #[test]
    fn format_rfc3339_millis_midnight_boundary() {
        // End of day: 23:59:59.999
        let ts = UNIX_EPOCH + Duration::from_millis(86_399_999);
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "1970-01-01T23:59:59.999Z");
    }

    #[test]
    fn format_rfc3339_millis_start_of_day_plus_one_second() {
        let ts = UNIX_EPOCH + Duration::from_secs(86400); // 1970-01-02T00:00:00.000Z
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "1970-01-02T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_leap_year_feb_29() {
        // 2024 is a leap year. 2024-02-29T00:00:00.000Z
        // Days from epoch to 2024-02-29: calculate via known timestamp
        // 2024-02-29T00:00:00Z = 1709164800 seconds
        let ts = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_end_of_year_dec_31() {
        // 2023-12-31T23:59:59.999Z = 1704067199.999
        let ts = UNIX_EPOCH + Duration::from_millis(1_704_067_199_999);
        let result = super::super::format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2023-12-31T23:59:59.999Z");
    }

    // =========================================================================
    // Slice 2.3: encode_log() tests
    // =========================================================================

    /// Build a LogEvent with a fixed timestamp for deterministic tests.
    fn make_log_event(
        severity: crate::model::log::Severity,
        message: &str,
        fields: &[(&str, &str)],
        ts: std::time::SystemTime,
    ) -> crate::model::log::LogEvent {
        let mut map = std::collections::BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v.to_string());
        }
        crate::model::log::LogEvent::with_timestamp(
            ts,
            severity,
            message.to_string(),
            crate::model::metric::Labels::default(),
            map,
        )
    }

    // --- encode_log: output is valid JSON ---

    #[test]
    fn encode_log_produces_valid_json() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello world", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        let line = line.trim_end_matches('\n');
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("encode_log output must be valid JSON");
        assert!(parsed.is_object(), "output must be a JSON object");
    }

    // --- encode_log: all required fields are present ---

    #[test]
    fn encode_log_includes_timestamp_field() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            parsed.get("timestamp").is_some(),
            "encode_log output must include 'timestamp' field"
        );
    }

    #[test]
    fn encode_log_includes_severity_field() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Warn, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            parsed.get("severity").is_some(),
            "encode_log output must include 'severity' field"
        );
    }

    #[test]
    fn encode_log_includes_message_field() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "test message here", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            parsed.get("message").is_some(),
            "encode_log output must include 'message' field"
        );
    }

    #[test]
    fn encode_log_includes_fields_field() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[("ip", "10.0.0.1")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            parsed.get("fields").is_some(),
            "encode_log output must include 'fields' field"
        );
    }

    // --- encode_log: severity is lowercase ---

    #[test]
    fn encode_log_severity_info_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["severity"], "info",
            "severity must be lowercase 'info'"
        );
    }

    #[test]
    fn encode_log_severity_error_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Error, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["severity"], "error");
    }

    #[test]
    fn encode_log_severity_warn_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Warn, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["severity"], "warn");
    }

    #[test]
    fn encode_log_severity_trace_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Trace, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["severity"], "trace");
    }

    #[test]
    fn encode_log_severity_debug_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Debug, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["severity"], "debug");
    }

    #[test]
    fn encode_log_severity_fatal_is_lowercase() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Fatal, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["severity"], "fatal");
    }

    // --- encode_log: roundtrip — all fields survive encode → parse ---

    #[test]
    fn encode_log_roundtrip_message_matches_original() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "Request from 10.0.0.1", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["message"], "Request from 10.0.0.1");
    }

    #[test]
    fn encode_log_roundtrip_fields_match_original() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(
            Severity::Info,
            "req",
            &[("ip", "10.0.0.1"), ("endpoint", "/api")],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["fields"]["ip"], "10.0.0.1");
        assert_eq!(parsed["fields"]["endpoint"], "/api");
    }

    #[test]
    fn encode_log_roundtrip_timestamp_matches_original() {
        use crate::model::log::Severity;
        // 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["timestamp"], "2026-03-20T12:00:00.000Z",
            "roundtrip timestamp must match"
        );
    }

    // --- encode_log: empty fields produces empty JSON object ---

    #[test]
    fn encode_log_empty_fields_produces_empty_json_object() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["fields"],
            serde_json::json!({}),
            "empty fields must serialize as empty JSON object"
        );
    }

    // --- encode_log: line ends with newline ---

    #[test]
    fn encode_log_line_ends_with_newline() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        assert_eq!(
            *buf.last().unwrap(),
            b'\n',
            "encode_log line must end with newline"
        );
    }

    // --- encode_log: field order — timestamp, severity, message, fields ---

    #[test]
    fn encode_log_fields_appear_in_spec_order() {
        // Spec: timestamp, severity, message, labels, fields
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "msg", &[("k", "v")], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let line = output.trim_end_matches('\n');
        let ts_pos = line.find("\"timestamp\"").unwrap();
        let sev_pos = line.find("\"severity\"").unwrap();
        let msg_pos = line.find("\"message\"").unwrap();
        let labels_pos = line.find("\"labels\"").unwrap();
        let fields_pos = line.find("\"fields\"").unwrap();
        assert!(ts_pos < sev_pos, "timestamp must come before severity");
        assert!(sev_pos < msg_pos, "severity must come before message");
        assert!(msg_pos < labels_pos, "message must come before labels");
        assert!(labels_pos < fields_pos, "labels must come before fields");
    }

    // --- encode_log: regression anchor — exact byte output ---

    #[test]
    fn encode_log_regression_anchor_simple_info_event() {
        use crate::model::log::Severity;
        // 2026-03-20T12:00:00.000Z
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "Request from 10.0.0.1", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "{\"timestamp\":\"2026-03-20T12:00:00.000Z\",\"severity\":\"info\",\"message\":\"Request from 10.0.0.1\",\"labels\":{},\"fields\":{}}\n"
        );
    }

    #[test]
    fn encode_log_regression_anchor_with_fields() {
        use crate::model::log::Severity;
        // Fields must be sorted by key (BTreeMap)
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(
            Severity::Error,
            "db timeout",
            &[("endpoint", "/api"), ("ip", "10.0.0.1")],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "{\"timestamp\":\"2026-03-20T12:00:00.000Z\",\"severity\":\"error\",\"message\":\"db timeout\",\"labels\":{},\"fields\":{\"endpoint\":\"/api\",\"ip\":\"10.0.0.1\"}}\n"
        );
    }

    // --- encode_log: prometheus encoder still returns "not supported" error ---

    // --- encode_log: labels in JSON output ---

    /// Build a LogEvent with labels and a fixed timestamp for deterministic tests.
    fn make_log_event_with_labels(
        severity: crate::model::log::Severity,
        message: &str,
        labels: &[(&str, &str)],
        fields: &[(&str, &str)],
        ts: std::time::SystemTime,
    ) -> crate::model::log::LogEvent {
        let mut field_map = std::collections::BTreeMap::new();
        for (k, v) in fields {
            field_map.insert(k.to_string(), v.to_string());
        }
        let label_set = crate::model::metric::Labels::from_pairs(labels).unwrap();
        crate::model::log::LogEvent::with_timestamp(
            ts,
            severity,
            message.to_string(),
            label_set,
            field_map,
        )
    }

    #[test]
    fn encode_log_with_labels_includes_labels_in_json() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "labeled event",
            &[("device", "wlan0"), ("hostname", "router_01")],
            &[],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["labels"]["device"], "wlan0");
        assert_eq!(parsed["labels"]["hostname"], "router_01");
    }

    #[test]
    fn encode_log_labels_are_sorted_by_key() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        // Labels inserted in reverse alphabetical order; BTreeMap must sort them.
        let event = make_log_event_with_labels(
            Severity::Info,
            "sorted labels",
            &[("zone", "eu1"), ("env", "prod"), ("app", "sonda")],
            &[],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let line = output.trim_end_matches('\n');

        // Verify key order in the raw JSON string (BTreeMap guarantees sort)
        let app_pos = line.find("\"app\"").unwrap();
        let env_pos = line.find("\"env\"").unwrap();
        let zone_pos = line.find("\"zone\"").unwrap();
        assert!(
            app_pos < env_pos,
            "app must come before env in sorted labels"
        );
        assert!(
            env_pos < zone_pos,
            "env must come before zone in sorted labels"
        );
    }

    #[test]
    fn encode_log_with_empty_labels_produces_empty_labels_object() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "no labels", &[], ts);
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed["labels"],
            serde_json::json!({}),
            "empty labels must serialize as empty JSON object"
        );
    }

    #[test]
    fn encode_log_regression_anchor_with_labels_exact_output() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "Request from 10.0.0.1",
            &[("device", "wlan0")],
            &[("ip", "10.0.0.1")],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "{\"timestamp\":\"2026-03-20T12:00:00.000Z\",\"severity\":\"info\",\"message\":\"Request from 10.0.0.1\",\"labels\":{\"device\":\"wlan0\"},\"fields\":{\"ip\":\"10.0.0.1\"}}\n"
        );
    }

    #[test]
    fn encode_log_with_labels_and_fields_both_present() {
        use crate::model::log::Severity;
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Error,
            "timeout",
            &[("env", "prod")],
            &[("endpoint", "/api")],
            ts,
        );
        let encoder = JsonLines::new();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end_matches('\n');
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        // Both labels and fields must be present and correct
        assert_eq!(parsed["labels"]["env"], "prod");
        assert_eq!(parsed["fields"]["endpoint"], "/api");
    }

    // --- encode_log: prometheus encoder still returns "not supported" error ---

    #[test]
    fn prometheus_encoder_encode_log_still_returns_not_supported_after_slice_2_3() {
        use crate::encoder::{create_encoder, EncoderConfig};
        let encoder = create_encoder(&EncoderConfig::PrometheusText);
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(crate::model::log::Severity::Info, "should fail", &[], ts);
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        assert!(
            result.is_err(),
            "prometheus encoder must still return error for encode_log"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported"),
            "error must mention 'not supported', got: {msg}"
        );
        assert!(buf.is_empty(), "buffer must remain empty on error");
    }
}
