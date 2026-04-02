//! Prometheus text exposition format encoder.
//!
//! Implements the Prometheus text format version 0.0.4.
//! Reference: <https://prometheus.io/docs/instrumenting/exposition_formats/>

use std::io::Write as _;
use std::time::UNIX_EPOCH;

use crate::model::metric::MetricEvent;
use crate::{EncoderError, SondaError};

use super::Encoder;

/// Encodes [`MetricEvent`]s into Prometheus text exposition format (version 0.0.4).
///
/// Output format for a metric with labels:
/// ```text
/// metric_name{label1="val1",label2="val2"} value timestamp_ms\n
/// ```
///
/// Output format for a metric with no labels:
/// ```text
/// metric_name value timestamp_ms\n
/// ```
///
/// The timestamp is in milliseconds since the Unix epoch (integer).
///
/// Label values are escaped: `\` → `\\`, `"` → `\"`, newline → `\n`.
///
/// When `precision` is set, metric values are formatted to the specified number
/// of decimal places (e.g., precision=2 formats `99.60573` as `99.61`).
pub struct PrometheusText {
    /// Optional decimal precision for metric values.
    precision: Option<u8>,
}

impl PrometheusText {
    /// Create a new `PrometheusText` encoder.
    ///
    /// `precision` optionally limits the number of decimal places in metric values.
    /// `None` preserves full `f64` precision (default behavior).
    pub fn new(precision: Option<u8>) -> Self {
        Self { precision }
    }
}

impl Default for PrometheusText {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Escape a label value per Prometheus exposition format rules.
///
/// Escapes: `\` → `\\`, `"` → `\"`, newline (`\n`) → literal `\n` (two characters).
fn escape_label_value(value: &str, buf: &mut Vec<u8>) {
    for byte in value.bytes() {
        match byte {
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\n' => buf.extend_from_slice(b"\\n"),
            other => buf.push(other),
        }
    }
}

impl Encoder for PrometheusText {
    /// Encode a metric event into Prometheus text exposition format.
    ///
    /// Writes the formatted line into `buf`. Bytes are appended; the buffer is not
    /// cleared before writing. Writes into the caller-provided buffer without
    /// additional heap allocations.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        // Metric name
        buf.extend_from_slice(event.name.as_bytes());

        // Labels (only if non-empty)
        if !event.labels.is_empty() {
            buf.push(b'{');
            let mut first = true;
            for (key, value) in event.labels.iter() {
                if !first {
                    buf.push(b',');
                }
                first = false;
                buf.extend_from_slice(key.as_bytes());
                buf.extend_from_slice(b"=\"");
                escape_label_value(value, buf);
                buf.push(b'"');
            }
            buf.push(b'}');
        }

        // Space before value
        buf.push(b' ');

        // Value: write f64, optionally with fixed decimal precision
        super::write_value(buf, event.value, self.precision);

        // Timestamp in milliseconds since epoch
        let timestamp_ms = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(EncoderError::TimestampBeforeEpoch(e)))?
            .as_millis();

        buf.push(b' ');
        write!(buf, "{timestamp_ms}").expect("write to Vec<u8> is infallible");

        buf.push(b'\n');

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::metric::{Labels, MetricEvent};
    use std::time::{Duration, UNIX_EPOCH};

    /// Helper: build a MetricEvent with a fixed timestamp for deterministic tests.
    fn make_event(name: &str, value: f64, labels: Labels, timestamp_ms: u64) -> MetricEvent {
        let ts = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
        MetricEvent::with_timestamp(name.to_string(), value, labels, ts).unwrap()
    }

    /// Helper: encode one event and return the result as a UTF-8 String.
    fn encode_to_string(event: &MetricEvent) -> String {
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        enc.encode_metric(event, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    // --- Happy path: no labels ---

    #[test]
    fn no_labels_omits_braces() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000);
        let output = encode_to_string(&event);
        assert_eq!(output, "up 1 1000000\n");
    }

