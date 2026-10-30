//! SNMP trap sink: sends linkDown, linkUp and coldStart notifications to a
//! trap receiver as SNMPv2c traps or INFORMs.
//!
//! An [`SnmpTrapSink`] reads the `snmp` encoder's publish records and watches
//! the IF-MIB `ifOperStatus` column (`1.3.6.1.2.1.2.2.1.8.<ifIndex>`): an
//! instance whose value changes from up (1) to anything else sends linkDown,
//! one that changes back to up sends linkUp. `ifAdminStatus`
//! (`1.3.6.1.2.1.2.2.1.7.<ifIndex>`) is remembered for the link traps'
//! varbinds. The sink sends coldStart once, when it is created.
//!
//! A trap is sent once and waits for nothing. An INFORM waits for the
//! receiver's `Response` and is sent again each `timeout` until one arrives
//! or `retries` more sends have gone unanswered, so a send stalls the
//! scenario for `timeout × (retries + 1)` at most.
//!
//! The sinks of one launch that send to one destination are one device: the
//! first of them to be created sends coldStart and the others do not, as a
//! pack entry's metrics, each an entry with a sink of its own, describe one
//! interface.
//!
//! Requires the `snmp` feature flag.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;
use std::io;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::UdpSocket;
use tokio::time::Instant;

use super::agent::warn_due;
use crate::config::validate::parse_duration;
use crate::encoder::snmp::ber::{self, Message, Reader, Tag};
use crate::encoder::snmp::oid::{decode_ber_body, Oid};
use crate::encoder::snmp::split_record;
use crate::model::metric::MetricEvent;
use crate::schedule::LaunchId;
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

/// `timeout` when it is not set.
pub(crate) const DEFAULT_TIMEOUT: &str = "2s";

/// `retries` when it is not set.
pub(crate) const DEFAULT_RETRIES: u8 = 3;

/// The shortest accepted `timeout`.
pub(crate) const MIN_TIMEOUT: Duration = Duration::from_millis(100);

/// The longest accepted `timeout`.
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(60);

/// The most `retries` accepted.
pub(crate) const MAX_RETRIES: u8 = 10;

/// `sysUpTime.0` (SNMPv2-MIB), the first varbind of every notification.
const SYS_UP_TIME_INSTANCE: [u32; 9] = [1, 3, 6, 1, 2, 1, 1, 3, 0];

/// `snmpTrapOID.0` (SNMPv2-MIB), the second varbind of every notification.
const SNMP_TRAP_OID_INSTANCE: [u32; 11] = [1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0];

/// `snmpTraps` (SNMPv2-MIB): coldStart is `.1`, linkDown `.3`, linkUp `.4`.
const SNMP_TRAPS: [u32; 9] = [1, 3, 6, 1, 6, 3, 1, 1, 5];

/// `ifEntry` (IF-MIB): the columns below are its arcs.
const IF_ENTRY: [u32; 9] = [1, 3, 6, 1, 2, 1, 2, 2, 1];

/// `ifIndex`, a column of [`IF_ENTRY`].
const IF_INDEX: u32 = 1;

/// `ifAdminStatus`, a column of [`IF_ENTRY`].
const IF_ADMIN_STATUS: u32 = 7;

/// `ifOperStatus`, a column of [`IF_ENTRY`].
const IF_OPER_STATUS: u32 = 8;

/// `ifOperStatus` and `ifAdminStatus` up(1).
const UP: i32 = 1;

/// The largest datagram an INFORM's acknowledgement is read into; a
/// `Response` echoing a link trap's five varbinds takes about 130 octets.
const RECEIVE_BUFFER: usize = 4096;

/// Configuration for [`SnmpTrapSink`]; the body of
/// `sink: { type: snmp_trap, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `retires:` is an
/// error rather than a silent fallback to the defaults.
///
/// `Debug` prints `"***"` in place of the community.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SnmpTrapConfig {
    /// The trap receiver, `"host:port"` or a socket address such as
    /// `"127.0.0.1:162"`. A host name is resolved once, when the sink is
    /// created.
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub destination: String,
    /// The SNMPv2c community every notification carries. Default
    /// `"public"`.
    #[cfg_attr(
        feature = "config",
        serde(
            default = "super::default_community",
            deserialize_with = "crate::config::scalar_text::required"
        )
    )]
    pub community: String,
    /// Send each notification as an INFORM, which the receiver
    /// acknowledges, instead of a trap. Default `false`.
    #[cfg_attr(feature = "config", serde(default))]
    pub inform: bool,
    /// How long an INFORM waits for its acknowledgement before it is sent
    /// again, from `"100ms"` to `"60s"`. Default `"2s"`. Read only with
    /// `inform: true`.
    #[cfg_attr(
        feature = "config",
        serde(
            default = "default_timeout",
            deserialize_with = "crate::config::scalar_text::required"
        )
    )]
    pub timeout: String,
    /// How many times an unacknowledged INFORM is sent again, 0 to 10.
    /// Default 3. Read only with `inform: true`.
    #[cfg_attr(feature = "config", serde(default = "default_retries"))]
    #[cfg_attr(feature = "schema", schemars(range(max = MAX_RETRIES)))]
    pub retries: u8,
}

#[cfg(feature = "config")]
fn default_timeout() -> String {
    DEFAULT_TIMEOUT.to_string()
}

#[cfg(feature = "config")]
fn default_retries() -> u8 {
    DEFAULT_RETRIES
}

/// Prints `"***"` in place of the community.
impl fmt::Debug for SnmpTrapConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnmpTrapConfig")
            .field("destination", &self.destination)
            .field("community", &"***")
            .field("inform", &self.inform)
            .field("timeout", &self.timeout)
            .field("retries", &self.retries)
            .finish()
    }
}

impl SnmpTrapConfig {
    /// A config sending traps to `destination` with every other field at
    /// its default.
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            community: super::DEFAULT_COMMUNITY.to_string(),
            inform: false,
            timeout: DEFAULT_TIMEOUT.to_string(),
            retries: DEFAULT_RETRIES,
        }
    }

    /// Resolve the config into the settings a sink runs with.
    ///
    /// This is the one place the fields are checked; validation and
    /// [`SnmpTrapSink::new`] both call it. Returns [`SondaError::Config`]
    /// when `destination` is neither a socket address nor `host:port` with
    /// a port from 1 to 65535, `timeout` is not a duration from 100ms to
    /// 60s, or `retries` is above 10. Resolving the host is left to the
    /// sink.
    pub(crate) fn settings(&self) -> Result<TrapSettings, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        if !is_destination(&self.destination) {
            return Err(invalid(format!(
                "snmp_trap destination {:?} is not \"host:port\" or a socket address such as \
                 \"127.0.0.1:162\", with a port from 1 to 65535",
                self.destination
            )));
        }
        let timeout = parse_duration(&self.timeout)
            .map_err(|e| invalid(format!("snmp_trap timeout {:?}: {e}", self.timeout)))?;
        if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&timeout) {
            return Err(invalid(format!(
                "snmp_trap timeout must be between 100ms and 60s, got {:?}",
                self.timeout
            )));
        }
        if self.retries > MAX_RETRIES {
            return Err(invalid(format!(
                "snmp_trap retries must be between 0 and {MAX_RETRIES}, got {}",
                self.retries
            )));
        }
        Ok(TrapSettings {
            destination: self.destination.clone(),
            community: self.community.clone().into_bytes(),
            inform: self.inform,
            timeout,
            retries: self.retries,
        })
    }
}

/// Whether `text` is a socket address, or `host:port` with a host that is
/// not empty and holds no `:` and a port from 1 to 65535.
fn is_destination(text: &str) -> bool {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return addr.port() != 0;
    }
    let Some((host, port)) = text.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && !host.contains([':', '[', ']'])
        && port.parse::<u16>().is_ok_and(|port| port != 0)
}

/// An [`SnmpTrapConfig`] with every field checked.
///
/// `Debug` prints `"***"` in place of the community.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TrapSettings {
    /// `host:port` or a socket address, not yet resolved.
    pub(crate) destination: String,
    pub(crate) community: Vec<u8>,
    pub(crate) inform: bool,
    pub(crate) timeout: Duration,
    pub(crate) retries: u8,
}

impl fmt::Debug for TrapSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrapSettings")
            .field("destination", &self.destination)
            .field("community", &"***")
            .field("inform", &self.inform)
            .field("timeout", &self.timeout)
            .field("retries", &self.retries)
            .finish()
    }
}

