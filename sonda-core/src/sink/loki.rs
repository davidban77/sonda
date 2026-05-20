//! Loki sink — batches encoded log lines, groups by (constructor labels +
//! per-event overlay), and POSTs as a multi-stream push envelope.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::model::log::LogEvent;
use crate::sink::retry::RetryPolicy;
use crate::sink::Sink;
use crate::SondaError;

pub const DEFAULT_BATCH_SIZE: usize = 5;

/// Default cap on unique streams per push. Above this, flushes error so
/// high-cardinality `dynamic_labels` configurations surface as a config
/// issue rather than a silent Loki melt.
pub const DEFAULT_MAX_STREAMS_PER_PUSH: u32 = 128;

struct LokiEntry {
    timestamp_ns: String,
    line: String,
    overlay: BTreeMap<String, String>,
}

pub struct LokiSink {
    client: ureq::Agent,
    url: String,
    labels: HashMap<String, String>,
    batch_size: usize,
    max_streams_per_push: u32,
    batch: Vec<LokiEntry>,
    retry_policy: Option<RetryPolicy>,
    max_buffer_age: Duration,
    last_flush_at: Instant,
    last_write_delivered: bool,
}

impl LokiSink {
    pub fn new(
        url: String,
        labels: HashMap<String, String>,
        batch_size: usize,
        max_streams_per_push: u32,
        retry_policy: Option<RetryPolicy>,
        max_buffer_age: Duration,
    ) -> Result<Self, SondaError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid Loki URL '{}': must start with http:// or https://",
                    url
                ),
            )));
        }

        let client = ureq::AgentBuilder::new().build();

        Ok(Self {
            client,
            url,
            labels,
            batch_size,
            max_streams_per_push,
            batch: Vec::with_capacity(batch_size),
            retry_policy,
            max_buffer_age,
            last_flush_at: Instant::now(),
            last_write_delivered: false,
        })
    }

    fn group_by_overlay(&self) -> BTreeMap<&BTreeMap<String, String>, Vec<&LokiEntry>> {
        let mut groups: BTreeMap<&BTreeMap<String, String>, Vec<&LokiEntry>> = BTreeMap::new();
        for entry in &self.batch {
            // TODO(perf): LogEvent.labels could be Arc<Labels> so the overlay
            // doesn't allocate per event; deferred from 1.9.4 scope.
            groups.entry(&entry.overlay).or_default().push(entry);
        }
        groups
    }

    fn build_envelope(
        &self,
        groups: &BTreeMap<&BTreeMap<String, String>, Vec<&LokiEntry>>,
    ) -> String {
        let streams = groups
            .iter()
            .map(|(overlay, entries)| {
                let stream_labels = self.format_stream_labels(overlay);
                let values = entries
                    .iter()
                    .map(|e| format!("[\"{}\",\"{}\"]", e.timestamp_ns, escape_json(&e.line)))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{{\"stream\":{{{}}},\"values\":[{}]}}",
                    stream_labels, values
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        format!("{{\"streams\":[{}]}}", streams)
    }

    /// Merge constructor labels with the per-group overlay and emit them as
    /// sorted JSON object members. Overlay wins on key collision so
    /// `dynamic_labels` rotation can take precedence over constructor labels.
    fn format_stream_labels(&self, overlay: &BTreeMap<String, String>) -> String {
        let mut combined: BTreeMap<&str, &str> = self
            .labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for (k, v) in overlay {
            combined.insert(k.as_str(), v.as_str());
        }
        combined
            .iter()
            .map(|(k, v)| format!("\"{}\":\"{}\"", escape_json(k), escape_json(v)))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn flush_batch(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }

        // Scoped so `groups`' borrows into `self.batch` drop before we mutate
        // other `self` fields below.
        let body = {
            let groups = self.group_by_overlay();
            let stream_count = groups.len();

            if stream_count as u32 > self.max_streams_per_push {
                let batch_len = self.batch.len();
                let cap = self.max_streams_per_push;
                drop(groups);
                self.batch.clear();
                self.last_flush_at = Instant::now();
                return Err(SondaError::Sink(std::io::Error::other(format!(
                    "loki sink: flush would produce {stream_count} streams from {batch_len} entries, \
                     but max_streams_per_push is {cap}. Reduce dynamic_labels cardinality, \
                     raise max_streams_per_push on the sink, or split into per-value scenarios."
                ))));
            }
            self.build_envelope(&groups)
        };

        let push_url = format!("{}/loki/api/v1/push", self.url);

        // Reset on attempt, not success — the batch is cleared either way below.
        self.last_flush_at = Instant::now();

        let result = match &self.retry_policy {
            Some(policy) => {
                let policy = policy.clone();
                let client = &self.client;
                policy.execute(
                    || Self::do_post_checked(client, &push_url, &body),
                    Self::is_retryable,
                )
            }
            None => Self::do_post_checked(&self.client, &push_url, &body),
        };

        self.batch.clear();

        // 4xx errors (except 429) are non-retryable and treated as warn-and-discard.
        // The batch is already cleared; suppress the error so the sink continues.
        match &result {
            Err(SondaError::Sink(io_err)) if io_err.kind() == std::io::ErrorKind::InvalidInput => {
                Ok(())
            }
            _ => result,
        }
    }

    /// Perform a single Loki push and classify the response.
    ///
    /// - 2xx: `Ok(())`.
    /// - 4xx (except 429): warns and returns non-retryable `Err` with
    ///   `ErrorKind::InvalidInput` (same convention as `http.rs` and
    ///   `remote_write.rs`).
    /// - 429, 5xx, transport errors: retryable `Err`.
    fn do_post_checked(client: &ureq::Agent, push_url: &str, body: &str) -> Result<(), SondaError> {
        let status = Self::do_post(client, push_url, body)?;

        if (200..300).contains(&status) {
            return Ok(());
        }

        if (400..500).contains(&status) && status != 429 {
            eprintln!(
                "sonda: loki sink: received HTTP {} from '{}'; discarding batch",
                status, push_url
            );
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("HTTP {} from '{}'", status, push_url),
            )));
        }

        Err(SondaError::Sink(std::io::Error::other(format!(
            "HTTP {} from '{}'",
            status, push_url
        ))))
    }

    /// Perform a single HTTP POST of the Loki push body.
    ///
    /// Returns the HTTP status code on a successful transport-level exchange,
    /// or a [`SondaError::Sink`] on connection failure.
    fn do_post(client: &ureq::Agent, push_url: &str, body: &str) -> Result<u16, SondaError> {
        let response = client
            .post(push_url)
            .set("Content-Type", "application/json")
            .send_string(body);

        match response {
            Ok(resp) => Ok(resp.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("Loki push to '{}' failed: {}", push_url, e),
            ))),
        }
    }

    /// Classify whether an error from `do_post_checked` is retryable.
    ///
    /// Transport errors and 5xx/429 HTTP errors are retryable. 4xx errors
    /// (except 429) are not — they are tagged with `ErrorKind::InvalidInput`
    /// by `do_post_checked`.
    fn is_retryable(err: &SondaError) -> bool {
        if let SondaError::Sink(io_err) = err {
            // 4xx (except 429) are tagged InvalidInput → not retryable.
            if io_err.kind() == std::io::ErrorKind::InvalidInput {
                return false;
            }
            return true;
        }
        false
    }
}

