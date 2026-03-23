//! HTTP push sink — batches encoded telemetry and delivers it via HTTP POST.
//!
//! The sink accumulates encoded bytes in an internal buffer. When the buffer
//! reaches the configured `batch_size`, or when `flush` is called explicitly,
//! the accumulated bytes are sent as a single HTTP POST request.

use std::collections::HashMap;

use crate::sink::Sink;
use crate::SondaError;

/// Default batch size in bytes (64 KiB).
pub const DEFAULT_BATCH_SIZE: usize = 64 * 1024;

/// Delivers encoded telemetry to an HTTP endpoint via POST requests.
///
/// Bytes are accumulated in a batch buffer. When the buffer reaches
/// `batch_size`, the batch is automatically flushed. Call `flush()` at
/// shutdown to send any remaining buffered data.
///
/// Response handling:
/// - 2xx → Ok
/// - 4xx → log warning and continue (do not retry; client-side issue)
/// - 5xx → retry once, then return `SondaError::Sink`
pub struct HttpPushSink {
    /// The ureq HTTP agent used for all requests.
    client: ureq::Agent,
    /// Target URL for HTTP POST requests.
    url: String,
    /// Content-Type header value sent with every POST.
    content_type: String,
    /// Additional HTTP headers sent with every POST request.
    headers: HashMap<String, String>,
    /// Accumulated bytes waiting to be sent.
    batch: Vec<u8>,
    /// Flush threshold in bytes. When `batch.len() >= batch_size`, auto-flush.
    batch_size: usize,
}

impl HttpPushSink {
    /// Create a new `HttpPushSink`.
    ///
    /// # Arguments
    ///
    /// - `url` — the endpoint to POST batches to.
    /// - `content_type` — the `Content-Type` header value for each request.
    /// - `batch_size` — flush threshold in bytes. Use [`DEFAULT_BATCH_SIZE`]
    ///   if no override is needed.
    /// - `headers` — additional HTTP headers sent with every POST request.
    ///   These are applied after the `Content-Type` header. Use this for
    ///   protocol-specific headers such as `Content-Encoding: snappy` for
    ///   Prometheus remote write.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the URL cannot be parsed by ureq. Note:
    /// the actual TCP connection is not established until the first flush.
    pub fn new(
        url: &str,
        content_type: &str,
        batch_size: usize,
        headers: HashMap<String, String>,
    ) -> Result<Self, SondaError> {
        // Validate the URL scheme before accepting the config.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid HTTP push URL '{}': must start with http:// or https://",
                    url
                ),
            )
            .into());
        }

        let client = ureq::AgentBuilder::new().build();

        Ok(Self {
            client,
            url: url.to_owned(),
            content_type: content_type.to_owned(),
            headers,
            batch: Vec::with_capacity(batch_size),
            batch_size,
        })
    }

    /// Send the current batch to the configured endpoint.
    ///
    /// - 2xx responses are treated as success.
    /// - 4xx responses are logged as warnings and treated as success (no
    ///   retry — client-side errors such as auth failures should not block
    ///   metric generation).
    /// - 5xx responses are retried once. If the retry also returns 5xx or
    ///   fails, a [`SondaError::Sink`] is returned.
    /// - Transport errors (connection refused, DNS failure, etc.) clear the
    ///   batch to prevent unbounded buffer growth on repeated failures.
    fn send_batch(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }

        // Split the borrow: extract the fields needed by `do_post` so we can
        // pass `&self.batch` directly without cloning it.
        let client = &self.client;
        let url = &self.url;
        let content_type = &self.content_type;
        let headers = &self.headers;
        let result = Self::do_post(client, url, content_type, headers, &self.batch);

        match result {
            Ok(status) if (200..300).contains(&status) => {
                // Success — clear the batch.
                self.batch.clear();
                Ok(())
            }
            Ok(status) if (400..500).contains(&status) => {
                // 4xx: client-side error. Warn and continue without retrying.
                // The batch is cleared to prevent unbounded buffer growth.
                eprintln!(
                    "sonda: HTTP push sink received {} response from '{}'; discarding batch",
                    status, self.url
                );
                self.batch.clear();
                Ok(())
            }
            Ok(status) => {
                // 5xx or unexpected: retry once.
                let client = &self.client;
                let url = &self.url;
                let content_type = &self.content_type;
                let headers = &self.headers;
                let retry_result = Self::do_post(client, url, content_type, headers, &self.batch);
                match retry_result {
                    Ok(retry_status) if (200..300).contains(&retry_status) => {
                        self.batch.clear();
                        Ok(())
                    }
                    Ok(retry_status) => {
                        self.batch.clear();
                        Err(std::io::Error::other(format!(
                            "HTTP push to '{}' failed with status {} (retry status {})",
                            self.url, status, retry_status
                        ))
                        .into())
                    }
                    Err(e) => {
                        // Retry transport failure — clear batch to prevent unbounded growth.
                        self.batch.clear();
                        Err(e)
                    }
                }
            }
            Err(e) => {
                // Transport failure — clear batch to prevent unbounded growth.
                self.batch.clear();
                Err(e)
            }
        }
    }

    /// Perform a single HTTP POST of `body` to `url`.
    ///
    /// This is a free-standing helper (not `&self`) so that `send_batch` can
    /// hold a reference to `self.batch` while calling it — avoiding a clone.
    ///
    /// Returns the HTTP status code on a successful transport-level exchange,
    /// or a [`SondaError::Sink`] on connection failure.
    fn do_post(
        client: &ureq::Agent,
        url: &str,
        content_type: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<u16, SondaError> {
        let mut request = client.post(url).set("Content-Type", content_type);

        for (key, value) in headers {
            request = request.set(key, value);
        }

        let response = request.send_bytes(body);

        match response {
            Ok(resp) => Ok(resp.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("HTTP push to '{}' failed: {}", url, e),
            )
            .into()),
        }
    }
}