/// The notification a [`write_trap`] message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrapKind {
    /// `coldStart`, `1.3.6.1.6.3.1.1.5.1`.
    ColdStart,
    /// `linkDown`, `1.3.6.1.6.3.1.1.5.3`.
    LinkDown,
    /// `linkUp`, `1.3.6.1.6.3.1.1.5.4`.
    LinkUp,
}

impl TrapKind {
    /// The last arc of the notification's OID under `snmpTraps`.
    const fn arc(self) -> u32 {
        match self {
            TrapKind::ColdStart => 1,
            TrapKind::LinkDown => 3,
            TrapKind::LinkUp => 4,
        }
    }
}

impl fmt::Display for TrapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrapKind::ColdStart => "coldStart",
            TrapKind::LinkDown => "linkDown",
            TrapKind::LinkUp => "linkUp",
        })
    }
}

/// The interface a linkDown or linkUp is about: its `ifIndex`, and the
/// `ifAdminStatus` and `ifOperStatus` it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkVars {
    /// The interface's `ifIndex`, at most 2³¹−1.
    pub(crate) if_index: u32,
    pub(crate) admin: i32,
    pub(crate) oper: i32,
}

/// Write an SNMPv2c notification: version 1, `community`, an `SNMPv2-Trap`
/// PDU (0xA7), or an `InformRequest` (0xA6) with `inform`, carrying
/// `request_id`, error-status and error-index 0, and the varbinds of RFC
/// 3416 §4.2.6 in order: `sysUpTime.0` (TimeTicks `uptime`), `snmpTrapOID.0`
/// (the OID of `kind`), then, with `link`, `ifIndex.N`, `ifAdminStatus.N`
/// and `ifOperStatus.N` as INTEGERs, the objects RFC 2863 names for
/// linkDown and linkUp. An `if_index` above 2³¹−1, which is no ifIndex, is
/// written as 2³¹−1.
pub(crate) fn write_trap(
    out: &mut Vec<u8>,
    community: &[u8],
    inform: bool,
    request_id: i32,
    uptime: u32,
    kind: TrapKind,
    link: Option<LinkVars>,
) {
    let pdu = if inform {
        Tag::InformRequest
    } else {
        Tag::TrapV2
    };
    ber::write_message(out, 1, community, pdu, i64::from(request_id), 0, 0, |out| {
        write_varbind(out, &SYS_UP_TIME_INSTANCE, &[], |out| {
            ber::write_timeticks(out, uptime)
        });
        write_varbind(out, &SNMP_TRAP_OID_INSTANCE, &[], |out| {
            write_oid(out, &SNMP_TRAPS, &[kind.arc()])
        });
        if let Some(link) = link {
            let index = i32::try_from(link.if_index).unwrap_or(i32::MAX);
            let columns = [
                (IF_INDEX, index),
                (IF_ADMIN_STATUS, link.admin),
                (IF_OPER_STATUS, link.oper),
            ];
            for (column, value) in columns {
                write_varbind(out, &IF_ENTRY, &[column, link.if_index], |out| {
                    ber::write_integer(out, value)
                });
            }
        }
    });
}

/// Write a VarBind named `prefix` followed by `suffix`, whose value `value`
/// appends.
fn write_varbind(
    out: &mut Vec<u8>,
    prefix: &[u32],
    suffix: &[u32],
    value: impl FnOnce(&mut Vec<u8>),
) {
    ber::write_tlv(out, Tag::Sequence, |out| {
        write_oid(out, prefix, suffix);
        value(out);
    });
}

/// Write the OBJECT IDENTIFIER `prefix` followed by `suffix`.
fn write_oid(out: &mut Vec<u8>, prefix: &[u32], suffix: &[u32]) {
    // Every name here starts 1.3, which `from_arcs` always takes.
    if let Some(oid) = Oid::from_arcs([prefix, suffix].concat()) {
        ber::write_oid(out, &oid);
    }
}

/// A column of the interface table the sink watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    AdminStatus,
    OperStatus,
}

/// What the sink knows of one interface: the last `ifOperStatus` and
/// `ifAdminStatus` it read.
#[derive(Debug, Default, Clone, Copy)]
struct Link {
    oper: Option<i32>,
    admin: Option<i32>,
}

/// Request-ids: a counter from a random start, kept within 0 to 2³¹−1.
#[derive(Debug)]
struct RequestIds(u32);

impl RequestIds {
    fn random() -> Self {
        Self(std::collections::hash_map::RandomState::new().hash_one(std::process::id()) as u32)
    }

    fn next(&mut self) -> i32 {
        let id = self.0 & 0x7FFF_FFFF;
        self.0 = self.0.wrapping_add(1);
        // `id` is at most 2³¹−1.
        i32::try_from(id).unwrap_or(0)
    }
}

/// The live sinks of each launch on each destination, by launch id and
/// destination, for every sink in the process.
fn devices() -> &'static Mutex<HashMap<(u64, SocketAddr), usize>> {
    static DEVICES: OnceLock<Mutex<HashMap<(u64, SocketAddr), usize>>> = OnceLock::new();
    DEVICES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Count one more live sink of `launch` on `destination`; `true` when it is
/// the only one, which then sends coldStart.
fn join_device(launch: u64, destination: SocketAddr) -> bool {
    let mut devices = devices().lock().unwrap_or_else(PoisonError::into_inner);
    let sinks = devices.entry((launch, destination)).or_insert(0);
    *sinks += 1;
    *sinks == 1
}

/// Count one live sink of `launch` on `destination` fewer, forgetting the
/// pair at none.
fn leave_device(launch: u64, destination: SocketAddr) {
    let mut devices = devices().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(sinks) = devices.get_mut(&(launch, destination)) {
        *sinks -= 1;
        if *sinks == 0 {
            devices.remove(&(launch, destination));
        }
    }
}

/// When each destination was last warned about an unacknowledged INFORM,
/// for every sink in the process.
fn unacknowledged_warnings() -> &'static Mutex<HashMap<SocketAddr, Instant>> {
    static WARNED: OnceLock<Mutex<HashMap<SocketAddr, Instant>>> = OnceLock::new();
    WARNED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether an unacknowledged INFORM to `destination` may be warned about
/// now, recording it if so: once per [`WARN_INTERVAL`](super::agent::WARN_INTERVAL)
/// per destination.
fn warn_unacknowledged_due(destination: SocketAddr) -> bool {
    let now = Instant::now();
    let mut warned = unacknowledged_warnings()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if !warn_due(warned.get(&destination).copied(), now) {
        return false;
    }
    warned.insert(destination, now);
    true
}

/// Sends SNMPv2c notifications for the interface transitions in the `snmp`
/// encoder's publish records.
///
/// Only [`Sink::write_metric_event`] carries data: [`Sink::write`] is an
/// error, as it is for the `snmp_agent` sink. Each write is one publish
/// record ([`split_record`]); the sink reads the record's `VarBind` and each
/// companion's, and every other byte of it is ignored:
///
/// - an INTEGER at `ifOperStatus.N` (`1.3.6.1.2.1.2.2.1.8.N`, with `N` from
///   0 to 2³¹−1) is kept as interface `N`'s state. The first value an
///   interface has sends nothing; a change from up (1) to any other value
///   sends linkDown, a change from any other value to up sends linkUp, and
///   any other change sends nothing;
/// - an INTEGER at `ifAdminStatus.N` is kept for interface `N`'s next link
///   trap, which carries up (1) until one has been read;
/// - anything else is ignored.
///
/// The interfaces it keeps are the ones its records name, never pruned.
/// coldStart is sent when the sink is created, by the first sink of its
/// launch on its destination: while that launch has a live sink on the
/// destination, no other of its sinks sends one. Every notification's
/// `sysUpTime` is the hundredths of a second since the sink was created.
///
/// With `inform`, a send waits up to `timeout` for a `Response` carrying the
/// request-id it sent; any other datagram on the socket is ignored and
/// counted. When none arrives, the same message is sent again, `retries`
/// times at most, then the send fails: that write returns
/// [`SondaError::Sink`], and the failure is logged at warn at most once a
/// minute per destination. coldStart failing so is returned by the first
/// write instead. Without `inform`, a send waits for nothing, and a socket
/// error fails the write.
pub struct SnmpTrapSink {
    socket: UdpSocket,
    destination: SocketAddr,
    launch: u64,
    settings: TrapSettings,
    created: Instant,
    request_ids: RequestIds,
    /// Each interface's state, by ifIndex.
    links: HashMap<u32, Link>,
    /// The message being sent.
    out: Vec<u8>,
    /// Where an INFORM's acknowledgement is read; empty without `inform`.
    received: Vec<u8>,
    /// Scratch for a record's OID arcs.
    arcs: Vec<u32>,
    /// Datagrams read while waiting that were not the acknowledgement.
    ignored: u64,
    /// coldStart's failure, returned by the first write.
    pending: Option<SondaError>,
}