impl Sink for LokiSink {
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let ts_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();
        self.push_entry(ts_ns, data, BTreeMap::new())
    }

    fn write_log_event(&mut self, event: &LogEvent, encoded: &[u8]) -> Result<(), SondaError> {
        // Prefer event-time over wall-clock — meaningful for log-replay
        // scenarios where the generator sets a deterministic timestamp.
        let ts_ns = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();
        let overlay: BTreeMap<String, String> = event
            .labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        self.push_entry(ts_ns, encoded, overlay)
    }

    fn flush(&mut self) -> Result<(), SondaError> {
        self.flush_batch()
    }

    fn last_write_delivered(&self) -> bool {
        self.last_write_delivered
    }
}

impl LokiSink {
    fn push_entry(
        &mut self,
        timestamp_ns: String,
        data: &[u8],
        overlay: BTreeMap<String, String>,
    ) -> Result<(), SondaError> {
        // Strip any trailing newline so log lines are clean in the Loki UI.
        let line = String::from_utf8_lossy(data);
        let line = line.trim_end_matches('\n').to_string();

        self.batch.push(LokiEntry {
            timestamp_ns,
            line,
            overlay,
        });

        let size_reached = self.batch.len() >= self.batch_size;
        let age_reached =
            !self.max_buffer_age.is_zero() && self.last_flush_at.elapsed() >= self.max_buffer_age;
        let should_flush = size_reached || age_reached;
        if should_flush {
            self.flush_batch()?;
        }
        self.last_write_delivered = should_flush;

        Ok(())
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;
    use crate::sink::{create_sink, SinkConfig};

    // -------------------------------------------------------------------------
    // Helpers — minimal mock HTTP server (same pattern as http.rs tests)
    // -------------------------------------------------------------------------

    /// Bind a TCP listener on an OS-chosen port; return (listener, base_url).
    fn mock_loki_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();
        // LokiSink will append /loki/api/v1/push to this base URL.
        let url = format!("http://127.0.0.1:{port}");
        (listener, url)
    }

