//! Prometheus remote write sink — batches TimeSeries and delivers as a single
//! snappy-compressed WriteRequest per HTTP POST.
//!
//! This sink is designed to work with the [`RemoteWriteEncoder`](crate::encoder::remote_write::RemoteWriteEncoder),
//! which writes length-prefixed protobuf `TimeSeries` bytes. The sink:
//!
//! 1. Receives raw bytes from the encoder via `write()`.
//! 2. Parses each length-prefixed `TimeSeries` and accumulates them in a Vec.
//! 3. When the batch reaches `batch_size` entries (or on `flush()`), wraps all
//!    accumulated `TimeSeries` into a single `WriteRequest`, prost-encodes it,
//!    snappy-compresses the result, and HTTP POSTs it with the correct headers.
//!
//! This design solves the batching corruption problem: individually snappy-compressed
//! protobuf chunks cannot be concatenated. By deferring compression to flush time,
//! each HTTP POST contains exactly one valid snappy-compressed `WriteRequest`.
//!
//! Requires the `remote-write` feature flag.

use std::time::{Duration, Instant};

use prost::Message;

use crate::encoder::remote_write::{parse_length_prefixed_timeseries, TimeSeries, WriteRequest};
use crate::sink::retry::RetryPolicy;
use crate::sink::Sink;
use crate::{EncoderError, SondaError};

/// Default batch size in TimeSeries entries — sized so low-rate scenarios flush within seconds.
pub const DEFAULT_BATCH_SIZE: usize = 5;

