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
                    Err(SondaError::Sink(std::io::Error::other(format!(
                        "Loki push to '{}' returned unexpected status {}",
                        push_url, status
                    ))))
                }
            }
            Err(ureq::Error::Status(code, _)) => {
                self.batch.clear();
                Err(SondaError::Sink(std::io::Error::other(format!(
                    "Loki push to '{}' failed with HTTP status {}",
                    push_url, code
                ))))
            }
            Err(e) => {
                self.batch.clear();
                Err(SondaError::Sink(std::io::Error::other(format!(
                    "Loki push to '{}' failed: {}",
                    push_url, e
                ))))
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
            if lower.starts_with("content-length:") {
                let val = lower["content-length:".len()..].trim().to_string();
                content_length = val.parse().unwrap_or(0);
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
        let result = LokiSink::new("http://localhost:3100".to_string(), HashMap::new(), 100);
        assert!(result.is_ok(), "http:// URL must be accepted");
    }

    #[test]
    fn new_with_https_url_succeeds() {
        let result = LokiSink::new("https://loki.example.com".to_string(), HashMap::new(), 100);
        assert!(result.is_ok(), "https:// URL must be accepted");
    }

    #[test]
    fn new_with_invalid_scheme_returns_sink_error() {
        let result = LokiSink::new("ftp://loki.example.com".to_string(), HashMap::new(), 100);
        assert!(result.is_err(), "non-http:// URL must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_bare_hostname_returns_sink_error() {
        let result = LokiSink::new("loki.example.com".to_string(), HashMap::new(), 100);
        assert!(result.is_err(), "URL without scheme must be rejected");
    }

    #[test]
    fn new_with_empty_url_returns_sink_error() {
        let result = LokiSink::new(String::new(), HashMap::new(), 100);
        assert!(result.is_err(), "empty URL must be rejected");
    }

    #[test]
    fn new_error_message_contains_the_bad_url() {
        let bad_url = "not-a-url";
        let result = LokiSink::new(bad_url.to_string(), HashMap::new(), 100);
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

        let mut sink = LokiSink::new(url, labels, 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, labels, 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, HashMap::new(), 50).expect("construct sink");

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

        let mut sink = LokiSink::new(url, HashMap::new(), 50).expect("construct sink");

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

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");

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

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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
        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");

        // Empty batch — must return Ok without any network I/O.
        assert!(
            sink.flush().is_ok(),
            "flush on empty batch must return Ok without making a network call"
        );
    }

    // -------------------------------------------------------------------------
    // Log line trailing newline stripping
    // -------------------------------------------------------------------------

    #[test]
    fn trailing_newline_is_stripped_from_log_lines() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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
    fn four_xx_response_returns_sink_error() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 400));

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
        sink.write(b"line\n").expect("write buffered");
        let result = sink.flush();
        handle.join().expect("mock server thread panicked");

        assert!(result.is_err(), "4xx response must return Err");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn flush_to_refused_port_returns_sink_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}");
        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, HashMap::new(), 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, labels, 100).expect("construct sink");
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

        let mut sink = LokiSink::new(url, HashMap::new(), 2).expect("construct sink");

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
    // SinkConfig::Loki deserialization
    // -------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_loki_deserializes_with_url_only() {
        let yaml = "type: loki\nurl: \"http://localhost:3100\"";
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki {
                ref url,
                batch_size,
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
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Loki {
                ref url,
                batch_size,
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
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "loki variant without url must fail deserialization"
        );
    }

    // -------------------------------------------------------------------------
    // Factory: create_sink for Loki config
    // -------------------------------------------------------------------------

    #[test]
    fn create_sink_loki_with_valid_url_returns_ok() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
        };
        assert!(
            create_sink(&config, None).is_ok(),
            "factory must return Ok for valid loki config"
        );
    }

    #[test]
    fn create_sink_loki_with_labels_passes_them_to_sink() {
        let (listener, url) = mock_loki_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 204));

        let config = SinkConfig::Loki {
            url,
            batch_size: None,
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
    fn create_sink_loki_uses_default_batch_size_of_100_when_none() {
        // We can't inspect the internal batch_size directly, but we can verify
        // construction succeeds (batch_size=100 is valid) and write 99 lines
        // without triggering a flush (no server running → would fail if it tried).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener); // No server — any HTTP call would fail.

        let url = format!("http://127.0.0.1:{port}");
        let config = SinkConfig::Loki {
            url,
            batch_size: None, // should default to 100
        };
        let mut sink = create_sink(&config, None).expect("factory ok");

        // Write 99 lines — must not trigger a flush (no server running).
        for i in 0..99u32 {
            sink.write(format!("line {i}\n").as_bytes())
                .expect("write must succeed below batch_size");
        }
        // If we reach here without an error, batch_size defaults to ≥ 100.
    }

    #[test]
    fn create_sink_loki_with_invalid_url_returns_err() {
        let config = SinkConfig::Loki {
            url: "not-http://bad".to_string(),
            batch_size: None,
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
            serde_yaml::from_str(yaml).expect("loki-json-lines.yaml must deserialize correctly");
        assert_eq!(config.name, "app_logs_loki");
        assert!((config.rate - 10.0).abs() < f64::EPSILON);
        // Labels are at the scenario level, not inside the sink config.
        let labels = config.labels.as_ref().expect("labels must be present");
        assert_eq!(labels.get("job").map(String::as_str), Some("sonda"));
        assert_eq!(labels.get("env").map(String::as_str), Some("dev"));
        match &config.sink {
            SinkConfig::Loki { url, batch_size } => {
                assert_eq!(url, "http://localhost:3100");
                assert_eq!(batch_size, &Some(50));
            }
            other => panic!("expected Loki sink, got {other:?}"),
        }
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
