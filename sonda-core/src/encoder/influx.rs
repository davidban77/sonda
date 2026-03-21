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
use crate::SondaError;

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
pub struct InfluxLineProtocol {
    /// Pre-escaped field key bytes written into the buffer on every encode call.
    ///
    /// Built once at construction from the configured field key (default: `"value"`).
    field_key_escaped: Vec<u8>,
}

impl InfluxLineProtocol {
    /// Create a new `InfluxLineProtocol` encoder.
    ///
    /// `field_key` sets the InfluxDB field key for the metric value. If `None`, defaults
    /// to `"value"`. The field key is escaped and stored at construction time to avoid
    /// per-event work.
    pub fn new(field_key: Option<String>) -> Self {
        let field_key = field_key.unwrap_or_else(|| "value".to_string());
        let mut field_key_escaped = Vec::with_capacity(field_key.len() + 4);
        escape_tag(&field_key, &mut field_key_escaped);
        Self { field_key_escaped }
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
        // Write the float value; use integer form when value is an integer
        write!(buf, "{}", event.value).expect("write to Vec<u8> is infallible");

        // Timestamp in nanoseconds since epoch
        let timestamp_ns = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?
            .as_nanos();

        buf.push(b' ');
        write!(buf, "{timestamp_ns}").expect("write to Vec<u8> is infallible");

        buf.push(b'\n');

        Ok(())
    }
}
