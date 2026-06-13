//! HTTP push sink — batches encoded telemetry and delivers it via HTTP POST.
//!
//! The sink accumulates encoded bytes in an internal buffer. When the buffer
//! reaches the configured `batch_size`, or when `flush` is called explicitly,
//! the accumulated bytes are sent as a single HTTP POST request.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::sink::retry::RetryPolicy;
use crate::sink::Sink;
use crate::{RuntimeError, SondaError};

/// Default batch size in bytes (4 KiB) — sized so low-rate scenarios flush within seconds.
pub const DEFAULT_BATCH_SIZE: usize = 4 * 1024;

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
    /// Optional retry policy for transient failures.
    retry_policy: Option<RetryPolicy>,
    /// Maximum age a non-empty batch may reach before a time-based flush.
    /// `Duration::ZERO` disables time-based flushing.
    max_buffer_age: Duration,
    /// When the batch was last sent — drives the time-based flush check.
    last_flush_at: Instant,
    /// Whether the most recent `write()` triggered a successful flush rather than only buffering.
    last_write_delivered: bool,
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
    /// - `retry_policy` — optional retry policy for transient failures.
    ///   When `None`, errors are returned immediately (no retry).
    /// - `max_buffer_age` — maximum age a non-empty batch may reach before a
    ///   time-based flush. `Duration::ZERO` disables time-based flushing.
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
        retry_policy: Option<RetryPolicy>,
        max_buffer_age: Duration,
    ) -> Result<Self, SondaError> {
        // Validate the URL scheme before accepting the config.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid HTTP push URL '{}': must start with http:// or https://",
                    url
                ),
            )));
        }

        let client = ureq::AgentBuilder::new().build();

        Ok(Self {
            client,
            url: url.to_owned(),
            content_type: content_type.to_owned(),
            headers,
            batch: Vec::with_capacity(batch_size),
            batch_size,
            retry_policy,
            max_buffer_age,
            last_flush_at: Instant::now(),
            last_write_delivered: false,
        })
    }

    /// Classify whether an error from `do_post_checked` is retryable.
    ///
    /// Transport errors and 5xx/429 HTTP errors are retryable. 4xx errors
    /// (except 429) are not.
    fn is_retryable(err: &SondaError) -> bool {
        if let SondaError::Sink(io_err) = err {
            let msg = io_err.to_string();
            // 4xx (except 429) are not retryable.
            if msg.contains("HTTP 4") && !msg.contains("HTTP 429") {
                return false;
            }
            return true;
        }
        false
    }

    /// Perform a single HTTP POST and classify the response.
    ///
    /// - 2xx: returns `Ok(())`.
    /// - 4xx (except 429): logs a warning and returns `Err` with an
    ///   `InvalidInput` kind (non-retryable by convention).
    /// - 429, 5xx, transport errors: returns `Err` (retryable).
    ///
    /// This is a free-standing helper (not `&self`) so that `send_batch` can
    /// hold a reference to `self.batch` while calling it — avoiding a clone.
    fn do_post_checked(
        client: &ureq::Agent,
        url: &str,
        content_type: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<(), SondaError> {
        let status = Self::do_post(client, url, content_type, headers, body)?;

        if (200..300).contains(&status) {
            return Ok(());
        }

        if (400..500).contains(&status) && status != 429 {
            eprintln!(
                "sonda: http_push sink: received HTTP {} from '{}'; discarding batch",
                status, url
            );
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("HTTP {} from '{}'", status, url),
            )));
        }

        Err(SondaError::Sink(std::io::Error::other(format!(
            "HTTP {} from '{}'",
            status, url
        ))))
    }

    /// Perform a single HTTP POST of `body` to `url`.
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
            Err(e) => Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("HTTP push to '{}' failed: {}", url, e),
            ))),
        }
    }
}

