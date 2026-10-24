//! The shared state behind one SNMP listen address.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, Weak};
use std::time::Duration;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::AgentSettings;
use crate::encoder::snmp::ber::{self, Message, Tag};
use crate::encoder::snmp::oid::Oid;
use crate::schedule::LaunchId;
use crate::sink::listener_registry::ListenerTarget;
use crate::{ConfigError, SondaError};

/// The system group's OID, `1.3.6.1.2.1.1`. The agent serves every
/// instance under it from its `system:` settings; no sink may publish
/// one.
const SYSTEM_GROUP: [u32; 7] = [1, 3, 6, 1, 2, 1, 1];

/// The sub-identifier of `sysUpTime` under the system group.
const SYS_UP_TIME: u32 = 3;

/// The largest datagram the responder reads whole: the most a UDP datagram
/// over IPv4 carries.
const RECEIVE_BUFFER: usize = 65_507;

/// How long the responder waits before reading again after a receive error.
const RECEIVE_BACKOFF: Duration = Duration::from_millis(100);

/// The launch an instance belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner(u64);

impl From<LaunchId> for Owner {
    fn from(launch: LaunchId) -> Self {
        Self(launch.get())
    }
}

/// What [`SnmpAgent::get`] finds for an OID.
// The read side (`Lookup`, `get`, `next_after`, `uptime_ticks`) has no caller
// in the crate outside tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The instance's `VarBind`, as encoded.
    Value(Bytes),
    /// No such instance, but the OID is a registered object or under one.
    NoSuchInstance,
    /// The OID is neither an instance nor under a registered object.
    NoSuchObject,
}

/// The requests an agent has received, by kind, as [`SnmpAgent::requests`]
/// reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RequestCounters {
    /// `GetRequest` PDUs.
    pub get: u64,
    /// `GetNextRequest` PDUs.
    pub get_next: u64,
    /// `GetBulkRequest` PDUs.
    pub get_bulk: u64,
    /// `SetRequest` PDUs, none of which is applied.
    pub set_refused: u64,
    /// Datagrams that are not an SNMPv1 or SNMPv2c message, or that carry
    /// any other PDU.
    pub malformed: u64,
    /// Messages whose community is not the agent's.
    pub wrong_community: u64,
    /// Requests whose answer did not fit `max_message_size`.
    pub too_big: u64,
}

/// The live counters behind [`RequestCounters`].
#[derive(Debug, Default)]
struct Counters {
    get: AtomicU64,
    get_next: AtomicU64,
    get_bulk: AtomicU64,
    set_refused: AtomicU64,
    malformed: AtomicU64,
    wrong_community: AtomicU64,
    too_big: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> RequestCounters {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        RequestCounters {
            get: read(&self.get),
            get_next: read(&self.get_next),
            get_bulk: read(&self.get_bulk),
            set_refused: read(&self.set_refused),
            malformed: read(&self.malformed),
            wrong_community: read(&self.wrong_community),
            too_big: read(&self.too_big),
        }
    }
}

/// One published instance.
struct Slot {
    /// The instance's `VarBind`, as encoded.
    varbind: Bytes,
    /// The only launch whose sinks may publish the instance.
    owner: Owner,
    /// Sinks of `owner` that have published the instance; it leaves the
    /// table when the last of them is released.
    publishers: usize,
}

/// The instances sinks have published, and the objects they belong to.
struct MibTable {
    slots: BTreeMap<Oid, Slot>,
    /// Every object an instance was published under, and the system group's
    /// seven. Never pruned.
    objects: BTreeSet<Oid>,
}

/// One instance of the system group.
struct SystemInstance {
    oid: Oid,
    /// The `VarBind`, or `None` for `sysUpTime`, which is made on each read.
    #[cfg_attr(not(test), allow(dead_code))]
    varbind: Option<Bytes>,
}

