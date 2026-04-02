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
use crate::sink::Sink;
use crate::SondaError;

/// Default batch size in TimeSeries entries (not bytes).
pub const DEFAULT_BATCH_SIZE: usize = 100;

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
    pub fn new(url: &str, batch_size: usize) -> Result<Self, SondaError> {
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
        write_request
            .encode(&mut proto_bytes)
            .map_err(|e| SondaError::Encoder(format!("protobuf encode error: {e}")))?;

        // Snappy-compress using raw (block) format.
        let mut snappy_encoder = snap::raw::Encoder::new();
        let compressed = snappy_encoder
            .compress_vec(&proto_bytes)
            .map_err(|e| SondaError::Encoder(format!("snappy compression error: {e}")))?;

        // HTTP POST with the required remote write headers.
        let result = self.do_post(&compressed);

        match result {
            Ok(status) if (200..300).contains(&status) => Ok(()),
            Ok(status) if (400..500).contains(&status) => {
                eprintln!(
                    "sonda: remote write sink received {} response from '{}'; discarding batch",
                    status, self.url
                );
                Ok(())
            }
            Ok(status) => {
                // 5xx or unexpected: retry once.
                let retry_result = self.do_post(&compressed);
                match retry_result {
                    Ok(retry_status) if (200..300).contains(&retry_status) => Ok(()),
                    Ok(retry_status) => Err(SondaError::Sink(std::io::Error::other(format!(
                        "remote write to '{}' failed with status {} (retry status {})",
                        self.url, status, retry_status
                    )))),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
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
