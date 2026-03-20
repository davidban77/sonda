//! Prometheus text exposition format encoder.
//!
//! Implements the Prometheus text format version 0.0.4.
//! Reference: <https://prometheus.io/docs/instrumenting/exposition_formats/>

use std::io::Write as _;
use std::time::UNIX_EPOCH;

use crate::model::metric::MetricEvent;
use crate::SondaError;

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
pub struct PrometheusText;

impl PrometheusText {
    /// Create a new `PrometheusText` encoder.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PrometheusText {
    fn default() -> Self {
        Self::new()
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

        // Value: write f64 using write! into the Vec<u8>
        // write! on Vec<u8> never fails (infallible I/O)
        write!(buf, "{}", event.value).expect("write to Vec<u8> is infallible");

        // Timestamp in milliseconds since epoch
        let timestamp_ms = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?
            .as_millis();

        buf.push(b' ');
        write!(buf, "{timestamp_ms}").expect("write to Vec<u8> is infallible");

        buf.push(b'\n');

        Ok(())
    }
}