    /// Accept one HTTP request from the listener, send back the given status,
    /// and return the raw request body bytes.
    fn accept_one_and_respond(listener: &TcpListener, status: u16) -> Vec<u8> {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let body = read_http_body(&mut stream);
        let reason = if status < 300 { "OK" } else { "Error" };
        let resp =
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(resp.as_bytes()).ok();
        body
    }

    /// Parse the Content-Length header from an HTTP request and read that many
    /// bytes from the stream as the body.
    fn read_http_body(stream: &mut TcpStream) -> Vec<u8> {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            let lower = line.to_lowercase();
            if let Some(rest) = lower.strip_prefix("content-length:") {
                content_length = rest.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).expect("read body");
        body
    }

    // -------------------------------------------------------------------------
    // Construction — URL validation
    // -------------------------------------------------------------------------

    #[test]
    fn new_with_http_url_succeeds() {
        let result = LokiSink::new(
            "http://localhost:3100".to_string(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        assert!(result.is_ok(), "http:// URL must be accepted");
    }

    #[test]
    fn new_with_https_url_succeeds() {
        let result = LokiSink::new(
            "https://loki.example.com".to_string(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        assert!(result.is_ok(), "https:// URL must be accepted");
    }

    #[test]
    fn new_with_invalid_scheme_returns_sink_error() {
        let result = LokiSink::new(
            "ftp://loki.example.com".to_string(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        assert!(result.is_err(), "non-http:// URL must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_bare_hostname_returns_sink_error() {
        let result = LokiSink::new(
            "loki.example.com".to_string(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        assert!(result.is_err(), "URL without scheme must be rejected");
    }

    #[test]
    fn new_with_empty_url_returns_sink_error() {
        let result = LokiSink::new(
            String::new(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        assert!(result.is_err(), "empty URL must be rejected");
    }

    #[test]
    fn new_error_message_contains_the_bad_url() {
        let bad_url = "not-a-url";
        let result = LokiSink::new(
            bad_url.to_string(),
            HashMap::new(),
            100,
            u32::MAX,
            None,
            Duration::ZERO,
        );
        let err = result.err().expect("should be Err");
        let msg = err.to_string();
        assert!(
            msg.contains(bad_url),
            "error message should contain the bad URL; got: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Loki push JSON envelope format
    // -------------------------------------------------------------------------

    /// `build_envelope()` is private, so we test it indirectly by flushing a
    /// batch to a mock server and inspecting the body that arrives.
    #[test]
    fn flush_produces_valid_loki_push_json_envelope() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "sonda".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"hello loki\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("valid UTF-8");

        // Must be valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("envelope must be valid JSON");

        // Top-level key must be "streams"
        let streams = parsed.get("streams").expect("must have 'streams' key");
        let streams_arr = streams.as_array().expect("'streams' must be an array");
        assert_eq!(streams_arr.len(), 1, "exactly one stream expected");

        // Each stream has "stream" and "values"
        let stream_obj = &streams_arr[0];
        assert!(
            stream_obj.get("stream").is_some(),
            "stream object must have 'stream' key"
        );
        assert!(
            stream_obj.get("values").is_some(),
            "stream object must have 'values' key"
        );

        // "values" is an array of [timestamp, log_line] pairs
        let values = stream_obj["values"]
            .as_array()
            .expect("'values' must be array");
        assert_eq!(values.len(), 1, "exactly one value expected");
        let pair = values[0].as_array().expect("each value must be an array");
        assert_eq!(pair.len(), 2, "each value must be [timestamp, log_line]");

        // The timestamp must be a non-empty numeric string (nanoseconds)
        let ts = pair[0].as_str().expect("timestamp must be a string");
        assert!(!ts.is_empty(), "timestamp must not be empty");
        ts.parse::<u128>()
            .expect("timestamp must be numeric nanoseconds");

        // The log line must match what we wrote (trailing newline stripped)
        let log_line = pair[1].as_str().expect("log line must be a string");
        assert_eq!(log_line, "hello loki", "log line content must match");
    }

    // -------------------------------------------------------------------------
    // Labels in stream object
    // -------------------------------------------------------------------------

    #[test]
    fn labels_appear_in_stream_object_of_push_envelope() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "sonda".to_string());
        labels.insert("env".to_string(), "dev".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"test\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8 body");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        let stream = &parsed["streams"][0]["stream"];
        assert_eq!(
            stream["job"].as_str(),
            Some("sonda"),
            "'job' label must be present"
        );
        assert_eq!(
            stream["env"].as_str(),
            Some("dev"),
            "'env' label must be present"
        );
    }

    #[test]
    fn empty_labels_produce_empty_stream_object() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        let stream = &parsed["streams"][0]["stream"];
        assert!(
            stream.as_object().map(|m| m.is_empty()).unwrap_or(false),
            "stream object must be empty when no labels configured"
        );
    }

    // -------------------------------------------------------------------------
    // Batch accumulation — no HTTP call until batch_size reached
    // -------------------------------------------------------------------------

    #[test]
    fn write_below_batch_size_does_not_trigger_http_call() {
        let (listener, url) = mock_loki_listener();

        let mut sink = LokiSink::new(url, HashMap::new(), 50, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        // Write 49 lines — one short of the 50-entry threshold.
        for i in 0..49 {
            sink.write(format!("line {i}\n").as_bytes())
                .expect("write should buffer");
        }

        // No connection should have arrived.
        listener.set_nonblocking(true).expect("set non-blocking");
        let accepted = listener.accept();
        assert!(
            accepted.is_err(),
            "no HTTP request should fire before batch_size is reached"
        );
    }

    #[test]
    fn write_at_batch_size_triggers_exactly_one_http_call() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 50, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        // Write exactly 50 lines → must trigger an auto-flush.
        for i in 0..50 {
            sink.write(format!("line {i}\n").as_bytes()).expect("write");
        }

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        let values = &parsed["streams"][0]["values"];
        assert_eq!(
            values.as_array().map(|v| v.len()),
            Some(50),
            "all 50 lines must be in the flushed batch"
        );
    }

    // -------------------------------------------------------------------------
    // Explicit flush — sends remaining entries below batch_size
    // -------------------------------------------------------------------------

    #[test]
    fn explicit_flush_sends_partial_batch() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        // Write only 3 lines (far below batch_size of 100).
        sink.write(b"alpha\n").expect("write 1");
        sink.write(b"beta\n").expect("write 2");
        sink.write(b"gamma\n").expect("write 3");
        sink.flush().expect("explicit flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        let values = parsed["streams"][0]["values"]
            .as_array()
            .expect("values array");
        assert_eq!(values.len(), 3, "all 3 partial lines must be flushed");
    }

    #[test]
    fn flush_is_idempotent() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"once\n").expect("write");
        sink.flush().expect("first flush sends data");
        let _body = handle.join().expect("mock server thread panicked");

        // After the first flush the batch is empty — second flush must be a no-op.
        assert!(sink.flush().is_ok(), "second flush must return Ok");
    }

    // -------------------------------------------------------------------------
    // Empty batch flush — no HTTP call
    // -------------------------------------------------------------------------

    #[test]
    fn flush_on_empty_batch_is_a_noop() {
        // Use a URL where no server is running; if flush() makes a network call it will fail.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}");
        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        // Empty batch — must return Ok without any network I/O.
        assert!(
            sink.flush().is_ok(),
            "flush on empty batch must return Ok without making a network call"
        );
    }

    // -------------------------------------------------------------------------
    // last_write_delivered — buffered vs flushed
    // -------------------------------------------------------------------------

    #[test]
    fn last_write_delivered_is_false_when_write_only_buffers() {
        let (listener, url) = mock_loki_listener();

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"buffered\n").expect("write buffers");

        assert!(
            !sink.last_write_delivered(),
            "a write that only buffers must report last_write_delivered() == false"
        );
        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(listener.accept().is_err(), "no flush should have fired");
    }

    #[test]
    fn last_write_delivered_is_true_when_write_triggers_flush() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 1, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"flushed\n").expect("write triggers flush");

        handle.join().expect("mock server thread panicked");
        assert!(
            sink.last_write_delivered(),
            "a write that triggers a successful flush must report last_write_delivered() == true"
        );
    }

    // -------------------------------------------------------------------------
    // Log line trailing newline stripping
    // -------------------------------------------------------------------------

    #[test]
    fn trailing_newline_is_stripped_from_log_lines() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"my log line\n").expect("write with newline");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

        let log_line = parsed["streams"][0]["values"][0][1]
            .as_str()
            .expect("log line string");
        assert_eq!(
            log_line, "my log line",
            "trailing newline must be stripped from the log line"
        );
    }

    // -------------------------------------------------------------------------
    // HTTP error handling
    // -------------------------------------------------------------------------

    #[test]
    fn five_xx_response_returns_sink_error() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 500));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line\n").expect("write buffered");
        let result = sink.flush();
        handle.join().expect("mock server thread panicked");

