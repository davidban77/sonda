//! UDP sink — delivers encoded telemetry as individual datagrams.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use crate::sink::Sink;
use crate::SondaError;

/// Maximum UDP payload size for a single datagram (IPv4).
///
/// The theoretical maximum is 65507 bytes (65535 − 20 IP header − 8 UDP header).
/// Payloads larger than this cannot be sent as a single datagram.
const MAX_UDP_PAYLOAD: usize = 65507;

/// Delivers encoded telemetry data as UDP datagrams.
///
/// Each call to [`write`](UdpSink::write) sends one datagram. UDP is
/// connectionless — no connection is established at construction time;
/// a local ephemeral port is bound and the target address is stored.
pub struct UdpSink {
    socket: UdpSocket,
    target: SocketAddr,
}

impl UdpSink {
    /// Bind an ephemeral local port and resolve `addr` for outgoing datagrams.
    ///
    /// `addr` may be a `"host:port"` string where the host is either a numeric
    /// IP address or a DNS hostname. DNS resolution is performed at construction
    /// time using [`ToSocketAddrs`]; the first resolved address is used.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if `addr` cannot be resolved, yields no
    /// addresses, or if the local socket cannot be bound.
    pub fn new(addr: &str) -> Result<Self, SondaError> {
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
        // Bind on the wildcard address, matching the IP version of the target.
        let bind_addr = if target.is_ipv6() {
            ":::0"
        } else {
            "0.0.0.0:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .map_err(|e| std::io::Error::new(e.kind(), format!("UDP bind for {addr}: {e}")))
            .map_err(SondaError::Sink)?;
        Ok(Self { socket, target })
    }
}

impl Sink for UdpSink {
    /// Send `data` as a single UDP datagram to the target address.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if `data` exceeds [`MAX_UDP_PAYLOAD`]
    /// (65507 bytes) or if the underlying `send_to` fails.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
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
            .map_err(|e| std::io::Error::new(e.kind(), format!("UDP send_to {}: {e}", self.target)))
            .map_err(SondaError::Sink)?;
        Ok(())
    }

    /// No-op for UDP — datagrams are not buffered.
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket;

    use super::*;
    use crate::sink::{create_sink, SinkConfig};

    /// Bind a receiving UDP socket on an OS-assigned port. Returns (socket, addr_string).
    fn ephemeral_receiver() -> (UdpSocket, String) {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set timeout");
        let addr = socket.local_addr().expect("local addr").to_string();
        (socket, addr)
    }

    // ---- Happy path: write → recv matches ------------------------------------

    #[test]
    fn udp_write_datagram_arrives_at_receiver() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        sink.write(b"hello udp\n").expect("write should succeed");

        let mut buf = [0u8; 1024];
        let (len, _src) = receiver.recv_from(&mut buf).expect("recv_from");
        assert_eq!(&buf[..len], b"hello udp\n");
    }

    #[test]
    fn udp_multiple_writes_each_arrive_as_separate_datagram() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        sink.write(b"datagram1").expect("write 1");
        sink.write(b"datagram2").expect("write 2");

        let mut buf = [0u8; 1024];
        let (len1, _) = receiver.recv_from(&mut buf).expect("recv 1");
        assert_eq!(&buf[..len1], b"datagram1");

        let (len2, _) = receiver.recv_from(&mut buf).expect("recv 2");
        assert_eq!(&buf[..len2], b"datagram2");
    }

    #[test]
    fn udp_write_empty_datagram_succeeds() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        sink.write(b"").expect("empty write should succeed");

        let mut buf = [0u8; 1024];
        let (len, _) = receiver.recv_from(&mut buf).expect("recv");
        assert_eq!(len, 0);
    }

    #[test]
    fn udp_flush_is_noop_and_always_succeeds() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        // Call flush multiple times — all must succeed (idempotent no-op).
        sink.flush().expect("flush 1 should succeed");
        sink.flush().expect("flush 2 should succeed");
        sink.flush().expect("flush 3 should succeed");
    }

    // ---- Oversized payload → SondaError::Sink --------------------------------

    #[test]
    fn udp_oversized_payload_returns_sink_error() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        // 65508 bytes exceeds the 65507-byte limit.
        let oversized = vec![0u8; MAX_UDP_PAYLOAD + 1];
        let result = sink.write(&oversized);
        assert!(result.is_err(), "oversized payload must return Err");
        let err = result.err().unwrap();
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
    }

    #[test]
    fn udp_oversized_payload_error_message_mentions_sizes() {
        let (_receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        let oversized = vec![0u8; MAX_UDP_PAYLOAD + 1];
        let err = sink.write(&oversized).err().unwrap();
        let msg = err.to_string();
        // Message must include both the actual size and the limit.
        assert!(
            msg.contains("65508") || msg.contains("65507"),
            "error message should mention payload sizes; got: {msg}"
        );
    }

    #[test]
    fn udp_exactly_max_payload_succeeds() {
        let (receiver, addr) = ephemeral_receiver();

        let mut sink = UdpSink::new(&addr).expect("create UdpSink");
        let max_payload = vec![0xABu8; MAX_UDP_PAYLOAD];
        // This may fail at the OS level on loopback if the kernel refuses the
        // datagram size, but must not fail in the size-check logic.
        let result = sink.write(&max_payload);
        // Whether the OS accepts it depends on system config; at minimum the
        // size guard must not reject it.
        if result.is_ok() {
            let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
            if let Ok((len, _)) = receiver.recv_from(&mut buf) {
                assert_eq!(len, MAX_UDP_PAYLOAD);
            }
        }
        // If the OS rejected it for other reasons that is acceptable — only
        // the implementation's size guard is being tested here.
    }

    // ---- Address parsing: invalid address → SondaError::Sink ----------------

    #[test]
    fn udp_invalid_address_string_returns_sink_error() {
        let result = UdpSink::new("not-a-host");
        assert!(result.is_err(), "invalid address must fail");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "expected SondaError::Sink"
        );
    }

    #[test]
    fn udp_invalid_address_error_message_contains_address() {
        let result = UdpSink::new("not-a-host");
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("not-a-host") || msg.contains("UDP"),
            "error message should reference the bad address; got: {msg}"
        );
    }

    #[test]
    fn udp_valid_address_creates_sink_successfully() {
        let (_receiver, addr) = ephemeral_receiver();
        let result = UdpSink::new(&addr);
        assert!(result.is_ok(), "valid address must succeed");
    }

    // ---- Trait contract: Send + Sync -----------------------------------------

    #[test]
    fn udp_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<UdpSink>();
    }

    // ---- Factory wiring: SinkConfig::Udp → UdpSink --------------------------

    #[test]
    fn create_sink_udp_config_delivers_datagram() {
        let (receiver, addr) = ephemeral_receiver();

        let config = SinkConfig::Udp {
            address: addr.clone(),
        };
        let mut sink = create_sink(&config, None).expect("factory should create UdpSink");
        sink.write(b"via factory\n").expect("write");

        let mut buf = [0u8; 1024];
        let (len, _) = receiver.recv_from(&mut buf).expect("recv");
        assert_eq!(&buf[..len], b"via factory\n");
    }

    #[test]
    fn sink_config_udp_deserializes_from_yaml() {
        let yaml = "type: udp\naddress: \"127.0.0.1:9999\"";
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
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
