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
