//! Loki sink — batches encoded log lines and delivers them to Grafana Loki via HTTP POST.
//!
//! The sink accumulates (timestamp, log_line) pairs in an internal batch. When the batch
//! reaches the configured `batch_size`, or when `flush` is called explicitly, the batch
//! is serialised into the Loki push API JSON envelope and sent as a single HTTP POST
//! to `{url}/loki/api/v1/push`.
//!
//! The Loki push API format:
//! ```json
//! {
//!   "streams": [{
//!     "stream": { "label1": "value1" },
//!     "values": [["<unix_nanoseconds>", "<log_line>"]]
//!   }]
//! }
//! ```

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sink::Sink;
use crate::SondaError;

/// Delivers encoded log lines to Grafana Loki via its HTTP push API.
///
/// Log lines are accumulated in a batch. When the batch reaches `batch_size` entries,
/// it is automatically flushed. Call `flush()` at shutdown to deliver any remaining
/// buffered entries.
///
/// Each entry in the batch is a pair of `(unix_nanoseconds, log_line)` strings, which
/// is the format required by the Loki push API.
pub struct LokiSink {
    /// The ureq HTTP agent used for all requests.
    client: ureq::Agent,
    /// Base URL for the Loki instance, e.g. `"http://localhost:3100"`.
    url: String,
    /// Stream labels sent with every batch, e.g. `{"job": "sonda", "env": "dev"}`.
    labels: HashMap<String, String>,
    /// Flush threshold in entries. When `batch.len() == batch_size`, the batch is sent.
    batch_size: usize,
    /// Accumulated entries waiting to be sent: `(unix_nanoseconds, log_line)`.
    batch: Vec<(String, String)>,
}

impl LokiSink {
    /// Create a new `LokiSink`.
    ///
    /// # Arguments
    ///
    /// - `url` — the base URL of the Loki instance, e.g. `"http://localhost:3100"`.
    ///   The push endpoint `/loki/api/v1/push` is appended automatically.
    /// - `labels` — stream labels attached to every log batch.
    /// - `batch_size` — number of log entries to accumulate before auto-flushing.
    ///   Use `100` if no override is needed.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the URL scheme is invalid (not `http://` or `https://`).
    pub fn new(
        url: String,
        labels: HashMap<String, String>,
        batch_size: usize,
    ) -> Result<Self, SondaError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid Loki URL '{}': must start with http:// or https://",
                    url
                ),
            )
            .into());
        }

        let client = ureq::AgentBuilder::new().build();

        Ok(Self {
            client,
            url,
            labels,
            batch_size,
            batch: Vec::with_capacity(batch_size),
        })
    }

    /// Build the Loki push JSON envelope from the current batch.
    ///
    /// The format follows the Loki push API specification:
    /// `{"streams": [{"stream": {...labels}, "values": [["<ns>", "<line>"], ...]}]}`
    fn build_envelope(&self) -> String {
        // Build the stream labels object.
        let stream_labels = self
            .labels
            .iter()
            .map(|(k, v)| format!("\"{}\":\"{}\"", escape_json(k), escape_json(v)))
            .collect::<Vec<_>>()
            .join(",");

        // Build the values array.
        let values = self
            .batch
            .iter()
            .map(|(ts, line)| format!("[\"{}\",\"{}\"]", ts, escape_json(line)))
            .collect::<Vec<_>>()
            .join(",");

        format!(
            "{{\"streams\":[{{\"stream\":{{{}}},\"values\":[{}]}}]}}",
            stream_labels, values
        )
    }

    /// POST the current batch to Loki and clear it on success.
    ///
    /// Returns `Ok(())` on a successful 2xx response. HTTP errors and transport
    /// failures are returned as [`SondaError::Sink`].
    fn flush_batch(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }

        let push_url = format!("{}/loki/api/v1/push", self.url);
        let body = self.build_envelope();

        let response = self
            .client
            .post(&push_url)
            .set("Content-Type", "application/json")
            .send_string(&body);

        match response {
            Ok(resp) => {
                let status = resp.status();
                self.batch.clear();
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!(
                        "Loki push to '{}' returned unexpected status {}",
                        push_url, status
                    ))
                    .into())
                }
            }
            Err(ureq::Error::Status(code, _)) => {
                self.batch.clear();
                Err(std::io::Error::other(format!(
                    "Loki push to '{}' failed with HTTP status {}",
                    push_url, code
                ))
                .into())
            }
            Err(e) => {
                self.batch.clear();
                Err(
                    std::io::Error::other(format!("Loki push to '{}' failed: {}", push_url, e))
                        .into(),
                )
            }
        }
    }
}

impl Sink for LokiSink {
    /// Append one encoded log line to the internal batch.
    ///
    /// The line is paired with the current wall-clock time as a Unix nanosecond
    /// timestamp string. When the batch reaches `batch_size` entries, the batch
    /// is automatically flushed to Loki.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if an auto-flush fails.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let ts_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();

        // Strip any trailing newline so log lines are clean in the Loki UI.
        let line = String::from_utf8_lossy(data);
        let line = line.trim_end_matches('\n').to_string();

        self.batch.push((ts_ns, line));

        if self.batch.len() >= self.batch_size {
            self.flush_batch()?;
        }

        Ok(())
    }

    /// Flush any remaining buffered entries to Loki.
    ///
    /// Safe to call multiple times. Returns `Ok(())` immediately if the batch is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the HTTP request fails.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.flush_batch()
    }
}

/// Escape a string for use inside a JSON string value.
///
/// Handles the minimal set of characters that must be escaped in JSON:
/// backslash, double quote, and the ASCII control characters.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 32 => {
                // Other ASCII control characters as \uXXXX.
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
