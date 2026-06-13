//! TCP sink — delivers encoded telemetry over a persistent TCP connection.

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

use crate::sink::retry::RetryPolicy;
use crate::sink::Sink;
use crate::SondaError;

/// Delivers encoded telemetry over a buffered TCP connection.
pub struct TcpSink {
    writer: BufWriter<TcpStream>,
    addr: String,
    retry_policy: Option<RetryPolicy>,
}

impl TcpSink {
    pub async fn new(addr: &str, retry_policy: Option<RetryPolicy>) -> Result<Self, SondaError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP connect to {addr}: {e}")))
            .map_err(SondaError::Sink)?;
        Ok(Self {
            writer: BufWriter::new(stream),
            addr: addr.to_owned(),
            retry_policy,
        })
    }

    async fn reconnect(&mut self) -> Result<(), SondaError> {
        let stream = TcpStream::connect(&self.addr)
            .await
            .map_err(|e| {
                std::io::Error::new(e.kind(), format!("TCP reconnect to {}: {e}", self.addr))
            })
            .map_err(SondaError::Sink)?;
        self.writer = BufWriter::new(stream);
        Ok(())
    }

    async fn write_once(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer
            .write_all(data)
            .await
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP write to {}: {e}", self.addr)))
            .map_err(SondaError::Sink)
    }

    async fn flush_once(&mut self) -> Result<(), SondaError> {
        self.writer
            .flush()
            .await
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP flush to {}: {e}", self.addr)))
            .map_err(SondaError::Sink)
    }
}

#[async_trait]
impl Sink for TcpSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let mut last_error = match self.write_once(data).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let Some(policy) = self.retry_policy.clone() else {
            return Err(last_error);
        };
        for attempt in 0..policy.max_attempts() {
            let backoff = policy.jittered_backoff(attempt);
            eprintln!(
                "sonda: retry {}/{} after {}ms (error: {})",
                attempt + 1,
                policy.max_attempts(),
                backoff.as_millis(),
                last_error,
            );
            tokio::time::sleep(backoff).await;
            if let Err(e) = self.reconnect().await {
                last_error = e;
                continue;
            }
            match self.write_once(data).await {
                Ok(()) => return Ok(()),
                Err(e) => last_error = e,
            }
        }
        eprintln!(
            "sonda: all {} retries exhausted (last error: {})",
            policy.max_attempts(),
            last_error,
        );
        Err(last_error)
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        let mut last_error = match self.flush_once().await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let Some(policy) = self.retry_policy.clone() else {
            return Err(last_error);
        };
        for attempt in 0..policy.max_attempts() {
            let backoff = policy.jittered_backoff(attempt);
            eprintln!(
                "sonda: retry {}/{} after {}ms (error: {})",
                attempt + 1,
                policy.max_attempts(),
                backoff.as_millis(),
                last_error,
            );
            tokio::time::sleep(backoff).await;
            if let Err(e) = self.reconnect().await {
                last_error = e;
                continue;
            }
            match self.flush_once().await {
                Ok(()) => return Ok(()),
                Err(e) => last_error = e,
            }
        }
        eprintln!(
            "sonda: all {} retries exhausted (last error: {})",
            policy.max_attempts(),
            last_error,
        );
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::sink::{create_sink, SinkConfig};

    fn ephemeral_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr").to_string();
        (listener, addr)
    }

    #[tokio::test]
    async fn tcp_write_and_flush_data_arrives_at_listener() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None)
            .await
            .expect("connect should succeed");
        sink.write(b"hello tcp\n")
            .await
            .expect("write should succeed");
        sink.flush().await.expect("flush should succeed");
        drop(sink);

        let received = receiver.join().expect("receiver thread panicked");
        assert_eq!(received, b"hello tcp\n");
    }

    #[tokio::test]
    async fn tcp_multiple_writes_arrive_in_order() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None).await.expect("connect");
        sink.write(b"line1\n").await.expect("write 1");
        sink.write(b"line2\n").await.expect("write 2");
        sink.write(b"line3\n").await.expect("write 3");
        sink.flush().await.expect("flush");
        drop(sink);

        let received = receiver.join().expect("receiver thread");
        assert_eq!(received, b"line1\nline2\nline3\n");
    }

    #[tokio::test]
    async fn tcp_write_empty_slice_succeeds() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None).await.expect("connect");
        sink.write(b"").await.expect("empty write should succeed");
        sink.flush().await.expect("flush");
        drop(sink);

        let received = receiver.join().expect("receiver thread");
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn tcp_connect_to_unused_port_returns_sink_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        let result = TcpSink::new(&addr, None).await;
        assert!(result.is_err(), "connecting to closed port must fail");
        let err = result.err().unwrap();
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn tcp_connect_refused_error_message_contains_address() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        let result = TcpSink::new(&addr, None).await;
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains(&addr) || msg.contains("TCP connect"),
            "error message should reference the address; got: {msg}"
        );
    }

    #[tokio::test]
    async fn tcp_invalid_address_string_returns_sink_error() {
        let result = TcpSink::new("not-a-host", None).await;
        assert!(result.is_err(), "invalid address must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[tokio::test]
    async fn tcp_valid_address_connects_successfully() {
        let (listener, addr) = ephemeral_listener();
        let _receiver = thread::spawn(move || {
            let _ = listener.accept();
        });
        let result = TcpSink::new(&addr, None).await;
        assert!(result.is_ok(), "valid address must connect");
    }

    #[test]
    fn tcp_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TcpSink>();
    }

    #[tokio::test]
    async fn create_sink_tcp_config_connects_and_delivers_data() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let config = SinkConfig::Tcp {
            address: addr.clone(),
            retry: None,
        };
        let mut sink = create_sink(&config, None)
            .await
            .expect("factory should create TcpSink");
        sink.write(b"via factory\n").await.expect("write");
        sink.flush().await.expect("flush");
        drop(sink);

        let received = receiver.join().expect("receiver thread");
        assert_eq!(received, b"via factory\n");
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_tcp_deserializes_from_yaml() {
        let yaml = "type: tcp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Tcp { address, .. } => {
                assert_eq!(address, "127.0.0.1:9999");
            }
            other => panic!("expected SinkConfig::Tcp, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_tcp_is_cloneable_and_debuggable() {
        let config = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: None,
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("Tcp"));
        assert!(debug_str.contains("9999"));
    }
}