#[async_trait]
impl Sink for HttpPushSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.batch.extend_from_slice(data);
        let size_reached = self.batch.len() >= self.batch_size;
        let age_reached =
            !self.max_buffer_age.is_zero() && self.last_flush_at.elapsed() >= self.max_buffer_age;
        let should_flush = size_reached || age_reached;
        if should_flush {
            self.send_via_blocking().await?;
        }
        self.last_write_delivered = should_flush;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        self.send_via_blocking().await
    }

    fn last_write_delivered(&self) -> bool {
        self.last_write_delivered
    }
}

impl HttpPushSink {
    /// Drain the batch into a blocking task that owns the ureq round-trip.
    async fn send_via_blocking(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }
        self.last_flush_at = Instant::now();
        let body = std::mem::replace(&mut self.batch, Vec::with_capacity(self.batch_size));
        let client = self.client.clone();
        let url = self.url.clone();
        let content_type = self.content_type.clone();
        let headers = self.headers.clone();
        let retry_policy = self.retry_policy.clone();
        let join = tokio::task::spawn_blocking(move || {
            do_send(
                &client,
                &url,
                &content_type,
                &headers,
                &body,
                retry_policy.as_ref(),
            )
        })
        .await;
        match join {
            Ok(r) => r,
            Err(e) => Err(SondaError::Runtime(RuntimeError::TaskPanicked(
                e.to_string(),
            ))),
        }
    }
}

