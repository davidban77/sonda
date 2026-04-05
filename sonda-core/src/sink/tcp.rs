//! TCP sink — delivers encoded telemetry over a persistent TCP connection.
//!
//! When a retry policy is configured, the sink attempts to reconnect on
//! write or flush failures before retrying the operation.

use std::io::{BufWriter, Write};
use std::net::TcpStream;

use crate::sink::retry::RetryPolicy;
use crate::sink::Sink;
use crate::SondaError;

/// Delivers encoded telemetry data over a TCP connection.
///
/// The underlying [`TcpStream`] is wrapped in a [`BufWriter`] to batch
/// writes and reduce syscall overhead. When a [`RetryPolicy`] is configured,
/// write and flush failures trigger a reconnection attempt before retrying.
pub struct TcpSink {
    writer: BufWriter<TcpStream>,
    /// Target address stored for reconnection and error messages.
    addr: String,
    /// Optional retry policy for transient failures.
    retry_policy: Option<RetryPolicy>,
}

impl TcpSink {
    /// Connect to `addr` and create a new [`TcpSink`].
    ///
    /// # Arguments
    ///
    /// - `addr` — remote address to connect to, e.g. `"127.0.0.1:9999"`.
    /// - `retry_policy` — optional retry policy for transient failures.
    ///   When `None`, errors are returned immediately (no retry).
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if the connection cannot be established
    /// (e.g., connection refused, invalid address).
    pub fn new(addr: &str, retry_policy: Option<RetryPolicy>) -> Result<Self, SondaError> {
        let stream = TcpStream::connect(addr)
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP connect to {addr}: {e}")))
            .map_err(SondaError::Sink)?;
        Ok(Self {
            writer: BufWriter::new(stream),
            addr: addr.to_owned(),
            retry_policy,
        })
    }

    /// Attempt to reconnect to the stored address, replacing the writer.
    fn reconnect(&mut self) -> Result<(), SondaError> {
        let stream = TcpStream::connect(&self.addr)
            .map_err(|e| {
                std::io::Error::new(e.kind(), format!("TCP reconnect to {}: {e}", self.addr))
            })
            .map_err(SondaError::Sink)?;
        self.writer = BufWriter::new(stream);
        Ok(())
    }
}

impl Sink for TcpSink {
    /// Write `data` to the buffered TCP stream.
    ///
    /// When a retry policy is configured, failures trigger reconnection
    /// attempts before retrying the write.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let initial_result = self
            .writer
            .write_all(data)
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP write to {}: {e}", self.addr)))
            .map_err(SondaError::Sink);

        match initial_result {
            Ok(()) => Ok(()),
            Err(first_err) => {
                if let Some(policy) = self.retry_policy.clone() {
                    // On failure, reconnect and retry.
                    policy.execute(
                        || {
                            self.reconnect()?;
                            self.writer
                                .write_all(data)
                                .map_err(|e| {
                                    std::io::Error::new(
                                        e.kind(),
                                        format!("TCP write to {}: {e}", self.addr),
                                    )
                                })
                                .map_err(SondaError::Sink)
                        },
                        |_| true, // all TCP errors are retryable
                    )
                } else {
                    Err(first_err)
                }
            }
        }
    }

    /// Flush buffered data to the TCP stream.
    ///
    /// When a retry policy is configured, failures trigger reconnection
    /// attempts before retrying the flush.
    fn flush(&mut self) -> Result<(), SondaError> {
        let initial_result = self
            .writer
            .flush()
            .map_err(|e| std::io::Error::new(e.kind(), format!("TCP flush to {}: {e}", self.addr)))
            .map_err(SondaError::Sink);

        match initial_result {
            Ok(()) => Ok(()),
            Err(first_err) => {
                if let Some(policy) = self.retry_policy.clone() {
                    policy.execute(
                        || {
                            self.reconnect()?;
                            self.writer
                                .flush()
                                .map_err(|e| {
                                    std::io::Error::new(
                                        e.kind(),
                                        format!("TCP flush to {}: {e}", self.addr),
                                    )
                                })
                                .map_err(SondaError::Sink)
                        },
                        |_| true,
                    )
                } else {
                    Err(first_err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::sink::{create_sink, SinkConfig};

    /// Bind a listener on an OS-assigned port and return (listener, addr_string).
    fn ephemeral_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr").to_string();
        (listener, addr)
    }

    // ---- Happy path: write + flush → data received on listener ---------------

    #[test]
    fn tcp_write_and_flush_data_arrives_at_listener() {
        let (listener, addr) = ephemeral_listener();

        // Accept one connection in a background thread so we don't deadlock.
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None).expect("connect should succeed");
        sink.write(b"hello tcp\n").expect("write should succeed");
        sink.flush().expect("flush should succeed");
        // Drop the sink to close the connection so the receiver can finish.
        drop(sink);

        let received = receiver.join().expect("receiver thread panicked");
        assert_eq!(received, b"hello tcp\n");
    }

    #[test]
    fn tcp_multiple_writes_arrive_in_order() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None).expect("connect");
        sink.write(b"line1\n").expect("write 1");
        sink.write(b"line2\n").expect("write 2");
        sink.write(b"line3\n").expect("write 3");
        sink.flush().expect("flush");
        drop(sink);

        let received = receiver.join().expect("receiver thread");
        assert_eq!(received, b"line1\nline2\nline3\n");
    }

    #[test]
    fn tcp_write_empty_slice_succeeds() {
        let (listener, addr) = ephemeral_listener();

        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        });

        let mut sink = TcpSink::new(&addr, None).expect("connect");
        sink.write(b"").expect("empty write should succeed");
        sink.flush().expect("flush");
        drop(sink);

        let received = receiver.join().expect("receiver thread");
        assert!(received.is_empty());
    }

    // ---- Error path: connection refused → SondaError::Sink ------------------

    #[test]
    fn tcp_connect_to_unused_port_returns_sink_error() {
        // Find a port that is not listening by binding then dropping.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener); // Port is now free and not listening.

        let result = TcpSink::new(&addr, None);
        assert!(result.is_err(), "connecting to closed port must fail");
        let err = result.err().unwrap();
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
    }

    #[test]
    fn tcp_connect_refused_error_message_contains_address() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        let result = TcpSink::new(&addr, None);
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains(&addr) || msg.contains("TCP connect"),
            "error message should reference the address; got: {msg}"
        );
    }

    // ---- Address parsing: invalid address → SondaError::Sink ----------------

    #[test]
    fn tcp_invalid_address_string_returns_sink_error() {
        let result = TcpSink::new("not-a-host", None);
        assert!(result.is_err(), "invalid address must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn tcp_valid_address_connects_successfully() {
        let (listener, addr) = ephemeral_listener();
        let _receiver = thread::spawn(move || {
            let _ = listener.accept();
        });
        let result = TcpSink::new(&addr, None);
        assert!(result.is_ok(), "valid address must connect");
    }

    // ---- Trait contract: Send + Sync -----------------------------------------

    #[test]
    fn tcp_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TcpSink>();
    }

    // ---- Factory wiring: SinkConfig::Tcp → TcpSink --------------------------

    #[test]
    fn create_sink_tcp_config_connects_and_delivers_data() {
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
        let mut sink = create_sink(&config, None).expect("factory should create TcpSink");
        sink.write(b"via factory\n").expect("write");
        sink.flush().expect("flush");
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