    #[test]
    fn no_labels_format_has_no_curly_braces() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("requests_total", 42.0, labels, 0);
        let output = encode_to_string(&event);
        assert!(
            !output.contains('{'),
            "output should not contain braces: {output:?}"
        );
        assert!(
            !output.contains('}'),
            "output should not contain braces: {output:?}"
        );
    }

    // --- Happy path: labels present ---

    #[test]
    fn single_label_produces_correct_format() {
        let labels = Labels::from_pairs(&[("host", "server1")]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000);
        let output = encode_to_string(&event);
        assert_eq!(output, "up{host=\"server1\"} 1 1000000\n");
    }

    #[test]
    fn two_labels_sorted_by_key_comma_separated() {
        // Insert in reverse alphabetical order — BTreeMap must sort them.
        let labels = Labels::from_pairs(&[("zone", "eu1"), ("host", "server1")]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000);
        let output = encode_to_string(&event);
        // "host" < "zone" alphabetically
        assert_eq!(output, "up{host=\"server1\",zone=\"eu1\"} 1 1000000\n");
    }

    #[test]
    fn labels_are_always_sorted_by_key() {
        let labels =
            Labels::from_pairs(&[("zone", "eu1"), ("env", "prod"), ("host", "t0-a1")]).unwrap();
        let event = make_event("metric", 0.0, labels, 0);
        let output = encode_to_string(&event);
        // env < host < zone
        assert!(
            output.starts_with("metric{env=\"prod\",host=\"t0-a1\",zone=\"eu1\"}"),
            "unexpected output: {output:?}"
        );
    }

    // --- Regression anchor: hardcoded expected bytes ---

    #[test]
    fn regression_anchor_exact_byte_output_no_labels() {
        let labels = Labels::from_pairs(&[]).unwrap();
        // Timestamp: exactly 1_700_000_000_000 ms (i.e. 1_700_000_000 seconds since epoch)
        let event = make_event("http_requests_total", 123.456, labels, 1_700_000_000_000);
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        assert_eq!(buf, b"http_requests_total 123.456 1700000000000\n");
    }

    #[test]
    fn regression_anchor_exact_byte_output_with_labels() {
        let labels = Labels::from_pairs(&[("hostname", "t0-a1"), ("zone", "eu1")]).unwrap();
        let event = make_event("interface_oper_state", 1.0, labels, 1_700_000_000_000);
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        assert_eq!(
            buf,
            b"interface_oper_state{hostname=\"t0-a1\",zone=\"eu1\"} 1 1700000000000\n"
        );
    }

    // --- Timestamp is milliseconds since epoch (integer, not float) ---

    #[test]
    fn timestamp_is_integer_milliseconds_since_epoch() {
        let labels = Labels::from_pairs(&[]).unwrap();
        // 1500 ms = 1.5 seconds since epoch
        let event = make_event("up", 1.0, labels, 1500);
        let output = encode_to_string(&event);
        // Must end with "1 1500\n" — timestamp is an integer
        assert!(
            output.ends_with(" 1500\n"),
            "timestamp should be integer ms: {output:?}"
        );
    }

    #[test]
    fn timestamp_at_epoch_zero_is_zero() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 0);
        let output = encode_to_string(&event);
        assert!(
            output.ends_with(" 0\n"),
            "timestamp at epoch should be 0: {output:?}"
        );
    }

    #[test]
    fn timestamp_does_not_include_decimal_point() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_234_567_890_123);
        let output = encode_to_string(&event);
        // Extract the timestamp portion (last token before newline)
        let ts_str = output
            .trim_end_matches('\n')
            .split_whitespace()
            .last()
            .unwrap();
        assert!(
            !ts_str.contains('.'),
            "timestamp must not contain decimal point: {ts_str:?}"
        );
    }

    // --- Label value escaping ---

    #[test]
    fn label_value_with_double_quote_is_escaped() {
        let labels = Labels::from_pairs(&[("label", "say \"hi\"")]).unwrap();
        let event = make_event("metric", 1.0, labels, 0);
        let output = encode_to_string(&event);
        assert!(
            output.contains(r#"label="say \"hi\"""#),
            "double quote not escaped: {output:?}"
        );
    }

    #[test]
    fn label_value_with_backslash_is_escaped() {
        let labels = Labels::from_pairs(&[("path", r"C:\Users\bob")]).unwrap();
        let event = make_event("metric", 1.0, labels, 0);
        let output = encode_to_string(&event);
        // C:\Users\bob should become C:\\Users\\bob in the output
        assert!(
            output.contains(r#"path="C:\\Users\\bob""#),
            "backslash not escaped: {output:?}"
        );
    }

    #[test]
    fn label_value_with_newline_is_escaped() {
        let labels = Labels::from_pairs(&[("msg", "line1\nline2")]).unwrap();
        let event = make_event("metric", 1.0, labels, 0);
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // The literal newline inside the label value must be rendered as \n (two chars)
        assert!(
            output.contains(r#"msg="line1\nline2""#),
            "newline not escaped: {output:?}"
        );
        // The encoded line itself should have exactly one newline — the trailing one.
        assert_eq!(
            output.chars().filter(|&c| c == '\n').count(),
            1,
            "should have exactly one newline (the trailing one): {output:?}"
        );
    }

    #[test]
    fn label_value_with_all_three_escape_sequences() {
        // backslash, double-quote, newline all in one value
        let value = "a\\b\"c\nd";
        let labels = Labels::from_pairs(&[("v", value)]).unwrap();
        let event = make_event("metric", 1.0, labels, 0);
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains(r#"v="a\\b\"c\nd""#),
            "combined escaping incorrect: {output:?}"
        );
    }

    #[test]
    fn label_value_with_no_special_chars_is_not_escaped() {
        let labels = Labels::from_pairs(&[("env", "production")]).unwrap();
        let event = make_event("metric", 1.0, labels, 0);
        let output = encode_to_string(&event);
        assert!(
            output.contains(r#"env="production""#),
            "plain value unexpectedly altered: {output:?}"
        );
    }

    // --- Pre-epoch timestamp error ---

    #[test]
    fn pre_epoch_timestamp_returns_encoder_error() {
        // SystemTime::UNIX_EPOCH - 1 second is before epoch
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event =
            MetricEvent::with_timestamp("up".to_string(), 1.0, labels, before_epoch).unwrap();
        let enc = PrometheusText::new(None);
        let mut buf = Vec::new();
        let result = enc.encode_metric(&event, &mut buf);
        assert!(
            matches!(result, Err(SondaError::Encoder(_))),
            "expected Encoder error for pre-epoch timestamp, got: {result:?}"
        );
    }

    // --- Buffer appending behaviour ---

    #[test]
    fn encode_appends_to_existing_buffer_content() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 0);
        let enc = PrometheusText::new(None);
        let mut buf = b"existing_content\n".to_vec();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.starts_with("existing_content\n"),
            "encoder must append, not overwrite: {output:?}"
        );
        assert!(
            output.ends_with("up 1 0\n"),
            "appended content missing: {output:?}"
        );
    }

    #[test]
    fn encode_does_not_reallocate_when_buffer_pre_sized() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 0);
        let enc = PrometheusText::new(None);
        // Pre-allocate well beyond what a single line needs
        let mut buf = Vec::with_capacity(1024);
        let ptr_before = buf.as_ptr();
        enc.encode_metric(&event, &mut buf).unwrap();
        let ptr_after = buf.as_ptr();
        assert_eq!(
            ptr_before, ptr_after,
            "buffer reallocated during encode — pointer changed"
        );
    }

    // --- Output ends with newline ---

    #[test]
    fn output_ends_with_newline() {
        let labels = Labels::from_pairs(&[("k", "v")]).unwrap();
        let event = make_event("metric", 3.14, labels, 999);
        let output = encode_to_string(&event);
        assert!(
            output.ends_with('\n'),
            "output must end with newline: {output:?}"
        );
    }

    // --- Send + Sync contract ---

    #[test]
    fn prometheus_text_encoder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PrometheusText>();
    }

    // --- Factory and EncoderConfig ---

    #[test]
    fn create_encoder_returns_working_encoder_for_prometheus_text() {
        use crate::encoder::{create_encoder, EncoderConfig};
        let enc = create_encoder(&EncoderConfig::PrometheusText { precision: None });
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "up 1 1000000\n");
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_deserialization_prometheus_text() {
        use crate::encoder::EncoderConfig;
        let config: EncoderConfig = serde_yaml_ng::from_str("type: prometheus_text").unwrap();
        assert!(matches!(config, EncoderConfig::PrometheusText { .. }));
    }

    // --- Precision: None preserves full output ---

    #[test]
    fn precision_none_preserves_full_output() {
        let enc = PrometheusText::new(None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("cpu", 99.60573506572389, labels, 1_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.starts_with("cpu 99.60573506572389 "),
            "full precision must be preserved: {output:?}"
        );
    }

    // --- Precision: 2 limits decimal places ---

    #[test]
    fn precision_two_limits_decimals() {
        let enc = PrometheusText::new(Some(2));
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("cpu", 99.60573, labels, 1_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "cpu 99.61 1000000\n");
    }

    #[test]
    fn precision_zero_rounds_to_integer() {
        let enc = PrometheusText::new(Some(0));
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 99.6, labels, 0);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "up 100 0\n");
    }

    #[test]
    fn precision_two_preserves_trailing_zeros() {
        let enc = PrometheusText::new(Some(2));
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 0);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "up 1.00 0\n");
    }
}
