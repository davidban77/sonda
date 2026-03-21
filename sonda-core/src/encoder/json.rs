//! JSON Lines encoder.
//!
//! Encodes metric events as newline-delimited JSON (NDJSON). Each line is a self-contained
//! JSON object, making the output compatible with Elasticsearch, Loki, and generic HTTP
//! ingest endpoints.
//!
//! Output format:
//! ```text
//! {"name":"metric","value":1.0,"labels":{"k":"v"},"timestamp":"2026-03-20T12:00:00.000Z"}
//! ```
//!
//! Timestamp uses RFC 3339 / ISO 8601 format with millisecond precision. Formatted without
//! pulling in `chrono` — derived directly from [`std::time::SystemTime`] arithmetic.

use std::collections::BTreeMap;
use std::time::UNIX_EPOCH;

use serde::Serialize;

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

/// Format a [`std::time::SystemTime`] as an RFC 3339 string with millisecond precision.
///
/// Produces strings of the form `2026-03-20T12:00:00.000Z`. Computed entirely from
/// `UNIX_EPOCH` arithmetic — no external crate required.
///
/// Returns an error if the timestamp predates the Unix epoch.
fn format_rfc3339_millis(ts: std::time::SystemTime) -> Result<String, SondaError> {
    let duration = ts
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?;

    let total_secs = duration.as_secs();
    let millis = duration.subsec_millis();

    // Decompose total_secs into calendar fields.
    // Algorithm: Gregorian calendar conversion from Unix timestamp.
    // Reference: https://howardhinnant.github.io/date_algorithms.html (civil_from_days)

    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;

    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // civil_from_days: converts days since Unix epoch to (year, month, day)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    ))
}

impl Encoder for JsonLines {
    /// Encode a metric event as a JSON object and append it to `buf`, followed by `\n`.
    ///
    /// Uses `serde_json::to_writer` to write directly into the caller-provided buffer,
    /// avoiding an intermediate `String` allocation.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let timestamp = format_rfc3339_millis(event.timestamp)?;

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
        let result = format_rfc3339_millis(UNIX_EPOCH).unwrap();
        assert_eq!(result, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_known_timestamp_2026_03_20_returns_correct_string() {
        // 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let result = format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2026-03-20T12:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_preserves_milliseconds() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_456);
        let result = format_rfc3339_millis(ts).unwrap();
        assert!(
            result.ends_with(".456Z"),
            "must end with .456Z but got: {result}"
        );
    }

    #[test]
    fn format_rfc3339_millis_midnight_boundary() {
        // End of day: 23:59:59.999
        let ts = UNIX_EPOCH + Duration::from_millis(86_399_999);
        let result = format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "1970-01-01T23:59:59.999Z");
    }

    #[test]
    fn format_rfc3339_millis_start_of_day_plus_one_second() {
        let ts = UNIX_EPOCH + Duration::from_secs(86400); // 1970-01-02T00:00:00.000Z
        let result = format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "1970-01-02T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_leap_year_feb_29() {
        // 2024 is a leap year. 2024-02-29T00:00:00.000Z
        // Days from epoch to 2024-02-29: calculate via known timestamp
        // 2024-02-29T00:00:00Z = 1709164800 seconds
        let ts = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        let result = format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_end_of_year_dec_31() {
        // 2023-12-31T23:59:59.999Z = 1704067199.999
        let ts = UNIX_EPOCH + Duration::from_millis(1_704_067_199_999);
        let result = format_rfc3339_millis(ts).unwrap();
        assert_eq!(result, "2023-12-31T23:59:59.999Z");
    }
}
