//! Answers SNMPv2c requests from an [`SnmpAgent`]'s table.
//!
//! [`decode`] classifies a datagram's PDU, [`answer`] writes the response
//! into a caller's buffer, and [`serve`] is the task that reads the socket
//! and sends each response to the datagram's source.
//!
//! | Received | Answer |
//! |---|---|
//! | `GetRequest` | each varbind its instance's value, `noSuchInstance` or `noSuchObject`; error-status `noError` |
//! | `GetNextRequest` | each varbind the next instance in OID order, or `endOfMibView` past the last |
//! | `GetBulkRequest` | the non-repeaters as `GetNext`, then up to max-repetitions rounds of the repeaters, ending after a round that is all `endOfMibView` |
//! | `SetRequest` | the request's varbinds, error-status `notWritable`, error-index 1; nothing is changed |
//! | any other PDU | dropped |
//! | another community | dropped |
//! | SNMPv1 or SNMPv3 | dropped, the community never compared |
//!
//! Every response fits `max_message_size`. The first varbind that would
//! not fit ends a `GetBulkRequest` response, which is sent with the
//! varbinds before it; it turns a `GetRequest`, `GetNextRequest` or
//! `SetRequest` response into `tooBig` with an empty varbind list. A
//! request with no varbinds is answered with none, `noError`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::ops::Range;
use std::sync::Weak;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::agent::{warn_due, Counter, Found, SnmpAgent};
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

/// The version field of an SNMPv2c message.
const V2C: i64 = 1;

/// The version field of an SNMPv3 message.
const V3: i64 = 3;

/// error-status `tooBig`.
const TOO_BIG: i64 = 1;

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
    /// SNMPv1 or SNMPv3; nothing is sent.
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
/// version but SNMPv1 and SNMPv2c among them; [`BerError::Overflow`] for a
/// request-id outside 32 bits; the error of the first malformed `VarBind`
/// of a request the agent answers.
pub(crate) fn decode(datagram: &[u8]) -> Result<Request<'_>, BerError> {
    let message = Message::decode(datagram)?;
    // `Message::decode` admits versions 0 and 1 only.
    let version = if message.version == V2C {
        Version::V2c
    } else {
        Version::V1
    };
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
/// The version is checked first, then the community, in constant time, then
/// the PDU. Allocates nothing beyond what `out` and `scratch` already hold
/// once both have answered a request of the same shape.
pub(crate) fn answer(
    req: &Request<'_>,
    agent: &SnmpAgent,
    budget: usize,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) -> Outcome {
    out.clear();
    if req.version != Version::V2c {
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
    let reply = Reply {
        community,
        id: i64::from(id),
        budget,
    };
    let (status, index) = match req.pdu {
        Pdu::Set { .. } if !vars.is_empty() => (NOT_WRITABLE, 1),
        _ => (0, 0),
    };
    let mut written = Ok(Written::Complete);
    ber::write_message(
        out,
        V2C,
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
                Pdu::Get { .. } => get(agent, &vars, list, scratch, out),
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
    match written {
        Ok(Written::Complete) => {
            agent.count(counter);
            Outcome::Sent
        }
        Ok(Written::Full) => {
            agent.count(counter);
            too_big(agent, &reply, &vars, out)
        }
        Err(_) => {
            out.clear();
            agent.count(Counter::Malformed);
            Outcome::DroppedOther
        }
    }
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
/// to its source.
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
    loop {
        let received = tokio::select! {
            () = cancel.cancelled() => break,
            received = socket.recv_from(&mut datagram) => received,
        };
        let Some(strong) = agent.upgrade() else {
            break;
        };
        let (len, peer) = match received {
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
        let outcome = respond(
            &datagram[..len],
            &strong,
            budget,
            &mut scratch,
            &mut out,
            |request| {
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
            },
        );
        drop(strong);
        if outcome != Outcome::Sent {
            continue;
        }
        if let Err(e) = socket.send_to(&out, peer).await {
            if let Some(strong) = agent.upgrade() {
                strong.count(Counter::SendErrors);
                tracing::debug!(error = %e, addr = %strong.local_addr(), %peer, "snmp_agent send failed");
            }
        }
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
}

/// The fields of the response that its size depends on.
struct Reply<'a> {
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
        // value here (0, 1 or 17).
        ber::message_len(V2C, r.community, r.id, 0, 0, list) <= r.budget
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
    agent.bulk(&scratch.name, rounds, |_, varbind| {
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

/// Replace the response in `out` with `tooBig` and an empty varbind list,
/// count it, and warn naming the request's first OID; nothing is sent if
/// even that does not fit.
fn too_big(agent: &SnmpAgent, reply: &Reply<'_>, vars: &VarList<'_>, out: &mut Vec<u8>) -> Outcome {
    out.clear();
    agent.count(Counter::TooBig);
    let first = vars.iter().next().map(|var| var.oid);
    agent.warn_too_big(&Dotted(first.unwrap_or_default()), reply.budget);
    if ber::message_len(V2C, reply.community, reply.id, TOO_BIG, 0, 0) > reply.budget {
        return Outcome::DroppedOther;
    }
    ber::write_message(
        out,
        V2C,
        reply.community,
        Tag::Response,
        reply.id,
        TOO_BIG,
        0,
        |_| {},
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
    agent.bulk(arcs, 1, |_, varbind| {
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
        let cfg = SnmpAgentConfig {
            system: system(),
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
        let mut out = Vec::new();
        ber::write_message(
            &mut out,
            V2C,
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
        let message = Message::decode(datagram).expect("the reply is a v2c message");
        assert_eq!(message.version, V2C);
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
        assert_eq!(
            counting::count_allocations(|| std::hint::black_box(Box::new(1u8))).1,
            1,
            "the lib's counting allocator is installed"
        );
        let ((outcome, len), allocations) = counting::count_allocations(|| {
            let outcome = answer(&request, &agent, 1472, &mut scratch, &mut out);
            (outcome, out.len())
        });
        assert_eq!(outcome, Outcome::Sent);
        assert_eq!(response(&out).varbinds.len(), 10, "{len} bytes");
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
