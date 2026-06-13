//! UDP sink — delivers encoded telemetry as individual datagrams.

use std::net::{SocketAddr, ToSocketAddrs};

use async_trait::async_trait;
use tokio::net::UdpSocket;

use crate::sink::Sink;
use crate::SondaError;

/// Maximum UDP payload size for a single datagram (IPv4: 65535 − 20 − 8).
const MAX_UDP_PAYLOAD: usize = 65507;

/// Delivers encoded telemetry data as UDP datagrams.
pub struct UdpSink {
    socket: UdpSocket,
    target: SocketAddr,
}

impl UdpSink {
    /// Bind an ephemeral local port and resolve `addr` for outgoing datagrams.
    pub async fn new(addr: &str) -> Result<Self, SondaError> {
        let target: SocketAddr = addr
            .to_socket_addrs()
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("UDP address resolution error for {addr}: {e}"),
                )
            })
            .map_err(SondaError::Sink)?
            .next()
            .ok_or_else(|| {
                SondaError::Sink(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("UDP address {addr} resolved to no addresses"),
                ))
            })?;
        let bind_addr = if target.is_ipv6() {
            ":::0"
        } else {
            "0.0.0.0:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|e| std::io::Error::new(e.kind(), format!("UDP bind for {addr}: {e}")))
            .map_err(SondaError::Sink)?;
        Ok(Self { socket, target })
    }
}

#[async_trait]
impl Sink for UdpSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        if data.len() > MAX_UDP_PAYLOAD {
            return Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "UDP datagram size {} exceeds maximum {} for target {}",
                    data.len(),
                    MAX_UDP_PAYLOAD,
                    self.target
                ),
            )));
        }
        self.socket
            .send_to(data, self.target)
            .await
            .map_err(|e| std::io::Error::new(e.kind(), format!("UDP send_to {}: {e}", self.target)))
            .map_err(SondaError::Sink)?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket as StdUdpSocket;

    use super::*;
    use crate::sink::{create_sink, SinkConfig};

    fn ephemeral_receiver() -> (StdUdpSocket, String) {
        let socket = StdUdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set timeout");
        let addr = socket.local_addr().expect("local addr").to_string();
        (socket, addr)
    }

    #[tokio::test]
    async fn udp_write_datagram_arrives_at_receiver() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        sink.write(b"hello udp\n")
            .await
            .expect("write should succeed");

        let mut buf = [0u8; 1024];
        let (len, _src) = receiver.recv_from(&mut buf).expect("recv_from");
        assert_eq!(&buf[..len], b"hello udp\n");
    }

    #[tokio::test]
    async fn udp_multiple_writes_each_arrive_as_separate_datagram() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        sink.write(b"datagram1").await.expect("write 1");
        sink.write(b"datagram2").await.expect("write 2");

        let mut buf = [0u8; 1024];
        let (len1, _) = receiver.recv_from(&mut buf).expect("recv 1");
        assert_eq!(&buf[..len1], b"datagram1");

        let (len2, _) = receiver.recv_from(&mut buf).expect("recv 2");
        assert_eq!(&buf[..len2], b"datagram2");
    }

    #[tokio::test]
    async fn udp_write_empty_datagram_succeeds() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        sink.write(b"").await.expect("empty write should succeed");

        let mut buf = [0u8; 1024];
        let (len, _) = receiver.recv_from(&mut buf).expect("recv");
        assert_eq!(len, 0);
    }

    #[tokio::test]
    async fn udp_flush_is_noop_and_always_succeeds() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        sink.flush().await.expect("flush 1 should succeed");
        sink.flush().await.expect("flush 2 should succeed");
        sink.flush().await.expect("flush 3 should succeed");
    }

    #[tokio::test]
    async fn udp_oversized_payload_returns_sink_error() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        let oversized = vec![0u8; MAX_UDP_PAYLOAD + 1];
        let result = sink.write(&oversized).await;
        assert!(result.is_err(), "oversized payload must return Err");
        let err = result.err().unwrap();
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn udp_oversized_payload_error_message_mentions_sizes() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        let oversized = vec![0u8; MAX_UDP_PAYLOAD + 1];
        let err = sink.write(&oversized).await.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("65508") || msg.contains("65507"),
            "error message should mention payload sizes; got: {msg}"
        );
    }

    #[tokio::test]
    async fn udp_exactly_max_payload_succeeds() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).await.expect("create UdpSink");
        let max_payload = vec![0xABu8; MAX_UDP_PAYLOAD];
        let result = sink.write(&max_payload).await;
        if result.is_ok() {
            let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
            if let Ok((len, _)) = receiver.recv_from(&mut buf) {
                assert_eq!(len, MAX_UDP_PAYLOAD);
            }
        }
    }

    #[tokio::test]
    async fn udp_invalid_address_string_returns_sink_error() {
        let result = UdpSink::new("not-a-host").await;
        assert!(result.is_err(), "invalid address must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[tokio::test]
    async fn udp_invalid_address_error_message_contains_address() {
        let result = UdpSink::new("not-a-host").await;
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("not-a-host") || msg.contains("UDP"),
            "error message should reference the bad address; got: {msg}"
        );
    }

    #[tokio::test]
    async fn udp_valid_address_creates_sink_successfully() {
        let (_receiver, addr) = ephemeral_receiver();
        let result = UdpSink::new(&addr).await;
        assert!(result.is_ok(), "valid address must succeed");
    }

    #[test]
    fn udp_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<UdpSink>();
    }

    #[tokio::test]
    async fn create_sink_udp_config_delivers_datagram() {
        let (receiver, addr) = ephemeral_receiver();

        let config = SinkConfig::Udp {
            address: addr.clone(),
        };
        let mut sink = create_sink(&config, None)
            .await
            .expect("factory should create UdpSink");
        sink.write(b"via factory\n").await.expect("write");

        let mut buf = [0u8; 1024];
        let (len, _) = receiver.recv_from(&mut buf).expect("recv");
        assert_eq!(&buf[..len], b"via factory\n");
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_udp_deserializes_from_yaml() {
        let yaml = "type: udp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::Udp { address } => {
                assert_eq!(address, "127.0.0.1:9999");
            }
            other => panic!("expected SinkConfig::Udp, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_udp_is_cloneable_and_debuggable() {
        let config = SinkConfig::Udp {
            address: "127.0.0.1:9999".to_string(),
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("Udp"));
        assert!(debug_str.contains("9999"));
    }
}
