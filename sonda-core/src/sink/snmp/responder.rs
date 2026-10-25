//! Answers SNMPv1 and SNMPv2c requests from an [`SnmpAgent`]'s table.
//!
//! [`decode`] classifies a datagram's PDU, [`answer`] writes the response
//! into a caller's buffer, and [`serve`] is the task that reads the socket
//! and sends each response to the datagram's source.
//!
//! A response leaves from the address its request was sent to. On a
//! specific-address bind the socket guarantees it; on a wildcard bind
//! (`0.0.0.0` or `[::]`) the socket reports each datagram's packet info
//! (`IP_PKTINFO` / `IPV6_RECVPKTINFO`, Linux and macOS) and the response
//! is sent with [`reply_source`]'s address in its packet-info control
//! message. Elsewhere a wildcard bind's responses leave from the address
//! the route chooses.
//!
//! | Received | SNMPv2c answer | SNMPv1 answer |
//! |---|---|---|
//! | `GetRequest` | each varbind its instance's value, `noSuchInstance` or `noSuchObject`; error-status `noError` | each varbind its instance's value; the first with no instance, or a `Counter64` one, makes it `noSuchName` at that varbind |
//! | `GetNextRequest` | each varbind the next instance in OID order, or `endOfMibView` past the last | each varbind the next instance that is not a `Counter64`; the first past the last makes it `noSuchName` at that varbind |
//! | `GetBulkRequest` | the non-repeaters as `GetNext`, then up to max-repetitions rounds of the repeaters, ending after a round that is all `endOfMibView` | dropped as malformed |
//! | `SetRequest` | the request's varbinds, error-status `notWritable`, error-index 1; nothing is changed | the same, error-status `noSuchName` |
//! | any other PDU | dropped | dropped (a `Trap` as malformed) |
//! | another community | dropped | dropped |
//! | a version the agent does not serve, or SNMPv3 | dropped, the community never compared | dropped, the community never compared |
//!
//! A response carries its request's version. An SNMPv1 error response
//! carries the request's varbinds unchanged.
//!
//! Every response fits `max_message_size`. The first varbind that would
//! not fit ends a `GetBulkRequest` response, which is sent with the
//! varbinds before it; it turns a `GetRequest`, `GetNextRequest` or
//! `SetRequest` response into `tooBig`, with an empty varbind list under
//! SNMPv2c and the request's varbinds under SNMPv1. An SNMPv1 `noSuchName`
//! takes precedence over `tooBig`. A request with no varbinds is answered
//! with none, `noError`.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::Range;
use std::sync::Weak;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::agent::{self, warn_due, Counter, Found, SnmpAgent};
use super::SnmpVersion;
use crate::encoder::snmp::ber::{self, BerError, Message, Reader, Tag, VarBind};
use crate::encoder::snmp::oid::{self, Oid};

/// The largest datagram the responder reads whole; a longer one is read
/// truncated and does not decode.
const RECEIVE_BUFFER: usize = 65_535;

/// How long the responder waits before reading again after a receive error.
const RECEIVE_BACKOFF: Duration = Duration::from_millis(100);

/// The most peers whose last wrong-community warning is remembered. A peer
/// beyond them is counted but not logged until older entries expire.
const WARNED_PEERS: usize = 1024;

/// The version field of an SNMPv1 message.
const V1: i64 = 0;

/// The version field of an SNMPv2c message.
const V2C: i64 = 1;

/// The version field of an SNMPv3 message.
const V3: i64 = 3;

/// error-status `tooBig`.
const TOO_BIG: i64 = 1;

/// error-status `noSuchName`, SNMPv1's error for any name it cannot answer.
const NO_SUCH_NAME: i64 = 2;

/// error-status `notWritable`.
const NOT_WRITABLE: i64 = 17;

/// A message's SNMP version, as [`decode`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    /// SNMPv1, version field 0.
    V1,
    /// SNMPv2c, version field 1.
    V2c,
}

impl Version {
    /// The message's version field.
    fn field(self) -> i64 {
        match self {
            Version::V1 => V1,
            Version::V2c => V2C,
        }
    }

    /// The configured version this is.
    fn configured(self) -> SnmpVersion {
        match self {
            Version::V1 => SnmpVersion::V1,
            Version::V2c => SnmpVersion::V2c,
        }
    }
}

/// A decoded request, borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request<'a> {
    pub(crate) version: Version,
    pub(crate) community: &'a [u8],
    pub(crate) pdu: Pdu<'a>,
}

/// A request's PDU, by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pdu<'a> {
    /// `GetRequest`.
    Get { id: i32, vars: VarList<'a> },
    /// `GetNextRequest`.
    GetNext { id: i32, vars: VarList<'a> },
    /// `GetBulkRequest`, a negative non-repeaters or max-repetitions read
    /// as 0.
    GetBulk {
        id: i32,
        non_repeaters: u32,
        max_repetitions: u32,
        vars: VarList<'a>,
    },
    /// `SetRequest`.
    Set { id: i32, vars: VarList<'a> },
    /// Any other PDU, which the agent does not answer.
    Other(Tag),
}

/// A request's varbind list, every entry a well-formed `VarBind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VarList<'a> {
    message: Message<'a>,
    len: usize,
}

impl<'a> VarList<'a> {
    /// The list of `message`, or the error of its first malformed entry.
    fn of(message: Message<'a>) -> Result<Self, BerError> {
        let mut len = 0;
        for var in message.varbinds() {
            var?;
            len += 1;
        }
        Ok(VarList { message, len })
    }

    /// How many varbinds the list holds.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Whether the list holds no varbinds.
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The varbinds, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = VarBind<'a>> {
        // `of` checked every entry.
        self.message.varbinds().map_while(Result::ok)
    }

    /// The body of the varbind list SEQUENCE, as received.
    fn body(&self) -> &'a [u8] {
        self.message.varbinds
    }
}

/// What became of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A response is in the buffer, to be sent to the request's source.
    Sent,
    /// The community is not the agent's; nothing is sent.
    DroppedWrongCommunity,
    /// A version the agent does not serve, or SNMPv3; nothing is sent.
    DroppedUnsupportedVersion,
    /// Anything else not answered; nothing is sent.
    DroppedOther,
}

/// Buffers [`answer`] reuses from one request to the next.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    /// The arcs of the name being looked up.
    name: Vec<u32>,
    /// Where each `GetBulk` repeater stands.
    cursors: Vec<Cursor>,
}

/// A `GetBulk` repeater's last varbind in the response.
#[derive(Debug, Clone)]
struct Cursor {
    /// The OBJECT IDENTIFIER body of its name, as a range of the response.
    name: Range<usize>,
    /// Whether it was `endOfMibView`.
    ended: bool,
}

/// Decode a datagram: [`Message::decode`], then the PDU classified.
///
/// # Errors
///
/// [`Message::decode`]'s error, [`BerError::UnsupportedVersion`] for any
/// version but SNMPv1 and SNMPv2c among them, and [`BerError::BadTag`] for
/// SNMPv1's `Trap`, which is not a PDU it reads; [`BerError::BadTag`] for a
/// `GetBulkRequest` in an SNMPv1 message, which has none;
/// [`BerError::Overflow`] for a request-id outside 32 bits; the error of the
/// first malformed `VarBind` of a request the agent answers.
pub(crate) fn decode(datagram: &[u8]) -> Result<Request<'_>, BerError> {
    let message = Message::decode(datagram)?;
    // `Message::decode` admits versions 0 and 1 only.
    let version = if message.version == V2C {
        Version::V2c
    } else {
        Version::V1
    };
    if version == Version::V1 && message.pdu == Tag::GetBulkRequest {
        return Err(BerError::BadTag(Tag::GetBulkRequest.byte()));
    }
    let id = || i32::try_from(message.request_id).map_err(|_| BerError::Overflow);
    // A negative count is read as 0; RFC 3416 bounds both at 2³¹ - 1.
    let count = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
    let pdu = match message.pdu {
        Tag::GetRequest => Pdu::Get {
            id: id()?,
            vars: VarList::of(message)?,
        },
        Tag::GetNextRequest => Pdu::GetNext {
            id: id()?,
            vars: VarList::of(message)?,
        },
        Tag::GetBulkRequest => Pdu::GetBulk {
            id: id()?,
            non_repeaters: count(message.error_status),
            max_repetitions: count(message.error_index),
            vars: VarList::of(message)?,
        },
        Tag::SetRequest => Pdu::Set {
            id: id()?,
            vars: VarList::of(message)?,
        },
        other => Pdu::Other(other),
    };
    Ok(Request {
        version,
        community: message.community,
        pdu,
    })
}

/// Answer `req` from `agent` into `out`, which is cleared first, within
/// `budget` octets, and count it in the agent's requests.
///
/// The version is checked first against the agent's
/// [`versions`](SnmpAgent::versions), then the community, in constant time,
/// then the PDU, answered under the request's version. Allocates nothing
/// beyond what `out` and `scratch` already hold once both have answered a
/// request of the same shape.
pub(crate) fn answer(
    req: &Request<'_>,
    agent: &SnmpAgent,
    budget: usize,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Outcome {
    out.clear();
    if !agent.versions().contains(&req.version.configured()) {
        agent.count(Counter::UnsupportedVersion);
        return Outcome::DroppedUnsupportedVersion;
    }
    let community = agent.settings().community.as_bytes();
    if !crate::util::constant_time_eq(req.community, community) {
        agent.count(Counter::WrongCommunity);
        return Outcome::DroppedWrongCommunity;
    }
    let (counter, id, vars) = match req.pdu {
        Pdu::Get { id, vars } => (Counter::Get, id, vars),
        Pdu::GetNext { id, vars } => (Counter::GetNext, id, vars),
        Pdu::GetBulk { id, vars, .. } => (Counter::GetBulk, id, vars),
        Pdu::Set { id, vars } => (Counter::SetRefused, id, vars),
        Pdu::Other(_) => {
            agent.count(Counter::Malformed);
            return Outcome::DroppedOther;
        }
    };
    let v1 = req.version == Version::V1;
    let reply = Reply {
        version: req.version.field(),
        community,
        id: i64::from(id),
        budget,
    };
    let (status, index) = match req.pdu {
        Pdu::Set { .. } if !vars.is_empty() && v1 => (NO_SUCH_NAME, 1),
        Pdu::Set { .. } if !vars.is_empty() => (NOT_WRITABLE, 1),
        _ => (0, 0),
    };
    let mut written = Ok(Written::Complete);
    ber::write_message(
        out,
        reply.version,
        community,
        Tag::Response,
        reply.id,
        status,
        index,
        |out| {
            let list = List {
                start: out.len(),
                reply: &reply,
            };
            written = match req.pdu {
                Pdu::Get { .. } if v1 => get_v1(agent, &vars, list, scratch, out),
                Pdu::Get { .. } => get(agent, &vars, list, scratch, out),
                Pdu::GetNext { .. } if v1 => get_next_v1(agent, &vars, list, scratch, out),
                Pdu::GetNext { .. } => get_next(agent, &vars, list, scratch, out),
                Pdu::GetBulk {
                    non_repeaters,
                    max_repetitions,
                    ..
                } => get_bulk(
                    agent,
                    &vars,
                    non_repeaters,
                    max_repetitions,
                    list,
                    scratch,
                    out,
                ),
                Pdu::Set { .. } => echo(&vars, list, out),
                Pdu::Other(_) => Ok(Written::Complete),
            };
        },
    );
    let outcome = match written {
        Ok(Written::Complete) => Outcome::Sent,
        Ok(Written::Full) => too_big(agent, &reply, &vars, out),
        Ok(Written::NoSuchName(index)) => no_such_name(agent, &reply, &vars, index, out),
        Err(_) => {
            out.clear();
            agent.count(Counter::Malformed);
            return Outcome::DroppedOther;
        }
    };
    agent.count(counter);
    if v1 {
        agent.count(Counter::V1);
    }
    outcome
}

/// [`decode`] `datagram` and [`answer`] it, calling `wrong_community` with
/// the request when its community is not the agent's. A datagram that does
/// not decode is counted: as unsupported for an SNMPv3 message, otherwise
/// as malformed.
pub(crate) fn respond(
    datagram: &[u8],
    agent: &SnmpAgent,
    budget: usize,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
    wrong_community: impl FnOnce(&Request<'_>),
) -> Outcome {
    match decode(datagram) {
        Ok(request) => {
            let outcome = answer(&request, agent, budget, scratch, out);
            if outcome == Outcome::DroppedWrongCommunity {
                wrong_community(&request);
            }
            outcome
        }
        Err(e) => {
            out.clear();
            undecodable(agent, e)
        }
    }
}

/// Count a datagram that did not decode with `e`.
fn undecodable(agent: &SnmpAgent, e: BerError) -> Outcome {
    if e == BerError::UnsupportedVersion(V3) {
        agent.count(Counter::UnsupportedVersion);
        Outcome::DroppedUnsupportedVersion
    } else {
        agent.count(Counter::Malformed);
        Outcome::DroppedOther
    }
}

/// Read datagrams until `cancel` fires or the agent is gone, answering each
/// to its source, from the address it was sent to when the socket reports
/// packet info ([`enable_pktinfo`]).
///
/// A wrong community is logged at warn once per peer IP per
/// [`WARN_INTERVAL`](super::agent::WARN_INTERVAL), with the community's
/// length and never its value. A send error is counted and logged at
/// debug; nothing the task does fails it.
pub(crate) async fn serve(socket: UdpSocket, agent: Weak<SnmpAgent>, cancel: CancellationToken) {
    let mut datagram = vec![0; RECEIVE_BUFFER];
    let mut out = Vec::new();
    let mut scratch = Scratch::default();
    let mut warned = PeerWarnings::default();
    let pktinfo = pktinfo::enabled(&socket);
    loop {
        let received = tokio::select! {
            () = cancel.cancelled() => break,
            received = recv(&socket, &mut datagram, pktinfo) => received,
        };
        let Some(strong) = agent.upgrade() else {
            break;
        };
        let Datagram { bytes, peer, from } = match received {
            Ok(received) => received,
            Err(e) => {
                tracing::debug!(error = %e, addr = %strong.local_addr(), "snmp_agent receive failed");
                drop(strong);
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(RECEIVE_BACKOFF) => {}
                }
                continue;
            }
        };
        let budget = strong.settings().max_message_size;
        out.clear();
        out.reserve(budget);
        let outcome = respond(bytes, &strong, budget, &mut scratch, &mut out, |request| {
            if !warned.due(peer.ip()) {
                return;
            }
            tracing::warn!(
                addr = %strong.local_addr(),
                %peer,
                community_len = request.community.len(),
                "snmp_agent dropped a request from {peer} with another community ({} bytes)",
                request.community.len()
            );
        });
        drop(strong);
        if outcome != Outcome::Sent {
            continue;
        }
        if let Err(e) = send(&socket, &out, peer, from).await {
            if let Some(strong) = agent.upgrade() {
                strong.count(Counter::SendErrors);
                tracing::debug!(error = %e, addr = %strong.local_addr(), %peer, "snmp_agent send failed");
            }
        }
    }
}

/// A datagram the responder read.
#[derive(Debug)]
struct Datagram<'a> {
    bytes: &'a [u8],
    peer: SocketAddr,
    /// Where its response leaves from: [`reply_source`] of its packet info,
    /// when the socket reported it.
    from: Option<ReplySource>,
}

/// The source of a response, as a packet-info control message carries it.
/// An unspecified `addr` leaves the address to the kernel; an `ifindex` of
/// 0 leaves the interface to the route. An IPv4 source has no interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplySource {
    addr: IpAddr,
    ifindex: u32,
}

/// The packet info a datagram arrived with.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketInfo {
    /// `IP_PKTINFO`: the header's destination, and the local address the
    /// datagram was received for (`ipi_spec_dst`: the receiving interface's
    /// address for a broadcast or multicast; unspecified where the platform
    /// does not report it).
    V4 {
        destination: Ipv4Addr,
        local: Ipv4Addr,
    },
    /// `IPV6_PKTINFO`: the header's destination and the receiving interface.
    /// An IPv4 datagram on a dual-stack socket has its destination
    /// IPv4-mapped.
    V6 { destination: Ipv6Addr, ifindex: u32 },
}