impl SnmpTrapSink {
    /// Resolve `cfg.destination`, bind an ephemeral UDP port of its family,
    /// and send coldStart, waiting for its acknowledgement with `inform`, as
    /// a launch of its own.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config; [`SondaError::Sink`]
    /// when the destination resolves to no address or the socket cannot be
    /// bound. A coldStart that is not sent or not acknowledged does not fail
    /// creation: the first write returns it.
    pub async fn new(cfg: &SnmpTrapConfig) -> Result<Self, SondaError> {
        Self::new_in(cfg, LaunchId::next()).await
    }

    /// [`new`](Self::new) for a scenario of `launch`: coldStart is sent only
    /// when no other sink of `launch` on the destination is live.
    pub(crate) async fn new_in(cfg: &SnmpTrapConfig, launch: LaunchId) -> Result<Self, SondaError> {
        let settings = cfg.settings()?;
        let destination = resolve(&settings.destination).await?;
        let bind: SocketAddr = if destination.is_ipv6() {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let socket = UdpSocket::bind(bind).await.map_err(|e| {
            SondaError::Sink(io::Error::new(
                e.kind(),
                format!("snmp_trap could not bind a UDP port to send to {destination}: {e}"),
            ))
        })?;
        let received = if settings.inform {
            vec![0; RECEIVE_BUFFER]
        } else {
            Vec::new()
        };
        let launch = launch.get();
        let cold_start = join_device(launch, destination);
        let mut sink = Self {
            socket,
            destination,
            launch,
            settings,
            created: Instant::now(),
            request_ids: RequestIds::random(),
            links: HashMap::new(),
            out: Vec::new(),
            received,
            arcs: Vec::new(),
            ignored: 0,
            pending: None,
        };
        if cold_start {
            if let Err(e) = sink.send(TrapKind::ColdStart, None).await {
                sink.pending = Some(e);
            }
        }
        Ok(sink)
    }

    /// Datagrams read while waiting for an acknowledgement that were not
    /// it.
    #[cfg(test)]
    pub(crate) fn ignored(&self) -> u64 {
        self.ignored
    }

    /// Hundredths of a second since the sink was created, wrapping.
    fn uptime_ticks(&self) -> u32 {
        let centiseconds = self.created.elapsed().as_millis() / 10;
        // TimeTicks wrap; keep the low 32 bits.
        (centiseconds % (u128::from(u32::MAX) + 1)) as u32
    }

    /// Read one `VarBind` of a record: keep an `ifAdminStatus` or
    /// `ifOperStatus` INTEGER, and send the link trap a change of
    /// `ifOperStatus` calls for.
    async fn watch(&mut self, varbind: &[u8]) -> Result<(), SondaError> {
        let Some((column, if_index, value)) = self.link_column(varbind)? else {
            return Ok(());
        };
        let link = self.links.entry(if_index).or_default();
        let kind = match column {
            Column::AdminStatus => {
                link.admin = Some(value);
                return Ok(());
            }
            Column::OperStatus => match link.oper.replace(value) {
                Some(UP) if value != UP => TrapKind::LinkDown,
                Some(before) if before != UP && value == UP => TrapKind::LinkUp,
                _ => return Ok(()),
            },
        };
        let link = LinkVars {
            if_index,
            admin: link.admin.unwrap_or(UP),
            oper: value,
        };
        self.send(kind, Some(link)).await
    }

    /// The column, ifIndex and INTEGER value of `varbind` when it is an
    /// `ifAdminStatus` or `ifOperStatus` instance with an ifIndex from 0 to
    /// 2³¹−1 and an INTEGER value; `None` for any other `VarBind`.
    fn link_column(&mut self, varbind: &[u8]) -> Result<Option<(Column, u32, i32)>, SondaError> {
        let body = Reader::new(varbind)
            .expect(Tag::Sequence)
            .map_err(not_a_record)?;
        let mut fields = Reader::new(body);
        let name = fields.expect(Tag::Oid).map_err(not_a_record)?;
        decode_ber_body(name, &mut self.arcs).ok_or_else(|| not_a_record("a malformed OID"))?;
        let column = match self.arcs.as_slice() {
            [entry @ .., column, _] if entry == IF_ENTRY.as_slice() => match *column {
                IF_ADMIN_STATUS => Column::AdminStatus,
                IF_OPER_STATUS => Column::OperStatus,
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        let if_index = self.arcs[IF_ENTRY.len() + 1];
        if i32::try_from(if_index).is_err() {
            return Ok(None);
        }
        let Ok(value) = fields.i64(Tag::Integer) else {
            return Ok(None);
        };
        Ok(i32::try_from(value)
            .ok()
            .map(|value| (column, if_index, value)))
    }

    /// Send one notification: once as a trap; as an INFORM, until a
    /// `Response` carrying its request-id arrives, `retries + 1` times at
    /// most, `timeout` apart.
    async fn send(&mut self, kind: TrapKind, link: Option<LinkVars>) -> Result<(), SondaError> {
        let request_id = self.request_ids.next();
        let uptime = self.uptime_ticks();
        let TrapSettings {
            inform,
            timeout,
            retries,
            ..
        } = self.settings;
        self.out.clear();
        write_trap(
            &mut self.out,
            &self.settings.community,
            inform,
            request_id,
            uptime,
            kind,
            link,
        );
        if !inform {
            return match self.socket.send_to(&self.out, self.destination).await {
                Ok(_) => Ok(()),
                Err(e) => Err(SondaError::Sink(io::Error::new(
                    e.kind(),
                    format!(
                        "snmp_trap could not send {kind} to {}: {e}",
                        self.destination
                    ),
                ))),
            };
        }
        let mut send_error = None;
        for _ in 0..=retries {
            if let Err(e) = self.socket.send_to(&self.out, self.destination).await {
                send_error = Some(e);
            }
            if self
                .acknowledged(request_id, Instant::now() + timeout)
                .await
            {
                return Ok(());
            }
        }
        let sends = u32::from(retries) + 1;
        if warn_unacknowledged_due(self.destination) {
            tracing::warn!(
                destination = %self.destination,
                notification = %kind,
                sends,
                "snmp_trap sent a {kind} INFORM to {} {sends} times, {timeout:?} apart, and none \
                 was acknowledged",
                self.destination
            );
        }
        let cause = send_error.map_or(String::new(), |e| format!(" (a send failed: {e})"));
        Err(SondaError::Sink(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "snmp_trap: {} acknowledged none of {sends} sends of a {kind} INFORM, {timeout:?} \
                 apart{cause}",
                self.destination
            ),
        )))
    }

    /// Read datagrams until one is a `Response` carrying `request_id`
    /// (`true`), or until `deadline` (`false`). Every other datagram is
    /// ignored and counted.
    async fn acknowledged(&mut self, request_id: i32, deadline: Instant) -> bool {
        loop {
            let received =
                tokio::time::timeout_at(deadline, self.socket.recv_from(&mut self.received)).await;
            match received {
                Err(_elapsed) => return false,
                Ok(Ok((len, _peer))) => {
                    if is_acknowledgement(&self.received[..len], request_id) {
                        return true;
                    }
                    self.ignored += 1;
                }
                // A receive error (an ICMP report on some platforms) ends
                // this send's wait without an acknowledgement.
                Ok(Err(_)) => {
                    tokio::time::sleep_until(deadline).await;
                    return false;
                }
            }
        }
    }
}

/// Whether `datagram` is an SNMPv2c `Response` carrying `request_id`.
fn is_acknowledgement(datagram: &[u8], request_id: i32) -> bool {
    Message::decode(datagram).is_ok_and(|message| {
        message.version == 1
            && message.pdu == Tag::Response
            && message.request_id == i64::from(request_id)
    })
}

/// Resolve `destination` (`host:port` or a socket address) to its first
/// address.
async fn resolve(destination: &str) -> Result<SocketAddr, SondaError> {
    let mut addrs = tokio::net::lookup_host(destination).await.map_err(|e| {
        SondaError::Sink(io::Error::new(
            e.kind(),
            format!("snmp_trap destination {destination:?} does not resolve: {e}"),
        ))
    })?;
    addrs.next().ok_or_else(|| {
        SondaError::Sink(io::Error::new(
            io::ErrorKind::NotFound,
            format!("snmp_trap destination {destination:?} resolves to no address"),
        ))
    })
}

/// [`SondaError::Sink`]: `encoded` is not a publish record, for `detail`.
fn not_a_record(detail: impl fmt::Display) -> SondaError {
    SondaError::Sink(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the event is not an snmp publish record ({detail}); the snmp_trap sink takes the \
             snmp encoder's output"
        ),
    ))
}

