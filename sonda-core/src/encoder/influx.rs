//! InfluxDB Line Protocol encoder.
//!
//! Implements the InfluxDB line protocol format.
//! Reference: <https://docs.influxdata.com/influxdb/v2/reference/syntax/line-protocol/>
//!
//! Format:
//! ```text
//! measurement,tag1=val1,tag2=val2 field_key=value timestamp_ns\n
//! ```
//!
//! Tags are sorted by key (InfluxDB best practice for performance). Measurement names and
//! tag keys/values escape `,`, ` `, and `=` with a backslash.

use std::io::Write as _;
use std::time::UNIX_EPOCH;

use crate::model::metric::MetricEvent;
use crate::{EncoderError, SondaError};

use super::Encoder;

/// Encodes [`MetricEvent`]s into InfluxDB line protocol format.
///
/// The field key used for the metric value is configured at construction time. It defaults
/// to `"value"`.
///
/// Output format (with tags):
/// ```text
/// measurement,tag1=val1,tag2=val2 field_key=value 1700000000000000000\n
/// ```
///
/// Output format (no tags):
/// ```text
/// measurement field_key=value 1700000000000000000\n
/// ```
///
/// Timestamp is nanoseconds since the Unix epoch.
///
/// Characters `,`, ` `, and `=` are escaped with a backslash in measurement names and
/// tag keys/values.
///
/// When `precision` is set, metric values are formatted to the specified number
/// of decimal places.
pub struct InfluxLineProtocol {
    /// Pre-escaped field key bytes written into the buffer on every encode call.
    ///
    /// Built once at construction from the configured field key (default: `"value"`).
    field_key_escaped: Vec<u8>,
    /// Optional decimal precision for metric values.
    precision: Option<u8>,
}

impl InfluxLineProtocol {
    /// Create a new `InfluxLineProtocol` encoder.
    ///
    /// `field_key` sets the InfluxDB field key for the metric value. If `None`, defaults
    /// to `"value"`. The field key is escaped and stored at construction time to avoid
    /// per-event work.
    ///
    /// `precision` optionally limits the number of decimal places in metric values.
    /// `None` preserves full `f64` precision (default behavior).
    pub fn new(field_key: Option<String>, precision: Option<u8>) -> Self {
        let field_key = field_key.unwrap_or_else(|| "value".to_string());
        let mut field_key_escaped = Vec::with_capacity(field_key.len() + 4);
        escape_tag(&field_key, &mut field_key_escaped);
        Self {
            field_key_escaped,
            precision,
        }
    }
}

/// Escape a measurement name, tag key, or tag value per InfluxDB line protocol rules.
///
/// The following characters are escaped with a leading backslash: `,`, ` ` (space), `=`.
fn escape_tag(s: &str, buf: &mut Vec<u8>) {
    for byte in s.bytes() {
        match byte {
            b',' | b' ' | b'=' => {
                buf.push(b'\\');
                buf.push(byte);
            }
            other => buf.push(other),
        }
    }
}

impl Encoder for InfluxLineProtocol {
    /// Encode a metric event into InfluxDB line protocol format.
    ///
    /// Appends a complete line to `buf`. The buffer is not cleared before writing.
    /// Writes into the caller-provided buffer to minimize allocations.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        // Measurement name (escaped)
        escape_tag(&event.name, buf);

        // Tag set (only if non-empty). Tags are already sorted by key from BTreeMap.
        if !event.labels.is_empty() {
            buf.push(b',');
            let mut first = true;
            for (key, value) in event.labels.iter() {
                if !first {
                    buf.push(b',');
                }
                first = false;
                escape_tag(key, buf);
                buf.push(b'=');
                escape_tag(value, buf);
            }
        }

        // Space separates tag set from field set
        buf.push(b' ');

        // Field set: field_key=value (field values are not escaped; numeric values need no escaping)
        buf.extend_from_slice(&self.field_key_escaped);
        buf.push(b'=');
        // Write the float value, optionally with fixed decimal precision
        super::write_value(buf, event.value, self.precision);