/// The source of the response to a datagram that arrived with `info`: the
/// local unicast address it was received for.
///
/// IPv4: the local address; where it is unspecified, the destination when
/// that is unicast, otherwise none (the response leaves as `send_to` sends
/// it). IPv6: the destination when it is unicast, otherwise the receiving
/// interface with the address unspecified. An IPv4-mapped destination is
/// unicast unless it maps a multicast, the limited broadcast or the
/// unspecified address; otherwise its unspecified address is IPv4-mapped
/// too.
fn reply_source(info: PacketInfo) -> Option<ReplySource> {
    let unicast =
        |addr: Ipv4Addr| !(addr.is_multicast() || addr.is_broadcast() || addr.is_unspecified());
    match info {
        PacketInfo::V4 { destination, local } => {
            let addr = if !local.is_unspecified() {
                local
            } else if unicast(destination) {
                destination
            } else {
                return None;
            };
            Some(ReplySource {
                addr: IpAddr::V4(addr),
                ifindex: 0,
            })
        }
        PacketInfo::V6 {
            destination,
            ifindex,
        } => {
            let (is_unicast, unspecified) = match destination.to_ipv4_mapped() {
                Some(mapped) => (unicast(mapped), Ipv4Addr::UNSPECIFIED.to_ipv6_mapped()),
                None => (
                    !(destination.is_multicast() || destination.is_unspecified()),
                    Ipv6Addr::UNSPECIFIED,
                ),
            };
            Some(if is_unicast {
                ReplySource {
                    addr: IpAddr::V6(destination),
                    ifindex: 0,
                }
            } else {
                ReplySource {
                    addr: IpAddr::V6(unspecified),
                    ifindex,
                }
            })
        }
    }
}

/// Whether a socket bound to `local` needs packet info to answer from the
/// address each request was sent to: a wildcard address.
pub(crate) fn wants_pktinfo(local: SocketAddr) -> bool {
    local.ip().is_unspecified()
}

/// Have `socket`, bound to `local`, report each datagram's packet info
/// (`IP_PKTINFO` or `IPV6_RECVPKTINFO`, by the address family), which
/// [`serve`] then answers from.
///
/// # Errors
///
/// The socket option's error, or [`io::ErrorKind::Unsupported`] on a
/// platform other than Linux and macOS.
pub(crate) fn enable_pktinfo(socket: &UdpSocket, local: SocketAddr) -> io::Result<()> {
    pktinfo::enable(socket, local)
}

/// Read one datagram into `buf`: with `recvmsg` and its packet info when
/// `pktinfo`, with `recv_from` otherwise.
async fn recv<'b>(
    socket: &UdpSocket,
    buf: &'b mut [u8],
    pktinfo: bool,
) -> io::Result<Datagram<'b>> {
    let (len, peer, info) = if pktinfo {
        pktinfo::recv(socket, &mut *buf).await?
    } else {
        let (len, peer) = socket.recv_from(&mut *buf).await?;
        (len, peer, None)
    };
    let buf: &'b [u8] = buf;
    Ok(Datagram {
        bytes: &buf[..len],
        peer,
        from: info.and_then(reply_source),
    })
}

/// Send `reply` to `to`: with `send_to` when `from` is `None`, otherwise
/// with `sendmsg` carrying `from` as its packet info.
async fn send(
    socket: &UdpSocket,
    reply: &[u8],
    to: SocketAddr,
    from: Option<ReplySource>,
) -> io::Result<usize> {
    match from {
        None => socket.send_to(reply, to).await,
        Some(from) => pktinfo::send(socket, reply, to, from).await,
    }
}

/// `recvmsg` and `sendmsg` with packet info, through the socket's readiness.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod pktinfo {
    use std::io::{self, IoSlice, IoSliceMut};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use std::os::fd::AsRawFd;

    use nix::libc;
    use nix::sys::socket::{
        self, sockopt, ControlMessage, ControlMessageOwned, MsgFlags, SockaddrStorage,
    };
    use tokio::io::Interest;
    use tokio::net::UdpSocket;

    use super::{PacketInfo, ReplySource};

    /// Room for one packet-info control message of either family, aligned
    /// as a `cmsghdr`.
    #[repr(C, align(8))]
    struct Control([u8; 64]);

    pub(super) fn enable(socket: &UdpSocket, local: SocketAddr) -> io::Result<()> {
        match local {
            SocketAddr::V4(_) => socket::setsockopt(socket, sockopt::Ipv4PacketInfo, &true),
            SocketAddr::V6(_) => socket::setsockopt(socket, sockopt::Ipv6RecvPacketInfo, &true),
        }
        .map_err(io::Error::from)
    }

    /// Whether `socket` reports packet info.
    pub(super) fn enabled(socket: &UdpSocket) -> bool {
        match socket.local_addr() {
            Ok(SocketAddr::V4(_)) => socket::getsockopt(socket, sockopt::Ipv4PacketInfo),
            Ok(SocketAddr::V6(_)) => socket::getsockopt(socket, sockopt::Ipv6RecvPacketInfo),
            Err(_) => return false,
        }
        .unwrap_or(false)
    }

    pub(super) async fn recv(
        socket: &UdpSocket,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, Option<PacketInfo>)> {
        let fd = socket.as_raw_fd();
        let mut control = Control([0; 64]);
        loop {
            socket.readable().await?;
            let received = socket.try_io(Interest::READABLE, || {
                let mut iov = [IoSliceMut::new(&mut *buf)];
                let message = socket::recvmsg::<SockaddrStorage>(
                    fd,
                    &mut iov,
                    Some(&mut control.0),
                    MsgFlags::empty(),
                )?;
                let peer = message
                    .address
                    .as_ref()
                    .and_then(socket_addr)
                    .ok_or_else(|| io::Error::other("recvmsg reported no peer address"))?;
                let info = message
                    .cmsgs()
                    .ok()
                    .and_then(|mut cmsgs| cmsgs.find_map(packet_info));
                Ok((message.bytes, peer, info))
            });
            match received {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                received => return received,
            }
        }
    }

    pub(super) async fn send(
        socket: &UdpSocket,
        reply: &[u8],
        to: SocketAddr,
        from: ReplySource,
    ) -> io::Result<usize> {
        let fd = socket.as_raw_fd();
        let iov = [IoSlice::new(reply)];
        let to = SockaddrStorage::from(to);
        let (v4, v6);
        let cmsgs = [match from.addr {
            IpAddr::V4(addr) => {
                v4 = libc::in_pktinfo {
                    ipi_ifindex: 0,
                    ipi_spec_dst: in_addr(addr),
                    ipi_addr: in_addr(Ipv4Addr::UNSPECIFIED),
                };
                ControlMessage::Ipv4PacketInfo(&v4)
            }
            IpAddr::V6(addr) => {
                v6 = libc::in6_pktinfo {
                    ipi6_addr: libc::in6_addr {
                        s6_addr: addr.octets(),
                    },
                    ipi6_ifindex: from.ifindex,
                };
                ControlMessage::Ipv6PacketInfo(&v6)
            }
        }];
        loop {
            socket.writable().await?;
            let sent = socket.try_io(Interest::WRITABLE, || {
                socket::sendmsg(fd, &iov, &cmsgs, MsgFlags::empty(), Some(&to))
                    .map_err(io::Error::from)
            });
            match sent {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                sent => return sent,
            }
        }
    }

    fn socket_addr(address: &SockaddrStorage) -> Option<SocketAddr> {
        if let Some(v4) = address.as_sockaddr_in() {
            return Some(SocketAddr::V4(SocketAddrV4::from(*v4)));
        }
        address
            .as_sockaddr_in6()
            .map(|v6| SocketAddr::V6(SocketAddrV6::from(*v6)))
    }

    fn packet_info(cmsg: ControlMessageOwned) -> Option<PacketInfo> {
        match cmsg {
            ControlMessageOwned::Ipv4PacketInfo(info) => Some(PacketInfo::V4 {
                destination: ipv4(info.ipi_addr),
                local: ipv4(info.ipi_spec_dst),
            }),
            ControlMessageOwned::Ipv6PacketInfo(info) => Some(PacketInfo::V6 {
                destination: Ipv6Addr::from(info.ipi6_addr.s6_addr),
                ifindex: info.ipi6_ifindex,
            }),
            _ => None,
        }
    }

    fn ipv4(addr: libc::in_addr) -> Ipv4Addr {
        Ipv4Addr::from(u32::from_be(addr.s_addr))
    }

    fn in_addr(addr: Ipv4Addr) -> libc::in_addr {
        libc::in_addr {
            s_addr: u32::from(addr).to_be(),
        }
    }
}

/// Without packet info: every datagram is read with `recv_from` and every
/// response sent with `send_to`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod pktinfo {
    use std::io;
    use std::net::SocketAddr;

    use tokio::net::UdpSocket;

    use super::{PacketInfo, ReplySource};

    pub(super) fn enable(_: &UdpSocket, _: SocketAddr) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "packet info is not available on this platform",
        ))
    }

    pub(super) fn enabled(_: &UdpSocket) -> bool {
        false
    }

    pub(super) async fn recv(
        socket: &UdpSocket,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, Option<PacketInfo>)> {
        let (len, peer) = socket.recv_from(buf).await?;
        Ok((len, peer, None))
    }

    pub(super) async fn send(
        socket: &UdpSocket,
        reply: &[u8],
        to: SocketAddr,
        _: ReplySource,
    ) -> io::Result<usize> {
        socket.send_to(reply, to).await
    }
}

/// When each peer IP was last warned about a wrong community.
#[derive(Debug, Default)]
struct PeerWarnings {
    last: HashMap<IpAddr, Instant>,
}

impl PeerWarnings {
    /// Whether `peer` may be warned about now, recording it if so.
    fn due(&mut self, peer: IpAddr) -> bool {
        let now = Instant::now();
        if !warn_due(self.last.get(&peer).copied(), now) {
            return false;
        }
        if self.last.len() >= WARNED_PEERS && !self.last.contains_key(&peer) {
            self.last.retain(|_, at| !warn_due(Some(*at), now));
            if self.last.len() >= WARNED_PEERS {
                return false;
            }
        }
        self.last.insert(peer, now);
        true
    }
}

/// How a response's varbind list was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    /// Every varbind the request asks for, or a `GetBulk` trimmed to fit.
    Complete,
    /// A varbind did not fit; the response is to be `tooBig`.
    Full,
    /// The varbind at this 1-based position has no SNMPv1 answer; the
    /// response is to be `noSuchName` there.
    NoSuchName(usize),
}

/// The fields of the response that its size depends on.
struct Reply<'a> {
    /// The version field, the request's.
    version: i64,
    community: &'a [u8],
    id: i64,
    budget: usize,
}

/// The varbind list being written: where its body starts in the response.
#[derive(Clone, Copy)]
struct List<'a> {
    start: usize,
    reply: &'a Reply<'a>,
}

impl List<'_> {
    /// Whether `more` octets after what `out` holds still fit the budget.
    fn fits(&self, out: &[u8], more: usize) -> bool {
        let r = self.reply;
        let list = out.len() - self.start + more;
        // error-status and error-index take three octets each whatever their
        // value here (0, 1, 2 or 17).
        ber::message_len(r.version, r.community, r.id, 0, 0, list) <= r.budget
    }
}

/// The `GetRequest` varbinds: each name's value or exception.
fn get(
    agent: &SnmpAgent,
    vars: &VarList<'_>,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    for var in vars.iter() {
        var.name_into(&mut scratch.name)?;
        let fit = agent.lookup(&scratch.name, |found| {
            let exception = match found {
                Found::Value(varbind) => {
                    if !list.fits(out, varbind.len()) {
                        return false;
                    }
                    out.extend_from_slice(varbind);
                    return true;
                }
                Found::NoSuchInstance => Tag::NoSuchInstance,
                Found::NoSuchObject => Tag::NoSuchObject,
            };
            if !list.fits(out, exception_len(var.oid.len())) {
                return false;
            }
            write_exception(out, Name::Request(var.oid), exception);
            true
        });
        if !fit {
            return Ok(Written::Full);
        }
    }
    Ok(Written::Complete)
}

/// The SNMPv1 `GetRequest` varbinds: each name's value while they fit, or
/// [`Written::NoSuchName`] at the first name with no instance or a
/// `Counter64` one, which SNMPv1 cannot carry. Every name is looked up even
/// once one does not fit, so a later name with no answer still makes the
/// response `noSuchName`.
fn get_v1(
    agent: &SnmpAgent,
    vars: &VarList<'_>,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    let mut full = false;
    for (index, var) in vars.iter().enumerate() {
        var.name_into(&mut scratch.name)?;
        let answered = agent.lookup(&scratch.name, |found| match found {
            Found::Value(varbind) if !agent::is_counter64(varbind) => {
                if !full && list.fits(out, varbind.len()) {
                    out.extend_from_slice(varbind);
                } else {
                    full = true;
                }
                true
            }
            _ => false,
        });
        if !answered {
            return Ok(Written::NoSuchName(index + 1));
        }
    }
    Ok(if full {
        Written::Full
    } else {
        Written::Complete
    })
}

/// The SNMPv1 `GetNextRequest` varbinds: each name's successor among the
/// instances that are not a `Counter64`, while they fit, or
/// [`Written::NoSuchName`] at the first name with no successor. Every name
/// is looked up even once one does not fit.
fn get_next_v1(
    agent: &SnmpAgent,
    vars: &VarList<'_>,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    let mut full = false;
    for (index, var) in vars.iter().enumerate() {
        var.name_into(&mut scratch.name)?;
        let mut found = false;
        agent.bulk(&scratch.name, 1, true, |_, varbind| {
            found = true;
            if !full && list.fits(out, varbind.len()) {
                out.extend_from_slice(varbind);
            } else {
                full = true;
            }
            false
        });
        if !found {
            return Ok(Written::NoSuchName(index + 1));
        }
    }
    Ok(if full {
        Written::Full
    } else {
        Written::Complete
    })
}

/// The `GetNextRequest` varbinds: each name's successor, or `endOfMibView`.
fn get_next(
    agent: &SnmpAgent,
    vars: &VarList<'_>,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    for var in vars.iter() {
        var.name_into(&mut scratch.name)?;
        if next(agent, &scratch.name, Name::Request(var.oid), list, out) == Step::Full {
            return Ok(Written::Full);
        }
    }
    Ok(Written::Complete)
}

/// The `GetBulkRequest` varbinds: the first `non_repeaters` names' successors,
/// then the rest's successors round by round, up to `max_repetitions`
/// rounds and stopping after one that is all `endOfMibView`. A varbind that
/// does not fit ends the list.
fn get_bulk(
    agent: &SnmpAgent,
    vars: &VarList<'_>,
    non_repeaters: u32,
    max_repetitions: u32,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    let non_repeaters = vars.len().min(non_repeaters as usize);
    let mut vars = vars.iter();
    for var in vars.by_ref().take(non_repeaters) {
        var.name_into(&mut scratch.name)?;
        if next(agent, &scratch.name, Name::Request(var.oid), list, out) == Step::Full {
            return Ok(Written::Complete);
        }
    }
    if max_repetitions == 0 {
        return Ok(Written::Complete);
    }
    // The first round, from the request's names.
    scratch.cursors.clear();
    for var in vars {
        var.name_into(&mut scratch.name)?;
        let at = out.len();
        let step = next(agent, &scratch.name, Name::Request(var.oid), list, out);
        if step == Step::Full {
            return Ok(Written::Complete);
        }
        scratch.cursors.push(Cursor {
            name: name_in(out, at)?,
            ended: step == Step::Ended,
        });
    }
    if scratch.cursors.len() == 1 {
        let cursor = scratch.cursors[0].clone();
        return column(agent, cursor, max_repetitions - 1, list, scratch, out);
    }
    // Each later round, from the names the round before answered.
    for _ in 1..max_repetitions {
        if scratch.cursors.iter().all(|cursor| cursor.ended) {
            break;
        }
        for repeater in 0..scratch.cursors.len() {
            let cursor = scratch.cursors[repeater].clone();
            let at = out.len();
            let step = if cursor.ended {
                end_of_mib_view(Name::Response(cursor.name), list, out)
            } else {
                oid::decode_ber_body(&out[cursor.name.clone()], &mut scratch.name)
                    .ok_or(BerError::BadOid)?;
                next(agent, &scratch.name, Name::Response(cursor.name), list, out)
            };
            if step == Step::Full {
                return Ok(Written::Complete);
            }
            scratch.cursors[repeater] = Cursor {
                name: name_in(out, at)?,
                ended: step == Step::Ended,
            };
        }
    }
    Ok(Written::Complete)
}

/// The rounds after the first for a lone repeater whose first round
/// answered `first`: up to `rounds` successors under one read lock, then
/// `endOfMibView` if the table ends before them.
fn column(
    agent: &SnmpAgent,
    first: Cursor,
    rounds: u32,
    list: List<'_>,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Written, BerError> {
    if first.ended || rounds == 0 {
        return Ok(Written::Complete);
    }
    oid::decode_ber_body(&out[first.name.clone()], &mut scratch.name).ok_or(BerError::BadOid)?;
    let (mut emitted, mut full, mut last) = (0, false, None);
    agent.bulk(&scratch.name, rounds, false, |_, varbind| {
        if !list.fits(out, varbind.len()) {
            full = true;
            return false;
        }
        last = Some(out.len());
        out.extend_from_slice(varbind);
        emitted += 1;
        true
    });
    if full || emitted == rounds {
        return Ok(Written::Complete);
    }
    let name = match last {
        Some(at) => name_in(out, at)?,
        None => first.name,
    };
    end_of_mib_view(Name::Response(name), list, out);
    Ok(Written::Complete)
}