        assert!(result.is_err(), "5xx response must return Err");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn four_xx_response_warns_and_discards_batch_returning_ok() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 400));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line\n").expect("write buffered");
        let result = sink.flush();
        handle.join().expect("mock server thread panicked");

        // 4xx → warn + discard, but NOT an error from the sink's perspective.
        assert!(
            result.is_ok(),
            "4xx response must return Ok (warn-and-continue)"
        );
    }

    #[test]
    fn flush_to_refused_port_returns_sink_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}");
        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line\n").expect("write buffered");
        let result = sink.flush();

        assert!(result.is_err(), "connection refused must return Err");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    // -------------------------------------------------------------------------
    // JSON escaping in log lines and label values
    // -------------------------------------------------------------------------

    #[test]
    fn log_line_with_double_quotes_is_properly_escaped() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        // A log line containing a JSON double-quote character.
        sink.write(b"msg=\"hello world\"").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        // Body must parse as valid JSON (escaping is correct).
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("must parse as valid JSON after escaping");
        let log_line = parsed["streams"][0]["values"][0][1]
            .as_str()
            .expect("log line");
        assert_eq!(log_line, r#"msg="hello world""#);
    }

    #[test]
    fn label_value_with_special_characters_is_properly_escaped() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("app".to_string(), r#"my "special" app"#.to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        // If escaping is correct, serde_json can parse the entire envelope.
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("envelope with escaped labels must be valid JSON");
        let app_label = parsed["streams"][0]["stream"]["app"]
            .as_str()
            .expect("app label");
        assert_eq!(app_label, r#"my "special" app"#);
    }

    // -------------------------------------------------------------------------
    // Batch cleared after flush
    // -------------------------------------------------------------------------

    #[test]
    fn batch_is_cleared_after_auto_flush() {
        let (listener, url) = mock_loki_listener();
        // Expect two sequential flushes.
        let handle = thread::spawn(move || {
            let first = accept_one_and_respond(&listener, 204);
            let second = accept_one_and_respond(&listener, 204);
            (first, second)
        });

        let mut sink = LokiSink::new(url, HashMap::new(), 2, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        // First batch: lines 0-1 → triggers auto-flush at batch_size=2.
        sink.write(b"line 0\n").expect("write 0");
        sink.write(b"line 1\n").expect("write 1");

        // Second batch: lines 2-3 → triggers second auto-flush.
        sink.write(b"line 2\n").expect("write 2");
        sink.write(b"line 3\n").expect("write 3");

        let (first_body, second_body) = handle.join().expect("mock server thread panicked");

        let p1: serde_json::Value =
            serde_json::from_str(&String::from_utf8(first_body).expect("UTF-8"))
                .expect("first batch JSON");
        let p2: serde_json::Value =
            serde_json::from_str(&String::from_utf8(second_body).expect("UTF-8"))
                .expect("second batch JSON");

        assert_eq!(
            p1["streams"][0]["values"].as_array().map(|v| v.len()),
            Some(2),
            "first batch must contain exactly 2 entries"
        );
        assert_eq!(
            p2["streams"][0]["values"].as_array().map(|v| v.len()),
            Some(2),
            "second batch must contain exactly 2 entries"
        );
    }

    // -------------------------------------------------------------------------
    // Time-based flush
    // -------------------------------------------------------------------------

    #[test]
    fn time_based_flush_fires_when_buffer_age_exceeded() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        // batch_size large enough that size never triggers; short max_buffer_age.
        let mut sink = LokiSink::new(
            url,
            HashMap::new(),
            10_000,
            u32::MAX,
            None,
            Duration::from_millis(50),
        )
        .expect("construct sink");

        sink.write(b"first\n").expect("write 1");
        thread::sleep(Duration::from_millis(200));
        // Second write is past max_buffer_age → triggers a time-based flush.
        sink.write(b"second\n").expect("write 2");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let values = parsed["streams"][0]["values"]
            .as_array()
            .expect("values array");
        assert_eq!(
            values.len(),
            2,
            "time-based flush must deliver both buffered entries"
        );
    }

    #[test]
    fn zero_max_buffer_age_disables_time_based_flush() {
        let (listener, url) = mock_loki_listener();

        let mut sink = LokiSink::new(url, HashMap::new(), 10_000, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        sink.write(b"first\n").expect("write 1");
        thread::sleep(Duration::from_millis(150));
        sink.write(b"second\n").expect("write 2");

        // With time-based flush disabled, no request should have arrived.
        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(
            listener.accept().is_err(),
            "zero max_buffer_age must disable time-based flush"
        );
    }

    #[test]
    fn size_triggered_flush_resets_the_buffer_age_timer() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        // Small batch_size, max_buffer_age comfortably longer than the test runs.
        let mut sink = LokiSink::new(
            url,
            HashMap::new(),
            2,
            u32::MAX,
            None,
            Duration::from_secs(60),
        )
        .expect("construct sink");

        // Fill the batch immediately — the size trigger fires.
        sink.write(b"a\n").expect("write 1");
        sink.write(b"b\n").expect("write 2"); // batch_size reached → size flush

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            parsed["streams"][0]["values"].as_array().map(|v| v.len()),
            Some(2),
            "size-triggered flush must deliver the full batch"
        );

        // The size flush reset last_flush_at; a subsequent partial-batch write
        // must NOT immediately time-flush against the (now closed) listener.
        sink.write(b"c\n")
            .expect("partial write after a size flush must not time-flush immediately");
    }

    // -------------------------------------------------------------------------
    // SinkConfig::Loki deserialization
    // -------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_deserializes_with_url_only() {
        let yaml = "type: loki\nurl: \"http://localhost:3100\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki {
                ref url,
                batch_size,
                ..
            } => {
                assert_eq!(url, "http://localhost:3100");
                assert!(batch_size.is_none(), "batch_size should default to None");
            }
            other => panic!("expected Loki variant, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_deserializes_with_batch_size() {
        let yaml = r#"
type: loki
url: "http://localhost:3100"
batch_size: 50
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki {
                ref url,
                batch_size,
                ..
            } => {
                assert_eq!(url, "http://localhost:3100");
                assert_eq!(batch_size, Some(50));
            }
            other => panic!("expected Loki variant, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_requires_url_field() {
        let yaml = "type: loki";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "loki variant without url must fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_deserializes_with_max_buffer_age() {
        let yaml = r#"
type: loki
url: "http://localhost:3100"
max_buffer_age: 10s
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki { max_buffer_age, .. } => {
                assert_eq!(max_buffer_age.as_deref(), Some("10s"));
            }
            other => panic!("expected Loki variant, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_max_buffer_age_defaults_to_none() {
        let yaml = "type: loki\nurl: \"http://localhost:3100\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki { max_buffer_age, .. } => {
                assert!(
                    max_buffer_age.is_none(),
                    "max_buffer_age should default to None"
                );
            }
            other => panic!("expected Loki variant, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // Factory: create_sink for Loki config
    // -------------------------------------------------------------------------

    #[test]
    fn create_sink_loki_with_valid_url_returns_ok() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: None,
        };
        assert!(
            create_sink(&config, None).is_ok(),
            "factory must return Ok for valid loki config"
        );
    }

    #[test]
    fn create_sink_loki_with_invalid_max_buffer_age_returns_err() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
        };
        let result = create_sink(&config, None);
        assert!(
            result.is_err(),
            "invalid max_buffer_age must cause the factory to fail"
        );
    }

    #[test]
    fn create_sink_loki_with_labels_passes_them_to_sink() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let config = SinkConfig::Loki {
            url,
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: None,
        };
        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "sonda".to_string());
        let mut sink = create_sink(&config, Some(&labels)).expect("factory ok");

        sink.write(b"test\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            parsed["streams"][0]["stream"]["job"].as_str(),
            Some("sonda"),
            "labels passed to create_sink must appear in Loki stream"
        );
    }

    #[test]
    fn create_sink_loki_with_none_labels_uses_empty_labels() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let config = SinkConfig::Loki {
            url,
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: None,
        };
        let mut sink = create_sink(&config, None).expect("factory ok");

        sink.write(b"test\n").expect("write");
        sink.flush().expect("flush");

        let body_bytes = handle.join().expect("mock server thread panicked");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let stream = &parsed["streams"][0]["stream"];
        assert!(
            stream.as_object().map(|m| m.is_empty()).unwrap_or(false),
            "None labels must produce empty stream object"
        );
    }

    #[test]
    fn default_batch_size_is_5() {
        assert_eq!(DEFAULT_BATCH_SIZE, 5);
    }

    #[test]
    fn create_sink_loki_with_no_batch_size_uses_default() {
        // Construction succeeds with `batch_size: None`; writing fewer entries
        // than DEFAULT_BATCH_SIZE must not trigger a flush attempt against the
        // non-existent server.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}");
        let config = SinkConfig::Loki {
            url,
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: None,
        };
        let mut sink = create_sink(&config, None).expect("factory ok");

        for i in 0..(DEFAULT_BATCH_SIZE - 1) as u32 {
            sink.write(format!("line {i}\n").as_bytes())
                .expect("write must succeed below batch_size");
        }
    }

    #[test]
    fn create_sink_loki_with_invalid_url_returns_err() {
        let config = SinkConfig::Loki {
            url: "not-http://bad".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: None,
        };
        let result = create_sink(&config, None);
        assert!(result.is_err(), "invalid URL must cause factory to fail");
    }

    // -------------------------------------------------------------------------
    // Trait contract: Send + Sync
    // -------------------------------------------------------------------------

    #[test]
    fn loki_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LokiSink>();
    }

    // -------------------------------------------------------------------------
    // Example YAML file round-trip
    // -------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn loki_json_lines_example_yaml_deserializes_to_log_scenario_config() {
        use crate::config::LogScenarioConfig;

        // Read the content inline to avoid filesystem coupling in unit tests.
        // Labels are at the top level, not inside the sink block.
        let yaml = r#"
