//! Buffered stdout sink.

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufWriter, Stdout};

use super::Sink;
use crate::SondaError;

/// A sink that writes encoded event data to stdout using a buffered writer.
pub struct StdoutSink {
    writer: BufWriter<Stdout>,
}

impl StdoutSink {
    pub fn new() -> Self {
        Self {
            writer: BufWriter::new(tokio::io::stdout()),
        }
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Sink for StdoutSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer
            .write_all(data)
            .await
            .map_err(SondaError::Sink)?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        self.writer.flush().await.map_err(SondaError::Sink)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_sink_constructs_without_panicking() {
        let _sink = StdoutSink::new();
    }

    #[test]
    fn stdout_sink_default_constructs_without_panicking() {
        let _sink = StdoutSink::default();
    }

    #[tokio::test]
    async fn write_and_flush_do_not_error() {
        let mut sink = StdoutSink::new();
        let write_result = sink.write(b"").await;
        assert!(write_result.is_ok());
        let flush_result = sink.flush().await;
        assert!(flush_result.is_ok());
    }

    #[tokio::test]
    async fn write_non_empty_data_does_not_error() {
        let mut sink = StdoutSink::new();
        let result = sink.write(b"up{} 1 1700000000000\n").await;
        assert!(result.is_ok());
        assert!(sink.flush().await.is_ok());
    }

    #[test]
    fn stdout_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StdoutSink>();
    }
}