impl Sink for HttpPushSink {
    /// Append encoded event data to the internal batch buffer.
    ///
    /// When the buffer reaches `batch_size`, the batch is automatically
    /// flushed via an HTTP POST. Returns an error only if the auto-flush
    /// fails.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.batch.extend_from_slice(data);
        if self.batch.len() >= self.batch_size {
            self.send_batch()?;
        }
        Ok(())
    }

    /// Flush any remaining buffered data to the HTTP endpoint.
    ///
    /// Safe to call multiple times. Returns `Ok(())` immediately if the
    /// batch is empty.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.send_batch()
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
    // Helpers
    // -------------------------------------------------------------------------

    /// Bind a TCP listener on an OS-chosen port; return (listener, url).
    fn mock_server_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();
        let url = format!("http://127.0.0.1:{port}/push");
        (listener, url)
    }

    /// Accept one connection, read the full HTTP request, and respond with the
    /// given status code (e.g. 200, 400, 500).  Returns the request body bytes.
    fn accept_one_and_respond(listener: &TcpListener, status: u16) -> Vec<u8> {
        let (mut stream, _) = listener.accept().expect("accept");
        let body = read_http_request_body(&mut stream);
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            reason = if status < 300 {
                "OK"
            } else if status < 500 {
                "Bad Request"
            } else {
                "Internal Server Error"
            }
        );
        stream.write_all(response.as_bytes()).ok();
        body
    }

    /// Read HTTP request headers and return the body bytes.
    fn read_http_request_body(stream: &mut TcpStream) -> Vec<u8> {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        // Read headers until blank line.
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
    // Construction
    // -------------------------------------------------------------------------

    #[test]
    fn new_with_http_url_succeeds() {
        let result = HttpPushSink::new(
            "http://127.0.0.1:9999/push",
            "text/plain",
            1024,
            HashMap::new(),
        );
        assert!(result.is_ok(), "http:// URL should be accepted");
    }

    #[test]
    fn new_with_https_url_succeeds() {
        let result = HttpPushSink::new(
            "https://example.com/push",
            "text/plain",
            1024,
            HashMap::new(),
        );
        assert!(result.is_ok(), "https:// URL should be accepted");
    }

    #[test]
    fn new_with_invalid_scheme_returns_sink_error() {
        let result =
            HttpPushSink::new("ftp://example.com/push", "text/plain", 1024, HashMap::new());
        assert!(result.is_err(), "non-http URL must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_bare_hostname_returns_sink_error() {
        let result = HttpPushSink::new("example.com/push", "text/plain", 1024, HashMap::new());
        assert!(result.is_err(), "URL without scheme must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_empty_url_returns_sink_error() {
        let result = HttpPushSink::new("", "text/plain", 1024, HashMap::new());
        assert!(result.is_err(), "empty URL must be rejected");
    }

    #[test]
    fn new_error_message_contains_invalid_url() {
        let bad_url = "not-a-url://bad";
        let result = HttpPushSink::new(bad_url, "text/plain", 1024, HashMap::new());
        let err = result.err().expect("should be Err");
        let msg = err.to_string();
        assert!(
            msg.contains(bad_url),
            "error message should contain the bad URL; got: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Batch accumulation — no HTTP call until threshold
    // -------------------------------------------------------------------------

    #[test]
    fn write_below_batch_size_does_not_trigger_flush() {
        // batch_size = 1000; write 3 × 100 bytes → no request should go out.
        // We start a server that would panic if it received a connection.
        let (listener, url) = mock_server_listener();

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1000, HashMap::new()).expect("construct sink");

        // Write 300 bytes total — below the 1000-byte threshold.
        for _ in 0..3 {
            sink.write(&[b'x'; 100]).expect("write should succeed");
        }

        // Set a very short timeout so the test does not hang waiting for a
        // connection that should never arrive.
        listener.set_nonblocking(true).expect("set non-blocking");
        let accepted = listener.accept();
        assert!(
            accepted.is_err(),
            "no HTTP request should have been sent yet; got a connection"
        );
    }

    #[test]
    fn write_at_batch_size_triggers_flush() {
        let (listener, url) = mock_server_listener();

        // Accept exactly one request in a background thread.
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 100, HashMap::new()).expect("construct sink");
        // Write exactly batch_size bytes → should auto-flush.
        sink.write(&[b'a'; 100]).expect("write should succeed");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body.len(), 100, "server should receive exactly 100 bytes");
        assert!(body.iter().all(|&b| b == b'a'));
    }

    #[test]
    fn write_over_batch_size_triggers_flush() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 50, HashMap::new()).expect("construct sink");
        // Write 80 bytes → exceeds 50-byte threshold → auto-flush.
        sink.write(&[b'z'; 80]).expect("write should succeed");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body.len(), 80);
    }

    // -------------------------------------------------------------------------
    // Explicit flush — remaining data sent
    // -------------------------------------------------------------------------

    #[test]
    fn explicit_flush_sends_buffered_data() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new()).expect("construct sink");
        // Write 42 bytes — well below 10 000-byte threshold.
        sink.write(b"hello flush").expect("write");
        sink.flush().expect("flush should send remaining data");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"hello flush");
    }

    #[test]
    fn flush_on_empty_batch_is_a_no_op() {
        // No server running — if flush() sent a request it would fail.
        let mut sink = HttpPushSink::new(
            "http://127.0.0.1:19999/push",
            "text/plain",
            1024,
            HashMap::new(),
        )
        .expect("construct sink");
        // Empty batch: flush should return Ok without making any network call.
        assert!(sink.flush().is_ok(), "flush on empty batch must be Ok");
    }

    #[test]
    fn flush_is_idempotent() {
        let (listener, url) = mock_server_listener();

        // First flush sends data; second flush is a no-op.
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new()).expect("construct sink");
        sink.write(b"data").expect("write");
        sink.flush().expect("first flush");

        let _body = handle.join().expect("mock server thread panicked");

        // Second flush — batch is now empty, must succeed without panicking.
        assert!(sink.flush().is_ok(), "second flush must also be Ok");
    }

    // -------------------------------------------------------------------------
    // Response handling
    // -------------------------------------------------------------------------

    #[test]
    fn two_xx_response_clears_batch_and_returns_ok() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new()).expect("construct sink");
        // batch_size=1 → immediate flush on write.
        let result = sink.write(b"x");
        let _body = handle.join().expect("mock server thread panicked");
        assert!(result.is_ok(), "2xx response must return Ok");
    }

    #[test]
    fn four_xx_response_warns_and_discards_batch_returning_ok() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 400));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new()).expect("construct sink");
        let result = sink.write(b"x");
        let _body = handle.join().expect("mock server thread panicked");
        // 4xx → warn + discard, but NOT an error from the sink's perspective.
        assert!(
            result.is_ok(),
            "4xx response must return Ok (warn-and-continue)"
        );
    }

    #[test]
    fn five_xx_response_retries_once_and_returns_sink_error() {
        // Respond with 500 to both the initial attempt and the retry.
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || {
            // First request (original attempt).
            accept_one_and_respond(&listener, 500);
            // Second request (retry).
            accept_one_and_respond(&listener, 500);
        });

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new()).expect("construct sink");
        let result = sink.write(b"x");
        handle.join().expect("mock server thread panicked");
        assert!(result.is_err(), "persistent 5xx must return Err");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn five_xx_then_two_xx_on_retry_returns_ok() {
        // First request returns 500; retry returns 200.
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || {
            accept_one_and_respond(&listener, 500);
            accept_one_and_respond(&listener, 200);
        });

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new()).expect("construct sink");
        let result = sink.write(b"x");
        handle.join().expect("mock server thread panicked");
        assert!(result.is_ok(), "5xx + successful retry must return Ok");
    }

    // -------------------------------------------------------------------------
    // Connection refused
    // -------------------------------------------------------------------------

    #[test]
    fn flush_to_refused_port_returns_sink_error() {
        // Bind then immediately drop — port is unused.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}/push");
        let mut sink =
            HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new()).expect("construct sink");
        sink.write(b"hello").expect("write buffered ok");
        let result = sink.flush();
        assert!(result.is_err(), "flush to refused port must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    // -------------------------------------------------------------------------
    // Body content — mock server verifies exact bytes
    // -------------------------------------------------------------------------

    #[test]
    fn body_sent_to_server_matches_written_data() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let payload = b"metric_name{label=\"val\"} 42 1700000000000\n";
        let mut sink =
            HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new()).expect("construct sink");
        sink.write(payload).expect("write");
        sink.flush().expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, payload);
    }

    #[test]
    fn multiple_writes_accumulated_correctly_before_flush() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new()).expect("construct sink");
        sink.write(b"part1").expect("write 1");
        sink.write(b"part2").expect("write 2");
        sink.write(b"part3").expect("write 3");
        sink.flush().expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"part1part2part3");
    }

    // -------------------------------------------------------------------------
    // Default batch size constant
    // -------------------------------------------------------------------------

    #[test]
    fn default_batch_size_is_64_kib() {
        assert_eq!(DEFAULT_BATCH_SIZE, 64 * 1024);
    }

    // -------------------------------------------------------------------------
    // Trait contract: Send + Sync
    // -------------------------------------------------------------------------

    #[test]
    fn http_push_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HttpPushSink>();
    }

    // -------------------------------------------------------------------------
    // SinkConfig::HttpPush deserialization
    // -------------------------------------------------------------------------

    #[test]
    fn sink_config_http_push_deserializes_with_required_fields() {
        let yaml = "type: http_push\nurl: \"http://localhost:9090/push\"";
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush {
                url,
                content_type,
                batch_size,
                ..
            } => {
                assert_eq!(url, "http://localhost:9090/push");
                assert!(
                    content_type.is_none(),
                    "content_type should default to None"
                );
                assert!(batch_size.is_none(), "batch_size should default to None");
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_http_push_deserializes_with_all_fields() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
content_type: "application/x-www-form-urlencoded"
batch_size: 8192
"#;
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::HttpPush {
                url,
                content_type,
                batch_size,
                ..
            } => {
                assert_eq!(url, "http://localhost:9090/push");
                assert_eq!(
                    content_type.as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
                assert_eq!(batch_size, Some(8192));
            }
            other => panic!("expected HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_http_push_requires_url_field() {
        let yaml = "type: http_push";
        let result: Result<SinkConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "http_push without url must fail deserialization"
        );
    }

    #[test]
    fn sink_config_http_push_is_cloneable_and_debuggable() {
        let config = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: Some("text/plain".to_string()),
            batch_size: Some(1024),
            headers: None,
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("HttpPush"));
        assert!(debug_str.contains("9090"));
    }

    // -------------------------------------------------------------------------
    // Factory wiring: create_sink for HttpPush config
    // -------------------------------------------------------------------------

    #[test]
    fn create_sink_http_push_config_with_valid_url_returns_ok() {
        let config = SinkConfig::HttpPush {
            url: "http://127.0.0.1:19998/push".to_string(),
            content_type: None,
            batch_size: None,
            headers: None,
        };
        // Construction must succeed (no network call yet).
        let result = create_sink(&config);
        assert!(
            result.is_ok(),
            "factory must return Ok for valid http_push config"
        );
    }

    #[test]
    fn create_sink_http_push_uses_default_batch_size_when_none() {
        // No network call on construction, so any host is fine.
        let config = SinkConfig::HttpPush {
            url: "http://127.0.0.1:19997/push".to_string(),
            content_type: None,
            batch_size: None,
            headers: None,
        };
        assert!(create_sink(&config).is_ok());
    }

    #[test]
    fn create_sink_http_push_with_invalid_url_returns_err() {
        let config = SinkConfig::HttpPush {
            url: "not-http://bad".to_string(),
            content_type: None,
            batch_size: None,
            headers: None,
        };
        let result = create_sink(&config);
        assert!(result.is_err(), "invalid URL must cause factory to fail");
    }

    #[test]
    fn create_sink_http_push_sends_data_end_to_end() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let config = SinkConfig::HttpPush {
            url,
            content_type: Some("application/octet-stream".to_string()),
            batch_size: Some(10_000),
            headers: None,
        };
        let mut sink = create_sink(&config).expect("factory ok");
        sink.write(b"end-to-end").expect("write");
        sink.flush().expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"end-to-end");
    }
}