/// Delivers metric events to a Prometheus remote write endpoint.
///
/// TimeSeries are accumulated in an internal batch. When the batch reaches
/// `batch_size` entries, or when `flush()` is called, the batch is wrapped
/// in a single `WriteRequest`, prost-encoded, snappy-compressed, and sent
/// via HTTP POST with the appropriate protocol headers:
///
/// - `Content-Type: application/x-protobuf`
/// - `Content-Encoding: snappy`
/// - `X-Prometheus-Remote-Write-Version: 0.1.0`
///
/// Response handling follows the same policy as `HttpPushSink`:
/// - 2xx: success
/// - 4xx: log warning, discard batch, continue
/// - 5xx: retry once, then error
pub struct RemoteWriteSink {
    /// The ureq HTTP agent used for all requests.
    client: ureq::Agent,
    /// Target URL for HTTP POST requests.
    url: String,
    /// Accumulated TimeSeries waiting to be sent.
    batch: Vec<TimeSeries>,
    /// Flush threshold in number of TimeSeries entries.
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

impl RemoteWriteSink {
    /// Create a new `RemoteWriteSink`.
    ///
    /// # Arguments
    ///
    /// - `url` — the remote write endpoint to POST to (e.g.,
    ///   `http://localhost:8428/api/v1/write`).
    /// - `batch_size` — flush threshold in number of TimeSeries entries.
    ///   Use [`DEFAULT_BATCH_SIZE`] if no override is needed.
    /// - `max_buffer_age` — maximum age a non-empty batch may reach before a
    ///   time-based flush. `Duration::ZERO` disables time-based flushing.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the URL is not a valid HTTP(S) URL.
    pub fn new(
        url: &str,
        batch_size: usize,
        retry_policy: Option<RetryPolicy>,
        max_buffer_age: Duration,
    ) -> Result<Self, SondaError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "invalid remote write URL '{}': must start with http:// or https://",
                    url
                ),
            )));
        }

        let client = ureq::AgentBuilder::new().build();

        Ok(Self {
            client,
            url: url.to_owned(),
            batch: Vec::with_capacity(batch_size),
            batch_size,
            retry_policy,
            max_buffer_age,
            last_flush_at: Instant::now(),
            last_write_delivered: false,
        })
    }

    /// Build a WriteRequest from the current batch, prost-encode, snappy-compress,
    /// and HTTP POST to the configured endpoint.
    ///
    /// Clears the batch on success or on unrecoverable error (to prevent unbounded
    /// buffer growth).
    fn send_batch(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }

        // Reset on attempt, not success — the batch is cleared either way below.
        self.last_flush_at = Instant::now();

        // Build one WriteRequest containing all accumulated TimeSeries.
        let write_request = WriteRequest {
            timeseries: std::mem::take(&mut self.batch),
        };

        // Prost-encode the WriteRequest.
        let encoded_len = write_request.encoded_len();
        let mut proto_bytes = Vec::with_capacity(encoded_len);
        write_request.encode(&mut proto_bytes).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!("protobuf encode error: {e}")))
        })?;

        // Snappy-compress using raw (block) format.
        let mut snappy_encoder = snap::raw::Encoder::new();
        let compressed = snappy_encoder.compress_vec(&proto_bytes).map_err(|e| {
            SondaError::Encoder(EncoderError::Other(format!(
                "snappy compression error: {e}"
            )))
        })?;

        // POST with retry if configured.
        let result = match &self.retry_policy {
            Some(policy) => {
                let policy = policy.clone();
                policy.execute(|| self.do_post_checked(&compressed), Self::is_retryable)
            }
            None => self.do_post_checked(&compressed),
        };

        // 4xx errors (except 429) are non-retryable and treated as warn-and-discard.
        match &result {
            Err(SondaError::Sink(io_err)) if io_err.kind() == std::io::ErrorKind::InvalidInput => {
                Ok(())
            }
            _ => result,
        }
    }

    /// Perform a single HTTP POST and classify the response.
    ///
    /// - 2xx: `Ok(())`.
    /// - 4xx (except 429): warns and returns non-retryable `Err`.
    /// - 429, 5xx, transport: retryable `Err`.
    fn do_post_checked(&self, body: &[u8]) -> Result<(), SondaError> {
        let status = self.do_post(body)?;

        if (200..300).contains(&status) {
            return Ok(());
        }

        if (400..500).contains(&status) && status != 429 {
            eprintln!(
                "sonda: remote_write sink: received HTTP {} from '{}'; discarding batch",
                status, self.url
            );
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("HTTP {} from '{}'", status, self.url),
            )));
        }

        Err(SondaError::Sink(std::io::Error::other(format!(
            "HTTP {} from '{}'",
            status, self.url
        ))))
    }

    /// Classify whether an error is retryable.
    fn is_retryable(err: &SondaError) -> bool {
        if let SondaError::Sink(io_err) = err {
            let msg = io_err.to_string();
            if msg.contains("HTTP 4") && !msg.contains("HTTP 429") {
                return false;
            }
            return true;
        }
        false
    }

    /// Perform a single HTTP POST of snappy-compressed protobuf to the endpoint.
    ///
    /// Sets the required Prometheus remote write headers:
    /// - `Content-Type: application/x-protobuf`
    /// - `Content-Encoding: snappy`
    /// - `X-Prometheus-Remote-Write-Version: 0.1.0`
    fn do_post(&self, body: &[u8]) -> Result<u16, SondaError> {
        let response = self
            .client
            .post(&self.url)
            .set("Content-Type", "application/x-protobuf")
            .set("Content-Encoding", "snappy")
            .set("X-Prometheus-Remote-Write-Version", "0.1.0")
            .send_bytes(body);

        match response {
            Ok(resp) => Ok(resp.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("remote write to '{}' failed: {}", self.url, e),
            ))),
        }
    }
}

impl Sink for RemoteWriteSink {
    /// Accept length-prefixed TimeSeries bytes from the encoder.
    ///
    /// Parses each `TimeSeries` from the data and adds it to the internal batch.
    /// When the batch reaches `batch_size` entries, an automatic flush is triggered.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let timeseries_list = parse_length_prefixed_timeseries(data)?;
        self.batch.extend(timeseries_list);

        let size_reached = self.batch.len() >= self.batch_size;
        let age_reached =
            !self.max_buffer_age.is_zero() && self.last_flush_at.elapsed() >= self.max_buffer_age;
        let should_flush = size_reached || age_reached;
        if should_flush {
            self.send_batch()?;
        }
        self.last_write_delivered = should_flush;