name: app_logs_loki
rate: 10
duration: 60s
generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
        endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
labels:
  job: sonda
  env: dev
encoder:
  type: json_lines
sink:
  type: loki
  url: http://localhost:3100
  batch_size: 50
"#;
        let config: LogScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("loki-json-lines.yaml must deserialize correctly");
        assert_eq!(config.name, "app_logs_loki");
        assert!((config.rate - 10.0).abs() < f64::EPSILON);
        // Labels are at the scenario level, not inside the sink config.
        let labels = config.labels.as_ref().expect("labels must be present");
        assert_eq!(labels.get("job").map(String::as_str), Some("sonda"));
        assert_eq!(labels.get("env").map(String::as_str), Some("dev"));
        match &config.sink {
            SinkConfig::Loki {
                url, batch_size, ..
            } => {
                assert_eq!(url, "http://localhost:3100");
                assert_eq!(batch_size, &Some(50));
            }
            other => panic!("expected Loki sink, got {other:?}"),
        }
    }

    fn make_log_event(labels: &[(&str, &str)], message: &str) -> crate::model::log::LogEvent {
        use crate::model::log::{LogEvent, Severity};
        use crate::model::metric::Labels;
        use std::collections::BTreeMap;
        use std::time::SystemTime;

        let labels = Labels::from_pairs(labels).expect("valid label pairs");
        LogEvent::with_timestamp(
            SystemTime::UNIX_EPOCH,
            Severity::Info,
            message.to_string(),
            labels,
            BTreeMap::new(),
        )
    }

    fn parse_streams(body: Vec<u8>) -> Vec<serde_json::Value> {
        let body = String::from_utf8(body).expect("UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("envelope must be valid JSON");
        parsed["streams"]
            .as_array()
            .expect("'streams' must be an array")
            .clone()
    }

    #[test]
    fn write_log_event_single_stream_when_no_per_event_labels() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "sonda".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        let event = make_log_event(&[], "hello");
        sink.write_log_event(&event, b"hello").expect("write");
        sink.flush().expect("flush");

        let streams = parse_streams(handle.join().expect("mock server"));
        assert_eq!(streams.len(), 1, "no overlay → exactly one stream");
        let stream_obj = streams[0]["stream"].as_object().expect("stream object");
        assert_eq!(stream_obj.len(), 1, "only the constructor label is present");
        assert_eq!(stream_obj["job"].as_str(), Some("sonda"));
    }

    #[test]
    fn write_log_event_multi_stream_grouping_by_event_labels() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("device".to_string(), "srl1".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");

        let ev1 = make_log_event(&[("peer_address", "10.1.2.2")], "to peer 1");
        let ev2 = make_log_event(&[("peer_address", "10.1.7.2")], "to peer 2");
        let ev1b = make_log_event(&[("peer_address", "10.1.2.2")], "to peer 1 again");
        sink.write_log_event(&ev1, b"line-a").expect("write 1");
        sink.write_log_event(&ev2, b"line-b").expect("write 2");
        sink.write_log_event(&ev1b, b"line-c").expect("write 3");
        sink.flush().expect("flush");

        let streams = parse_streams(handle.join().expect("mock server"));
        assert_eq!(
            streams.len(),
            2,
            "two distinct peer_address values must produce two streams"
        );

        let mut by_peer: std::collections::HashMap<String, usize> = Default::default();
        for s in &streams {
            let peer = s["stream"]["peer_address"]
                .as_str()
                .expect("peer_address must be a stream label")
                .to_string();
            let values_len = s["values"].as_array().expect("values must be array").len();
            by_peer.insert(peer, values_len);
            assert_eq!(
                s["stream"]["device"].as_str(),
                Some("srl1"),
                "constructor labels must appear in every stream"
            );
        }
        assert_eq!(by_peer.get("10.1.2.2"), Some(&2), "ev1 + ev1b group");
        assert_eq!(by_peer.get("10.1.7.2"), Some(&1), "ev2 alone");
    }

    #[test]
    fn write_log_event_overlay_overrides_constructor_label_on_conflict() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("device".to_string(), "srl1".to_string());
        labels.insert("region".to_string(), "us-east".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        let event = make_log_event(&[("region", "eu-west")], "overridden");
        sink.write_log_event(&event, b"line").expect("write");
        sink.flush().expect("flush");

        let streams = parse_streams(handle.join().expect("mock server"));
        assert_eq!(streams.len(), 1);
        let stream_obj = streams[0]["stream"].as_object().expect("stream object");
        assert_eq!(
            stream_obj["region"].as_str(),
            Some("eu-west"),
            "event overlay must win on key collision with constructor labels"
        );
        assert_eq!(
            stream_obj["device"].as_str(),
            Some("srl1"),
            "non-conflicting constructor labels must still pass through"
        );
    }

    #[test]
    fn flush_with_stream_count_at_cap_succeeds() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100, 3, None, Duration::ZERO)
            .expect("construct sink");
        for i in 0..3 {
            let ev = make_log_event(&[("peer", &format!("p{i}"))], "line");
            sink.write_log_event(&ev, format!("line-{i}").as_bytes())
                .expect("write");
        }
        let flush_result = sink.flush();
        assert!(
            flush_result.is_ok(),
            "exactly-at-cap must succeed: {flush_result:?}"
        );

        let streams = parse_streams(handle.join().expect("mock server"));
        assert_eq!(streams.len(), 3, "all three streams must be sent");
    }

    #[test]
    fn flush_exceeding_cap_returns_helpful_error_and_clears_batch() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}");

        let mut sink = LokiSink::new(url, HashMap::new(), 100, 2, None, Duration::ZERO)
            .expect("construct sink");
        for i in 0..3 {
            let ev = make_log_event(&[("peer", &format!("p{i}"))], "line");
            sink.write_log_event(&ev, format!("line-{i}").as_bytes())
                .expect("write");
        }
        let err = sink.flush().expect_err("over-cap flush must Err");
        let msg = err.to_string();
        assert!(
            msg.contains("3 streams"),
            "must report the actual count: {msg}"
        );
        assert!(
            msg.contains("max_streams_per_push is 2"),
            "must report the cap: {msg}"
        );
        assert!(
            msg.contains("dynamic_labels") || msg.contains("per-value scenarios"),
            "must point at the workaround: {msg}"
        );

        assert!(
            sink.flush().is_ok(),
            "batch must be cleared after cap error"
        );
    }

    #[test]
    fn direct_write_falls_back_to_constructor_labels_only() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "sonda".to_string());

        let mut sink = LokiSink::new(url, labels, 100, u32::MAX, None, Duration::ZERO)
            .expect("construct sink");
        sink.write(b"line via direct write").expect("write");
        sink.write(b"another line via direct write").expect("write");
        sink.flush().expect("flush");

        let streams = parse_streams(handle.join().expect("mock server"));
        assert_eq!(streams.len(), 1, "direct write produces a single stream");
        assert_eq!(
            streams[0]["stream"]
                .as_object()
                .expect("stream object")
                .len(),
            1,
            "only the constructor label is present"
        );
        assert_eq!(streams[0]["stream"]["job"].as_str(), Some("sonda"));
        assert_eq!(
            streams[0]["values"].as_array().expect("values").len(),
            2,
            "both direct writes group into the single stream"
        );
    }
}