/// The `SetRequest` varbinds: the request's, unchanged.
fn echo(vars: &VarList<'_>, list: List<'_>, out: &mut Vec<u8>) -> Result<Written, BerError> {
    let body = vars.body();
    if !list.fits(out, body.len()) {
        return Ok(Written::Full);
    }
    out.extend_from_slice(body);
    Ok(Written::Complete)
}

/// Replace the response in `out` with `tooBig`, count it, and warn naming
/// the request's first OID; nothing is sent if even that does not fit. The
/// varbind list is empty under SNMPv2c and the request's under SNMPv1.
fn too_big(agent: &SnmpAgent, reply: &Reply<'_>, vars: &VarList<'_>, out: &mut Vec<u8>) -> Outcome {
    out.clear();
    agent.count(Counter::TooBig);
    let first = vars.iter().next().map(|var| var.oid);
    agent.warn_too_big(&Dotted(first.unwrap_or_default()), reply.budget);
    let echoed: &[u8] = if reply.version == V1 {
        vars.body()
    } else {
        &[]
    };
    error_response(reply, TOO_BIG, 0, echoed, out)
}

/// Replace the response in `out` with SNMPv1's `noSuchName` at the 1-based
/// `index`, carrying the request's varbinds; `tooBig` when that does not
/// fit.
fn no_such_name(
    agent: &SnmpAgent,
    reply: &Reply<'_>,
    vars: &VarList<'_>,
    index: usize,
    out: &mut Vec<u8>,
) -> Outcome {
    out.clear();
    let index = i64::try_from(index).unwrap_or(i64::MAX);
    match error_response(reply, NO_SUCH_NAME, index, vars.body(), out) {
        Outcome::Sent => Outcome::Sent,
        _ => too_big(agent, reply, vars, out),
    }
}

/// Write into `out` a response of error-status `status` at `index` whose
/// varbind list body is `varbinds`, or nothing when it does not fit.
fn error_response(
    reply: &Reply<'_>,
    status: i64,
    index: i64,
    varbinds: &[u8],
    out: &mut Vec<u8>,
) -> Outcome {
    let len = ber::message_len(
        reply.version,
        reply.community,
        reply.id,
        status,
        index,
        varbinds.len(),
    );
    if len > reply.budget {
        return Outcome::DroppedOther;
    }
    ber::write_message(
        out,
        reply.version,
        reply.community,
        Tag::Response,
        reply.id,
        status,
        index,
        |out| out.extend_from_slice(varbinds),
    );
    Outcome::Sent
}

/// What one successor lookup wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The successor's `VarBind`.
    Found,
    /// `endOfMibView` under the name looked up.
    Ended,
    /// Nothing: the varbind did not fit.
    Full,
}

/// Where a name's OBJECT IDENTIFIER body is: in the request, or already in
/// the response.
#[derive(Debug, Clone)]
enum Name<'a> {
    Request(&'a [u8]),
    Response(Range<usize>),
}

impl Name<'_> {
    fn len(&self) -> usize {
        match self {
            Name::Request(body) => body.len(),
            Name::Response(range) => range.len(),
        }
    }
}

/// Write the successor of `arcs`, or `endOfMibView` under `name`, which is
/// the same OID's body.
fn next(
    agent: &SnmpAgent,
    arcs: &[u32],
    name: Name<'_>,
    list: List<'_>,
    out: &mut Vec<u8>,
) -> Step {
    let mut step = Step::Ended;
    agent.bulk(arcs, 1, false, |_, varbind| {
        if list.fits(out, varbind.len()) {
            out.extend_from_slice(varbind);
            step = Step::Found;
        } else {
            step = Step::Full;
        }
        false
    });
    if step == Step::Ended {
        return end_of_mib_view(name, list, out);
    }
    step
}

/// Write `endOfMibView` under `name`, if it fits.
fn end_of_mib_view(name: Name<'_>, list: List<'_>, out: &mut Vec<u8>) -> Step {
    if !list.fits(out, exception_len(name.len())) {
        return Step::Full;
    }
    write_exception(out, name, Tag::EndOfMibView);
    Step::Ended
}

/// Octets a `VarBind` of a name of `name_len` body octets and an exception
/// takes.
fn exception_len(name_len: usize) -> usize {
    ber::tlv_len(ber::tlv_len(name_len) + 2)
}

/// Write a `VarBind` of `name` and the exception `tag`.
fn write_exception(out: &mut Vec<u8>, name: Name<'_>, tag: Tag) {
    ber::write_header(out, Tag::Sequence, ber::tlv_len(name.len()) + 2);
    ber::write_header(out, Tag::Oid, name.len());
    match name {
        Name::Request(body) => out.extend_from_slice(body),
        Name::Response(range) => out.extend_from_within(range),
    }
    ber::write_octets(out, tag, &[]);
}

/// An OBJECT IDENTIFIER body, displayed dotted, or `?` when it does not
/// decode.
struct Dotted<'a>(&'a [u8]);

impl std::fmt::Display for Dotted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match Oid::from_ber_body(self.0) {
            Some(oid) => write!(f, "{oid}"),
            None => f.write_str("?"),
        }
    }
}