/// The SNMP agent for one listen address: the instances its sinks publish,
/// the system group, and the UDP socket pollers send requests to.
///
/// Instances are kept in OID order, which is the order a walk visits them
/// in; the system group is part of the same order. An instance is owned by
/// one launch and held while any sink of that launch has published it; it
/// leaves when the last of those sinks drops, and at no other time. The
/// object an instance belongs to stays registered after its instances
/// leave, so a request for it answers "no such instance" rather than "no
/// such object".
///
/// The socket is read by one responder task, which decodes each datagram,
/// counts it under its PDU type in [`requests`](Self::requests), and drops
/// it. Dropping the last handle stops the task and closes the socket.
pub struct SnmpAgent {
    local_addr: SocketAddr,
    settings: AgentSettings,
    /// When the socket was bound; `sysUpTime` counts from here.
    #[cfg_attr(not(test), allow(dead_code))]
    started: tokio::time::Instant,
    /// The system group's seven instances, in OID order.
    #[cfg_attr(not(test), allow(dead_code))]
    system: Vec<SystemInstance>,
    table: RwLock<MibTable>,
    counters: Counters,
    token: CancellationToken,
    /// Whether the socket is open: `true` until the responder task ends.
    state: watch::Receiver<bool>,
}

impl SnmpAgent {
    /// Bind `settings.listen` and start the responder on a spawned task.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] naming the address when it cannot be bound; on a
    /// privileged port without the permission to bind it, the message names
    /// the port and `CAP_NET_BIND_SERVICE`.
    pub(crate) async fn bind(settings: AgentSettings) -> Result<Arc<Self>, SondaError> {
        let listen = settings.listen;
        let socket = UdpSocket::bind(listen)
            .await
            .map_err(|e| bind_error(listen, e))?;
        let local_addr = socket.local_addr().map_err(SondaError::Sink)?;
        if listen.port() == 0 {
            tracing::warn!(addr = %local_addr, "snmp_agent listening on an ephemeral port");
        } else {
            tracing::info!(addr = %local_addr, "snmp_agent listening");
        }
        let system = system_instances(&settings);
        let table = MibTable {
            slots: BTreeMap::new(),
            objects: system
                .iter()
                .map(|instance| object_of(&instance.oid))
                .collect(),
        };
        let token = CancellationToken::new();
        let (state_tx, state) = watch::channel(true);
        let responder = Responder {
            socket,
            buffer: vec![0; RECEIVE_BUFFER],
            _closed: ClosedOnDrop(state_tx),
        };
        Ok(Arc::new_cyclic(|agent| {
            tokio::spawn(respond(responder, agent.clone(), token.clone()));
            SnmpAgent {
                local_addr,
                settings,
                started: tokio::time::Instant::now(),
                system,
                table: RwLock::new(table),
                counters: Counters::default(),
                token,
                state,
            }
        }))
    }

    /// The address the socket bound, with an ephemeral port resolved.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Instances sinks have published, the system group not included.
    pub fn instance_count(&self) -> usize {
        self.read().slots.len()
    }

    /// Whether a request must carry the agent's community. Always `true`.
    pub fn community_required(&self) -> bool {
        true
    }

    /// The requests the agent has received, by kind.
    pub fn requests(&self) -> RequestCounters {
        self.counters.snapshot()
    }

