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
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the URL is not a valid HTTP(S) URL.
    pub fn new(
        url: &str,
        batch_size: usize,
        retry_policy: Option<RetryPolicy>,
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

        if self.batch.len() >= self.batch_size {
            self.send_batch()?;
        }

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
}