/// The range of `out` holding the OBJECT IDENTIFIER body of the `VarBind`
/// that starts at `at`.
fn name_in(out: &[u8], at: usize) -> Result<Range<usize>, BerError> {
    let body = Reader::new(&out[at..]).expect(Tag::Sequence)?;
    let name = Reader::new(body).expect(Tag::Oid)?;
    // `name` is a subslice of `out`.
    let start = name.as_ptr() as usize - out.as_ptr() as usize;
    Ok(start..start + name.len())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::fmt::Write as _;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use rstest::rstest;

    use super::*;
    use crate::counting;
    use crate::schedule::LaunchId;
    use crate::sink::snmp::agent::Owner;
    use crate::sink::snmp::tests::{
        hex, NET_SNMP_GET, NET_SNMP_GETBULK, NET_SNMP_GETNEXT, NET_SNMP_SET, NET_SNMP_V3_DISCOVERY,
    };
    use crate::sink::snmp::{SnmpAgentConfig, SystemGroup};
    use crate::util::splitmix64;

    /// `snmpget -v1 -c private 127.0.0.1:<port> 1.3.6.1.2.1.1.3.0`, net-snmp 5.9.4.
    const NET_SNMP_V1_GET_PRIVATE: &str =
        "302a020100040770726976617465a01c020439f72d0e020100020100300e300c06082b060102010103\
        000500";

    /// `snmpget -v2c -c private 127.0.0.1:<port> 1.3.6.1.2.1.1.3.0`, net-snmp 5.9.4.
    const NET_SNMP_GET_PRIVATE: &str =
        "302a020101040770726976617465a01c02042b598b62020100020100300e300c06082b060102010103\
        000500";

    /// This codec's reply to [`NET_SNMP_GET`], which snmpget printed as
    /// `Timeticks: (852)`.
    const NET_SNMP_GET_REPLY: &str =
        "302b02010104067075626c6963a21e02043ac25c770201000201003010300e06082b060102010103\
        0043020354";

    /// This codec's reply to [`NET_SNMP_GETNEXT`], which snmpgetnext printed
    /// as `…6.1 = Counter64: 1001000`.
    const NET_SNMP_GETNEXT_REPLY: &str =
        "302f02010104067075626c6963a22202042db414d602010002010030143012060b2b060102011f01\
        0101060146030f4628";

    /// This codec's reply to [`NET_SNMP_GETBULK`], which snmpbulkget printed
    /// as ten Counter64 instances, the last 9223372036855275808 (2^63 + 500000).
    const NET_SNMP_GETBULK_REPLY: &str =
        "3081ec02010104067075626c6963a281de02041573e9b50201000201003081cf3012060b2b060102\
        011f010101060146030f46283012060b2b060102011f010101060246031e8c503012060b2b060102\
        011f010101060346032dd2783012060b2b060102011f010101060446033d18a03012060b2b060102\
        011f010101060546034c5ec83012060b2b060102011f010101060646035ba4f03012060b2b060102\
        011f010101060746036aeb183012060b2b060102011f010101060846037a31403013060b2b060102\
        011f01010106094604008977683018060b2b060102011f010101060a460900800000000007a120";

    /// This codec's tooBig reply to a gosnmp GET under a 200-byte budget.
    const TOO_BIG_REPLY: &str = "301b02010104067075626c6963a20e02042c261a7f0201010201003000";

    /// `snmpget -v1 -c public -On 127.0.0.1:<port> 1.3.6.1.2.1.2.2.1.8.3`,
    /// net-snmp 5.9.4.pre2.
    const NET_SNMP_V1_GET: &str =
        "302b02010004067075626c6963a01e020431704fc80201000201003010300e060a2b060102010202\
        0108030500";

    /// This codec's reply to [`NET_SNMP_V1_GET`], which snmpget printed as
    /// `.1.3.6.1.2.1.2.2.1.8.3 = INTEGER: 2`.
    const NET_SNMP_V1_GET_REPLY: &str =
        "302c02010004067075626c6963a21f020431704fc80201000201003011300f060a2b060102010202\
        010803020102";

    /// `snmpget -v1 -c public -On 127.0.0.1:<port> 1.3.6.1.2.1.31.1.1.1.6.1`,
    /// net-snmp 5.9.4.pre2: a Counter64 instance.
    const NET_SNMP_V1_GET_COUNTER64: &str =
        "302c02010004067075626c6963a01f0204383223ac0201000201003011300f060b2b060102011f01\
        010106010500";

    /// This codec's reply to [`NET_SNMP_V1_GET_COUNTER64`], which snmpget
    /// printed as `Reason: (noSuchName) There is no such variable name in
    /// this MIB.` and `Failed object: .1.3.6.1.2.1.31.1.1.1.6.1`.
    const NET_SNMP_V1_GET_COUNTER64_REPLY: &str =
        "302c02010004067075626c6963a21f0204383223ac0201020201013011300f060b2b060102011f01\
        010106010500";

    /// `snmpgetnext -v1 -c public -On 127.0.0.1:<port> 1.3.6.1.2.1.2.2.1.14.10`,
    /// net-snmp 5.9.4.pre2: the last ifInErrors, before the Counter64 column.
    const NET_SNMP_V1_GETNEXT: &str =
        "302b02010004067075626c6963a11e02041882cd920201000201003010300e060a2b060102010202\
        010e0a0500";

    /// This codec's reply to [`NET_SNMP_V1_GETNEXT`], which snmpgetnext
    /// printed as `.1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32: 10000`.
    const NET_SNMP_V1_GETNEXT_REPLY: &str =
        "302e02010004067075626c6963a22102041882cd9202010002010030133011060b2b060102011f01\
        01010f0142022710";

    /// `snmpset -v1 -c public -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0 s rtr-2`,
    /// net-snmp 5.9.4.pre2.
    const NET_SNMP_V1_SET: &str =
        "302e02010004067075626c6963a32102040e790ca00201000201003013301106082b060102010105\
        0004057274722d32";

    /// This codec's reply to [`NET_SNMP_V1_SET`], which snmpset printed as
    /// `Reason: (noSuchName) There is no such variable name in this MIB.` and
    /// `Failed object: .1.3.6.1.2.1.1.5.0`.
    const NET_SNMP_V1_SET_REPLY: &str =
        "302e02010004067075626c6963a22102040e790ca00201020201013013301106082b060102010105\
        0004057274722d32";

    /// `snmptrap -v1 -c public 127.0.0.1:<port> 1.3.6.1.4.1.8072.9999 127.0.0.1 2 0 ''
    /// 1.3.6.1.2.1.2.2.1.1.1 i 1`, net-snmp 5.9.4.pre2: a Trap-PDU (0xA4) at
    /// octet 13.
    const NET_SNMP_V1_TRAP: &str =
        "303c02010004067075626c6963a42f06092b06010401bf08ce0f40047f000001020102020100430300\
        c6943011300f060a2b060102010202010101020101";

    /// The system group's seven instances, in OID order.
    const SYSTEM: [&str; 7] = [
        "1.3.6.1.2.1.1.1.0",
        "1.3.6.1.2.1.1.2.0",
        "1.3.6.1.2.1.1.3.0",
        "1.3.6.1.2.1.1.4.0",
        "1.3.6.1.2.1.1.5.0",
        "1.3.6.1.2.1.1.6.0",
        "1.3.6.1.2.1.1.7.0",
    ];

    /// The columns of [`interface_table`], ten indices each.
    const COLUMNS: [&str; 4] = [
        "1.3.6.1.2.1.2.2.1.8",
        "1.3.6.1.2.1.2.2.1.14",
        "1.3.6.1.2.1.31.1.1.1.6",
        "1.3.6.1.2.1.31.1.1.1.15",
    ];

    /// The scalar after the interface table, and the last instance of it.
    const LAST: &str = "1.3.6.1.4.1.8072.9999.1.1.0";

    fn oid(text: &str) -> Oid {
        Oid::parse(text).expect("a valid OID")
    }

    fn system() -> SystemGroup {
        SystemGroup {
            name: "rtr-core-01".to_string(),
            descr: "Sonda synthetic router".to_string(),
            contact: "noc@example.com".to_string(),
            location: "lab-1".to_string(),
            ..SystemGroup::default()
        }
    }

    async fn agent() -> Arc<SnmpAgent> {
        agent_on("127.0.0.1:0").await
    }

    async fn agent_on(listen: &str) -> Arc<SnmpAgent> {
        let cfg = SnmpAgentConfig {
            system: system(),
            ..SnmpAgentConfig::new(listen)
        };
        SnmpAgent::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    /// An agent on an ephemeral port answering `versions`.
    async fn agent_serving(versions: &[SnmpVersion]) -> Arc<SnmpAgent> {
        let cfg = SnmpAgentConfig {
            system: system(),
            versions: versions.to_vec(),
            ..SnmpAgentConfig::new("127.0.0.1:0")
        };
        SnmpAgent::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    fn owner() -> Owner {
        Owner::from(LaunchId::next())
    }

    /// Publish the instance `text` with the value TLV `value` writes, and
    /// register its object, the instance without its last arc.
    fn publish(agent: &SnmpAgent, owner: Owner, text: &str, value: impl FnOnce(&mut Vec<u8>)) {
        let instance = oid(text);
        let mut encoded = Vec::new();
        value(&mut encoded);
        let mut varbind = Vec::new();
        ber::write_varbind(&mut varbind, &instance, &encoded);
        agent
            .publish(owner, &instance, Bytes::from(varbind), true)
            .expect("publishes");
        let arcs = instance.arcs();
        let object = Oid::from_arcs(arcs[..arcs.len() - 1].to_vec()).expect("an object");
        agent.register_object(&object);
    }

    /// Four columns of ten indices, then [`LAST`]: ifOperStatus (INTEGER,
    /// 2 on every third index), ifInErrors (Counter32, 7 per index),
    /// ifHCInOctets (Counter64, 1 250 000 per index) and ifHighSpeed (Gauge32
    /// 10 000).
    fn interface_table(agent: &SnmpAgent) {
        let owner = owner();
        for index in 1..=10u32 {
            let instance = |column: usize| format!("{}.{index}", COLUMNS[column]);
            let status = if index % 3 == 0 { 2 } else { 1 };
            publish(agent, owner, &instance(0), |out| {
                ber::write_integer(out, status)
            });
            publish(agent, owner, &instance(1), |out| {
                ber::write_counter32(out, index * 7)
            });
            publish(agent, owner, &instance(2), |out| {
                ber::write_counter64(out, u64::from(index) * 1_250_000)
            });
            publish(agent, owner, &instance(3), |out| {
                ber::write_gauge32(out, 10_000)
            });
        }
        publish(agent, owner, LAST, |out| ber::write_integer(out, 42));
    }

    /// A v2c request as `community`, carrying `names` with NULL values.
    fn request_as(
        community: &str,
        pdu: Tag,
        id: i64,
        fields: (i64, i64),
        names: &[&str],
    ) -> Vec<u8> {
        message_as(V2C, community, pdu, id, fields, names)
    }

    /// A request of the version field `version` as `community`, carrying
    /// `names` with NULL values.
    fn message_as(
        version: i64,
        community: &str,
        pdu: Tag,
        id: i64,
        fields: (i64, i64),
        names: &[&str],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        ber::write_message(
            &mut out,
            version,
            community.as_bytes(),
            pdu,
            id,
            fields.0,
            fields.1,
            |out| {
                for name in names {
                    ber::write_varbind(out, &oid(name), &[0x05, 0x00]);
                }
            },
        );
        out
    }

    fn request(pdu: Tag, names: &[&str]) -> Vec<u8> {
        request_as("public", pdu, 0x1234, (0, 0), names)
    }

    fn bulk(non_repeaters: i64, max_repetitions: i64, names: &[&str]) -> Vec<u8> {
        request_as(
            "public",
            Tag::GetBulkRequest,
            0x1234,
            (non_repeaters, max_repetitions),
            names,
        )
    }

    /// What `agent` answers `datagram` with under `budget`.
    fn answered(agent: &SnmpAgent, datagram: &[u8], budget: usize) -> (Outcome, Vec<u8>) {
        let mut out = Vec::new();
        let outcome = respond(
            datagram,
            agent,
            budget,
            &mut Scratch::default(),
            &mut out,
            |_| {},
        );
        (outcome, out)
    }

    /// The reply `agent` sends to `datagram` under 1472 bytes.
    fn reply(agent: &SnmpAgent, datagram: &[u8]) -> Response {
        let (outcome, out) = answered(agent, datagram, 1472);
        assert_eq!(outcome, Outcome::Sent);
        response(&out)
    }

    /// A Response-PDU, decoded.
    #[derive(Debug, PartialEq)]
    struct Response {
        id: i64,
        status: i64,
        index: i64,
        /// Each varbind as `name = value`.
        varbinds: Vec<String>,
    }

    fn response(datagram: &[u8]) -> Response {
        response_in(datagram, V2C)
    }

    /// A Response-PDU of the version field `version`, decoded.
    fn response_in(datagram: &[u8], version: i64) -> Response {
        let message = Message::decode(datagram).expect("the reply is a v1 or v2c message");
        assert_eq!(message.version, version);
        assert_eq!(message.community, b"public");
        assert_eq!(message.pdu, Tag::Response);
        Response {
            id: message.request_id,
            status: message.error_status,
            index: message.error_index,
            varbinds: message
                .varbinds()
                .map(|var| text(&var.expect("a complete VarBind")))
                .collect(),
        }
    }

    /// `name = value`, the value by its type.
    fn text(var: &VarBind<'_>) -> String {
        let name = var.name().expect("an OID");
        let unsigned = || ber::decode_u64(var.value).expect("an unsigned value");
        let value = match var.value_tag {
            0x02 => format!(
                "INTEGER {}",
                ber::decode_i64(var.value).expect("an INTEGER")
            ),
            0x04 => format!("STRING {:?}", String::from_utf8_lossy(var.value)),
            0x05 => "NULL".to_string(),
            0x06 => format!("OID {}", Oid::from_ber_body(var.value).expect("an OID")),
            0x41 => format!("Counter32 {}", unsigned()),
            0x42 => format!("Gauge32 {}", unsigned()),
            0x43 => format!("Timeticks {}", unsigned()),
            0x46 => format!("Counter64 {}", unsigned()),
            0x80 => "noSuchObject".to_string(),
            0x81 => "noSuchInstance".to_string(),
            0x82 => "endOfMibView".to_string(),
            other => format!("tag 0x{other:02x}"),
        };
        format!("{name} = {value}")
    }

    /// A request summarised: its type, id, bulk fields and names.
    fn summary(request: &Request<'_>) -> String {
        let names = |vars: &VarList<'_>| {
            let names: Vec<String> = vars
                .iter()
                .map(|var| var.name().expect("an OID").to_string())
                .collect();
            names.join(" ")
        };
        let version = match request.version {
            Version::V1 => "v1",
            Version::V2c => "v2c",
        };
        let community = String::from_utf8_lossy(request.community);
        let pdu = match &request.pdu {
            Pdu::Get { id, vars } => format!("get {id} [{}]", names(vars)),
            Pdu::GetNext { id, vars } => format!("getnext {id} [{}]", names(vars)),
            Pdu::GetBulk {
                id,
                non_repeaters,
                max_repetitions,
                vars,
            } => format!(
                "getbulk {id} {non_repeaters} {max_repetitions} [{}]",
                names(vars)
            ),
            Pdu::Set { id, vars } => format!("set {id} [{}]", names(vars)),
            Pdu::Other(tag) => format!("other {tag:?}"),
        };
        format!("{version} {community} {pdu}")
    }

    // ---- decode -------------------------------------------------------------

    /// Each captured request decodes to its PDU type, request-id, bulk
    /// fields and names.
    #[rustfmt::skip]
    #[rstest]
    #[case::net_snmp_get(          NET_SNMP_GET,            "v2c public get 985816183 [1.3.6.1.2.1.1.3.0]")]
    #[case::net_snmp_getnext(      NET_SNMP_GETNEXT,        "v2c public getnext 766776534 [1.3.6.1.2.1.31.1.1.1.6]")]
    #[case::net_snmp_getbulk(      NET_SNMP_GETBULK,        "v2c public getbulk 359918005 0 10 [1.3.6.1.2.1.31.1.1.1.6]")]
    #[case::net_snmp_set(          NET_SNMP_SET,            "v2c public set 1919400767 [1.3.6.1.2.1.1.5.0]")]
    #[case::net_snmp_get_private(  NET_SNMP_GET_PRIVATE,    "v2c private get 727288674 [1.3.6.1.2.1.1.3.0]")]
    #[case::net_snmp_v1_private(   NET_SNMP_V1_GET_PRIVATE, "v1 private get 972500238 [1.3.6.1.2.1.1.3.0]")]
    fn a_captured_request_decodes_to_its_pdu(#[case] datagram: &str, #[case] expected: &str) {
        let datagram = hex(datagram);
        assert_eq!(datagram.first(), Some(&0x30), "fixture present");
        let request = decode(&datagram).expect("decodes");
        assert_eq!(summary(&request), expected);
    }

    /// A negative non-repeaters or max-repetitions is read as 0.
    #[test]
    fn negative_bulk_fields_are_read_as_zero() {
        let datagram = bulk(-1, -5, &["1.3.6.1.2.1.1.1.0"]);
        let request = decode(&datagram).expect("decodes");
        assert_eq!(
            summary(&request),
            "v2c public getbulk 4660 0 0 [1.3.6.1.2.1.1.1.0]"
        );
    }

    /// Any PDU but the four requests is classified by its tag alone.
    #[rstest]
    #[case::response(Tag::Response)]
    #[case::inform(Tag::InformRequest)]
    #[case::trap(Tag::TrapV2)]
    #[case::report(Tag::Report)]
    fn other_pdus_are_classified_by_tag(#[case] tag: Tag) {
        let datagram = request(tag, &["1.3.6.1.2.1.1.3.0"]);
        let request = decode(&datagram).expect("decodes");
        assert_eq!(request.pdu, Pdu::Other(tag));
    }

    /// The datagram as [`NET_SNMP_GET`], with `edit` applied.
    fn edited(edit: fn(&mut Vec<u8>)) -> Vec<u8> {
        let mut datagram = hex(NET_SNMP_GET);
        edit(&mut datagram);
        datagram
    }

    /// A GET whose only varbind has a name and no value.
    fn without_a_value() -> Vec<u8> {
        let mut out = Vec::new();
        ber::write_message(&mut out, V2C, b"public", Tag::GetRequest, 7, 0, 0, |out| {
            ber::write_tlv(out, Tag::Sequence, |out| {
                ber::write_oid(out, &oid(SYSTEM[2]))
            });
        });
        out
    }

    /// What does not decode, and why.
    #[rstest]
    #[case::truncated(edited(|d| { d.pop(); }), BerError::Truncated)]
    #[case::indefinite_length(edited(|d| d[1] = 0x80), BerError::BadLength)]
    #[case::trailing_octet(edited(|d| d.push(0)), BerError::BadLength)]
    #[case::version_2(edited(|d| d[4] = 2), BerError::UnsupportedVersion(2))]
    #[case::v3(hex(NET_SNMP_V3_DISCOVERY), BerError::UnsupportedVersion(3))]
    #[case::request_id_past_32_bits(request_as("public", Tag::GetRequest, 1 << 31, (0, 0), &[SYSTEM[0]]), BerError::Overflow)]
    #[case::varbind_without_a_value(without_a_value(), BerError::Truncated)]
    #[case::empty(Vec::new(), BerError::Truncated)]
    fn a_malformed_datagram_is_refused_with_its_error(
        #[case] datagram: Vec<u8>,
        #[case] expected: BerError,
    ) {
        assert_eq!(decode(&datagram), Err(expected));
    }

    /// How [`respond`] counts what it does not answer.
    #[rstest]
    #[case::truncated(edited(|d| { d.pop(); }), Outcome::DroppedOther, "malformed")]
    #[case::version_2(edited(|d| d[4] = 2), Outcome::DroppedOther, "malformed")]
    #[case::v3(
        hex(NET_SNMP_V3_DISCOVERY),
        Outcome::DroppedUnsupportedVersion,
        "unsupported_version"
    )]
    #[case::v1(
        hex(NET_SNMP_V1_GET_PRIVATE),
        Outcome::DroppedUnsupportedVersion,
        "unsupported_version"
    )]
    #[case::inform(request(Tag::InformRequest, &[SYSTEM[0]]), Outcome::DroppedOther, "malformed")]
    #[case::trap(request(Tag::TrapV2, &[SYSTEM[0]]), Outcome::DroppedOther, "malformed")]
    #[case::report(request(Tag::Report, &[SYSTEM[0]]), Outcome::DroppedOther, "malformed")]
    #[case::response(request(Tag::Response, &[SYSTEM[0]]), Outcome::DroppedOther, "malformed")]
    #[case::name_not_an_oid(request_with_name(&[0x2b, 0x80, 0x01]), Outcome::DroppedOther, "malformed")]
    #[tokio::test]
    async fn an_unanswered_datagram_is_counted_once(
        #[case] datagram: Vec<u8>,
        #[case] expected: Outcome,
        #[case] counter: &str,
    ) {
        let agent = agent().await;
        assert_eq!(
            reply(&agent, &hex(NET_SNMP_GET)).status,
            0,
            "the agent answers"
        );
        let before = agent.requests();
        let (outcome, out) = answered(&agent, &datagram, 1472);
        assert_eq!(outcome, expected);
        assert!(out.is_empty(), "nothing to send");
        let after = agent.requests();
        let moved = |r: &crate::sink::snmp::RequestCounters| match counter {
            "malformed" => r.malformed,
            _ => r.unsupported_version,
        };
        assert_eq!(moved(&after), moved(&before) + 1, "{after:?}");
        assert_eq!(
            after.get + after.malformed + after.unsupported_version,
            before.get + before.malformed + before.unsupported_version + 1,
            "counted once: {after:?}"
        );
    }

    /// A GET whose only name has the OBJECT IDENTIFIER body `body`.
    fn request_with_name(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        ber::write_message(&mut out, V2C, b"public", Tag::GetRequest, 7, 0, 0, |out| {
            ber::write_tlv(out, Tag::Sequence, |out| {
                ber::write_octets(out, Tag::Oid, body);
                ber::write_null(out);
            });
        });
        out
    }

    // ---- answer, without a socket -------------------------------------------

    /// The captured net-snmp requests are answered byte for byte as the
    /// replies net-snmp printed, `sysUpTime` read 8.52 s after bind.
    #[tokio::test(start_paused = true)]
    async fn captured_requests_are_answered_as_net_snmp_read_them() {
        let agent = agent().await;
        let owner = owner();
        for index in 1..=10u64 {
            let value = if index == 10 {
                (1 << 63) + 500_000
            } else {
                index * 1_001_000
            };
            publish(&agent, owner, &format!("{}.{index}", COLUMNS[2]), |out| {
                ber::write_counter64(out, value)
            });
        }
        tokio::time::advance(Duration::from_millis(8_520)).await;
        for (request, expected) in [
            (NET_SNMP_GET, NET_SNMP_GET_REPLY),
            (NET_SNMP_GETNEXT, NET_SNMP_GETNEXT_REPLY),
            (NET_SNMP_GETBULK, NET_SNMP_GETBULK_REPLY),
        ] {
            let (outcome, out) = answered(&agent, &hex(request), 1472);
            assert_eq!(outcome, Outcome::Sent);
            assert_eq!(out, hex(expected), "the reply to {request}");
        }
        assert_eq!(
            (
                agent.requests().get,
                agent.requests().get_next,
                agent.requests().get_bulk
            ),
            (1, 1, 1)
        );
    }

    /// Each name of a GET is answered on its own: a value, noSuchInstance
    /// for a missing index of a known column, noSuchObject for an unknown
    /// column.
    #[rustfmt::skip]
    #[rstest]
    #[case::present(&["1.3.6.1.2.1.31.1.1.1.6.3"], &["1.3.6.1.2.1.31.1.1.1.6.3 = Counter64 3750000"])]
    #[case::missing_instance(&["1.3.6.1.2.1.31.1.1.1.6.11"], &["1.3.6.1.2.1.31.1.1.1.6.11 = noSuchInstance"])]
    #[case::unknown_column(&["1.3.6.1.2.1.31.1.1.1.7.1"], &["1.3.6.1.2.1.31.1.1.1.7.1 = noSuchObject"])]
    #[case::system_scalar(&["1.3.6.1.2.1.1.5.0"], &["1.3.6.1.2.1.1.5.0 = STRING \"rtr-core-01\""])]
    #[case::all_three(
        &["1.3.6.1.2.1.2.2.1.14.4", "1.3.6.1.2.1.2.2.1.14.0", "1.3.6.1.2.1.99.1"],
        &["1.3.6.1.2.1.2.2.1.14.4 = Counter32 28", "1.3.6.1.2.1.2.2.1.14.0 = noSuchInstance", "1.3.6.1.2.1.99.1 = noSuchObject"],
    )]
    #[tokio::test]
    async fn a_get_answers_each_name(#[case] names: &[&str], #[case] expected: &[&str]) {
        let agent = agent().await;
        interface_table(&agent);
        let reply = reply(&agent, &request(Tag::GetRequest, names));
        assert_eq!((reply.id, reply.status, reply.index), (0x1234, 0, 0));
        assert_eq!(reply.varbinds, expected);
        assert_eq!(agent.requests().get, 1);
    }

    /// A GETNEXT answers the next instance in OID order, across the system
    /// group and the table, and endOfMibView under the name past the last.
    #[rustfmt::skip]
    #[rstest]
    #[case::before_the_first("0.0", "1.3.6.1.2.1.1.1.0 = STRING \"Sonda synthetic router\"")]
    #[case::between_two("1.3.6.1.2.1.2.2.1.8.2", "1.3.6.1.2.1.2.2.1.8.3 = INTEGER 2")]
    #[case::index_10_after_9("1.3.6.1.2.1.2.2.1.8.9", "1.3.6.1.2.1.2.2.1.8.10 = INTEGER 1")]
    #[case::end_of_a_column("1.3.6.1.2.1.2.2.1.8.10", "1.3.6.1.2.1.2.2.1.14.1 = Counter32 7")]
    #[case::system_to_table("1.3.6.1.2.1.1.7.0", "1.3.6.1.2.1.2.2.1.8.1 = INTEGER 1")]
    #[case::the_last(LAST, "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView")]
    #[case::after_the_last("2.0", "2.0 = endOfMibView")]
    #[tokio::test]
    async fn a_getnext_answers_the_next_instance(#[case] name: &str, #[case] expected: &str) {
        let agent = agent().await;
        interface_table(&agent);
        let reply = reply(&agent, &request(Tag::GetNextRequest, &[name]));
        assert_eq!((reply.id, reply.status), (0x1234, 0));
        assert_eq!(reply.varbinds, [expected]);
        assert_eq!(agent.requests().get_next, 1);
    }

    /// GETBULK with one non-repeater and two repeaters, ten repetitions: the
    /// non-repeater's successor, then the two columns interleaved round by
    /// round.
    #[tokio::test]
    async fn a_getbulk_interleaves_its_repeaters_after_the_non_repeaters() {
        let agent = agent().await;
        interface_table(&agent);
        let reply = reply(
            &agent,
            &bulk(1, 10, &["1.3.6.1.2.1.1.5.0", COLUMNS[0], COLUMNS[2]]),
        );
        assert_eq!((reply.id, reply.status, reply.index), (0x1234, 0, 0));
        let expected = [
            "1.3.6.1.2.1.1.6.0 = STRING \"lab-1\"",
            "1.3.6.1.2.1.2.2.1.8.1 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.1 = Counter64 1250000",
            "1.3.6.1.2.1.2.2.1.8.2 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.2 = Counter64 2500000",
            "1.3.6.1.2.1.2.2.1.8.3 = INTEGER 2",
            "1.3.6.1.2.1.31.1.1.1.6.3 = Counter64 3750000",
            "1.3.6.1.2.1.2.2.1.8.4 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.4 = Counter64 5000000",
            "1.3.6.1.2.1.2.2.1.8.5 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.5 = Counter64 6250000",
            "1.3.6.1.2.1.2.2.1.8.6 = INTEGER 2",
            "1.3.6.1.2.1.31.1.1.1.6.6 = Counter64 7500000",
            "1.3.6.1.2.1.2.2.1.8.7 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.7 = Counter64 8750000",
            "1.3.6.1.2.1.2.2.1.8.8 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.8 = Counter64 10000000",
            "1.3.6.1.2.1.2.2.1.8.9 = INTEGER 2",
            "1.3.6.1.2.1.31.1.1.1.6.9 = Counter64 11250000",
            "1.3.6.1.2.1.2.2.1.8.10 = INTEGER 1",
            "1.3.6.1.2.1.31.1.1.1.6.10 = Counter64 12500000",
        ];
        assert_eq!(reply.varbinds, expected);
        assert_eq!(agent.requests().get_bulk, 1);
    }

    /// Repetitions stop after the first round in which every repeater is
    /// endOfMibView, which names the last instance it reached.
    #[rustfmt::skip]
    #[rstest]
    #[case::two_repeaters(
        &["1.3.6.1.2.1.31.1.1.1.15.10", LAST],
        &[
            "1.3.6.1.4.1.8072.9999.1.1.0 = INTEGER 42",
            "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView",
            "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView",
            "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView",
        ],
    )]
    #[case::one_repeater(
        &["1.3.6.1.2.1.31.1.1.1.15.9"],
        &[
            "1.3.6.1.2.1.31.1.1.1.15.10 = Gauge32 10000",
            "1.3.6.1.4.1.8072.9999.1.1.0 = INTEGER 42",
            "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView",
        ],
    )]
    #[case::one_repeater_past_the_end(&["2.0"], &["2.0 = endOfMibView"])]
    #[tokio::test]
    async fn a_getbulk_stops_after_a_round_all_end_of_mib_view(
        #[case] names: &[&str],
        #[case] expected: &[&str],
    ) {
        let agent = agent().await;
        interface_table(&agent);
        let reply = reply(&agent, &bulk(0, 10, names));
        assert!(
            expected.len() < 10 * names.len(),
            "fewer varbinds than the repetitions asked for"
        );
        assert_eq!(reply.varbinds, expected);
    }

    /// 10 000 instances and 1 000 repetitions: the reply is the longest
    /// prefix of the untrimmed answer that fits 1472 bytes, ending on a
    /// complete varbind.
    #[tokio::test]
    async fn a_getbulk_is_trimmed_to_the_budget() {
        let agent = agent().await;
        let owner = owner();
        for index in 1..=10_000u64 {
            publish(&agent, owner, &format!("{}.{index}", COLUMNS[2]), |out| {
                ber::write_counter64(out, index * 1_000_003)
            });
        }
        assert_eq!(agent.instance_count(), 10_000);
        let datagram = bulk(0, 1_000, &[COLUMNS[2]]);

        let (_, untrimmed) = answered(&agent, &datagram, usize::MAX);
        let untrimmed = response(&untrimmed);
        assert_eq!(untrimmed.varbinds.len(), 1_000);
        let (outcome, out) = answered(&agent, &datagram, 1472);
        assert_eq!(outcome, Outcome::Sent);
        let trimmed = response(&out);
        assert!(out.len() <= 1472, "{} bytes", out.len());
        let kept = trimmed.varbinds.len();
        assert!(kept > 0 && kept < 1_000, "kept {kept}");
        assert_eq!(trimmed.varbinds, untrimmed.varbinds[..kept], "a prefix");
        assert_eq!(trimmed.status, 0);

        // One more varbind would not have fitted.
        let (_, full) = answered(&agent, &datagram, usize::MAX);
        let message = Message::decode(&full).expect("decodes");
        let mut list = Reader::new(message.varbinds);
        let lengths: Vec<usize> = (0..=kept)
            .map(|_| ber::tlv_len(list.raw_tlv().expect("a VarBind").1.len()))
            .collect();
        let one_more = ber::message_len(V2C, b"public", 0x1234, 0, 0, lengths.iter().sum());
        assert!(one_more > 1472, "{one_more} bytes with one more");
        assert_eq!(
            agent.requests().too_big,
            0,
            "a trimmed GETBULK is not tooBig"
        );
    }

    /// A GET whose answer does not fit the budget is tooBig with an empty
    /// varbind list, counted.
    #[tokio::test]
    async fn a_get_that_does_not_fit_is_too_big() {
        let agent = agent().await;
        let long = "1.3.6.1.4.1.8072.9999.1.2.0";
        publish(&agent, owner(), long, |out| {
            ber::write_string(out, &[b'x'; 500])
        });
        let datagram = request(Tag::GetRequest, &[long]);
        // Guard: under the default budget the value is answered.
        let fits = reply(&agent, &datagram);
        assert_eq!(fits.varbinds.len(), 1);
        assert!(fits.varbinds[0].contains(&"x".repeat(500)));

        let (outcome, out) = answered(&agent, &datagram, 484);
        assert_eq!(outcome, Outcome::Sent);
        let too_big = response(&out);
        assert_eq!(
            too_big,
            Response {
                id: 0x1234,
                status: 1,
                index: 0,
                varbinds: Vec::new(),
            }
        );
        let requests = agent.requests();
        assert_eq!((requests.get, requests.too_big), (2, 1));
    }

    /// The tooBig reply, byte for byte.
    #[tokio::test]
    async fn the_too_big_reply_is_the_codecs() {
        let agent = agent().await;
        let datagram = request_as("public", Tag::GetRequest, 0x2c26_1a7f, (0, 0), &[SYSTEM[0]]);
        let (outcome, out) = answered(&agent, &datagram, 40);
        assert_eq!(outcome, Outcome::Sent);
        assert_eq!(out, hex(TOO_BIG_REPLY));
    }

    /// A SET is answered notWritable at index 1, its varbinds echoed, and
    /// nothing changes.
    #[tokio::test]
    async fn a_set_is_answered_not_writable() {
        let agent = agent().await;
        let datagram = hex(NET_SNMP_SET);
        let reply = reply(&agent, &datagram);
        assert_eq!(
            reply,
            Response {
                id: 0x7267_bb3f,
                status: 17,
                index: 1,
                varbinds: vec!["1.3.6.1.2.1.1.5.0 = STRING \"rtr-2\"".to_string()],
            }
        );
        let get = self::reply(&agent, &request(Tag::GetRequest, &[SYSTEM[4]]));
        assert_eq!(get.varbinds, ["1.3.6.1.2.1.1.5.0 = STRING \"rtr-core-01\""]);
        assert_eq!(agent.requests().set_refused, 1);
    }

    /// A request with no varbinds is answered with none, noError.
    #[rstest]
    #[case::get(request(Tag::GetRequest, &[]))]
    #[case::getnext(request(Tag::GetNextRequest, &[]))]
    #[case::getbulk(bulk(1, 10, &[]))]
    #[case::set(request(Tag::SetRequest, &[]))]
    #[tokio::test]
    async fn an_empty_varbind_list_is_answered_empty(#[case] datagram: Vec<u8>) {
        let agent = agent().await;
        interface_table(&agent);
        assert_eq!(
            reply(&agent, &datagram),
            Response {
                id: 0x1234,
                status: 0,
                index: 0,
                varbinds: Vec::new(),
            }
        );
    }

    /// A wrong community is dropped and counted; the same request with the
    /// agent's community is answered.
    #[tokio::test]
    async fn a_wrong_community_is_dropped_and_counted() {
        let agent = agent().await;
        assert_eq!(
            reply(&agent, &hex(NET_SNMP_GET)).varbinds.len(),
            1,
            "the agent answers"
        );
        let mut called = false;
        let mut out = Vec::new();
        let outcome = respond(
            &hex(NET_SNMP_GET_PRIVATE),
            &agent,
            1472,
            &mut Scratch::default(),
            &mut out,
            |request| called = request.community == b"private",
        );
        assert_eq!(outcome, Outcome::DroppedWrongCommunity);
        assert!(out.is_empty());
        assert!(called, "the request is handed over for the warning");
        let requests = agent.requests();
        assert_eq!((requests.wrong_community, requests.get), (1, 1));
    }

    /// SNMPv1 and SNMPv3 are dropped before any community is compared: the
    /// v1 datagram's community is wrong, yet the wrong-community counter
    /// stays put.
    #[rstest]
    #[case::v1(NET_SNMP_V1_GET_PRIVATE)]
    #[case::v3(NET_SNMP_V3_DISCOVERY)]
    #[tokio::test]
    async fn an_unsupported_version_is_dropped_before_the_community(#[case] datagram: &str) {
        let agent = agent().await;
        let datagram = hex(datagram);
        assert!(!datagram.is_empty(), "fixture present");
        // Guard: the v2c form of the v1 request is a wrong community.
        assert_eq!(
            answered(&agent, &hex(NET_SNMP_GET_PRIVATE), 1472).0,
            Outcome::DroppedWrongCommunity
        );
        assert_eq!(agent.requests().wrong_community, 1);

        let (outcome, out) = answered(&agent, &datagram, 1472);
        assert_eq!(outcome, Outcome::DroppedUnsupportedVersion);
        assert!(out.is_empty());
        let requests = agent.requests();
        assert_eq!(requests.unsupported_version, 1);
        assert_eq!(
            requests.wrong_community, 1,
            "the community was not compared"
        );
    }

    /// One GETBULK of ten varbinds allocates nothing once its buffers have
    /// answered one like it.
    #[tokio::test]
    async fn a_getbulk_allocates_nothing() {
        let agent = agent().await;
        interface_table(&agent);
        let datagram = bulk(0, 10, &[COLUMNS[2]]);
        let mut scratch = Scratch::default();
        let mut out = Vec::with_capacity(1472);
        let request = decode(&datagram).expect("decodes");
        assert_eq!(
            answer(&request, &agent, 1472, &mut scratch, &mut out),
            Outcome::Sent
        );
        counting::assert_count_allocations_counts();
        let ((outcome, len), allocations) = counting::count_allocations(|| {
            let outcome = answer(&request, &agent, 1472, &mut scratch, &mut out);
            (outcome, out.len())
        });
        assert_eq!(outcome, Outcome::Sent);
        assert_eq!(response(&out).varbinds.len(), 10, "{len} bytes");
        assert_eq!(allocations, 0);
    }

    /// An SNMPv1 GET and an SNMPv1 GETNEXT of ten varbinds each allocate
    /// nothing once their buffers have answered one like it; the GETNEXT's
    /// last name has to pass over the Counter64 column.
    #[rustfmt::skip]
    #[rstest]
    #[case::get(Tag::GetRequest, COLUMNS[0], "1.3.6.1.2.1.2.2.1.8.10 = INTEGER 1")]
    #[case::getnext(Tag::GetNextRequest, COLUMNS[1], "1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32 10000")]
    #[tokio::test]
    async fn a_v1_request_allocates_nothing(
        #[case] pdu: Tag,
        #[case] column: &str,
        #[case] last: &str,
    ) {
        let agent = agent_serving(&[SnmpVersion::V1]).await;
        interface_table(&agent);
        let names: Vec<String> = (1..=10).map(|index| format!("{column}.{index}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let datagram = message_as(V1, "public", pdu, 0x1234, (0, 0), &names);
        let mut scratch = Scratch::default();
        let mut out = Vec::with_capacity(1472);
        let request = decode(&datagram).expect("decodes");
        assert_eq!(request.version, Version::V1);
        assert_eq!(
            answer(&request, &agent, 1472, &mut scratch, &mut out),
            Outcome::Sent
        );
        counting::assert_count_allocations_counts();
        let (outcome, allocations) = counting::count_allocations(|| {
            answer(&request, &agent, 1472, &mut scratch, &mut out)
        });
        assert_eq!(outcome, Outcome::Sent);
        let reply = response_in(&out, V1);
        assert_eq!((reply.status, reply.varbinds.len()), (0, 10));
        assert_eq!(reply.varbinds.last().map(String::as_str), Some(last));
        assert_eq!(allocations, 0);
    }

    // ---- the walk -----------------------------------------------------------

    /// A seeded draw of numbers.
    struct Draw(u64, u64);

    impl Draw {
        fn next(&mut self, below: u64) -> u64 {
            self.1 += 1;
            splitmix64(self.0 ^ self.1.wrapping_mul(0x9e37_79b9)) % below
        }
    }

    /// 1 to 40 OIDs outside the system group, from `seed`: under a few
    /// subtrees, with arcs whose BER encodings take one to five octets.
    fn random_oids(seed: u64) -> BTreeSet<Oid> {
        const PREFIXES: [&[u32]; 6] = [
            &[0, 0],
            &[1, 0, 8802, 1],
            &[1, 3, 6, 1, 2, 1, 2, 2, 1],
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1],
            &[1, 3, 6, 1, 4, 1, 8072],
            &[2, 999],
        ];
        const ARCS: [u32; 8] = [0, 1, 127, 128, 16_383, 16_384, 2_097_152, u32::MAX];
        let mut draw = Draw(seed, 0);
        let count = 1 + draw.next(40);
        (0..count)
            .map(|_| {
                let mut arcs = PREFIXES[draw.next(6) as usize].to_vec();
                for _ in 0..1 + draw.next(4) {
                    let arc = if draw.next(2) == 0 {
                        ARCS[draw.next(8) as usize]
                    } else {
                        draw.next(50) as u32
                    };
                    arcs.push(arc);
                }
                Oid::from_arcs(arcs).expect("an encodable OID")
            })
            .collect()
    }

    /// The names a reply's varbinds carry, and whether each is endOfMibView.
    fn names(datagram: &[u8]) -> Vec<(Oid, bool)> {
        let message = Message::decode(datagram).expect("a reply");
        message
            .varbinds()
            .map(|var| {
                let var = var.expect("a VarBind");
                (var.name().expect("an OID"), var.value_tag == 0x82)
            })
            .collect()
    }

    /// A request of type `pdu` for the one name `name`.
    fn request_for(pdu: Tag, fields: (i64, i64), names: &[&Oid]) -> Vec<u8> {
        let mut out = Vec::new();
        ber::write_message(
            &mut out,
            V2C,
            b"public",
            pdu,
            1,
            fields.0,
            fields.1,
            |out| {
                for name in names {
                    ber::write_varbind(out, name, &[0x05, 0x00]);
                }
            },
        );
        out
    }

    /// For 200 seeded OID sets, a GETNEXT chain from 0.0 visits every
    /// published OID and the system group once each, in OID order, and a
    /// GETBULK of one repeater from 0.0 answers the same chain and then
    /// endOfMibView.
    #[tokio::test]
    async fn a_getnext_walk_visits_every_oid_once_in_order() {
        let mut checked = 0;
        for seed in 0..200 {
            let published = random_oids(seed);
            assert!(!published.is_empty(), "seed {seed}");
            let agent = agent().await;
            let owner = owner();
            for (n, instance) in published.iter().enumerate() {
                let mut value = Vec::new();
                ber::write_counter64(&mut value, n as u64);
                let mut varbind = Vec::new();
                ber::write_varbind(&mut varbind, instance, &value);
                agent
                    .publish(owner, instance, Bytes::from(varbind), true)
                    .expect("publishes");
            }
            let mut expected: Vec<Oid> = published.iter().cloned().collect();
            expected.extend(SYSTEM.iter().map(|text| oid(text)));
            expected.sort();

            let mut walked = Vec::new();
            let mut at = oid("0.0");
            loop {
                let (outcome, out) = answered(
                    &agent,
                    &request_for(Tag::GetNextRequest, (0, 0), &[&at]),
                    usize::MAX,
                );
                assert_eq!(outcome, Outcome::Sent);
                let [(next, ended)] = names(&out).try_into().expect("one varbind per GETNEXT");
                if ended {
                    assert_eq!(next, at, "endOfMibView names the request");
                    break;
                }
                assert!(next > at, "seed {seed}: {next} follows {at}");
                walked.push(next.clone());
                at = next;
            }
            assert_eq!(walked.len(), published.len() + 7, "seed {seed}");
            assert_eq!(walked, expected, "seed {seed}");

            let repetitions = expected.len() as i64 + 3;
            let (_, out) = answered(
                &agent,
                &request_for(Tag::GetBulkRequest, (0, repetitions), &[&oid("0.0")]),
                usize::MAX,
            );
            let bulk = names(&out);
            let (last, ended) = bulk.last().cloned().expect("varbinds");
            assert!(ended && last == expected[expected.len() - 1], "seed {seed}");
            let found: Vec<Oid> = bulk[..bulk.len() - 1]
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            assert_eq!(found, expected, "seed {seed}: the GETBULK");
            checked += 1;
        }
        assert_eq!(checked, 200);
    }

    /// For the same 200 sets, a GETBULK of two repeaters answers two GETNEXT
    /// chains interleaved, each ending in endOfMibView under its last name.
    #[tokio::test]
    async fn getbulk_rounds_are_getnext_chains_interleaved() {
        for seed in 0..200 {
            let published = random_oids(seed);
            let agent = agent().await;
            let owner = owner();
            for instance in &published {
                let mut varbind = Vec::new();
                ber::write_varbind(&mut varbind, instance, &[0x02, 0x01, 0x01]);
                agent
                    .publish(owner, instance, Bytes::from(varbind), true)
                    .expect("publishes");
            }
            let starts = [oid("0.0"), oid("1.3.6.1.2.1.2")];
            let repetitions = 1 + Draw(seed, 99).next(12);
            let chain = |start: &Oid| {
                let mut chain = Vec::new();
                let mut at = start.clone();
                for _ in 0..repetitions {
                    match agent.next_after(&at) {
                        Some((next, _)) => {
                            chain.push((next.clone(), false));
                            at = next;
                        }
                        None => chain.push((at.clone(), true)),
                    }
                }
                chain
            };
            let chains = [chain(&starts[0]), chain(&starts[1])];
            let mut expected = Vec::new();
            for round in 0..repetitions as usize {
                if round > 0 && chains.iter().all(|chain| chain[round - 1].1) {
                    break;
                }
                expected.push(chains[0][round].clone());
                expected.push(chains[1][round].clone());
            }
            let datagram = request_for(
                Tag::GetBulkRequest,
                (0, repetitions as i64),
                &[&starts[0], &starts[1]],
            );
            let (_, out) = answered(&agent, &datagram, usize::MAX);
            assert!(!expected.is_empty());
            assert_eq!(
                names(&out),
                expected,
                "seed {seed}, {repetitions} repetitions"
            );
        }
    }

    // ---- SNMPv1 -------------------------------------------------------------

    /// ifOperStatus.3, an INTEGER.
    const STATUS_3: &str = "1.3.6.1.2.1.2.2.1.8.3";

    /// ifHCInOctets.1, a Counter64.
    const OCTETS_1: &str = "1.3.6.1.2.1.31.1.1.1.6.1";

    /// What an agent serving v1 and v2c answers the request `pdu` for
    /// `names` with under `budget`, in `version`: the outcome and the
    /// decoded response.
    async fn answered_in(
        agent: &SnmpAgent,
        version: i64,
        pdu: Tag,
        names: &[&str],
        budget: usize,
    ) -> Response {
        let datagram = message_as(version, "public", pdu, 0x1234, (0, 0), names);
        let (outcome, out) = answered(agent, &datagram, budget);
        assert_eq!(outcome, Outcome::Sent, "version {version}: {names:?}");
        response_in(&out, version)
    }

    /// Each request is answered under SNMPv1 as RFC 1157 and RFC 3584
    /// §4.1.2.3 say. Every case is the same request in both versions, to an
    /// agent serving both, and asserts the v2c answer too, so the case shows
    /// the v1 difference: a name with no instance or a Counter64 one makes
    /// a GET noSuchName at its 1-based position with the request's varbinds;
    /// a GETNEXT skips Counter64 instances and is noSuchName past the end;
    /// tooBig carries the request's varbinds; noSuchName takes precedence
    /// over tooBig; a SET is noSuchName at index 1.
    #[rustfmt::skip]
    #[rstest]
    #[case::get_an_integer(Tag::GetRequest, &[STATUS_3], 1472,
        (0, 0, &["1.3.6.1.2.1.2.2.1.8.3 = INTEGER 2"][..]),
        (0, 0, &["1.3.6.1.2.1.2.2.1.8.3 = INTEGER 2"][..]))]
    #[case::get_a_counter64(Tag::GetRequest, &[OCTETS_1], 1472,
        (0, 0, &["1.3.6.1.2.1.31.1.1.1.6.1 = Counter64 1250000"][..]),
        (2, 1, &["1.3.6.1.2.1.31.1.1.1.6.1 = NULL"][..]))]
    #[case::get_three_the_second_missing(Tag::GetRequest, &["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.11", "1.3.6.1.2.1.2.2.1.14.2"], 1472,
        (0, 0, &["1.3.6.1.2.1.2.2.1.8.1 = INTEGER 1", "1.3.6.1.2.1.2.2.1.8.11 = noSuchInstance", "1.3.6.1.2.1.2.2.1.14.2 = Counter32 14"][..]),
        (2, 2, &["1.3.6.1.2.1.2.2.1.8.1 = NULL", "1.3.6.1.2.1.2.2.1.8.11 = NULL", "1.3.6.1.2.1.2.2.1.14.2 = NULL"][..]))]
    #[case::get_an_unknown_object(Tag::GetRequest, &["1.3.6.1.2.1.99.1"], 1472,
        (0, 0, &["1.3.6.1.2.1.99.1 = noSuchObject"][..]),
        (2, 1, &["1.3.6.1.2.1.99.1 = NULL"][..]))]
    #[case::getnext_before_a_counter64_column(Tag::GetNextRequest, &["1.3.6.1.2.1.2.2.1.14.10"], 1472,
        (0, 0, &["1.3.6.1.2.1.31.1.1.1.6.1 = Counter64 1250000"][..]),
        (0, 0, &["1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32 10000"][..]))]
    #[case::getnext_inside_a_counter64_column(Tag::GetNextRequest, &["1.3.6.1.2.1.31.1.1.1.6.4"], 1472,
        (0, 0, &["1.3.6.1.2.1.31.1.1.1.6.5 = Counter64 6250000"][..]),
        (0, 0, &["1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32 10000"][..]))]
    #[case::getnext_past_the_end(Tag::GetNextRequest, &[LAST], 1472,
        (0, 0, &["1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView"][..]),
        (2, 1, &["1.3.6.1.4.1.8072.9999.1.1.0 = NULL"][..]))]
    #[case::getnext_two_the_second_past_the_end(Tag::GetNextRequest, &["1.3.6.1.2.1.2.2.1.8.1", LAST], 1472,
        (0, 0, &["1.3.6.1.2.1.2.2.1.8.2 = INTEGER 1", "1.3.6.1.4.1.8072.9999.1.1.0 = endOfMibView"][..]),
        (2, 2, &["1.3.6.1.2.1.2.2.1.8.1 = NULL", "1.3.6.1.4.1.8072.9999.1.1.0 = NULL"][..]))]
    #[case::too_big(Tag::GetRequest, &["1.3.6.1.2.1.1.1.0"], 50,
        (1, 0, &[][..]),
        (1, 0, &["1.3.6.1.2.1.1.1.0 = NULL"][..]))]
    #[case::no_such_name_before_too_big(Tag::GetRequest, &["1.3.6.1.2.1.1.1.0", "1.3.6.1.2.1.2.2.1.8.11"], 60,
        (1, 0, &[][..]),
        (2, 2, &["1.3.6.1.2.1.1.1.0 = NULL", "1.3.6.1.2.1.2.2.1.8.11 = NULL"][..]))]
    #[case::set(Tag::SetRequest, &["1.3.6.1.2.1.1.5.0"], 1472,
        (17, 1, &["1.3.6.1.2.1.1.5.0 = NULL"][..]),
        (2, 1, &["1.3.6.1.2.1.1.5.0 = NULL"][..]))]
    #[tokio::test]
    async fn a_v1_request_is_answered_as_rfc_1157_says(
        #[case] pdu: Tag,
        #[case] names: &[&str],
        #[case] budget: usize,
        #[case] v2c: (i64, i64, &[&str]),
        #[case] v1: (i64, i64, &[&str]),
    ) {
        let agent = agent_serving(&[SnmpVersion::V1, SnmpVersion::V2c]).await;
        interface_table(&agent);
        let reply = answered_in(&agent, V2C, pdu, names, budget).await;
        assert_eq!((reply.id, reply.status, reply.index), (0x1234, v2c.0, v2c.1), "v2c");
        assert_eq!(reply.varbinds, v2c.2, "v2c");
        assert_eq!(agent.requests().v1, 0);

        let reply = answered_in(&agent, V1, pdu, names, budget).await;
        assert_eq!((reply.id, reply.status, reply.index), (0x1234, v1.0, v1.1), "v1");
        assert_eq!(reply.varbinds, v1.2, "v1");
        assert_eq!(agent.requests().v1, 1);
    }

    /// The captured net-snmp v1 requests are answered byte for byte as the
    /// replies net-snmp printed, each carrying version 0.
    #[tokio::test]
    async fn captured_v1_requests_are_answered_as_net_snmp_read_them() {
        let agent = agent_serving(&[SnmpVersion::V1]).await;
        interface_table(&agent);
        for (request, expected) in [
            (NET_SNMP_V1_GET, NET_SNMP_V1_GET_REPLY),
            (NET_SNMP_V1_GET_COUNTER64, NET_SNMP_V1_GET_COUNTER64_REPLY),
            (NET_SNMP_V1_GETNEXT, NET_SNMP_V1_GETNEXT_REPLY),
            (NET_SNMP_V1_SET, NET_SNMP_V1_SET_REPLY),
        ] {
            let request = hex(request);
            assert_eq!(
                request.get(2..5),
                Some(&[0x02, 0x01, 0x00][..]),
                "a v1 fixture"
            );
            let (outcome, out) = answered(&agent, &request, 1472);
            assert_eq!(outcome, Outcome::Sent);
            assert_eq!(out.get(2..5), Some(&[0x02, 0x01, 0x00][..]), "version 0");
            assert_eq!(out, hex(expected), "the reply to {request:02x?}");
        }
        let requests = agent.requests();
        assert_eq!(
            (
                requests.get,
                requests.get_next,
                requests.set_refused,
                requests.v1
            ),
            (2, 1, 1, 4)
        );
    }

    /// A GetBulkRequest or a Trap-PDU in a v1 message is malformed: dropped
    /// unanswered and counted, the community never compared. Guard: the same
    /// agent answers the v2c form of the GETBULK and a v1 GET.
    #[rstest]
    #[case::getbulk(message_as(V1, "public", Tag::GetBulkRequest, 0x1234, (0, 10), &[COLUMNS[0]]))]
    #[case::trap(hex(NET_SNMP_V1_TRAP))]
    #[tokio::test]
    async fn a_getbulk_or_a_trap_in_a_v1_message_is_malformed(#[case] datagram: Vec<u8>) {
        let agent = agent_serving(&[SnmpVersion::V1, SnmpVersion::V2c]).await;
        interface_table(&agent);
        assert_eq!(
            datagram.get(2..5),
            Some(&[0x02, 0x01, 0x00][..]),
            "a v1 message"
        );
        assert!(matches!(datagram.get(13), Some(0xa4 | 0xa5)), "the PDU tag");
        let v2c_bulk = bulk(0, 10, &[COLUMNS[0]]);
        assert_eq!(
            response(&answered(&agent, &v2c_bulk, 1472).1)
                .varbinds
                .len(),
            10
        );
        let v1_get = message_as(V1, "public", Tag::GetRequest, 1, (0, 0), &[STATUS_3]);
        assert_eq!(answered(&agent, &v1_get, 1472).0, Outcome::Sent);
        let before = agent.requests();
        assert_eq!((before.malformed, before.v1), (0, 1));

        let (outcome, out) = answered(&agent, &datagram, 1472);
        assert_eq!(outcome, Outcome::DroppedOther);
        assert!(out.is_empty());
        let after = agent.requests();
        assert_eq!((after.malformed, after.v1), (1, 1));
        assert_eq!(after.wrong_community, 0);
    }

    /// A request in a version the agent does not serve is dropped and
    /// counted as unsupported, the community never compared. Guard: the same
    /// agent answers the same request in the version it serves.
    #[rustfmt::skip]
    #[rstest]
    #[case::v1_to_a_v2c_agent(SnmpVersion::V2c, V1,  V2C)]
    #[case::v2c_to_a_v1_agent(SnmpVersion::V1,  V2C, V1)]
    #[tokio::test]
    async fn a_version_the_agent_does_not_serve_is_dropped(
        #[case] served: SnmpVersion,
        #[case] sent: i64,
        #[case] answered_as: i64,
    ) {
        let agent = agent_serving(&[served]).await;
        interface_table(&agent);
        let reply = answered_in(&agent, answered_as, Tag::GetRequest, &[STATUS_3], 1472).await;
        assert_eq!(reply.varbinds, ["1.3.6.1.2.1.2.2.1.8.3 = INTEGER 2"]);

        let datagram = message_as(sent, "another", Tag::GetRequest, 1, (0, 0), &[STATUS_3]);
        let (outcome, out) = answered(&agent, &datagram, 1472);
        assert_eq!(outcome, Outcome::DroppedUnsupportedVersion);
        assert!(out.is_empty());
        let requests = agent.requests();
        assert_eq!(requests.unsupported_version, 1);
        assert_eq!(requests.wrong_community, 0, "the community was not compared");
        assert_eq!(requests.get, 1);
    }

    /// `v1` counts the requests answered as SNMPv1; `get` and `get_next`
    /// count both versions; a request that is dropped counts no `v1`.
    #[tokio::test]
    async fn the_v1_counter_counts_requests_answered_as_v1() {
        let agent = agent_serving(&[SnmpVersion::V1, SnmpVersion::V2c]).await;
        interface_table(&agent);
        for (version, pdu, community) in [
            (V1, Tag::GetRequest, "public"),
            (V1, Tag::GetNextRequest, "public"),
            (V1, Tag::GetRequest, "another"),
            (V2C, Tag::GetRequest, "public"),
            (V2C, Tag::GetNextRequest, "public"),
        ] {
            let datagram = message_as(version, community, pdu, 1, (0, 0), &[STATUS_3]);
            answered(&agent, &datagram, 1472);
        }
        let requests = agent.requests();
        assert_eq!(
            (
                requests.get,
                requests.get_next,
                requests.wrong_community,
                requests.v1
            ),
            (2, 2, 1, 2)
        );
    }

    /// For 200 seeded OID sets with Counter64 and Counter32 values, a v1
    /// GETNEXT chain from 0.0 visits every instance that is not a Counter64,
    /// the system group included, once each in OID order, and only those,
    /// then answers noSuchName at index 1. Guard: each set walked holds at
    /// least one Counter64 instance and one other.
    #[tokio::test]
    async fn a_v1_getnext_walk_visits_every_instance_but_counter64_once_in_order() {
        let mut checked = 0;
        for seed in 0..200 {
            let published = random_oids(seed);
            if published.len() < 2 {
                continue;
            }
            let agent = agent_serving(&[SnmpVersion::V1]).await;
            let owner = owner();
            let mut expected: Vec<Oid> = SYSTEM.iter().map(|text| oid(text)).collect();
            let mut counter64 = 0;
            for (n, instance) in published.iter().enumerate() {
                let mut value = Vec::new();
                if n % 2 == 0 {
                    ber::write_counter64(&mut value, n as u64);
                    counter64 += 1;
                } else {
                    ber::write_counter32(&mut value, n as u32);
                    expected.push(instance.clone());
                }
                let mut varbind = Vec::new();
                ber::write_varbind(&mut varbind, instance, &value);
                agent
                    .publish(owner, instance, Bytes::from(varbind), true)
                    .expect("publishes");
            }
            expected.sort();
            assert!(
                counter64 >= 1 && expected.len() > SYSTEM.len(),
                "seed {seed}"
            );

            let mut walked = Vec::new();
            let mut at = oid("0.0");
            loop {
                let datagram = message_as(
                    V1,
                    "public",
                    Tag::GetNextRequest,
                    1,
                    (0, 0),
                    &[&at.to_string()],
                );
                let (outcome, out) = answered(&agent, &datagram, usize::MAX);
                assert_eq!(outcome, Outcome::Sent);
                let reply = response_in(&out, V1);
                let [(next, _)] = names(&out).try_into().expect("one varbind per GETNEXT");
                if reply.status == NO_SUCH_NAME {
                    assert_eq!((reply.index, &next), (1, &at), "seed {seed}: the end");
                    break;
                }
                assert_eq!(reply.status, 0, "seed {seed}");
                assert!(next > at, "seed {seed}: {next} follows {at}");
                walked.push(next.clone());
                at = next;
            }
            assert_eq!(walked, expected, "seed {seed}");
            checked += 1;
        }
        assert!(checked >= 150, "{checked} sets walked");
    }

    // ---- the socket ---------------------------------------------------------

    /// Send `datagram` from `client` to `addr` and return the reply, within
    /// two seconds.
    async fn exchange(client: &UdpSocket, addr: SocketAddr, datagram: &[u8]) -> Vec<u8> {
        client.send_to(datagram, addr).await.expect("sent");
        let mut buffer = vec![0; RECEIVE_BUFFER];
        let (len, from) =
            tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buffer))
                .await
                .expect("a reply within 2 s")
                .expect("received");
        assert_eq!(from, addr, "the reply comes from the agent");
        buffer.truncate(len);
        buffer
    }

    async fn client() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind a client socket")
    }

    /// The TimeTicks of a reply's only varbind.
    fn ticks(reply: &[u8]) -> u64 {
        let message = Message::decode(reply).expect("a reply");
        let var = message
            .varbinds()
            .next()
            .expect("a varbind")
            .expect("a VarBind");
        assert_eq!(var.value_tag, Tag::TimeTicks.byte());
        ber::decode_u64(var.value).expect("TimeTicks")
    }

    /// Two GETs of sysUpTime.0 100 ms apart answer increasing TimeTicks.
    #[tokio::test]
    async fn sys_up_time_advances_between_two_gets() {
        let agent = agent().await;
        let addr = agent.local_addr();
        let client = client().await;
        let datagram = hex(NET_SNMP_GET);
        let first = exchange(&client, addr, &datagram).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = exchange(&client, addr, &datagram).await;
        for reply in [&first, &second] {
            assert_eq!(
                Message::decode(reply).expect("a reply").request_id,
                0x3ac2_5c77
            );
        }
        let (first, second) = (ticks(&first), ticks(&second));
        assert!(second >= first + 10, "{first} then {second}");
    }

    /// A client walking the table by GETBULK, ten at a time from 0.0, sees
    /// what `next_after` walks, in its order.
    #[tokio::test]
    async fn a_client_bulk_walk_matches_next_after() {
        let agent = agent().await;
        interface_table(&agent);
        let mut expected = Vec::new();
        let mut at = oid("0.0");
        while let Some((next, _)) = agent.next_after(&at) {
            assert!(next > at, "{next} follows {at}");
            expected.push(next.clone());
            at = next;
        }
        assert_eq!(expected.len(), 7 + 41);

        let client = client().await;
        let mut walked = Vec::new();
        let mut at = oid("0.0");
        'walk: loop {
            let reply = exchange(
                &client,
                agent.local_addr(),
                &request_for(Tag::GetBulkRequest, (0, 10), &[&at]),
            )
            .await;
            let names = names(&reply);
            assert!(!names.is_empty());
            for (name, ended) in names {
                if ended {
                    break 'walk;
                }
                assert!(name > at, "{name} follows {at}");
                walked.push(name.clone());
                at = name;
            }
        }
        assert_eq!(walked, expected);
        assert!(agent.requests().get_bulk >= 5, "{:?}", agent.requests());
    }

    /// A datagram larger than the receive buffer cannot be sent over
    /// loopback; the largest that can is not a message, and is dropped and
    /// counted as malformed, after which the agent still answers.
    #[tokio::test]
    async fn the_largest_datagram_is_dropped_as_malformed() {
        let agent = agent().await;
        let addr = agent.local_addr();
        let client = client().await;
        let oversized = vec![0x30; 70_000];
        let sent = client.send_to(&oversized, addr).await;
        assert!(
            sent.is_err(),
            "loopback carries no 70 000-byte datagram: {sent:?}"
        );

        // The most a UDP datagram over IPv4 carries, a SEQUENCE that claims
        // more than it holds.
        let mut largest = vec![0; 65_507];
        largest[..4].copy_from_slice(&[0x30, 0x83, 0x01, 0x00]);
        largest[4] = 0xff;
        client.send_to(&largest, addr).await.expect("sent");
        let reply = exchange(&client, addr, &hex(NET_SNMP_GET)).await;
        assert_eq!(response(&reply).varbinds.len(), 1, "still answering");
        let requests = agent.requests();
        assert_eq!((requests.malformed, requests.get), (1, 1));
    }

    /// A message read truncated by the receive buffer does not decode.
    #[test]
    fn a_message_longer_than_the_receive_buffer_is_malformed() {
        let mut long = Vec::new();
        ber::write_message(
            &mut long,
            V2C,
            &[b'c'; 70_000],
            Tag::GetRequest,
            1,
            0,
            0,
            |_| {},
        );
        assert!(long.len() > RECEIVE_BUFFER);
        assert!(decode(&long).is_ok(), "whole, it is a message");
        assert_eq!(decode(&long[..RECEIVE_BUFFER]), Err(BerError::Truncated));
    }

    // ---- the reply's source address -------------------------------------------

    fn v4(destination: &str, local: &str) -> PacketInfo {
        PacketInfo::V4 {
            destination: destination.parse().expect("an IPv4 address"),
            local: local.parse().expect("an IPv4 address"),
        }
    }

    fn v6(destination: &str, ifindex: u32) -> PacketInfo {
        PacketInfo::V6 {
            destination: destination.parse().expect("an IPv6 address"),
            ifindex,
        }
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::ipv4_unicast_is_itself(                         v4("10.0.0.5", "10.0.0.5"),        Some(("10.0.0.5", 0)))]
    #[case::ipv4_subnet_broadcast_is_the_interface(         v4("10.0.0.255", "10.0.0.5"),      Some(("10.0.0.5", 0)))]
    #[case::ipv4_limited_broadcast_is_the_interface(        v4("255.255.255.255", "10.0.0.5"), Some(("10.0.0.5", 0)))]
    #[case::ipv4_multicast_is_the_interface(                v4("224.0.0.1", "10.0.0.5"),       Some(("10.0.0.5", 0)))]
    #[case::ipv4_unicast_without_a_local_address_is_itself( v4("10.0.0.5", "0.0.0.0"),         Some(("10.0.0.5", 0)))]
    #[case::ipv4_multicast_without_a_local_address_is_none( v4("224.0.0.1", "0.0.0.0"),        None)]
    #[case::ipv6_unicast_is_itself(                         v6("2001:db8::5", 3),              Some(("2001:db8::5", 0)))]
    #[case::ipv6_multicast_is_the_interface(                v6("ff02::1", 3),                  Some(("::", 3)))]
    #[case::ipv4_mapped_unicast_is_itself(                  v6("::ffff:10.0.0.5", 3),          Some(("::ffff:10.0.0.5", 0)))]
    #[case::ipv4_mapped_broadcast_is_the_interface(         v6("::ffff:255.255.255.255", 3),   Some(("::ffff:0.0.0.0", 3)))]
    #[case::ipv4_mapped_multicast_is_the_interface(         v6("::ffff:224.0.0.1", 3),         Some(("::ffff:0.0.0.0", 3)))]
    fn a_reply_leaves_from_the_local_unicast_address(
        #[case] info: PacketInfo,
        #[case] expected: Option<(&str, u32)>,
    ) {
        let expected = expected.map(|(addr, ifindex)| ReplySource {
            addr: addr.parse().expect("an address"),
            ifindex,
        });
        assert_eq!(reply_source(info), expected);
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::ipv4_wildcard("0.0.0.0:1161",   true)]
    #[case::ipv6_wildcard("[::]:1161",      true)]
    #[case::ipv4_address( "127.0.0.1:1161", false)]
    #[case::ipv6_address( "[::1]:1161",     false)]
    fn only_a_wildcard_bind_wants_packet_info(#[case] local: &str, #[case] wanted: bool) {
        assert_eq!(wants_pktinfo(local.parse().expect("a socket address")), wanted);
    }

    /// The responder reads packet info exactly when the socket was asked
    /// for it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn a_socket_reports_packet_info_once_enabled() {
        let socket = UdpSocket::bind("0.0.0.0:0").await.expect("bind");
        assert!(!pktinfo::enabled(&socket));
        enable_pktinfo(&socket, socket.local_addr().expect("bound")).expect("enabled");
        assert!(pktinfo::enabled(&socket));
    }

    /// Whether `[::1]` can be bound. Without it the caller is skipped, with
    /// a message, except under CI (`CI` set), where its absence fails.
    fn ipv6_loopback() -> bool {
        match std::net::UdpSocket::bind("[::1]:0") {
            Ok(_) => true,
            Err(e) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI has IPv6 loopback, so this test runs there: {e}"
                );
                eprintln!("skipped: no IPv6 loopback here ({e})");
                false
            }
        }
    }

    /// Send a GET of sysUpTime.0 from a socket bound on `client` and
    /// connected to `asked`, and return the reply it receives within 1 s,
    /// after the agent counted the GET.
    async fn connected_get(agent: &SnmpAgent, client: &str, asked: SocketAddr) -> Vec<u8> {
        let socket = UdpSocket::bind(client).await.expect("bind the client");
        socket.connect(asked).await.expect("connect the client");
        let before = agent.requests().get;
        socket.send(&hex(NET_SNMP_GET)).await.expect("sent");
        let requests = counted(agent, |r| r.get > before).await;
        assert_eq!(
            requests.get,
            before + 1,
            "the agent answered the GET to {asked}"
        );
        let mut buffer = vec![0; RECEIVE_BUFFER];
        let len = tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut buffer))
            .await
            .unwrap_or_else(|_| panic!("no reply from {asked} within 1 s"))
            .expect("received");
        buffer.truncate(len);
        assert_eq!(agent.requests().send_errors, 0);
        assert_eq!(
            Message::decode(&buffer).expect("a reply").request_id,
            0x3ac2_5c77
        );
        buffer
    }

    /// A wildcard-bound agent answers a request sent to 127.0.0.2 from
    /// 127.0.0.2, though the route back to the client leaves from
    /// 127.0.0.1: the client's connected socket drops a reply from any
    /// address but the one it sent to.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_wildcard_bind_answers_from_the_address_it_was_asked_on() {
        let agent = agent_on("0.0.0.0:0").await;
        let asked = SocketAddr::from(([127, 0, 0, 2], agent.local_addr().port()));
        let reply = connected_get(&agent, "127.0.0.1:0", asked).await;
        assert_eq!(response(&reply).varbinds.len(), 1);
    }

    /// An agent on `[::]` answers a request sent to `::1` from `::1`. On a
    /// host whose only IPv6 address is `::1` the route back chooses `::1`
    /// too, so this passes without packet info; the dual-stack test below is
    /// the one that needs the IPv6 `sendmsg` path.
    #[tokio::test]
    async fn an_ipv6_wildcard_bind_answers_from_the_address_it_was_asked_on() {
        if !ipv6_loopback() {
            return;
        }
        let agent = agent_on("[::]:0").await;
        let asked = SocketAddr::from((Ipv6Addr::LOCALHOST, agent.local_addr().port()));
        let reply = connected_get(&agent, "[::1]:0", asked).await;
        assert_eq!(response(&reply).varbinds.len(), 1);
    }

    /// An agent on `[::]` answers an IPv4 request sent to 127.0.0.2 from
    /// 127.0.0.2, as it does on `0.0.0.0`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_dual_stack_wildcard_bind_answers_ipv4_from_the_address_it_was_asked_on() {
        if !ipv6_loopback() {
            return;
        }
        let agent = agent_on("[::]:0").await;
        let asked = SocketAddr::from((
            Ipv4Addr::new(127, 0, 0, 2).to_ipv6_mapped(),
            agent.local_addr().port(),
        ));
        let reply = connected_get(&agent, "[::ffff:127.0.0.1]:0", asked).await;
        assert_eq!(response(&reply).varbinds.len(), 1);
    }

    /// An agent on a specific address reads without packet info, sends with
    /// `send_to`, and answers a connected client.
    #[tokio::test]
    async fn a_specific_bind_answers_a_connected_client() {
        let agent = agent().await;
        assert!(!wants_pktinfo(agent.local_addr()));
        let reply = connected_get(&agent, "127.0.0.1:0", agent.local_addr()).await;
        assert_eq!(response(&reply).varbinds.len(), 1);
    }

    /// Every event logged while it is a thread's default subscriber, as
    /// text: the level, then each field as `name=value`.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<String>>>);

    impl Captured {
        fn lines(&self) -> Vec<String> {
            self.0.lock().expect("lines").clone()
        }

        fn warnings(&self, needle: &str) -> Vec<String> {
            self.lines()
                .into_iter()
                .filter(|line| line.starts_with("WARN") && line.contains(needle))
                .collect()
        }
    }

    impl tracing::Subscriber for Captured {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = event.metadata().level().to_string();
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    let _ = write!(line, " {}={value:?}", field.name());
                },
            );
            self.0.lock().expect("lines").push(line);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// While alive, the calling thread's events go to a [`Captured`].
    struct Capturing {
        _default: tracing::subscriber::DefaultGuard,
        _second: tracing::Dispatch,
    }

    /// Capture what this thread logs, the agent's task on a current-thread
    /// runtime included, while the returned guard lives.
    ///
    /// tracing caches, per callsite, whether any subscriber wants it. While
    /// one dispatcher is registered it asks only the default of the thread
    /// that reaches the callsite first, which on another test's thread is no
    /// subscriber; a second one kept registered makes it ask every
    /// registered dispatcher. The cache is rebuilt once both are.
    fn capture() -> (Captured, Capturing) {
        let captured = Captured::default();
        let second = tracing::Dispatch::new(Captured::default());
        let default = tracing::subscriber::set_default(captured.clone());
        tracing::callsite::rebuild_interest_cache();
        (
            captured,
            Capturing {
                _default: default,
                _second: second,
            },
        )
    }

    /// Wait up to two seconds for `agent`'s counters to satisfy `done`.
    async fn counted(
        agent: &SnmpAgent,
        done: impl Fn(&crate::sink::snmp::RequestCounters) -> bool,
    ) -> crate::sink::snmp::RequestCounters {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let requests = agent.requests();
            if done(&requests) || Instant::now() >= deadline {
                return requests;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Two wrong-community requests from one IP on different source ports
    /// warn once; one from another IP warns again. The warning names the
    /// peer and the community's length, never the community.
    #[tokio::test]
    async fn a_wrong_community_warns_once_per_peer_ip() {
        let (captured, _capturing) = capture();
        let agent = agent().await;
        let addr = agent.local_addr();
        let (first, second) = (client().await, client().await);
        assert_ne!(
            first.local_addr().expect("bound"),
            second.local_addr().expect("bound")
        );
        // Guard: the socket answers this client.
        let reply = exchange(&first, addr, &hex(NET_SNMP_GET)).await;
        assert_eq!(response(&reply).varbinds.len(), 1);

        let wrong = hex(NET_SNMP_GET_PRIVATE);
        first.send_to(&wrong, addr).await.expect("sent");
        second.send_to(&wrong, addr).await.expect("sent");
        let requests = counted(&agent, |r| r.wrong_community == 2).await;
        assert_eq!(requests.wrong_community, 2, "both reached the agent");
        let warnings = captured.warnings("another community");
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0].contains("127.0.0.1") && warnings[0].contains("community_len=7"),
            "{}",
            warnings[0]
        );

        let other = UdpSocket::bind("127.0.0.2:0")
            .await
            .expect("bind 127.0.0.2");
        other.send_to(&wrong, addr).await.expect("sent");
        let requests = counted(&agent, |r| r.wrong_community == 3).await;
        assert_eq!(requests.wrong_community, 3);
        let warnings = captured.warnings("another community");
        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[1].contains("127.0.0.2"), "{}", warnings[1]);
        assert!(
            captured
                .lines()
                .iter()
                .all(|line| !line.contains("private")),
            "a community is never logged"
        );
    }

    /// tooBig warns once per minute per agent, naming the first OID and
    /// the budget.
    #[tokio::test(start_paused = true)]
    async fn too_big_warns_once_per_minute() {
        let (captured, _capturing) = capture();
        let agent = agent().await;
        let datagram = request(Tag::GetRequest, &[SYSTEM[0]]);
        let too_big = || {
            let (outcome, out) = answered(&agent, &datagram, 40);
            assert_eq!((outcome, response(&out).status), (Outcome::Sent, 1));
        };
        too_big();
        too_big();
        tokio::time::advance(Duration::from_secs(59)).await;
        too_big();
        let warnings = captured.warnings("tooBig");
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0].contains("1.3.6.1.2.1.1.1.0")
                && warnings[0].contains("max_message_size=40"),
            "{}",
            warnings[0]
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        too_big();
        assert_eq!(captured.warnings("tooBig").len(), 2);
        assert_eq!(agent.requests().too_big, 4);
    }

    // ---- net-snmp -----------------------------------------------------------

    /// `MIBS= snmpbulkwalk -v2c -c public -On 127.0.0.1:<port> 1.3.6.1.2.1`,
    /// net-snmp 5.9.4, over [`interface_table`] and [`system`]: its stdout,
    /// the TimeTicks masked.
    const NET_SNMP_BULKWALK: &str = ".1.3.6.1.2.1.1.1.0 = STRING: \"Sonda synthetic router\"\n\
        .1.3.6.1.2.1.1.2.0 = OID: .1.3.6.1.4.1.8072.3.2.10\n\
        .1.3.6.1.2.1.1.3.0 = Timeticks: (*)\n\
        .1.3.6.1.2.1.1.4.0 = STRING: \"noc@example.com\"\n\
        .1.3.6.1.2.1.1.5.0 = STRING: \"rtr-core-01\"\n\
        .1.3.6.1.2.1.1.6.0 = STRING: \"lab-1\"\n\
        .1.3.6.1.2.1.1.7.0 = INTEGER: 72\n\
        .1.3.6.1.2.1.2.2.1.8.1 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.2 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.3 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.5 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.6 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.7 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.8 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.9 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.10 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.14.1 = Counter32: 7\n\
        .1.3.6.1.2.1.2.2.1.14.2 = Counter32: 14\n\
        .1.3.6.1.2.1.2.2.1.14.3 = Counter32: 21\n\
        .1.3.6.1.2.1.2.2.1.14.4 = Counter32: 28\n\
        .1.3.6.1.2.1.2.2.1.14.5 = Counter32: 35\n\
        .1.3.6.1.2.1.2.2.1.14.6 = Counter32: 42\n\
        .1.3.6.1.2.1.2.2.1.14.7 = Counter32: 49\n\
        .1.3.6.1.2.1.2.2.1.14.8 = Counter32: 56\n\
        .1.3.6.1.2.1.2.2.1.14.9 = Counter32: 63\n\
        .1.3.6.1.2.1.2.2.1.14.10 = Counter32: 70\n\
        .1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: 1250000\n\
        .1.3.6.1.2.1.31.1.1.1.6.2 = Counter64: 2500000\n\
        .1.3.6.1.2.1.31.1.1.1.6.3 = Counter64: 3750000\n\
        .1.3.6.1.2.1.31.1.1.1.6.4 = Counter64: 5000000\n\
        .1.3.6.1.2.1.31.1.1.1.6.5 = Counter64: 6250000\n\
        .1.3.6.1.2.1.31.1.1.1.6.6 = Counter64: 7500000\n\
        .1.3.6.1.2.1.31.1.1.1.6.7 = Counter64: 8750000\n\
        .1.3.6.1.2.1.31.1.1.1.6.8 = Counter64: 10000000\n\
        .1.3.6.1.2.1.31.1.1.1.6.9 = Counter64: 11250000\n\
        .1.3.6.1.2.1.31.1.1.1.6.10 = Counter64: 12500000\n\
        .1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.2 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.3 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.4 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.5 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.6 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.7 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.8 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.9 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.10 = Gauge32: 10000\n";

    /// `stdout` with every TimeTicks value masked as `(*)`.
    fn mask_timeticks(stdout: &str) -> String {
        stdout
            .lines()
            .map(|line| match line.find("= Timeticks: (") {
                Some(at) => format!("{}= Timeticks: (*)\n", &line[..at]),
                None => format!("{line}\n"),
            })
            .collect()
    }

    /// Run a net-snmp tool with `args`, MIB loading off.
    async fn net_snmp(tool: &'static str, args: Vec<String>) -> std::process::Output {
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(tool)
                .env("MIBS", "")
                .args(&args)
                .output()
        })
        .await
        .expect("joined")
        .unwrap_or_else(|e| panic!("{tool} is on PATH: {e}"))
    }

    /// net-snmp's bulk walk of MIB-2 prints the golden walk, and its SET is
    /// refused notWritable.
    #[tokio::test]
    #[ignore = "needs net-snmp's snmpbulkwalk and snmpset on PATH; run with --ignored"]
    async fn net_snmp_walks_the_agent_and_is_refused_a_set() {
        let agent = agent().await;
        interface_table(&agent);
        let target = format!("127.0.0.1:{}", agent.local_addr().port());
        let args = |rest: &[&str]| {
            ["-v2c", "-c", "public", "-On", "-r", "0", "-t", "2", &target]
                .iter()
                .chain(rest)
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>()
        };

        let walk = net_snmp("snmpbulkwalk", args(&["1.3.6.1.2.1"])).await;
        assert!(
            walk.status.success(),
            "{}",
            String::from_utf8_lossy(&walk.stderr)
        );
        let stdout = String::from_utf8(walk.stdout).expect("UTF-8");
        println!("{stdout}");
        assert_eq!(
            stdout.lines().count(),
            7 + 40,
            "every instance under 1.3.6.1.2.1"
        );
        assert_eq!(mask_timeticks(&stdout), NET_SNMP_BULKWALK);

        let set = net_snmp("snmpset", args(&["1.3.6.1.2.1.1.5.0", "s", "rtr-2"])).await;
        let printed = String::from_utf8_lossy(&set.stderr).to_string()
            + &String::from_utf8_lossy(&set.stdout);
        println!("{printed}");
        assert!(printed.contains("notWritable"), "{printed}");
        let requests = agent.requests();
        assert!(
            requests.get_bulk >= 5 && requests.set_refused == 1,
            "{requests:?}"
        );
    }

    /// `MIBS= snmpwalk -v1 -c public -On 127.0.0.1:<port> 1.3.6.1.2.1`,
    /// net-snmp 5.9.4.pre2, over [`interface_table`] and [`system`]: its
    /// stdout, the TimeTicks masked. [`NET_SNMP_BULKWALK`] without the
    /// Counter64 column.
    const NET_SNMP_V1_WALK: &str = ".1.3.6.1.2.1.1.1.0 = STRING: \"Sonda synthetic router\"\n\
        .1.3.6.1.2.1.1.2.0 = OID: .1.3.6.1.4.1.8072.3.2.10\n\
        .1.3.6.1.2.1.1.3.0 = Timeticks: (*)\n\
        .1.3.6.1.2.1.1.4.0 = STRING: \"noc@example.com\"\n\
        .1.3.6.1.2.1.1.5.0 = STRING: \"rtr-core-01\"\n\
        .1.3.6.1.2.1.1.6.0 = STRING: \"lab-1\"\n\
        .1.3.6.1.2.1.1.7.0 = INTEGER: 72\n\
        .1.3.6.1.2.1.2.2.1.8.1 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.2 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.3 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.5 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.6 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.7 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.8 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.8.9 = INTEGER: 2\n\
        .1.3.6.1.2.1.2.2.1.8.10 = INTEGER: 1\n\
        .1.3.6.1.2.1.2.2.1.14.1 = Counter32: 7\n\
        .1.3.6.1.2.1.2.2.1.14.2 = Counter32: 14\n\
        .1.3.6.1.2.1.2.2.1.14.3 = Counter32: 21\n\
        .1.3.6.1.2.1.2.2.1.14.4 = Counter32: 28\n\
        .1.3.6.1.2.1.2.2.1.14.5 = Counter32: 35\n\
        .1.3.6.1.2.1.2.2.1.14.6 = Counter32: 42\n\
        .1.3.6.1.2.1.2.2.1.14.7 = Counter32: 49\n\
        .1.3.6.1.2.1.2.2.1.14.8 = Counter32: 56\n\
        .1.3.6.1.2.1.2.2.1.14.9 = Counter32: 63\n\
        .1.3.6.1.2.1.2.2.1.14.10 = Counter32: 70\n\
        .1.3.6.1.2.1.31.1.1.1.15.1 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.2 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.3 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.4 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.5 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.6 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.7 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.8 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.9 = Gauge32: 10000\n\
        .1.3.6.1.2.1.31.1.1.1.15.10 = Gauge32: 10000\n";

    /// net-snmp's v1 walk of MIB-2 prints every instance but the Counter64
    /// ones, and its v2c bulk walk of the same agent still prints the v2c
    /// golden walk.
    #[tokio::test]
    #[ignore = "needs net-snmp's snmpwalk and snmpbulkwalk on PATH; run with --ignored"]
    async fn net_snmp_walks_the_agent_as_v1_without_counter64() {
        let agent = agent_serving(&[SnmpVersion::V1, SnmpVersion::V2c]).await;
        interface_table(&agent);
        let target = format!("127.0.0.1:{}", agent.local_addr().port());
        let args = |version: &str, rest: &[&str]| {
            [
                version, "-c", "public", "-On", "-r", "0", "-t", "2", &target,
            ]
            .iter()
            .chain(rest)
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>()
        };

        let walk = net_snmp("snmpwalk", args("-v1", &["1.3.6.1.2.1"])).await;
        assert!(
            walk.status.success(),
            "{}",
            String::from_utf8_lossy(&walk.stderr)
        );
        let stdout = String::from_utf8(walk.stdout).expect("UTF-8");
        println!("{stdout}");
        assert_eq!(
            stdout.lines().count(),
            7 + 30,
            "every instance under 1.3.6.1.2.1 but the ten Counter64"
        );
        assert_eq!(mask_timeticks(&stdout), NET_SNMP_V1_WALK);
        assert!(agent.requests().v1 >= 37, "{:?}", agent.requests());

        let bulk = net_snmp("snmpbulkwalk", args("-v2c", &["1.3.6.1.2.1"])).await;
        assert!(
            bulk.status.success(),
            "{}",
            String::from_utf8_lossy(&bulk.stderr)
        );
        let stdout = String::from_utf8(bulk.stdout).expect("UTF-8");
        assert_eq!(mask_timeticks(&stdout), NET_SNMP_BULKWALK);
    }

    // ---- the hot path -------------------------------------------------------

    /// GETBULK requests per second the poller sends.
    const POLL_RATE: u64 = 2_000;

    /// The scenario's rate, and how long it runs.
    const SCENARIO_RATE: f64 = 100.0;
    const SCENARIO_RUN: Duration = Duration::from_secs(5);

    /// The end of a run in which a tick owed but not yet emitted counts as
    /// late rather than dropped.
    const END_ALLOWANCE: Duration = Duration::from_millis(20);

    /// A sink stamping the time of every write.
    struct Timed {
        inner: Box<dyn crate::sink::Sink>,
        at: Arc<Mutex<Vec<std::time::Instant>>>,
    }

    impl Timed {
        fn stamp(&self) {
            self.at
                .lock()
                .expect("stamps")
                .push(std::time::Instant::now());
        }
    }

    #[async_trait::async_trait]
    impl crate::sink::Sink for Timed {
        async fn write(&mut self, data: &[u8]) -> Result<(), crate::SondaError> {
            self.stamp();
            self.inner.write(data).await
        }

        async fn flush(&mut self) -> Result<(), crate::SondaError> {
            self.inner.flush().await
        }

        fn wants_metric_events(&self) -> bool {
            self.inner.wants_metric_events()
        }

        async fn write_metric_event(
            &mut self,
            event: &crate::model::metric::MetricEvent,
            encoded: &[u8],
        ) -> Result<(), crate::SondaError> {
            self.stamp();
            self.inner.write_metric_event(event, encoded).await
        }
    }

    /// Ticks owed but not emitted, and the 99th percentile of how far the
    /// interval between consecutive writes strayed from the scheduled one,
    /// the first tenth of the run left out as warm-up.
    #[derive(Debug)]
    struct PublisherRun {
        dropped: i64,
        p99_drift: Duration,
    }

    /// Run a [`SCENARIO_RATE`] scenario for [`SCENARIO_RUN`] into `sink`.
    async fn publisher_run(
        sink: Box<dyn crate::sink::Sink>,
        sink_config: crate::sink::SinkConfig,
    ) -> PublisherRun {
        use crate::config::{BaseScheduleConfig, ScenarioConfig};
        use crate::encoder::snmp::{SnmpEncoderConfig, SnmpValueType};

        let at = Arc::new(Mutex::new(Vec::new()));
        let mut sink: Box<dyn crate::sink::Sink> = Box::new(Timed {
            inner: sink,
            at: Arc::clone(&at),
        });
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "ifHCInOctets".to_string(),
                rate: SCENARIO_RATE,
                duration: Some(format!("{}ms", SCENARIO_RUN.as_millis())),
                gaps: None,
                gap_windows: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: Some(HashMap::from([("ifIndex".to_string(), "99".to_string())])),
                sink: sink_config,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Fail,
            },
            generator: crate::generator::GeneratorConfig::Step {
                start: Some(0.0),
                step_size: 1.0,
                max: None,
            },
            encoder: crate::encoder::EncoderConfig::Snmp(SnmpEncoderConfig {
                oids: HashMap::from([(
                    "ifHCInOctets".to_string(),
                    "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
                )]),
                types: HashMap::from([("ifHCInOctets".to_string(), SnmpValueType::Counter64)]),
                ..SnmpEncoderConfig::default()
            }),
            metric_type: None,
            help: None,
        };
        let stats = Arc::new(std::sync::RwLock::new(
            crate::schedule::stats::ScenarioStats::default(),
        ));
        crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");

        let owed = (SCENARIO_RATE * (SCENARIO_RUN - END_ALLOWANCE).as_secs_f64()) as i64;
        let emitted = stats.read().expect("stats").total_events as i64;
        let at = at.lock().expect("stamps");
        assert_eq!(at.len() as i64, emitted, "every tick reached the sink");
        assert!(emitted >= 100, "only {emitted} ticks");
        let scheduled = Duration::from_secs_f64(1.0 / SCENARIO_RATE);
        let mut drift: Vec<Duration> = at[at.len() / 10..]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs_diff(scheduled))
            .collect();
        drift.sort();
        PublisherRun {
            dropped: (owed - emitted).max(0),
            p99_drift: drift[drift.len() * 99 / 100],
        }
    }

    /// Send [`POLL_RATE`] GETBULK requests a second to `addr` until `stop`;
    /// the replies received.
    async fn poll(addr: SocketAddr, stop: CancellationToken) -> u64 {
        let client = client().await;
        let datagram = bulk(0, 10, &[COLUMNS[2]]);
        let per_tick = POLL_RATE / 1_000;
        let mut tick = tokio::time::interval(Duration::from_millis(1));
        let mut buffer = vec![0; RECEIVE_BUFFER];
        let mut replies = 0;
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                received = client.recv_from(&mut buffer) => {
                    if received.is_ok() {
                        replies += 1;
                    }
                }
                _ = tick.tick() => {
                    for _ in 0..per_tick {
                        let _ = client.send_to(&datagram, addr).await;
                    }
                }
            }
        }
        replies
    }

    /// A 100 Hz scenario publishing into an agent polled with 2 000 GETBULK
    /// a second, beside the same scenario into a memory sink: the dropped
    /// ticks and p99 drift of both, printed. A measurement: it asserts that
    /// the poller was answered and the scenarios ran, not a bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "measurement; run with --ignored and report the numbers"]
    async fn polling_the_agent_beside_a_publishing_scenario() {
        use crate::sink::{create_sink, SinkConfig};

        let cfg = SnmpAgentConfig::new("127.0.0.1:0");
        let held = crate::sink::snmp::registry()
            .acquire(&cfg)
            .await
            .expect("bind an ephemeral port");
        interface_table(&held);
        let addr = held.local_addr();
        let agent_config = SinkConfig::SnmpAgent(SnmpAgentConfig::new(addr.to_string()));
        let agent_sink = create_sink(&agent_config, None)
            .await
            .expect("join the agent");
        let memory_config = SinkConfig::Memory {
            capture: false,
            max_events: None,
            capture_handle: None,
        };
        let memory = create_sink(&memory_config, None)
            .await
            .expect("a memory sink");

        let stop = CancellationToken::new();
        let poller = tokio::spawn(poll(addr, stop.clone()));
        let started = std::time::Instant::now();
        let (agent_run, baseline) = tokio::join!(
            publisher_run(agent_sink, agent_config),
            publisher_run(memory, memory_config)
        );
        let elapsed = started.elapsed();
        stop.cancel();
        let replies = poller.await.expect("the poller ends");
        let answered = held.requests().get_bulk;

        eprintln!(
            "GETBULK answered {answered} ({:.0}/s), replies received {replies}; dropped ticks: \
             memory {}, snmp_agent {}; p99 drift: memory {:?}, snmp_agent {:?}",
            answered as f64 / elapsed.as_secs_f64(),
            baseline.dropped,
            agent_run.dropped,
            baseline.p99_drift,
            agent_run.p99_drift
        );
        let asked = POLL_RATE * SCENARIO_RUN.as_secs();
        assert!(
            answered >= asked / 2,
            "{answered} of {asked} GETBULK answered"
        );
        assert!(replies >= asked / 2, "{replies} replies of {asked}");
        assert_eq!(held.requests().send_errors, 0);
    }
}
