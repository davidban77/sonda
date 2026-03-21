//! HTTP push sink — batches encoded telemetry and delivers it via HTTP POST.
//!
//! The sink accumulates encoded bytes in an internal buffer. When the buffer
//! reaches the configured `batch_size`, or when `flush` is called explicitly,
//! the accumulated bytes are sent as a single HTTP POST request.

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
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the URL cannot be parsed by ureq. Note:
    /// the actual TCP connection is not established until the first flush.
    pub fn new(url: &str, content_type: &str, batch_size: usize) -> Result<Self, SondaError> {
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
    fn send_batch(&mut self) -> Result<(), SondaError> {
        if self.batch.is_empty() {
            return Ok(());
        }

        let body = self.batch.clone();
        let result = self.do_post(&body);

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
                let retry_result = self.do_post(&body);
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
                        self.batch.clear();
                        Err(e)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Perform a single HTTP POST of `body` to `self.url`.
    ///
    /// Returns the HTTP status code on a successful transport-level exchange,
    /// or a [`SondaError::Sink`] on connection failure.
    fn do_post(&self, body: &[u8]) -> Result<u16, SondaError> {
        let response = self
            .client
            .post(&self.url)
            .set("Content-Type", &self.content_type)
            .send_bytes(body);

        match response {
            Ok(resp) => Ok(resp.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("HTTP push to '{}' failed: {}", self.url, e),
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