    /// Resolves once the socket is closed. The returned future does not hold
    /// the agent, so it can be awaited after the last handle is dropped.
    pub fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut state = self.state.clone();
        async move {
            let _ = state.wait_for(|open| !open).await;
        }
    }

    /// Whether the responder task is still reading the socket. It is not
    /// once it has returned, panicked, or been dropped with its runtime.
    pub(crate) fn is_serving(&self) -> bool {
        *self.state.borrow()
    }

    /// `Ok` while the responder task runs; otherwise [`SondaError::Sink`]
    /// naming the address.
    pub(crate) fn check_serving(&self) -> Result<(), SondaError> {
        if self.is_serving() {
            return Ok(());
        }
        Err(SondaError::Sink(io::Error::new(
            io::ErrorKind::NotConnected,
            format!("snmp_agent {} is no longer serving", self.local_addr),
        )))
    }

    /// Stop the responder while handles to the agent remain.
    #[cfg(test)]
    pub(crate) fn stop_serving(&self) {
        self.token.cancel();
    }

    /// The settings the agent was bound with.
    pub(crate) fn settings(&self) -> &AgentSettings {
        &self.settings
    }

    /// Store `varbind` as the latest `VarBind` of the instance `oid`, which
    /// `owner` publishes. `first` says this is the publisher's first publish
    /// of `oid`: it counts one more publisher of the instance, which leaves
    /// the table when the last of them is [released](Self::release). A
    /// publish that finds no instance creates it, owned by `owner`.
    ///
    /// Takes the table's write lock for the upsert only; never awaits.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] naming the OID and the listen address when
    /// another launch owns the instance, or when `oid` is under the system
    /// group, which the agent serves itself.
    pub(crate) fn publish(
        &self,
        owner: Owner,
        oid: &Oid,
        varbind: Bytes,
        first: bool,
    ) -> Result<(), SondaError> {
        let mut table = self.write();
        match table.slots.get_mut(oid) {
            Some(slot) if slot.owner != owner => {
                Err(SondaError::Config(ConfigError::invalid(format!(
                    "the snmp OID {oid} on {} is already published by another running scenario",
                    self.local_addr
                ))))
            }
            Some(slot) => {
                slot.varbind = varbind;
                slot.publishers += usize::from(first);
                Ok(())
            }
            None => {
                if oid.arcs().starts_with(&SYSTEM_GROUP) {
                    return Err(SondaError::Config(ConfigError::invalid(format!(
                        "the snmp OID {oid} is under 1.3.6.1.2.1.1, the system group, which \
                         snmp_agent {} serves itself",
                        self.local_addr
                    ))));
                }
                table.slots.insert(
                    oid.clone(),
                    Slot {
                        varbind,
                        owner,
                        publishers: 1,
                    },
                );
                Ok(())
            }
        }
    }

    /// Replace the latest `VarBind` of the instance `oid` already in the
    /// table, whoever owns it, taking no ownership. `false` when no instance
    /// has that OID.
    pub(crate) fn overwrite(&self, oid: &Oid, varbind: Bytes) -> bool {
        match self.write().slots.get_mut(oid) {
            Some(slot) => {
                slot.varbind = varbind;
                true
            }
            None => false,
        }
    }

    /// Register `object` as an object instances are published under.
    /// Idempotent; an object is never removed.
    pub(crate) fn register_object(&self, object: &Oid) {
        if self.read().objects.contains(object) {
            return;
        }
        self.write().objects.insert(object.clone());
    }

    /// Release one publisher of the instance `oid` if `owner` owns it; the
    /// instance leaves the table with its last publisher. An OID that is
    /// unknown or owned by another launch is ignored.
    pub(crate) fn release(&self, owner: Owner, oid: &Oid) {
        let mut table = self.write();
        let Some(slot) = table.slots.get_mut(oid).filter(|slot| slot.owner == owner) else {
            return;
        };
        slot.publishers = slot.publishers.saturating_sub(1);
        if slot.publishers == 0 {
            table.slots.remove(oid);
        }
    }

    /// Count `datagram` under its PDU type, or as malformed.
    fn count_request(&self, datagram: &[u8]) {
        let counters = &self.counters;
        let counter = match Message::decode(datagram) {
            Ok(message) => match message.pdu {
                Tag::GetRequest => &counters.get,
                Tag::GetNextRequest => &counters.get_next,
                Tag::GetBulkRequest => &counters.get_bulk,
                Tag::SetRequest => &counters.set_refused,
                _ => &counters.malformed,
            },
            Err(_) => &counters.malformed,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, MibTable> {
        self.table.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, MibTable> {
        self.table.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The read side, which answers requests from the table.
#[cfg_attr(not(test), allow(dead_code))]
impl SnmpAgent {
    /// What the agent serves for `oid`: the instance's `VarBind`; else
    /// [`Lookup::NoSuchInstance`] when a registered object equals `oid` or
    /// is a prefix of it; else [`Lookup::NoSuchObject`].
    pub(crate) fn get(&self, oid: &Oid) -> Lookup {
        if let Some(instance) = self.system.iter().find(|instance| instance.oid == *oid) {
            return Lookup::Value(self.system_varbind(instance));
        }
        let table = self.read();
        if let Some(slot) = table.slots.get(oid) {
            return Lookup::Value(slot.varbind.clone());
        }
        let arcs = oid.arcs();
        if (2..=arcs.len()).any(|len| table.objects.contains(&arcs[..len])) {
            Lookup::NoSuchInstance
        } else {
            Lookup::NoSuchObject
        }
    }

    /// The instance with the smallest OID strictly greater than `oid`, and
    /// its `VarBind`, among the published instances and the system group;
    /// `None` past the last.
    pub(crate) fn next_after(&self, oid: &Oid) -> Option<(Oid, Bytes)> {
        let system = self.system.iter().find(|instance| instance.oid > *oid);
        let table = self.read();
        let published = table
            .slots
            .range::<[u32], _>((Bound::Excluded(oid.arcs()), Bound::Unbounded))
            .next();
        match (published, system) {
            (Some((next, slot)), Some(instance)) if *next < instance.oid => {
                Some((next.clone(), slot.varbind.clone()))
            }
            (_, Some(instance)) => Some((instance.oid.clone(), self.system_varbind(instance))),
            (Some((next, slot)), None) => Some((next.clone(), slot.varbind.clone())),
            (None, None) => None,
        }
    }

    /// Hundredths of a second since the socket was bound, wrapping at
    /// 2³²: the value of `sysUpTime`.
    pub(crate) fn uptime_ticks(&self) -> u32 {
        let centiseconds = self.started.elapsed().as_millis() / 10;
        // TimeTicks wrap; keep the low 32 bits.
        (centiseconds % (u128::from(u32::MAX) + 1)) as u32
    }

    /// The `VarBind` of a system-group instance, with `sysUpTime` read now.
    fn system_varbind(&self, instance: &SystemInstance) -> Bytes {
        match &instance.varbind {
            Some(varbind) => varbind.clone(),
            None => {
                let mut value = Vec::with_capacity(6);
                ber::write_timeticks(&mut value, self.uptime_ticks());
                varbind_of(&instance.oid, &value)
            }
        }
    }
}

impl ListenerTarget for SnmpAgent {
    type Config = AgentSettings;
    type SocketState = watch::Receiver<bool>;
    const SINK_TYPE: &'static str = "snmp_agent";

    fn bind(cfg: AgentSettings) -> impl Future<Output = Result<Arc<Self>, SondaError>> + Send {
        SnmpAgent::bind(cfg)
    }

    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    fn is_live(&self) -> bool {
        self.is_serving()
    }

    /// `Ok` when every setting but the listen address is equal; otherwise a
    /// [`SondaError::Config`] naming the settings that differ, the running
    /// agent's first, and never a community.
    fn serves(&self, wanted: &AgentSettings) -> Result<(), SondaError> {
        let running = self.settings();
        if running.serves(wanted) {
            return Ok(());
        }
        Err(SondaError::Config(ConfigError::invalid(format!(
            "snmp_agent {} is already serving with different settings ({}, the running \
             agent's first); every sink on one address must use the same settings",
            self.local_addr,
            running.beside(wanted),
        ))))
    }

    fn socket_state(&self) -> watch::Receiver<bool> {
        self.state.clone()
    }

    fn socket_open(state: &watch::Receiver<bool>) -> bool {
        *state.borrow()
    }

    async fn closed(mut state: watch::Receiver<bool>) {
        let _ = state.wait_for(|open| !open).await;
    }
}

impl Drop for SnmpAgent {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// The [`SondaError::Sink`] for a socket that could not bind `listen`. A
/// refused permission names the port and `CAP_NET_BIND_SERVICE`, which is
/// what binding a port below 1024 takes.
fn bind_error(listen: SocketAddr, e: io::Error) -> SondaError {
    let message = if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "snmp_agent cannot listen on {listen}: {e}; port {} needs CAP_NET_BIND_SERVICE \
             (or root) to bind, or use a port above 1023 such as 1161",
            listen.port()
        )
    } else {
        format!("snmp_agent cannot listen on {listen}: {e}")
    };
    SondaError::Sink(io::Error::new(e.kind(), message))
}

/// The OID of the object `instance` belongs to: the instance without its
/// last arc.
fn object_of(instance: &Oid) -> Oid {
    let arcs = instance.arcs();
    Oid::from_arcs(arcs[..arcs.len() - 1].to_vec()).unwrap_or_else(|| instance.clone())
}

/// A `VarBind` of `oid` and the already-encoded value TLV `value`.
fn varbind_of(oid: &Oid, value: &[u8]) -> Bytes {
    let mut varbind = Vec::with_capacity(ber::varbind_len(oid, value.len()));
    ber::write_varbind(&mut varbind, oid, value);
    Bytes::from(varbind)
}

/// The system group's seven instances, `1.3.6.1.2.1.1.N.0`, in OID order:
/// `sysDescr`, `sysObjectID`, `sysUpTime` (made on each read), `sysContact`,
/// `sysName`, `sysLocation` and `sysServices`.
fn system_instances(settings: &AgentSettings) -> Vec<SystemInstance> {
    let system = &settings.system;
    let string = |text: &str| {
        let mut value = Vec::new();
        ber::write_string(&mut value, text.as_bytes());
        value
    };
    let object_id = {
        let mut value = Vec::new();
        ber::write_oid(&mut value, &system.object_id);
        value
    };
    let services = {
        let mut value = Vec::new();
        ber::write_integer(&mut value, i32::from(system.services));
        value
    };
    let values: [(u32, Option<Vec<u8>>); 7] = [
        (1, Some(string(&system.descr))),
        (2, Some(object_id)),
        (SYS_UP_TIME, None),
        (4, Some(string(&system.contact))),
        (5, Some(string(&system.name))),
        (6, Some(string(&system.location))),
        (7, Some(services)),
    ];
    values
        .into_iter()
        .filter_map(|(arc, value)| {
            let mut arcs = SYSTEM_GROUP.to_vec();
            arcs.extend([arc, 0]);
            let oid = Oid::from_arcs(arcs)?;
            let varbind = value.map(|value| varbind_of(&oid, &value));
            Some(SystemInstance { oid, varbind })
        })
        .collect()
}

/// What the responder task owns. The socket is a field before `_closed`, so
/// however the task ends the socket closes before the agent reads as
/// stopped.
struct Responder {
    socket: UdpSocket,
    buffer: Vec<u8>,
    _closed: ClosedOnDrop,
}

/// Reports the socket closed when dropped, however the responder task ends:
/// by returning, by panicking, or by being dropped with its runtime.
struct ClosedOnDrop(watch::Sender<bool>);

impl Drop for ClosedOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

/// Read datagrams until `token` is cancelled or the agent is gone, counting
/// each and answering none.
async fn respond(mut responder: Responder, agent: Weak<SnmpAgent>, token: CancellationToken) {
    loop {
        let received = tokio::select! {
            () = token.cancelled() => break,
            received = responder.socket.recv_from(&mut responder.buffer) => received,
        };
        let Some(agent) = agent.upgrade() else {
            break;
        };
        match received {
            Ok((len, _peer)) => agent.count_request(&responder.buffer[..len]),
            Err(e) => {
                tracing::debug!(error = %e, addr = %agent.local_addr, "snmp_agent receive failed");
                drop(agent);
                tokio::select! {
                    () = token.cancelled() => break,
                    () = tokio::time::sleep(RECEIVE_BACKOFF) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::snmp::{instance_oid, SnmpAgentConfig, SystemGroup};

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("hex digits are ASCII");
                u8::from_str_radix(pair, 16).expect("hex digits")
            })
            .collect()
    }

    fn oid(text: &str) -> Oid {
        Oid::parse(text).expect("a valid OID")
    }

    /// A `VarBind` of `oid` holding the Counter64 `value`.
    fn counter64(oid: &Oid, value: u64) -> Bytes {
        let mut encoded = Vec::new();
        ber::write_counter64(&mut encoded, value);
        varbind_of(oid, &encoded)
    }

    async fn agent_with(cfg: SnmpAgentConfig) -> Arc<SnmpAgent> {
        SnmpAgent::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    async fn agent() -> Arc<SnmpAgent> {
        agent_with(SnmpAgentConfig::new("127.0.0.1:0")).await
    }

    fn owner() -> Owner {
        Owner::from(LaunchId::next())
    }

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

    #[tokio::test]
    async fn publish_then_get_returns_the_varbind() {
        let agent = agent().await;
        let instance = oid("1.3.6.1.2.1.31.1.1.1.6.3");
        let varbind = counter64(&instance, 1_250_000);
        agent
            .publish(owner(), &instance, varbind.clone(), true)
            .expect("the first publish stores the instance");
        assert_eq!(agent.get(&instance), Lookup::Value(varbind));
        assert_eq!(agent.instance_count(), 1);
    }

    /// A walk by `next_after` from `0.0` visits every published instance and
    /// the system group once each, in OID order, which compares arcs as
    /// numbers, and ends after the last.
    #[tokio::test]
    async fn next_after_walks_publishes_and_the_system_group_in_one_order() {
        let agent = agent().await;
        let owner = owner();
        let published = [
            "1.0.8802.1.1.2.1.1.1.0",
            "1.3.6.1.2.1.2.2.1.8.10",
            "1.3.6.1.2.1.2.2.1.8.2",
            "1.3.6.1.2.1.2.2.1.8.1",
            "1.3.6.1.4.1.8072.9999.1.1.0",
        ];
        for text in published {
            let instance = oid(text);
            let varbind = counter64(&instance, 1);
            agent
                .publish(owner, &instance, varbind, true)
                .expect("publishes");
        }

        let mut walked = Vec::new();
        let mut at = oid("0.0");
        while let Some((next, varbind)) = agent.next_after(&at) {
            assert!(next > at, "{next} follows {at}");
            let named = instance_oid(&varbind).expect("a VarBind");
            assert_eq!(named, next, "the VarBind is the instance's");
            walked.push(next.clone());
            at = next;
        }
        let mut expected: Vec<Oid> = published.iter().chain(&SYSTEM).map(|t| oid(t)).collect();
        expected.sort();
        assert!(walked.len() >= 5, "walked {walked:?}");
        assert_eq!(walked, expected);
        assert_eq!(walked[1], oid(SYSTEM[0]), "the system group follows LLDP");
        assert_eq!(walked[8], oid("1.3.6.1.2.1.2.2.1.8.1"));
        assert_eq!(walked[10], oid("1.3.6.1.2.1.2.2.1.8.10"), "10 after 2");
        assert!(agent.next_after(&at).is_none(), "nothing follows {at}");
    }

    /// A second launch's publish of an instance another launch owns is a
    /// config error naming the OID and the address, and changes nothing.
    #[tokio::test]
    async fn a_second_launch_publishing_an_owned_oid_is_a_config_error() {
        let agent = agent().await;
        let instance = oid("1.3.6.1.2.1.31.1.1.1.6.3");
        agent
            .publish(owner(), &instance, counter64(&instance, 1), true)
            .expect("the first launch publishes");

        let err = agent
            .publish(owner(), &instance, counter64(&instance, 2), true)
            .expect_err("another launch is refused the instance");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("1.3.6.1.2.1.31.1.1.1.6.3")
                && msg.contains(&agent.local_addr().to_string()),
            "{msg}"
        );
        assert_eq!(
            agent.get(&instance),
            Lookup::Value(counter64(&instance, 1)),
            "the owner's value stays"
        );
    }

    /// An instance two sinks of one launch published leaves with the second
    /// release; another launch's release is ignored. Its object stays
    /// registered, so the instance then answers "no such instance".
    #[tokio::test]
    async fn releasing_the_last_publisher_removes_the_instance_and_keeps_the_object() {
        let agent = agent().await;
        let owner = owner();
        let instance = oid("1.3.6.1.2.1.2.2.1.14.1");
        let object = oid("1.3.6.1.2.1.2.2.1.14");
        agent.register_object(&object);
        for value in [1, 2] {
            agent
                .publish(owner, &instance, counter64(&instance, value), true)
                .expect("two publishers of one launch");
        }
        assert_eq!(agent.get(&instance), Lookup::Value(counter64(&instance, 2)));

        agent.release(owner, &instance);
        assert!(matches!(agent.get(&instance), Lookup::Value(_)), "one left");
        agent.release(self::owner(), &instance);
        assert!(
            matches!(agent.get(&instance), Lookup::Value(_)),
            "another launch releases nothing"
        );
        agent.release(owner, &instance);
        assert_eq!(agent.instance_count(), 0);
        assert_eq!(agent.get(&instance), Lookup::NoSuchInstance);
        assert_eq!(agent.get(&object), Lookup::NoSuchInstance);
    }

    /// No sink may publish under `1.3.6.1.2.1.1`, which the agent serves.
    #[rstest::rstest]
    #[case::sys_name("1.3.6.1.2.1.1.5.0")]
    #[case::sys_or_table("1.3.6.1.2.1.1.9.1.2.1")]
    #[tokio::test]
    async fn a_publish_under_the_system_group_is_refused(#[case] text: &str) {
        let agent = agent().await;
        let sys_name = agent.get(&oid(SYSTEM[4]));
        assert!(matches!(sys_name, Lookup::Value(_)), "the agent serves it");
        let instance = oid(text);
        let err = agent
            .publish(owner(), &instance, counter64(&instance, 1), true)
            .expect_err("the system group is the agent's");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(err.to_string().contains("system group"), "{err}");
        assert_eq!(agent.instance_count(), 0);
        assert_eq!(agent.get(&oid(SYSTEM[4])), sys_name);
    }

    #[tokio::test(start_paused = true)]
    async fn uptime_ticks_advance_with_the_clock() {
        let agent = agent().await;
        let start = agent.uptime_ticks();
        tokio::time::advance(Duration::from_millis(1_500)).await;
        assert_eq!(agent.uptime_ticks() - start, 150);
        tokio::time::advance(Duration::from_millis(9)).await;
        assert_eq!(agent.uptime_ticks() - start, 150, "whole hundredths only");
    }

    /// `snmptrap -v2c -c public 127.0.0.1:<port> 0 1.3.6.1.4.1.8072.9999.0.3`
    /// with the system group as varbinds (`1.3.6.1.2.1.1.1.0 s "Sonda
    /// synthetic router"`, `1.3.6.1.2.1.1.2.0 o 1.3.6.1.4.1.8072.3.2.10`,
    /// `1.3.6.1.2.1.1.3.0 t 852`, `1.3.6.1.2.1.1.4.0 s noc@example.com`,
    /// `1.3.6.1.2.1.1.5.0 s rtr-core-01`, `1.3.6.1.2.1.1.6.0 s lab-1`,
    /// `1.3.6.1.2.1.1.7.0 i 72`), net-snmp 5.9.4.
    const NET_SNMP_SYSTEM_TRAP: &str =
        "3081eb02010104067075626c6963a781dd02046f9101ae0201000201003081ce300d06082b060102\
        010103004301003019060a2b060106030101040100060b2b06010401bf08ce0f0003302206082b06\
        0102010101000416536f6e64612073796e74686574696320726f75746572301606082b0601020101\
        0200060a2b06010401bf0803020a300e06082b0601020101030043020354301b06082b0601020101\
        0400040f6e6f63406578616d706c652e636f6d301706082b06010201010500040b7274722d636f72\
        652d3031301106082b0601020101060004056c61622d31300d06082b06010201010700020148";

    /// Each system-group instance is served as net-snmp encodes the same
    /// value, `sysUpTime` read at 8.52 s after bind.
    #[tokio::test(start_paused = true)]
    async fn the_system_group_is_served_as_net_snmp_encodes_it() {
        let trap = hex(NET_SNMP_SYSTEM_TRAP);
        assert_eq!(trap.first(), Some(&0x30), "fixture present");
        let agent = agent_with(SnmpAgentConfig {
            system: SystemGroup {
                name: "rtr-core-01".to_string(),
                descr: "Sonda synthetic router".to_string(),
                contact: "noc@example.com".to_string(),
                location: "lab-1".to_string(),
                ..SystemGroup::default()
            },
            ..SnmpAgentConfig::new("127.0.0.1:0")
        })
        .await;
        tokio::time::advance(Duration::from_millis(8_520)).await;
        assert_eq!(agent.uptime_ticks(), 852);

        let expected = [
            "302206082b060102010101000416536f6e64612073796e74686574696320726f75746572",
            "301606082b06010201010200060a2b06010401bf0803020a",
            "300e06082b0601020101030043020354",
            "301b06082b06010201010400040f6e6f63406578616d706c652e636f6d",
            "301706082b06010201010500040b7274722d636f72652d3031",
            "301106082b0601020101060004056c61622d31",
            "300d06082b06010201010700020148",
        ];
        for (instance, varbind) in SYSTEM.iter().zip(expected) {
            let varbind = hex(varbind);
            assert!(
                trap.windows(varbind.len()).any(|w| w == varbind),
                "the capture carries {instance}"
            );
            assert_eq!(
                agent.get(&oid(instance)),
                Lookup::Value(Bytes::from(varbind)),
                "{instance}"
            );
        }
    }

    /// A refused permission names the port and the capability; any other
    /// bind error names neither.
    #[test]
    fn a_refused_permission_names_the_port_and_cap_net_bind_service() {
        let listen: SocketAddr = "0.0.0.0:161".parse().expect("an address");
        let err = bind_error(listen, io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("0.0.0.0:161")
                && msg.contains("port 161")
                && msg.contains("CAP_NET_BIND_SERVICE"),
            "{msg}"
        );
        let other = bind_error(listen, io::Error::from(io::ErrorKind::AddrInUse)).to_string();
        assert!(
            other.contains("0.0.0.0:161") && !other.contains("CAP_NET_BIND_SERVICE"),
            "{other}"
        );
    }

    /// Binding SNMP's port without the permission to is a sink error naming
    /// the capability. Runs only where port 161 is unbindable, which the test
    /// establishes first.
    #[tokio::test]
    async fn binding_port_161_without_permission_names_cap_net_bind_service() {
        match std::net::UdpSocket::bind("127.0.0.1:161") {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
            other => {
                println!("skipped: binding 127.0.0.1:161 gave {other:?}, not a refused permission");
                return;
            }
        }
        let settings = SnmpAgentConfig::new("127.0.0.1:161")
            .settings()
            .expect("valid settings");
        let err = SnmpAgent::bind(settings)
            .await
            .err()
            .expect("port 161 is refused");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("port 161") && msg.contains("CAP_NET_BIND_SERVICE"),
            "{msg}"
        );
    }
}
