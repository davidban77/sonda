//! UDP sink — delivers encoded telemetry as individual datagrams.

use std::net::{SocketAddr, UdpSocket};

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
    /// Bind an ephemeral local port and target `addr` for outgoing datagrams.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if `addr` cannot be parsed or if the
    /// local socket cannot be bound.
    pub fn new(addr: &str) -> Result<Self, SondaError> {
        let target: SocketAddr = addr.parse().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("UDP address parse error for {addr}: {e}"),
            )
        })?;
        // Bind on the wildcard address, matching the IP version of the target.
        let bind_addr = if target.is_ipv6() {
            ":::0"
        } else {
            "0.0.0.0:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .map_err(|e| std::io::Error::new(e.kind(), format!("UDP bind for {addr}: {e}")))?;
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
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "UDP datagram size {} exceeds maximum {} for target {}",
                    data.len(),
                    MAX_UDP_PAYLOAD,
                    self.target
                ),
            )
            .into());
        }
        self.socket.send_to(data, self.target).map_err(|e| {
            std::io::Error::new(e.kind(), format!("UDP send_to {}: {e}", self.target))
        })?;
        Ok(())
    }

    /// No-op for UDP — datagrams are not buffered.
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}