impl Drop for SnmpTrapSink {
    /// Count this sink out of its launch's sinks on its destination.
    fn drop(&mut self) {
        leave_device(self.launch, self.destination);
    }
}

#[async_trait]
impl Sink for SnmpTrapSink {
    /// Always an error: this sink reads the `snmp` encoder's publish
    /// records, which only [`Sink::write_metric_event`] carries.
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        Err(SondaError::Sink(io::Error::other(
            "the snmp_trap sink takes metric events from the snmp encoder, not raw bytes; \
             pair it with `encoder: { type: snmp }` on a metrics entry",
        )))
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }

    fn wants_metric_events(&self) -> bool {
        true
    }

    /// Read the record `encoded`, its `VarBind` and then each companion's,
    /// sending the link traps their `ifOperStatus` changes call for.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when `encoded` is not a publish record, when a
    /// notification it calls for is not sent or, as an INFORM, not
    /// acknowledged, and, on the first write, when coldStart was not.
    async fn write_metric_event(
        &mut self,
        _event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        let record = split_record(encoded)?;
        self.watch(record.varbind).await?;
        for companion in record.companions {
            self.watch(companion.varbind).await?;
        }
        match self.pending.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use super::super::tests::hex;
    use super::*;
    use crate::config::{BaseScheduleConfig, ScenarioConfig};
    use crate::encoder::snmp::ber::{decode_i64, decode_u64};
    use crate::encoder::snmp::{LabelColumn, SnmpEncoder, SnmpEncoderConfig, SnmpValueType};
    use crate::encoder::{Encoder, EncoderConfig};
    use crate::generator::GeneratorConfig;
    use crate::model::metric::Labels;
    use crate::schedule::stats::ScenarioStats;
    use crate::sink::SinkConfig;
    use crate::OnSinkError;

    /// `snmptrap -v2c -c public 127.0.0.1:<port> 12345 1.3.6.1.6.3.1.1.5.1`,
    /// net-snmp 5.9.4.pre2 (with `MIBS=`): coldStart.
    const NET_SNMP_COLD_START: &str =
        "304402010104067075626c6963a73702047fac5b0c0201000201003029300e06082b060102010103\
        00430230393017060a2b06010603010104010006092b0601060301010501";

    /// `snmptrap -v2c -c public 127.0.0.1:<port> 12345 1.3.6.1.6.3.1.1.5.3
    /// 1.3.6.1.2.1.2.2.1.1.4 i 4 1.3.6.1.2.1.2.2.1.7.4 i 1
    /// 1.3.6.1.2.1.2.2.1.8.4 i 2`, net-snmp 5.9.4.pre2 (with `MIBS=`):
    /// linkDown of interface 4.
    const NET_SNMP_LINK_DOWN: &str =
        "307702010104067075626c6963a76a02047539e877020100020100305c300e06082b060102010103\
        00430230393017060a2b06010603010104010006092b0601060301010503300f060a2b0601020102\
        02010104020104300f060a2b060102010202010704020101300f060a2b0601020102020108040201\
        02";

    /// `snmptrap -v2c -c public 127.0.0.1:<port> 12345 1.3.6.1.6.3.1.1.5.4
    /// 1.3.6.1.2.1.2.2.1.1.4 i 4 1.3.6.1.2.1.2.2.1.7.4 i 1
    /// 1.3.6.1.2.1.2.2.1.8.4 i 1`, net-snmp 5.9.4.pre2 (with `MIBS=`):
    /// linkUp of interface 4.
    const NET_SNMP_LINK_UP: &str =
        "307702010104067075626c6963a76a02046ee36416020100020100305c300e06082b060102010103\
        00430230393017060a2b06010603010104010006092b0601060301010504300f060a2b0601020102\
        02010104020104300f060a2b060102010202010704020101300f060a2b0601020102020108040201\
        01";

    /// `snmpinform -v2c -c public -r0 -t1 127.0.0.1:<port> 12345
    /// 1.3.6.1.6.3.1.1.5.3 1.3.6.1.2.1.2.2.1.1.4 i 4 1.3.6.1.2.1.2.2.1.7.4 i 1
    /// 1.3.6.1.2.1.2.2.1.8.4 i 2`, net-snmp 5.9.4.pre2 (with `MIBS=`): the
    /// linkDown above as an INFORM.
    const NET_SNMP_INFORM_LINK_DOWN: &str =
        "307702010104067075626c6963a66a020408d35562020100020100305c300e06082b060102010103\
        00430230393017060a2b06010603010104010006092b0601060301010503300f060a2b0601020102\
        02010104020104300f060a2b060102010202010704020101300f060a2b0601020102020108040201\
        02";

    /// The sysUpTime every capture above was sent with.
    const CAPTURED_UPTIME: u32 = 12345;

    /// The interface whose linkDown [`finish`] sends to bound what a test
    /// reads.
    const SENTINEL: &str = "99";

    fn oid(text: &str) -> Oid {
        Oid::parse(text).expect("a valid OID")
    }

    fn link(if_index: u32, admin: i32, oper: i32) -> Option<LinkVars> {
        Some(LinkVars {
            if_index,
            admin,
            oper,
        })
    }

    // ---- the message --------------------------------------------------------

    /// What a notification carries, read back from its datagram.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Sent {
        inform: bool,
        kind: TrapKind,
        link: Option<LinkVars>,
    }

    impl Sent {
        fn trap(kind: TrapKind, link: Option<LinkVars>) -> Self {
            Self {
                inform: false,
                kind,
                link,
            }
        }

        fn inform(kind: TrapKind, link: Option<LinkVars>) -> Self {
            Self {
                inform: true,
                kind,
                link,
            }
        }
    }

    /// The notification `datagram` holds, its request-id and its sysUpTime,
    /// decoded with the BER reader and checked against RFC 3416 §4.2.6:
    /// sysUpTime.0 a TimeTicks, then snmpTrapOID.0 an OBJECT IDENTIFIER
    /// under snmpTraps, then nothing or ifIndex.N, ifAdminStatus.N and
    /// ifOperStatus.N as INTEGERs, in that order.
    fn decode(datagram: &[u8]) -> (Sent, i64, u32) {
        let message = Message::decode(datagram).expect("an SNMPv1/v2c message");
        assert_eq!(message.version, 1, "SNMPv2c");
        assert_eq!(message.community, b"public");
        assert_eq!((message.error_status, message.error_index), (0, 0));
        let inform = match message.pdu {
            Tag::TrapV2 => false,
            Tag::InformRequest => true,
            other => panic!("not a notification: {other:?}"),
        };
        let varbinds: Vec<_> = message
            .varbinds()
            .collect::<Result<_, _>>()
            .expect("well-formed varbinds");
        assert!(
            varbinds.len() == 2 || varbinds.len() == 5,
            "{} varbinds",
            varbinds.len()
        );
        let name = |i: usize| varbinds[i].name().expect("an OID");
        assert_eq!(name(0), oid("1.3.6.1.2.1.1.3.0"), "sysUpTime.0 first");
        assert_eq!(varbinds[0].value_tag, Tag::TimeTicks.byte());
        let uptime =
            u32::try_from(decode_u64(varbinds[0].value).expect("TimeTicks")).expect("32 bits");
        assert_eq!(
            name(1),
            oid("1.3.6.1.6.3.1.1.4.1.0"),
            "snmpTrapOID.0 second"
        );
        assert_eq!(varbinds[1].value_tag, Tag::Oid.byte());
        let trap = Oid::from_ber_body(varbinds[1].value).expect("an OID value");
        let kind = match trap.arcs() {
            [1, 3, 6, 1, 6, 3, 1, 1, 5, 1] => TrapKind::ColdStart,
            [1, 3, 6, 1, 6, 3, 1, 1, 5, 3] => TrapKind::LinkDown,
            [1, 3, 6, 1, 6, 3, 1, 1, 5, 4] => TrapKind::LinkUp,
            _ => panic!("not a notification this sink sends: {trap}"),
        };
        let link = (varbinds.len() == 5).then(|| {
            let integer = |i: usize, column: u32| {
                let arcs = name(i);
                assert_eq!(arcs.arcs()[..10], [1, 3, 6, 1, 2, 1, 2, 2, 1, column]);
                assert_eq!(varbinds[i].value_tag, Tag::Integer.byte());
                let value = decode_i64(varbinds[i].value).expect("an INTEGER");
                (arcs.arcs()[10], i32::try_from(value).expect("32 bits"))
            };
            let (index, if_index) = integer(2, 1);
            let (admin_index, admin) = integer(3, 7);
            let (oper_index, oper) = integer(4, 8);
            assert_eq!(index, u32::try_from(if_index).expect("an ifIndex"));
            assert_eq!((admin_index, oper_index), (index, index));
            LinkVars {
                if_index: index,
                admin,
                oper,
            }
        });
        assert_eq!(link.is_some(), kind != TrapKind::ColdStart, "{kind}");
        (Sent { inform, kind, link }, message.request_id, uptime)
    }

    /// Each capture re-encodes byte for byte, given its own sysUpTime and
    /// request-id: the version, the community, the PDU tag, every varbind's
    /// OID, type and order, and every length.
    #[rstest]
    #[case::cold_start(NET_SNMP_COLD_START, Sent::trap(TrapKind::ColdStart, None))]
    #[case::link_down(NET_SNMP_LINK_DOWN, Sent::trap(TrapKind::LinkDown, link(4, 1, 2)))]
    #[case::link_up(NET_SNMP_LINK_UP, Sent::trap(TrapKind::LinkUp, link(4, 1, 1)))]
    #[case::inform_link_down(
        NET_SNMP_INFORM_LINK_DOWN,
        Sent::inform(TrapKind::LinkDown, link(4, 1, 2))
    )]
    fn write_trap_matches_net_snmp_byte_for_byte(#[case] captured: &str, #[case] sent: Sent) {
        let captured = hex(captured);
        let (read, request_id, uptime) = decode(&captured);
        assert_eq!(
            read, sent,
            "guard: the capture is the notification its case names"
        );
        assert_eq!(uptime, CAPTURED_UPTIME, "guard: the capture's sysUpTime");
        let request_id = i32::try_from(request_id).expect("net-snmp's request-ids are 31-bit");
        let mut out = Vec::new();
        write_trap(
            &mut out,
            b"public",
            sent.inform,
            request_id,
            CAPTURED_UPTIME,
            sent.kind,
            sent.link,
        );
        assert_eq!(hex_text(&out), hex_text(&captured));
    }

    fn hex_text(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A link trap's varbinds, decoded by hand from the bytes `write_trap`
    /// writes: each TLV's tag, the OID's content octets and the value's.
    #[test]
    fn write_trap_writes_the_varbinds_in_rfc_order() {
        let mut out = Vec::new();
        write_trap(
            &mut out,
            b"c",
            false,
            7,
            300,
            TrapKind::LinkUp,
            link(200, 2, 1),
        );
        let varbinds = [
            // SEQUENCE { OID 1.3.6.1.2.1.1.3.0, TimeTicks 300 }
            "300e06082b060102010103004302012c",
            // SEQUENCE { OID 1.3.6.1.6.3.1.1.4.1.0, OID 1.3.6.1.6.3.1.1.5.4 }
            "3017060a2b06010603010104010006092b0601060301010504",
            // SEQUENCE { OID 1.3.6.1.2.1.2.2.1.1.200, INTEGER 200 }: the arc
            // 200 is the two octets 81 48, the INTEGER the two octets 00 c8.
            "3011060b2b06010201020201018148020200c8",
            // SEQUENCE { OID 1.3.6.1.2.1.2.2.1.7.200, INTEGER 2 }
            "3010060b2b06010201020201078148020102",
            // SEQUENCE { OID 1.3.6.1.2.1.2.2.1.8.200, INTEGER 1 }
            "3010060b2b06010201020201088148020101",
        ]
        .concat();
        // SEQUENCE { INTEGER 1, OCTET STRING "c", SNMPv2-Trap { INTEGER 7,
        // INTEGER 0, INTEGER 0, SEQUENCE { <varbinds> } } }
        let pdu_body = format!("020107020100020100{}", tlv("30", &varbinds));
        let message = format!("020101040163{}", tlv("a7", &pdu_body));
        assert_eq!(hex_text(&out), tlv("30", &message));
    }

    /// `tag` and `body` (hex) as a TLV, short-form length.
    fn tlv(tag: &str, body: &str) -> String {
        assert!(body.len() / 2 < 0x80, "short form only");
        format!("{tag}{:02x}{body}", body.len() / 2)
    }

    #[test]
    fn request_ids_count_up_within_31_bits() {
        let mut ids = RequestIds(0x7FFF_FFFE);
        assert_eq!(ids.next(), 0x7FFF_FFFE);
        assert_eq!(ids.next(), i32::MAX);
        assert_eq!(ids.next(), 0, "wraps to 0, never negative");
        assert_eq!(ids.next(), 1);
    }

    // ---- config ------------------------------------------------------------

    #[test]
    fn settings_resolve_the_defaults() {
        let settings = SnmpTrapConfig::new("127.0.0.1:162")
            .settings()
            .expect("the defaults are valid");
        assert_eq!(settings.destination, "127.0.0.1:162");
        assert_eq!(settings.community, b"public");
        assert!(!settings.inform);
        assert_eq!(settings.timeout, Duration::from_secs(2));
        assert_eq!(settings.retries, 3);
    }

    /// `settings()` refuses each field out of range, naming it, and takes
    /// the bounds.
    #[rustfmt::skip]
    #[rstest]
    #[case::destination_without_port(  |c: &mut SnmpTrapConfig| c.destination = "receiver".to_string(),        Some("snmp_trap destination \"receiver\""))]
    #[case::destination_empty_host(    |c: &mut SnmpTrapConfig| c.destination = ":162".to_string(),            Some("snmp_trap destination \":162\""))]
    #[case::destination_port_zero(     |c: &mut SnmpTrapConfig| c.destination = "receiver:0".to_string(),      Some("snmp_trap destination \"receiver:0\""))]
    #[case::destination_port_too_big(  |c: &mut SnmpTrapConfig| c.destination = "receiver:65536".to_string(),  Some("snmp_trap destination \"receiver:65536\""))]
    #[case::destination_bare_ipv6(     |c: &mut SnmpTrapConfig| c.destination = "::1:162".to_string(),         Some("snmp_trap destination \"::1:162\""))]
    #[case::destination_address_port_0(|c: &mut SnmpTrapConfig| c.destination = "127.0.0.1:0".to_string(),     Some("snmp_trap destination \"127.0.0.1:0\""))]
    #[case::destination_host_name(     |c: &mut SnmpTrapConfig| c.destination = "telegraf:1162".to_string(),   None)]
    #[case::destination_ipv6(          |c: &mut SnmpTrapConfig| c.destination = "[::1]:162".to_string(),       None)]
    #[case::timeout_without_unit(      |c: &mut SnmpTrapConfig| c.timeout = "2".to_string(),                   Some("snmp_trap timeout \"2\""))]
    #[case::timeout_below_minimum(     |c: &mut SnmpTrapConfig| c.timeout = "99ms".to_string(),                Some("snmp_trap timeout must be between 100ms and 60s, got \"99ms\""))]
    #[case::timeout_at_minimum(        |c: &mut SnmpTrapConfig| c.timeout = "100ms".to_string(),               None)]
    #[case::timeout_at_maximum(        |c: &mut SnmpTrapConfig| c.timeout = "60s".to_string(),                 None)]
    #[case::timeout_above_maximum(     |c: &mut SnmpTrapConfig| c.timeout = "61s".to_string(),                 Some("snmp_trap timeout must be between 100ms and 60s, got \"61s\""))]
    #[case::retries_at_maximum(        |c: &mut SnmpTrapConfig| c.retries = 10,                                None)]
    #[case::retries_above_maximum(     |c: &mut SnmpTrapConfig| c.retries = 11,                                Some("snmp_trap retries must be between 0 and 10, got 11"))]
    #[case::retries_zero(              |c: &mut SnmpTrapConfig| c.retries = 0,                                 None)]
    fn settings_check_every_field(#[case] change: fn(&mut SnmpTrapConfig), #[case] refused: Option<&str>) {
        let mut cfg = SnmpTrapConfig::new("127.0.0.1:162");
        cfg.settings().expect("the baseline is valid");
        change(&mut cfg);
        match (cfg.settings(), refused) {
            (Ok(_), None) => {}
            (Err(SondaError::Config(e)), Some(fragment)) => {
                assert!(e.to_string().contains(fragment), "{e}");
            }
            (other, _) => panic!("expected {refused:?}, got {other:?}"),
        }
    }

    #[test]
    fn debug_prints_no_community() {
        let cfg = SnmpTrapConfig {
            community: "s3cret-community".to_string(),
            ..SnmpTrapConfig::new("127.0.0.1:162")
        };
        let settings = cfg.settings().expect("valid settings");
        for debug in [format!("{cfg:?}"), format!("{settings:?}")] {
            assert!(debug.contains("127.0.0.1:162"), "{debug}");
            assert!(!debug.contains("s3cret-community"), "{debug}");
        }
    }

    /// Every field of the block reads as written; the text fields take any
    /// scalar as its text.
    #[cfg(feature = "config")]
    #[test]
    fn a_full_block_parses_and_numbers_are_read_as_text() {
        let yaml = "type: snmp_trap\ndestination: receiver:1162\ncommunity: 12345\n\
                    inform: true\ntimeout: 500ms\nretries: 5\n";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("the block parses");
        let SinkConfig::SnmpTrap(cfg) = config else {
            panic!("an snmp_trap sink, got {config:?}");
        };
        assert_eq!(
            cfg,
            SnmpTrapConfig {
                destination: "receiver:1162".to_string(),
                community: "12345".to_string(),
                inform: true,
                timeout: "500ms".to_string(),
                retries: 5,
            }
        );
        cfg.settings().expect("and its settings are valid");
    }

    /// A host name that resolves to nothing fails the sink's creation,
    /// naming the destination, after its settings were accepted.
    #[tokio::test]
    async fn a_destination_that_does_not_resolve_fails_at_creation() {
        let cfg = SnmpTrapConfig::new("no-such-host.invalid:162");
        cfg.settings()
            .expect("guard: the destination is well-formed");
        let err = SnmpTrapSink::new(&cfg)
            .await
            .err()
            .expect("an unresolvable destination is refused");
        assert!(matches!(err, SondaError::Sink(_)), "{err:?}");
        assert!(
            err.to_string().contains("no-such-host.invalid:162"),
            "{err}"
        );
    }

    // ---- the receiver --------------------------------------------------------

    /// A trap receiver on an ephemeral port of 127.0.0.1.
    struct Receiver {
        socket: UdpSocket,
    }

    impl Receiver {
        async fn bind() -> Self {
            Self {
                socket: UdpSocket::bind("127.0.0.1:0")
                    .await
                    .expect("bind a receiver"),
            }
        }

        fn destination(&self) -> String {
            self.socket.local_addr().expect("an address").to_string()
        }

        /// The next datagram and its sender, within five seconds.
        async fn next(&self) -> (Vec<u8>, SocketAddr) {
            let mut datagram = vec![0; 2048];
            let (len, peer) =
                tokio::time::timeout(Duration::from_secs(5), self.socket.recv_from(&mut datagram))
                    .await
                    .expect("a datagram arrives")
                    .expect("received");
            datagram.truncate(len);
            (datagram, peer)
        }

        /// Whether nothing arrives for `wait`.
        async fn silent(&self, wait: Duration) -> bool {
            let mut datagram = [0; 2048];
            tokio::time::timeout(wait, self.socket.recv_from(&mut datagram))
                .await
                .is_err()
        }

        /// Answer `datagram`, an INFORM from `peer`, with a `Response`
        /// carrying `request_id` and its varbinds, as a receiver does.
        async fn answer(&self, datagram: &[u8], peer: SocketAddr, request_id: i64) {
            let inform = Message::decode(datagram).expect("an INFORM");
            let mut out = Vec::new();
            ber::write_message(
                &mut out,
                1,
                inform.community,
                Tag::Response,
                request_id,
                0,
                0,
                |out| out.extend_from_slice(inform.varbinds),
            );
            self.socket.send_to(&out, peer).await.expect("answered");
        }
    }

    fn config(destination: String, inform: bool, timeout: &str, retries: u8) -> SnmpTrapConfig {
        SnmpTrapConfig {
            inform,
            timeout: timeout.to_string(),
            retries,
            ..SnmpTrapConfig::new(destination)
        }
    }

    // ---- the records ---------------------------------------------------------

    /// The encoder the tests write records with: ifOperStatus and
    /// ifAdminStatus as INTEGERs, two other interface columns, an
    /// ifOperStatus served as a Gauge32 (`operGauge`), one an arc deeper
    /// (`operDeeper`), and `columns`.
    fn encoder(columns: &[(&str, LabelColumn)]) -> SnmpEncoder {
        let strings = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>()
        };
        SnmpEncoder::new(&SnmpEncoderConfig {
            oids: strings(&[
                ("ifOperStatus", "1.3.6.1.2.1.2.2.1.8.{ifIndex}"),
                ("ifAdminStatus", "1.3.6.1.2.1.2.2.1.7.{ifIndex}"),
                ("ifInErrors", "1.3.6.1.2.1.2.2.1.14.{ifIndex}"),
                ("ifHCInOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"),
                ("operGauge", "1.3.6.1.2.1.2.2.1.8.{ifIndex}"),
                ("operDeeper", "1.3.6.1.2.1.2.2.1.8.{ifIndex}.{sub}"),
            ]),
            types: HashMap::from([
                ("ifOperStatus".to_string(), SnmpValueType::Integer),
                ("ifAdminStatus".to_string(), SnmpValueType::Integer),
                ("ifInErrors".to_string(), SnmpValueType::Counter32),
                ("ifHCInOctets".to_string(), SnmpValueType::Counter64),
                ("operGauge".to_string(), SnmpValueType::Gauge32),
                ("operDeeper".to_string(), SnmpValueType::Integer),
            ]),
            label_columns: columns
                .iter()
                .map(|(label, column)| (label.to_string(), column.clone()))
                .collect(),
            ..SnmpEncoderConfig::default()
        })
        .expect("a valid encoder")
    }

    /// Write `name` = `value` with `labels` to `sink` as the encoder's
    /// record.
    async fn write(
        sink: &mut SnmpTrapSink,
        encoder: &SnmpEncoder,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), SondaError> {
        let event = MetricEvent::new(
            name.to_string(),
            value,
            Labels::from_pairs(labels).expect("valid labels"),
        )
        .expect("a valid event");
        let mut record = Vec::new();
        encoder.encode_metric(&event, &mut record).expect("encodes");
        sink.write_metric_event(&event, &record).await
    }

    /// Write interface `if_index`'s ifOperStatus as `values`, in order.
    async fn oper(sink: &mut SnmpTrapSink, encoder: &SnmpEncoder, if_index: &str, values: &[f64]) {
        for &value in values {
            write(
                sink,
                encoder,
                "ifOperStatus",
                value,
                &[("ifIndex", if_index)],
            )
            .await
            .expect("the record is read");
        }
    }

    /// Send the linkDown of interface [`SENTINEL`] (up, then down), then
    /// read what the receiver got up to and including it. The receiver
    /// reads one socket the sinks send to in order, so nothing sent before
    /// the sentinel is missed and nothing after it is read. Each sink's
    /// request-ids count up by one.
    async fn finish(
        sink: &mut SnmpTrapSink,
        encoder: &SnmpEncoder,
        receiver: &Receiver,
    ) -> Vec<Sent> {
        oper(sink, encoder, SENTINEL, &[1.0, 2.0]).await;
        let mut sent = Vec::new();
        let mut request_ids: HashMap<SocketAddr, Vec<i64>> = HashMap::new();
        loop {
            let (datagram, peer) = receiver.next().await;
            let (notification, request_id, _) = decode(&datagram);
            sent.push(notification);
            request_ids.entry(peer).or_default().push(request_id);
            if notification.link.is_some_and(|link| link.if_index == 99) {
                break;
            }
        }
        for ids in request_ids.values() {
            for pair in ids.windows(2) {
                assert_eq!((pair[1] - pair[0]).rem_euclid(1 << 31), 1, "{ids:?}");
            }
        }
        sent
    }

    async fn trap_sink(receiver: &Receiver) -> SnmpTrapSink {
        SnmpTrapSink::new(&SnmpTrapConfig::new(receiver.destination()))
            .await
            .expect("the sink is created")
    }

    // ---- transitions ---------------------------------------------------------

    /// ifOperStatus 1, 1, 2, 2, 1 on one interface: coldStart from the
    /// sink's creation, one linkDown, one linkUp, nothing else, each carrying
    /// ifAdminStatus up while none has been read.
    #[tokio::test]
    async fn up_up_down_down_up_sends_cold_start_link_down_and_link_up_only() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        oper(&mut sink, &encoder, "4", &[1.0, 1.0, 2.0, 2.0, 1.0]).await;
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(4, 1, 2)),
                Sent::trap(TrapKind::LinkUp, link(4, 1, 1)),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// Two interfaces written in turn each change from their own last
    /// value: an interface's first value sends nothing even when another
    /// interface's was different.
    #[tokio::test]
    async fn interleaved_interfaces_keep_their_own_state() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        for (if_index, value) in [
            ("1", 1.0),
            ("2", 2.0),
            ("1", 2.0),
            ("2", 2.0),
            ("2", 1.0),
            ("1", 1.0),
        ] {
            oper(&mut sink, &encoder, if_index, &[value]).await;
        }
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(1, 1, 2)),
                Sent::trap(TrapKind::LinkUp, link(2, 1, 1)),
                Sent::trap(TrapKind::LinkUp, link(1, 1, 1)),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// Up to a value other than up is linkDown, from one other than up to
    /// up is linkUp, and a change between two values other than up sends
    /// nothing.
    #[tokio::test]
    async fn only_a_change_to_or_from_up_sends() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        // down, notPresent, up, testing, lowerLayerDown, up
        oper(&mut sink, &encoder, "3", &[2.0, 6.0, 1.0, 3.0, 7.0, 1.0]).await;
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkUp, link(3, 1, 1)),
                Sent::trap(TrapKind::LinkDown, link(3, 1, 3)),
                Sent::trap(TrapKind::LinkUp, link(3, 1, 1)),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// An ifAdminStatus record sends nothing and changes the ifAdminStatus
    /// the interface's next link trap carries; another interface's still
    /// carries up.
    #[tokio::test]
    async fn an_admin_status_record_changes_the_next_link_trap() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        oper(&mut sink, &encoder, "3", &[1.0]).await;
        write(
            &mut sink,
            &encoder,
            "ifAdminStatus",
            2.0,
            &[("ifIndex", "3")],
        )
        .await
        .expect("read");
        oper(&mut sink, &encoder, "3", &[2.0]).await;
        write(
            &mut sink,
            &encoder,
            "ifAdminStatus",
            1.0,
            &[("ifIndex", "3")],
        )
        .await
        .expect("read");
        oper(&mut sink, &encoder, "3", &[1.0]).await;
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(3, 2, 2)),
                Sent::trap(TrapKind::LinkUp, link(3, 1, 1)),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// A label column at ifAdminStatus.N rides the series' first record as
    /// a companion and is read as ifAdminStatus.
    #[tokio::test]
    async fn an_admin_status_label_column_is_read_from_the_companion() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[(
            "admin",
            LabelColumn::Typed {
                oid: "1.3.6.1.2.1.2.2.1.7.{ifIndex}".to_string(),
                r#type: SnmpValueType::Integer,
            },
        )]);
        let mut sink = trap_sink(&receiver).await;
        for value in [1.0, 2.0] {
            write(
                &mut sink,
                &encoder,
                "ifOperStatus",
                value,
                &[("ifIndex", "6"), ("admin", "2")],
            )
            .await
            .expect("read");
        }
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(6, 2, 2)),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// Records under any other object send nothing and change no state: two
    /// other columns, an ifOperStatus instance served as a Gauge32, and one
    /// an arc deeper than an instance, each going up and down.
    #[tokio::test]
    async fn a_record_under_another_object_sends_nothing() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        let index = [("ifIndex", "4")];
        let deeper = [("ifIndex", "4"), ("sub", "1")];
        for (name, labels) in [
            ("ifInErrors", &index[..]),
            ("ifHCInOctets", &index[..]),
            ("operGauge", &index[..]),
            ("operDeeper", &deeper[..]),
        ] {
            for value in [1.0, 2.0, 1.0, 2.0] {
                write(&mut sink, &encoder, name, value, labels)
                    .await
                    .expect("read");
            }
        }
        assert!(sink.links.is_empty(), "no interface kept: {:?}", sink.links);
        assert_eq!(
            finish(&mut sink, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
    }

    /// sysUpTime counts hundredths of a second from the sink's creation:
    /// about 0 in coldStart, at least the wait in a later trap.
    #[tokio::test]
    async fn sys_up_time_counts_from_the_sinks_creation() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = trap_sink(&receiver).await;
        let (cold_start, _) = receiver.next().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        oper(&mut sink, &encoder, "1", &[1.0, 2.0]).await;
        let (link_down, _) = receiver.next().await;
        let (_, _, at_start) = decode(&cold_start);
        let (sent, _, later) = decode(&link_down);
        assert_eq!(sent.kind, TrapKind::LinkDown);
        assert!(at_start <= 10, "coldStart at {at_start} hundredths");
        assert!((30..300).contains(&later), "linkDown at {later} hundredths");
    }

    /// The sinks of one launch on one destination send one coldStart between
    /// them, the first to be created; a sink of another launch sends its own,
    /// and once every sink of the launch has dropped, its next sink sends
    /// one again. The first two are made through `create_sink_in`, the path
    /// a scenario file's entries take.
    #[tokio::test]
    async fn the_sinks_of_one_launch_send_one_cold_start() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let cfg = SnmpTrapConfig::new(receiver.destination());
        let launch = LaunchId::next();
        let config = SinkConfig::SnmpTrap(cfg.clone());
        let first = crate::sink::create_sink_in(&config, None, launch)
            .await
            .expect("the first sink");
        let second = crate::sink::create_sink_in(&config, None, launch)
            .await
            .expect("the second sink");
        let mut third = SnmpTrapSink::new_in(&cfg, launch)
            .await
            .expect("a third sink of the launch");
        let mut other = SnmpTrapSink::new(&cfg)
            .await
            .expect("a sink of another launch");
        assert_eq!(
            finish(&mut third, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ],
            "the launch's first sink and the other launch's sink"
        );

        drop((first, second, third));
        let mut again = SnmpTrapSink::new_in(&cfg, launch)
            .await
            .expect("the launch's next sink");
        assert_eq!(
            finish(&mut again, &encoder, &receiver).await,
            [
                Sent::trap(TrapKind::ColdStart, None),
                Sent::trap(TrapKind::LinkDown, link(99, 1, 2)),
            ]
        );
        assert_eq!(
            finish(&mut other, &encoder, &receiver).await,
            [Sent::trap(TrapKind::LinkDown, link(99, 1, 2))],
            "guard: the other launch's sink is still live and sent nothing more"
        );
    }

    // ---- traps and INFORMs ---------------------------------------------------

    /// A trap is sent once and the write waits for nothing, however long the
    /// INFORM timeout would be.
    #[tokio::test]
    async fn a_trap_is_sent_once_and_waits_for_nothing() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let started = Instant::now();
        let mut sink = SnmpTrapSink::new(&config(receiver.destination(), false, "60s", 10))
            .await
            .expect("the sink is created");
        oper(&mut sink, &encoder, "1", &[1.0, 2.0]).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let (cold_start, _) = receiver.next().await;
        let (link_down, _) = receiver.next().await;
        assert_eq!(decode(&cold_start).0, Sent::trap(TrapKind::ColdStart, None));
        assert_eq!(
            decode(&link_down).0,
            Sent::trap(TrapKind::LinkDown, link(1, 1, 2))
        );
        assert!(
            receiver.silent(Duration::from_millis(300)).await,
            "a trap was sent again"
        );
    }

    /// A receiver that answers an INFORM's third send gets three sends of
    /// the same message and acknowledges one; the sink waited out two
    /// timeouts and reports nothing.
    #[tokio::test]
    async fn an_inform_answered_after_two_timeouts_takes_three_sends() {
        let receiver = Receiver::bind().await;
        let destination = receiver.destination();
        let answering = tokio::spawn(async move {
            let mut sends = Vec::new();
            loop {
                let (datagram, peer) = receiver.next().await;
                sends.push(datagram.clone());
                if sends.len() == 3 {
                    let (_, request_id, _) = decode(&datagram);
                    receiver.answer(&datagram, peer, request_id).await;
                    return (receiver, sends);
                }
            }
        });
        let started = Instant::now();
        let mut sink = SnmpTrapSink::new(&config(destination, true, "100ms", 3))
            .await
            .expect("the sink is created");
        let waited = started.elapsed();
        let (receiver, sends) = answering.await.expect("the receiver answered");
        assert_eq!(sends.len(), 3);
        assert!(
            sends.iter().all(|send| *send == sends[0]),
            "one message, sent again"
        );
        assert_eq!(decode(&sends[0]).0, Sent::inform(TrapKind::ColdStart, None));
        assert!(waited >= Duration::from_millis(200), "waited {waited:?}");
        assert!(
            receiver.silent(Duration::from_millis(300)).await,
            "a fourth send"
        );
        assert_eq!(sink.ignored(), 0);
        let encoder = encoder(&[]);
        write(&mut sink, &encoder, "ifInErrors", 0.0, &[("ifIndex", "1")])
            .await
            .expect("the acknowledged coldStart left nothing to report");
    }

    /// A receiver that never answers costs `retries + 1` sends of each
    /// INFORM and one failure: coldStart's on the first write, a linkDown's
    /// on the write that called for it; the writes after each succeed.
    #[tokio::test]
    async fn an_inform_never_answered_costs_retries_plus_one_sends_and_one_failure() {
        let receiver = Receiver::bind().await;
        let encoder = encoder(&[]);
        let mut sink = SnmpTrapSink::new(&config(receiver.destination(), true, "100ms", 2))
            .await
            .expect("an unacknowledged coldStart does not fail creation");
        let drain = |kind: TrapKind| {
            let receiver = &receiver;
            async move {
                let mut sends = Vec::new();
                for _ in 0..3 {
                    sends.push(receiver.next().await.0);
                }
                assert!(sends.iter().all(|send| *send == sends[0]));
                assert_eq!(decode(&sends[0]).0.kind, kind);
                assert!(
                    receiver.silent(Duration::from_millis(300)).await,
                    "a fourth send"
                );
            }
        };
        drain(TrapKind::ColdStart).await;

        let err = write(
            &mut sink,
            &encoder,
            "ifOperStatus",
            1.0,
            &[("ifIndex", "1")],
        )
        .await
        .expect_err("the first write reports coldStart");
        assert!(matches!(err, SondaError::Sink(_)), "{err:?}");
        assert!(
            err.to_string()
                .contains("acknowledged none of 3 sends of a coldStart INFORM"),
            "{err}"
        );
        oper(&mut sink, &encoder, "1", &[1.0]).await;

        let err = write(
            &mut sink,
            &encoder,
            "ifOperStatus",
            2.0,
            &[("ifIndex", "1")],
        )
        .await
        .expect_err("the unacknowledged linkDown fails its write");
        assert!(
            err.to_string()
                .contains("acknowledged none of 3 sends of a linkDown INFORM"),
            "{err}"
        );
        drain(TrapKind::LinkDown).await;
        oper(&mut sink, &encoder, "1", &[2.0]).await;
    }

    /// While an INFORM waits, a `Response` carrying another request-id, a
    /// request carrying its own, and bytes that are no SNMP message are
    /// ignored and counted; the answer to the next send is taken.
    #[tokio::test]
    async fn a_response_with_another_request_id_is_ignored() {
        let receiver = Receiver::bind().await;
        let destination = receiver.destination();
        let answering = tokio::spawn(async move {
            let (first, peer) = receiver.next().await;
            let (_, request_id, _) = decode(&first);
            receiver.answer(&first, peer, request_id + 1).await;
            let mut get = Vec::new();
            ber::write_message(
                &mut get,
                1,
                b"public",
                Tag::GetRequest,
                request_id,
                0,
                0,
                |_| {},
            );
            receiver.socket.send_to(&get, peer).await.expect("sent");
            receiver
                .socket
                .send_to(b"not snmp", peer)
                .await
                .expect("sent");
            let (second, peer) = receiver.next().await;
            assert_eq!(second, first, "the same message, sent again");
            receiver.answer(&second, peer, request_id).await;
            receiver
        });
        let sink = SnmpTrapSink::new(&config(destination, true, "200ms", 3))
            .await
            .expect("the sink is created");
        let receiver = answering.await.expect("the receiver answered");
        assert!(
            receiver.silent(Duration::from_millis(400)).await,
            "a third send"
        );
        assert_eq!(sink.ignored(), 3);
        assert!(sink.pending.is_none(), "coldStart was acknowledged");
    }

    /// Through the schedule loop, an INFORM that is never acknowledged is
    /// one sink failure in the scenario's stats, and the scenario goes on.
    #[tokio::test]
    async fn an_unacknowledged_inform_is_one_sink_failure_of_the_scenario() {
        let receiver = Receiver::bind().await;
        let destination = receiver.destination();
        // Acknowledge coldStart only.
        let answering = tokio::spawn(async move {
            let (cold_start, peer) = receiver.next().await;
            let (_, request_id, _) = decode(&cold_start);
            receiver.answer(&cold_start, peer, request_id).await;
            let mut later = Vec::new();
            while let Ok(Ok((datagram, _))) = tokio::time::timeout(Duration::from_secs(2), async {
                let mut datagram = vec![0; 2048];
                let (len, peer) = receiver.socket.recv_from(&mut datagram).await?;
                datagram.truncate(len);
                Ok::<_, io::Error>((datagram, peer))
            })
            .await
            {
                later.push(decode(&datagram).0);
            }
            later
        });
        let mut sink: Box<dyn Sink> = Box::new(
            SnmpTrapSink::new(&config(destination.clone(), true, "100ms", 0))
                .await
                .expect("the sink is created"),
        );
        let scenario = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "ifOperStatus".to_string(),
                rate: 20.0,
                duration: Some("500ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: Some(HashMap::from([("ifIndex".to_string(), "1".to_string())])),
                sink: SinkConfig::SnmpTrap(config(destination, true, "100ms", 0)),
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: OnSinkError::Warn,
            },
            generator: GeneratorConfig::Sequence {
                values: vec![1.0, 1.0, 2.0],
                repeat: Some(false),
            },
            encoder: EncoderConfig::Snmp(SnmpEncoderConfig {
                oids: HashMap::from([(
                    "ifOperStatus".to_string(),
                    "1.3.6.1.2.1.2.2.1.8.{ifIndex}".to_string(),
                )]),
                types: HashMap::from([("ifOperStatus".to_string(), SnmpValueType::Integer)]),
                ..SnmpEncoderConfig::default()
            }),
            metric_type: None,
            help: None,
        };
        crate::config::validate::validate_config(&scenario).expect("the entry validates");
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        crate::schedule::runner::run_with_sink(
            &scenario,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");
        let later = answering.await.expect("the receiver ran");
        assert_eq!(
            later,
            [Sent::inform(TrapKind::LinkDown, link(1, 1, 2))],
            "one linkDown, sent once (retries 0) and never answered"
        );
        let stats = stats.read().expect("stats lock");
        assert!(
            stats.total_events >= 4,
            "only {} ticks ran",
            stats.total_events
        );
        assert_eq!(stats.total_sink_failures, 1);
        assert_eq!(stats.errors, 1);
    }

    // ---- what the sink refuses ----------------------------------------------

    #[tokio::test]
    async fn write_is_an_error_and_a_record_is_required() {
        let receiver = Receiver::bind().await;
        let mut sink = trap_sink(&receiver).await;
        assert!(sink.wants_metric_events());
        let err = sink.write(b"bytes").await.expect_err("write is refused");
        assert!(
            err.to_string()
                .contains("pair it with `encoder: { type: snmp }`"),
            "{err}"
        );
        let event = MetricEvent::new(
            "ifOperStatus".to_string(),
            1.0,
            Labels::from_pairs(&[("ifIndex", "1")]).expect("labels"),
        )
        .expect("an event");
        let err = sink
            .write_metric_event(&event, b"ifOperStatus 1\n")
            .await
            .expect_err("bytes that are not a publish record are refused");
        assert!(matches!(err, SondaError::Sink(_)), "{err:?}");
        assert!(err.to_string().contains("publish record"), "{err}");
    }
}