fn do_send(
    client: &ureq::Agent,
    url: &str,
    content_type: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    retry_policy: Option<&RetryPolicy>,
) -> Result<(), SondaError> {
    let result = match retry_policy {
        Some(policy) => policy.execute(
            || HttpPushSink::do_post_checked(client, url, content_type, headers, body),
            HttpPushSink::is_retryable,
        ),
        None => HttpPushSink::do_post_checked(client, url, content_type, headers, body),
    };
    match &result {
        Err(SondaError::Sink(io_err)) if io_err.kind() == std::io::ErrorKind::InvalidInput => {
            Ok(())
        }
        _ => result,
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
            if let Some(rest) = lower.strip_prefix("content-length:") {
                content_length = rest.trim().parse().unwrap_or(0);
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
            None,
            Duration::ZERO,
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
            None,
            Duration::ZERO,
        );
        assert!(result.is_ok(), "https:// URL should be accepted");
    }

    #[test]
    fn new_with_invalid_scheme_returns_sink_error() {
        let result = HttpPushSink::new(
            "ftp://example.com/push",
            "text/plain",
            1024,
            HashMap::new(),
            None,
            Duration::ZERO,
        );
        assert!(result.is_err(), "non-http URL must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_bare_hostname_returns_sink_error() {
        let result = HttpPushSink::new(
            "example.com/push",
            "text/plain",
            1024,
            HashMap::new(),
            None,
            Duration::ZERO,
        );
        assert!(result.is_err(), "URL without scheme must be rejected");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn new_with_empty_url_returns_sink_error() {
        let result =
            HttpPushSink::new("", "text/plain", 1024, HashMap::new(), None, Duration::ZERO);
        assert!(result.is_err(), "empty URL must be rejected");
    }

    #[test]
    fn new_error_message_contains_invalid_url() {
        let bad_url = "not-a-url://bad";
        let result = HttpPushSink::new(
            bad_url,
            "text/plain",
            1024,
            HashMap::new(),
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
    // Batch accumulation — no HTTP call until threshold
    // -------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn write_below_batch_size_does_not_trigger_flush() {
        let (listener, url) = mock_server_listener();

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            1000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");

        for _ in 0..3 {
            sink.write(&[b'x'; 100])
                .await
                .expect("write should succeed");
        }

        listener.set_nonblocking(true).expect("set non-blocking");
        let accepted = listener.accept();
        assert!(
            accepted.is_err(),
            "no HTTP request should have been sent yet; got a connection"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_at_batch_size_triggers_flush() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            100,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(&[b'a'; 100])
            .await
            .expect("write should succeed");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body.len(), 100, "server should receive exactly 100 bytes");
        assert!(body.iter().all(|&b| b == b'a'));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_over_batch_size_triggers_flush() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 50, HashMap::new(), None, Duration::ZERO)
                .expect("construct sink");
        sink.write(&[b'z'; 80]).await.expect("write should succeed");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body.len(), 80);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_flush_sends_buffered_data() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"hello flush").await.expect("write");
        sink.flush()
            .await
            .expect("flush should send remaining data");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"hello flush");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_on_empty_batch_is_a_no_op() {
        let mut sink = HttpPushSink::new(
            "http://127.0.0.1:19999/push",
            "text/plain",
            1024,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        assert!(
            sink.flush().await.is_ok(),
            "flush on empty batch must be Ok"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_is_idempotent() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"data").await.expect("write");
        sink.flush().await.expect("first flush");

        let _body = handle.join().expect("mock server thread panicked");

        assert!(sink.flush().await.is_ok(), "second flush must also be Ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn last_write_delivered_is_false_when_write_only_buffers() {
        let (listener, url) = mock_server_listener();

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"buffered").await.expect("write buffers");

        assert!(
            !sink.last_write_delivered(),
            "a write that only buffers must report last_write_delivered() == false"
        );
        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(listener.accept().is_err(), "no flush should have fired");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn last_write_delivered_is_true_when_write_triggers_flush() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 4, HashMap::new(), None, Duration::ZERO)
                .expect("construct sink");
        sink.write(b"abcd").await.expect("write triggers flush");

        handle.join().expect("mock server thread panicked");
        assert!(
            sink.last_write_delivered(),
            "a write that triggers a successful flush must report last_write_delivered() == true"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_xx_response_clears_batch_and_returns_ok() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new(), None, Duration::ZERO)
                .expect("construct sink");
        let result = sink.write(b"x").await;
        let _body = handle.join().expect("mock server thread panicked");
        assert!(result.is_ok(), "2xx response must return Ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn four_xx_response_warns_and_discards_batch_returning_ok() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 400));

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new(), None, Duration::ZERO)
                .expect("construct sink");
        let result = sink.write(b"x").await;
        let _body = handle.join().expect("mock server thread panicked");
        assert!(
            result.is_ok(),
            "4xx response must return Ok (warn-and-continue)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_xx_without_retry_returns_error_after_one_attempt() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || {
            accept_one_and_respond(&listener, 500);
        });

        let mut sink =
            HttpPushSink::new(&url, "text/plain", 1, HashMap::new(), None, Duration::ZERO)
                .expect("construct sink");
        let result = sink.write(b"x").await;
        handle.join().expect("mock server thread panicked");
        assert!(result.is_err(), "5xx without retry must return Err");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_xx_with_retry_policy_retries_and_succeeds() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || {
            accept_one_and_respond(&listener, 500);
            accept_one_and_respond(&listener, 200);
        });

        use crate::sink::retry::RetryPolicy;
        let policy = RetryPolicy::from_config(&crate::sink::retry::RetryConfig {
            max_attempts: 2,
            initial_backoff: "1ms".to_string(),
            max_backoff: "10ms".to_string(),
        })
        .expect("valid retry config");

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            1,
            HashMap::new(),
            Some(policy),
            Duration::ZERO,
        )
        .expect("construct sink");
        let result = sink.write(b"x").await;
        handle.join().expect("mock server thread panicked");
        assert!(result.is_ok(), "5xx + successful retry must return Ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_xx_with_retry_policy_exhausted_returns_error() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || {
            accept_one_and_respond(&listener, 500);
            accept_one_and_respond(&listener, 500);
        });

        use crate::sink::retry::RetryPolicy;
        let policy = RetryPolicy::from_config(&crate::sink::retry::RetryConfig {
            max_attempts: 1,
            initial_backoff: "1ms".to_string(),
            max_backoff: "10ms".to_string(),
        })
        .expect("valid retry config");

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            1,
            HashMap::new(),
            Some(policy),
            Duration::ZERO,
        )
        .expect("construct sink");
        let result = sink.write(b"x").await;
        handle.join().expect("mock server thread panicked");
        assert!(result.is_err(), "persistent 5xx must return Err");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_to_refused_port_returns_sink_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}/push");
        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"hello").await.expect("write buffered ok");
        let result = sink.flush().await;
        assert!(result.is_err(), "flush to refused port must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn body_sent_to_server_matches_written_data() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let payload = b"metric_name{label=\"val\"} 42 1700000000000\n";
        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(payload).await.expect("write");
        sink.flush().await.expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, payload);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multiple_writes_accumulated_correctly_before_flush() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"part1").await.expect("write 1");
        sink.write(b"part2").await.expect("write 2");
        sink.write(b"part3").await.expect("write 3");
        sink.flush().await.expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"part1part2part3");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn time_based_flush_fires_when_buffer_age_exceeded() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            1_000_000,
            HashMap::new(),
            None,
            Duration::from_millis(50),
        )
        .expect("construct sink");

        sink.write(b"first").await.expect("write 1");
        thread::sleep(Duration::from_millis(200));
        sink.write(b"second").await.expect("write 2");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(
            body, b"firstsecond",
            "time-based flush must deliver both buffered writes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn zero_max_buffer_age_disables_time_based_flush() {
        let (listener, url) = mock_server_listener();

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            1_000_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");

        sink.write(b"first").await.expect("write 1");
        thread::sleep(Duration::from_millis(150));
        sink.write(b"second").await.expect("write 2");

        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(
            listener.accept().is_err(),
            "zero max_buffer_age must disable time-based flush"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn size_triggered_flush_resets_the_buffer_age_timer() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            4,
            HashMap::new(),
            None,
            Duration::from_secs(60),
        )
        .expect("construct sink");

        sink.write(b"abcd").await.expect("write fills batch");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"abcd", "size-triggered flush must deliver the batch");

        sink.write(b"e")
            .await
            .expect("partial write after a size flush must not time-flush immediately");
    }

    // -------------------------------------------------------------------------
    // Default batch size constant
    // -------------------------------------------------------------------------

    #[test]
    fn default_batch_size_is_4_kib() {
        assert_eq!(DEFAULT_BATCH_SIZE, 4 * 1024);
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

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_http_push_deserializes_with_required_fields() {
        let yaml = "type: http_push\nurl: \"http://localhost:9090/push\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
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

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_http_push_deserializes_with_all_fields() {
        let yaml = r#"
type: http_push
url: "http://localhost:9090/push"
content_type: "application/x-www-form-urlencoded"
batch_size: 8192
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
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

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_http_push_requires_url_field() {
        let yaml = "type: http_push";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
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
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("HttpPush"));
        assert!(debug_str.contains("9090"));
    }

    // -------------------------------------------------------------------------
    // Factory wiring: create_sink for HttpPush config
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn create_sink_http_push_config_with_valid_url_returns_ok() {
        let config = SinkConfig::HttpPush {
            url: "http://127.0.0.1:19998/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        let result = create_sink(&config, None).await;
        assert!(
            result.is_ok(),
            "factory must return Ok for valid http_push config"
        );
    }

    #[tokio::test]
    async fn create_sink_http_push_uses_default_batch_size_when_none() {
        let config = SinkConfig::HttpPush {
            url: "http://127.0.0.1:19997/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        assert!(create_sink(&config, None).await.is_ok());
    }

    #[tokio::test]
    async fn create_sink_http_push_with_invalid_url_returns_err() {
        let config = SinkConfig::HttpPush {
            url: "not-http://bad".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        let result = create_sink(&config, None).await;
        assert!(result.is_err(), "invalid URL must cause factory to fail");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_sink_http_push_sends_data_end_to_end() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let config = SinkConfig::HttpPush {
            url,
            content_type: Some("application/octet-stream".to_string()),
            batch_size: Some(10_000),
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        let mut sink = create_sink(&config, None).await.expect("factory ok");
        sink.write(b"end-to-end").await.expect("write");
        sink.flush().await.expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        assert_eq!(body, b"end-to-end");
    }

    // -------------------------------------------------------------------------
    // Custom headers support
    // -------------------------------------------------------------------------

    /// Accept one connection, read the full HTTP request, and return
    /// (headers_map, body_bytes). Responds with the given status.
    fn accept_one_capture_headers(
        listener: &TcpListener,
        status: u16,
    ) -> (HashMap<String, String>, Vec<u8>) {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut headers_map = HashMap::new();
        let mut content_length: usize = 0;

        // Read request line
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read request line");

        // Read headers until blank line
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.trim_end().split_once(':') {
                let k = key.trim().to_lowercase();
                let v = value.trim().to_string();
                if k == "content-length" {
                    content_length = v.parse().unwrap_or(0);
                }
                headers_map.insert(k, v);
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).expect("read body");

        let response =
            format!("HTTP/1.1 {status} OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",);
        stream.write_all(response.as_bytes()).ok();

        (headers_map, body)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_headers_are_sent_with_request() {
        let (listener, url) = mock_server_listener();

        let handle = thread::spawn(move || accept_one_capture_headers(&listener, 200));

        let mut custom = HashMap::new();
        custom.insert("Content-Encoding".to_string(), "snappy".to_string());
        custom.insert(
            "X-Prometheus-Remote-Write-Version".to_string(),
            "0.1.0".to_string(),
        );

        let mut sink = HttpPushSink::new(
            &url,
            "application/x-protobuf",
            10_000,
            custom,
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"test-payload").await.expect("write");
        sink.flush().await.expect("flush");

        let (headers, body) = handle.join().expect("mock server thread panicked");

        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("application/x-protobuf"),
            "Content-Type header must be set"
        );
        assert_eq!(
            headers.get("content-encoding").map(String::as_str),
            Some("snappy"),
            "Content-Encoding custom header must be sent"
        );
        assert_eq!(
            headers
                .get("x-prometheus-remote-write-version")
                .map(String::as_str),
            Some("0.1.0"),
            "X-Prometheus-Remote-Write-Version custom header must be sent"
        );
        assert_eq!(body, b"test-payload");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_custom_headers_does_not_break_request() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_capture_headers(&listener, 200));

        let mut sink = HttpPushSink::new(
            &url,
            "text/plain",
            10_000,
            HashMap::new(),
            None,
            Duration::ZERO,
        )
        .expect("construct sink");
        sink.write(b"data").await.expect("write");
        sink.flush().await.expect("flush");

        let (headers, body) = handle.join().expect("mock server thread panicked");
        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("text/plain"),
            "Content-Type should still be set with empty custom headers"
        );
        assert_eq!(body, b"data");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_headers_with_factory_config() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_capture_headers(&listener, 200));

        let mut hdr = HashMap::new();
        hdr.insert("X-Custom-Header".to_string(), "custom-value".to_string());

        let config = SinkConfig::HttpPush {
            url,
            content_type: Some("application/x-protobuf".to_string()),
            batch_size: Some(10_000),
            max_buffer_age: None,
            headers: Some(hdr),
            retry: None,
        };
        let mut sink = create_sink(&config, None).await.expect("factory ok");
        sink.write(b"factory-test").await.expect("write");
        sink.flush().await.expect("flush");

        let (headers, body) = handle.join().expect("mock server thread panicked");
        assert_eq!(
            headers.get("x-custom-header").map(String::as_str),
            Some("custom-value"),
            "custom header from factory config must be sent"
        );
        assert_eq!(body, b"factory-test");
    }
}