        Ok(())
    }

    /// Flush any remaining buffered TimeSeries to the remote write endpoint.
    ///
    /// Builds one `WriteRequest` containing all buffered `TimeSeries`, prost-encodes,
    /// snappy-compresses, and HTTP POSTs the result. Safe to call multiple times;
    /// returns `Ok(())` immediately if the batch is empty.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.send_batch()
    }

    fn last_write_delivered(&self) -> bool {
        self.last_write_delivered
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;
    use crate::encoder::remote_write::RemoteWriteEncoder;
    use crate::encoder::Encoder;
    use crate::model::metric::{Labels, MetricEvent};
    use crate::sink::{create_sink, Sink, SinkConfig};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn mock_server_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();
        let url = format!("http://127.0.0.1:{port}/api/v1/write");
        (listener, url)
    }

    /// Accept one connection, read the full HTTP request, respond with `status`.
    /// Returns the request body bytes (snappy-compressed protobuf WriteRequest).
    fn accept_one_and_respond(listener: &TcpListener, status: u16) -> Vec<u8> {
        let (mut stream, _) = listener.accept().expect("accept");
        let body = read_http_request_body(&mut stream);
        let response =
            format!("HTTP/1.1 {status} OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).ok();
        body
    }

    fn read_http_request_body(stream: &mut TcpStream) -> Vec<u8> {
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

    /// Decode the snappy-compressed protobuf WriteRequest a server received.
    fn decode_write_request(body: &[u8]) -> WriteRequest {
        let proto_bytes = snap::raw::Decoder::new()
            .decompress_vec(body)
            .expect("snappy decompress");
        WriteRequest::decode(proto_bytes.as_slice()).expect("protobuf decode")
    }

    /// Encode one metric event into length-prefixed TimeSeries bytes the sink
    /// accepts via `write()`.
    fn encode_one(name: &str, value: f64) -> Vec<u8> {
        let labels = Labels::from_pairs(&[("host", "server1")]).expect("valid labels");
        let event = MetricEvent::new(name.to_string(), value, labels).expect("valid metric name");
        let mut buf = Vec::new();
        RemoteWriteEncoder::new()
            .encode_metric(&event, &mut buf)
            .expect("encode ok");
        buf
    }

    // -------------------------------------------------------------------------
    // Construction
    // -------------------------------------------------------------------------

    #[test]
    fn new_with_http_url_succeeds() {
        let result = RemoteWriteSink::new(
            "http://127.0.0.1:9999/api/v1/write",
            5,
            None,
            Duration::ZERO,
        );
        assert!(result.is_ok(), "http:// URL must be accepted");
    }

    #[test]
    fn new_with_invalid_scheme_returns_sink_error() {
        let result = RemoteWriteSink::new("ftp://example.com/write", 5, None, Duration::ZERO);
        assert!(result.is_err(), "non-http URL must be rejected");
        assert!(matches!(result.err().unwrap(), SondaError::Sink(_)));
    }

    #[test]
    fn remote_write_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RemoteWriteSink>();
    }

    // -------------------------------------------------------------------------
    // Batch accumulation and explicit flush
    // -------------------------------------------------------------------------

    #[test]
    fn write_below_batch_size_does_not_trigger_flush() {
        let (listener, url) = mock_server_listener();

        let mut sink =
            RemoteWriteSink::new(&url, 100, None, Duration::ZERO).expect("construct sink");
        sink.write(&encode_one("cpu", 1.0)).expect("write");

        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(
            listener.accept().is_err(),
            "no request should have been sent below batch_size"
        );
    }

    #[test]
    fn explicit_flush_sends_buffered_data() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink =
            RemoteWriteSink::new(&url, 10_000, None, Duration::ZERO).expect("construct sink");
        sink.write(&encode_one("cpu", 1.0)).expect("write");
        sink.flush().expect("flush");

        let body = handle.join().expect("mock server thread panicked");
        let request = decode_write_request(&body);
        assert_eq!(request.timeseries.len(), 1, "flush must deliver the batch");
    }

    // -------------------------------------------------------------------------
    // last_write_delivered — buffered vs flushed
    // -------------------------------------------------------------------------

    #[test]
    fn last_write_delivered_is_false_when_write_only_buffers() {
        let (listener, url) = mock_server_listener();

        let mut sink =
            RemoteWriteSink::new(&url, 100, None, Duration::ZERO).expect("construct sink");
        sink.write(&encode_one("cpu", 1.0)).expect("write buffers");

        assert!(
            !sink.last_write_delivered(),
            "a write that only buffers must report last_write_delivered() == false"
        );
        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(listener.accept().is_err(), "no flush should have fired");
    }

    #[test]
    fn last_write_delivered_is_true_when_write_triggers_flush() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        let mut sink = RemoteWriteSink::new(&url, 1, None, Duration::ZERO).expect("construct sink");
        sink.write(&encode_one("cpu", 1.0))
            .expect("write triggers flush");

        handle.join().expect("mock server thread panicked");
        assert!(
            sink.last_write_delivered(),
            "a write that triggers a successful flush must report last_write_delivered() == true"
        );
    }

    // -------------------------------------------------------------------------
    // Time-based flush
    // -------------------------------------------------------------------------

    #[test]
    fn time_based_flush_fires_when_buffer_age_exceeded() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        // batch_size large enough that size never triggers; short max_buffer_age.
        let mut sink = RemoteWriteSink::new(&url, 10_000, None, Duration::from_millis(50))
            .expect("construct sink");

        sink.write(&encode_one("first", 1.0)).expect("write 1");
        thread::sleep(Duration::from_millis(200));
        // Second write is past max_buffer_age → triggers a time-based flush.
        sink.write(&encode_one("second", 2.0)).expect("write 2");

        let body = handle.join().expect("mock server thread panicked");
        let request = decode_write_request(&body);
        assert_eq!(
            request.timeseries.len(),
            2,
            "time-based flush must deliver both buffered TimeSeries"
        );
    }

    #[test]
    fn zero_max_buffer_age_disables_time_based_flush() {
        let (listener, url) = mock_server_listener();

        let mut sink =
            RemoteWriteSink::new(&url, 10_000, None, Duration::ZERO).expect("construct sink");

        sink.write(&encode_one("first", 1.0)).expect("write 1");
        thread::sleep(Duration::from_millis(150));
        sink.write(&encode_one("second", 2.0)).expect("write 2");

        // With time-based flush disabled, no request should have arrived.
        listener.set_nonblocking(true).expect("set non-blocking");
        assert!(
            listener.accept().is_err(),
            "zero max_buffer_age must disable time-based flush"
        );
    }

    #[test]
    fn size_triggered_flush_resets_the_buffer_age_timer() {
        let (listener, url) = mock_server_listener();
        let handle = thread::spawn(move || accept_one_and_respond(&listener, 200));

        // Small batch_size, max_buffer_age comfortably longer than the test runs.
        let mut sink =
            RemoteWriteSink::new(&url, 2, None, Duration::from_secs(60)).expect("construct sink");

        // Fill the batch — the size trigger fires.
        sink.write(&encode_one("a", 1.0)).expect("write 1");
        sink.write(&encode_one("b", 2.0)).expect("write 2");

        let body = handle.join().expect("mock server thread panicked");
        let request = decode_write_request(&body);
        assert_eq!(
            request.timeseries.len(),
            2,
            "size-triggered flush must deliver the full batch"
        );

        // The size flush reset last_flush_at; a subsequent partial-batch write
        // must NOT immediately time-flush against the (now closed) listener.
        sink.write(&encode_one("c", 3.0))
            .expect("partial write after a size flush must not time-flush immediately");
    }

    // -------------------------------------------------------------------------
    // Factory wiring: create_sink for RemoteWrite config
    // -------------------------------------------------------------------------

    #[test]
    fn create_sink_remote_write_with_valid_url_returns_ok() {
        let config = SinkConfig::RemoteWrite {
            url: "http://127.0.0.1:19999/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: None,
            retry: None,
        };
        assert!(create_sink(&config, None).is_ok());
    }

    #[test]
    fn create_sink_remote_write_with_invalid_max_buffer_age_returns_err() {
        let config = SinkConfig::RemoteWrite {
            url: "http://127.0.0.1:19999/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
        };
        assert!(
            create_sink(&config, None).is_err(),
            "invalid max_buffer_age must cause the factory to fail"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_remote_write_deserializes_with_max_buffer_age() {
        let yaml = r#"
type: remote_write
url: "http://localhost:8428/api/v1/write"
max_buffer_age: 10s
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::RemoteWrite { max_buffer_age, .. } => {
                assert_eq!(max_buffer_age.as_deref(), Some("10s"));
            }
            other => panic!("expected RemoteWrite variant, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_remote_write_max_buffer_age_defaults_to_none() {
        let yaml = "type: remote_write\nurl: \"http://localhost:8428/api/v1/write\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::RemoteWrite { max_buffer_age, .. } => {
                assert!(
                    max_buffer_age.is_none(),
                    "max_buffer_age should default to None"
                );
            }
            other => panic!("expected RemoteWrite variant, got {other:?}"),
        }
    }
}