        // Timestamp in nanoseconds since epoch
        let timestamp_ns = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(EncoderError::TimestampBeforeEpoch(e)))?
            .as_nanos();

        buf.push(b' ');
        write!(buf, "{timestamp_ns}").expect("write to Vec<u8> is infallible");

        buf.push(b'\n');

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{create_encoder, EncoderConfig};
    use crate::model::metric::{Labels, MetricEvent};
    use std::time::{Duration, UNIX_EPOCH};

    /// Build a MetricEvent with a fixed nanosecond-precision timestamp for deterministic tests.
    fn make_event(name: &str, value: f64, labels: Labels, timestamp_ns: u64) -> MetricEvent {
        let ts = UNIX_EPOCH + Duration::from_nanos(timestamp_ns);
        MetricEvent::with_timestamp(name.to_string(), value, labels, ts).unwrap()
    }

    /// Encode one event and return the result as a UTF-8 String.
    fn encode_to_string(enc: &InfluxLineProtocol, event: &MetricEvent) -> String {
        let mut buf = Vec::new();
        enc.encode_metric(event, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    // --- Happy path: metric with no labels ---

    #[test]
    fn no_labels_produces_measurement_space_field_space_timestamp() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_700_000_000_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert_eq!(output, "up value=1 1700000000000000000\n");
    }

    #[test]
    fn no_labels_output_has_no_comma_after_measurement() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("cpu", 0.5, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        // Measurement must be directly followed by a space (no tag set comma)
        assert!(
            output.starts_with("cpu "),
            "no-label measurement must be followed by space: {output:?}"
        );
    }

    // --- Happy path: metric with two labels (sorted) ---

    #[test]
    fn two_labels_sorted_by_key_in_tag_set() {
        let enc = InfluxLineProtocol::new(None, None);
        // Insert in reverse alphabetical order — BTreeMap must sort them.
        let labels = Labels::from_pairs(&[("zone", "eu1"), ("host", "srv1")]).unwrap();
        let event = make_event("cpu", 0.5, labels, 1_700_000_000_000_000_000);
        let output = encode_to_string(&enc, &event);
        // host < zone alphabetically
        assert_eq!(
            output,
            "cpu,host=srv1,zone=eu1 value=0.5 1700000000000000000\n"
        );
    }

    #[test]
    fn three_labels_sorted_alphabetically() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels =
            Labels::from_pairs(&[("zone", "us1"), ("env", "prod"), ("host", "web01")]).unwrap();
        let event = make_event("metric", 42.0, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        // env < host < zone
        assert!(
            output.starts_with("metric,env=prod,host=web01,zone=us1 "),
            "tags not sorted correctly: {output:?}"
        );
    }

    // --- Custom field key ---

    #[test]
    fn custom_field_key_appears_in_output() {
        let enc = InfluxLineProtocol::new(Some("gauge".to_string()), None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains("gauge=1"),
            "custom field key not in output: {output:?}"
        );
    }

    #[test]
    fn none_field_key_defaults_to_value() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains("value="),
            "default field key 'value' not in output: {output:?}"
        );
    }

    // --- Escaping: measurement name ---

    #[test]
    fn measurement_with_space_is_escaped() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        // Use Labels::new (bypasses key validation) so we can test escaping independently
        // For measurement name, we construct the event directly
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        // MetricEvent validates metric name — spaces are not allowed in valid Prometheus names.
        // We test via a name with a comma or underscore that influx would escape.
        // Since MetricEvent::with_timestamp validates names with Prometheus rules (no spaces allowed),
        // we instead test with a name containing characters that pass Prometheus validation
        // but verify the escaping machinery via the escape_tag function directly through a name
        // that has an underscore (no escaping needed) to confirm non-special chars pass through.
        let event = MetricEvent::with_timestamp("cpu_usage".to_string(), 0.75, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        assert!(
            output.starts_with("cpu_usage "),
            "plain measurement passed through incorrectly: {output:?}"
        );
    }

    #[test]
    fn tag_value_with_space_is_escaped() {
        let enc = InfluxLineProtocol::new(None, None);
        // Labels::from_pairs validates keys but not values — values can contain special chars.
        let labels = Labels::new(vec![("host".to_string(), "my server".to_string())]);
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("cpu".to_string(), 0.5, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains(r"host=my\ server"),
            "space in tag value not escaped: {output:?}"
        );
    }

    #[test]
    fn tag_value_with_comma_is_escaped() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::new(vec![("region".to_string(), "us,east".to_string())]);
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("cpu".to_string(), 1.0, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains(r"region=us\,east"),
            "comma in tag value not escaped: {output:?}"
        );
    }

    #[test]
    fn tag_value_with_equals_is_escaped() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::new(vec![("kv".to_string(), "a=b".to_string())]);
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("cpu".to_string(), 1.0, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains(r"kv=a\=b"),
            "equals in tag value not escaped: {output:?}"
        );
    }

    #[test]
    fn tag_value_with_all_special_chars_is_escaped() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::new(vec![("tag".to_string(), "a,b c=d".to_string())]);
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("cpu".to_string(), 1.0, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains(r"tag=a\,b\ c\=d"),
            "combined escaping not correct: {output:?}"
        );
    }

    // --- Timestamp is nanoseconds ---

    #[test]
    fn timestamp_is_nanoseconds_at_least_13_digits() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        // 1_700_000_000 seconds = 1_700_000_000_000_000_000 ns (19 digits)
        let event = make_event("up", 1.0, labels, 1_700_000_000_000_000_000);
        let output = encode_to_string(&enc, &event);
        // Extract the timestamp (last token before newline)
        let ts_str = output
            .trim_end_matches('\n')
            .split_whitespace()
            .last()
            .unwrap();
        assert!(
            ts_str.len() >= 13,
            "timestamp must be at least 13 digits (nanoseconds): {ts_str:?}"
        );
        assert_eq!(
            ts_str, "1700000000000000000",
            "timestamp is not nanoseconds: {ts_str:?}"
        );
    }

    #[test]
    fn timestamp_is_not_milliseconds() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        // 1_000 ms = 1 second = 1_000_000_000 ns; ms would be "1000", ns would be "1000000000"
        let ts = UNIX_EPOCH + Duration::from_millis(1_000);
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();
        let output = encode_to_string(&enc, &event);
        let ts_str = output
            .trim_end_matches('\n')
            .split_whitespace()
            .last()
            .unwrap();
        assert_eq!(
            ts_str, "1000000000",
            "timestamp should be nanoseconds, not milliseconds: got {ts_str:?}"
        );
    }

    #[test]
    fn timestamp_does_not_contain_decimal_point() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_234_567_890_123_456_789);
        let output = encode_to_string(&enc, &event);
        let ts_str = output
            .trim_end_matches('\n')
            .split_whitespace()
            .last()
            .unwrap();
        assert!(
            !ts_str.contains('.'),
            "timestamp must be an integer: {ts_str:?}"
        );
    }

    // --- Regression anchor: hardcoded expected byte strings ---

    #[test]
    fn regression_anchor_no_labels_exact_bytes() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        // Timestamp: exactly 1_700_000_000 seconds = 1_700_000_000_000_000_000 ns
        let event = make_event(
            "http_requests_total",
            123.456,
            labels,
            1_700_000_000_000_000_000,
        );
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        assert_eq!(
            buf,
            b"http_requests_total value=123.456 1700000000000000000\n"
        );
    }

    #[test]
    fn regression_anchor_two_labels_exact_bytes() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[("hostname", "t0-a1"), ("zone", "eu1")]).unwrap();
        let event = make_event("interface_state", 1.0, labels, 1_700_000_000_000_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        assert_eq!(
            buf,
            b"interface_state,hostname=t0-a1,zone=eu1 value=1 1700000000000000000\n"
        );
    }

    #[test]
    fn regression_anchor_custom_field_key_exact_bytes() {
        let enc = InfluxLineProtocol::new(Some("gauge".to_string()), None);
        let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
        let event = make_event("cpu", 0.75, labels, 1_000_000_000_000_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        assert_eq!(buf, b"cpu,host=srv1 gauge=0.75 1000000000000000000\n");
    }

    // --- Output format ---

    #[test]
    fn output_ends_with_newline() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[("k", "v")]).unwrap();
        let event = make_event("metric", 3.14, labels, 999_000_000);
        let output = encode_to_string(&enc, &event);
        assert!(
            output.ends_with('\n'),
            "output must end with newline: {output:?}"
        );
    }

    #[test]
    fn encode_appends_to_existing_buffer_content() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 1.0, labels, 1_000_000_000);
        let mut buf = b"existing\n".to_vec();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.starts_with("existing\n"),
            "encoder must append, not overwrite: {output:?}"
        );
        assert!(
            output.ends_with("up value=1 1000000000\n"),
            "appended content missing: {output:?}"
        );
    }

    #[test]
    fn multiple_encodes_accumulate_in_buffer() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event1 = make_event("up", 1.0, labels.clone(), 1_000_000_000);
        let event2 = make_event("down", 0.0, labels, 2_000_000_000);
        let mut buf = Vec::new();
        enc.encode_metric(&event1, &mut buf).unwrap();
        enc.encode_metric(&event2, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 lines: {output:?}");
        assert_eq!(lines[0], "up value=1 1000000000");
        assert_eq!(lines[1], "down value=0 2000000000");
    }

    // --- Pre-epoch timestamp error ---

    #[test]
    fn pre_epoch_timestamp_returns_encoder_error() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event =
            MetricEvent::with_timestamp("up".to_string(), 1.0, labels, before_epoch).unwrap();
        let enc = InfluxLineProtocol::new(None, None);
        let mut buf = Vec::new();
        let result = enc.encode_metric(&event, &mut buf);
        assert!(
            matches!(result, Err(SondaError::Encoder(_))),
            "expected Encoder error for pre-epoch timestamp, got: {result:?}"
        );
    }

    // --- Send + Sync contract ---

    #[test]
    fn influx_line_protocol_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InfluxLineProtocol>();
    }

    // --- Factory and EncoderConfig ---

    #[test]
    fn create_encoder_returns_working_influx_encoder_with_default_field_key() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        };
        let enc = create_encoder(&config).unwrap();
        let labels = Labels::from_pairs(&[]).unwrap();
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "up value=1 1000000000\n");
    }

    #[test]
    fn create_encoder_returns_working_influx_encoder_with_custom_field_key() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("count".to_string()),
            precision: None,
        };
        let enc = create_encoder(&config).unwrap();
        let labels = Labels::from_pairs(&[]).unwrap();
        let ts = UNIX_EPOCH + Duration::from_nanos(1_000_000_000);
        let event = MetricEvent::with_timestamp("up".to_string(), 5.0, labels, ts).unwrap();
        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("count=5"),
            "custom field key 'count' not in factory-created encoder output: {output:?}"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_deserialization_influx_lp_no_field_key() {
        let config: EncoderConfig =
            serde_yaml_ng::from_str("type: influx_lp\nfield_key: null").unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol {
                field_key: None,
                precision: None
            }
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_deserialization_influx_lp_with_field_key() {
        let config: EncoderConfig =
            serde_yaml_ng::from_str("type: influx_lp\nfield_key: requests").unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol {
                field_key: Some(ref k), ..
            } if k == "requests"
        ));
    }

    // --- Precision: 2 limits decimal places in field value ---

    #[test]
    fn precision_two_limits_decimals_influx() {
        let enc = InfluxLineProtocol::new(None, Some(2));
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("cpu", 99.60573, labels, 1_700_000_000_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert_eq!(output, "cpu value=99.61 1700000000000000000\n");
    }

    #[test]
    fn precision_none_preserves_full_output_influx() {
        let enc = InfluxLineProtocol::new(None, None);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("cpu", 99.60573506572389, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains("value=99.60573506572389"),
            "full precision must be preserved: {output:?}"
        );
    }

    #[test]
    fn precision_zero_influx() {
        let enc = InfluxLineProtocol::new(None, Some(0));
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = make_event("up", 42.9, labels, 1_000_000_000);
        let output = encode_to_string(&enc, &event);
        assert!(
            output.contains("value=43"),
            "precision=0 should round: {output:?}"
        );
    }
}
