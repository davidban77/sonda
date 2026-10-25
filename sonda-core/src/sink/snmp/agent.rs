//! The shared state behind one SNMP listen address.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::usm::Usm;
use super::{responder, AgentSettings, SnmpVersion};
use crate::encoder::snmp::ber::{self, Reader, Tag};
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

/// The most octets `sysUpTime`'s `VarBind` takes: a two-octet SEQUENCE
/// header, the ten-octet OID and a TimeTicks of five content octets.
const UPTIME_VARBIND_MAX: usize = 19;

/// How long the agent waits before logging the same warning again.
pub(crate) const WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Whether a warning last logged at `last` may be logged again at `now`.
pub(crate) fn warn_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.duration_since(at) >= WARN_INTERVAL)
}

/// The launch an instance belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner(u64);

impl From<LaunchId> for Owner {
    fn from(launch: LaunchId) -> Self {
        Self(launch.get())
    }
}

/// What [`SnmpAgent::lookup`] finds for an OID, borrowed from the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Found<'a> {
    /// The instance's `VarBind`, as encoded.
    Value(&'a [u8]),
    /// No such instance, but the OID is a registered object or under one.
    NoSuchInstance,
    /// The OID is neither an instance nor under a registered object.
    NoSuchObject,
}

/// What [`SnmpAgent::get`] finds for an OID: [`Found`], owned.
#[cfg(test)]
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
///
/// A datagram is checked in this order: its shape, its version, its
/// community, then its PDU. It is counted once, under the first check it
/// fails or under its PDU type; a request answered `tooBig` is also counted
/// under [`too_big`](Self::too_big), one answered as SNMPv1 under
/// [`v1`](Self::v1), and one answered as SNMPv3 under [`v3`](Self::v3).
/// The shape comes first: an SNMPv1 message carrying a `GetBulkRequest` or
/// a `Trap` is [`malformed`](Self::malformed) whatever versions the agent
/// serves and whatever its community.
///
/// An SNMPv3 message to an agent serving SNMPv3 is checked for its shape,
/// then by the User-based Security Model in this order: its engine ID, its
/// privacy flag, its user, its security level, its digest, its time window.
/// The first check it fails is counted under the `usm_` counter of the
/// `Report` it is answered with; a message that passes is answered and
/// counted under its PDU type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RequestCounters {
    /// `GetRequest` PDUs answered, SNMPv1 and SNMPv2c.
    pub get: u64,
    /// `GetNextRequest` PDUs answered, SNMPv1 and SNMPv2c.
    pub get_next: u64,
    /// `GetBulkRequest` PDUs answered.
    pub get_bulk: u64,
    /// `SetRequest` PDUs, each answered `notWritable` (SNMPv1: `noSuchName`)
    /// and none applied.
    pub set_refused: u64,
    /// Datagrams that are not an SNMP message of a version the agent knows,
    /// messages carrying a PDU it does not answer (`Response`,
    /// `InformRequest`, `SNMPv2-Trap`, `Report`), and SNMPv1 messages
    /// carrying a `GetBulkRequest` or a `Trap`. Dropped unanswered.
    pub malformed: u64,
    /// SNMPv1 and SNMPv2c messages whose community is not the agent's.
    /// Dropped unanswered.
    pub wrong_community: u64,
    /// `GetRequest`, `GetNextRequest` and `SetRequest` PDUs answered
    /// `tooBig`, because their answer did not fit `max_message_size`. A
    /// `GetBulkRequest` answer is trimmed to fit instead.
    pub too_big: u64,
    /// Messages in a version the agent does not serve. Dropped unanswered,
    /// the community never compared and an SNMPv3 message never parsed past
    /// its version.
    pub unsupported_version: u64,
    /// Answers the socket failed to send.
    pub send_errors: u64,
    /// Requests answered as SNMPv1, also counted under their PDU type.
    pub v1: u64,
    /// Requests answered as SNMPv3, also counted under their PDU type.
    pub v3: u64,
    /// SNMPv3 messages whose engine ID was empty (discovery) or not the
    /// agent's, answered with a `usmStatsUnknownEngineIDs` report.
    pub usm_unknown_engine_ids: u64,
    /// SNMPv3 messages from a user the agent does not have, answered with a
    /// `usmStatsUnknownUserNames` report.
    pub usm_unknown_user_names: u64,
    /// SNMPv3 messages asking for privacy, or for a security level other
    /// than their user's, answered with a `usmStatsUnsupportedSecLevels`
    /// report.
    pub usm_unsupported_sec_levels: u64,
    /// SNMPv3 messages whose digest was not their user's, answered with a
    /// `usmStatsWrongDigests` report.
    pub usm_wrong_digests: u64,
    /// Authenticated SNMPv3 messages outside the agent's time window,
    /// answered with a `usmStatsNotInTimeWindows` report.
    pub usm_not_in_time_windows: u64,
}

/// One of the counters [`RequestCounters`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counter {
    Get,
    GetNext,
    GetBulk,
    SetRefused,
    Malformed,
    WrongCommunity,
    TooBig,
    UnsupportedVersion,
    SendErrors,
    V1,
    V3,
    UsmUnknownEngineIds,
    UsmUnknownUserNames,
    UsmUnsupportedSecLevels,
    UsmWrongDigests,
    UsmNotInTimeWindows,
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
    unsupported_version: AtomicU64,
    send_errors: AtomicU64,
    v1: AtomicU64,
    v3: AtomicU64,
    usm_unknown_engine_ids: AtomicU64,
    usm_unknown_user_names: AtomicU64,
    usm_unsupported_sec_levels: AtomicU64,
    usm_wrong_digests: AtomicU64,
    usm_not_in_time_windows: AtomicU64,
}

impl Counters {
    fn counter(&self, counter: Counter) -> &AtomicU64 {
        match counter {
            Counter::Get => &self.get,
            Counter::GetNext => &self.get_next,
            Counter::GetBulk => &self.get_bulk,
            Counter::SetRefused => &self.set_refused,
            Counter::Malformed => &self.malformed,
            Counter::WrongCommunity => &self.wrong_community,
            Counter::TooBig => &self.too_big,
            Counter::UnsupportedVersion => &self.unsupported_version,
            Counter::SendErrors => &self.send_errors,
            Counter::V1 => &self.v1,
            Counter::V3 => &self.v3,
            Counter::UsmUnknownEngineIds => &self.usm_unknown_engine_ids,
            Counter::UsmUnknownUserNames => &self.usm_unknown_user_names,
            Counter::UsmUnsupportedSecLevels => &self.usm_unsupported_sec_levels,
            Counter::UsmWrongDigests => &self.usm_wrong_digests,
            Counter::UsmNotInTimeWindows => &self.usm_not_in_time_windows,
        }
    }

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
            unsupported_version: read(&self.unsupported_version),
            send_errors: read(&self.send_errors),
            v1: read(&self.v1),
            v3: read(&self.v3),
            usm_unknown_engine_ids: read(&self.usm_unknown_engine_ids),
            usm_unknown_user_names: read(&self.usm_unknown_user_names),
            usm_unsupported_sec_levels: read(&self.usm_unsupported_sec_levels),
            usm_wrong_digests: read(&self.usm_wrong_digests),
            usm_not_in_time_windows: read(&self.usm_not_in_time_windows),
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
/// The socket is read by one responder task, which answers `Get`, `GetNext`,
/// `GetBulk` and `Set` requests in the [`versions`](Self::versions) it
/// serves from the table under its read lock, drops everything else, and
/// counts each datagram in [`requests`](Self::requests). Dropping the last
/// handle stops the task and closes the socket.
///
/// For SNMPv3 the agent is the authoritative engine of
/// [`engine_id`](Self::engine_id): its `engineBoots` is 1 from every bind,
/// because boots are not persisted, and its `engineTime` counts whole
/// seconds from the bind.
pub struct SnmpAgent {
    local_addr: SocketAddr,
    settings: AgentSettings,
    /// When the socket was bound; `sysUpTime` and the engine's time count
    /// from here.
    started: Instant,
    /// The SNMPv3 engine and users, the users' keys localized at bind.
    usm: Usm,
    /// The system group's seven instances, in OID order.
    system: Vec<SystemInstance>,
    table: RwLock<MibTable>,
    /// Where `sysUpTime`'s `VarBind` is made on each read.
    uptime: Mutex<Vec<u8>>,
    counters: Counters,
    /// When the last `tooBig` warning was logged.
    too_big_warned: Mutex<Option<Instant>>,
    token: CancellationToken,
    /// Whether the socket is open: `true` until the responder task ends.
    state: watch::Receiver<bool>,
}

impl SnmpAgent {
    /// Bind `settings.listen` and start the responder on a spawned task. On a
    /// wildcard address the socket reports each datagram's packet info, so
    /// the responder answers from the address each request was sent to;
    /// where it cannot, a warning says so once and responses leave from the
    /// address the route chooses.
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
        if responder::wants_pktinfo(local_addr) {
            if let Err(e) = responder::enable_pktinfo(&socket, local_addr) {
                tracing::warn!(
                    addr = %local_addr,
                    error = %e,
                    "snmp_agent on a wildcard address cannot answer from the address each \
                     request was sent to here ({e}); responses leave from the address the \
                     route chooses, which a client with a connected socket drops"
                );
            }
        }
        if listen.port() == 0 {
            tracing::warn!(addr = %local_addr, "snmp_agent listening on an ephemeral port");
        } else {
            tracing::info!(addr = %local_addr, "snmp_agent listening");
        }
        let system = system_instances(&settings);
        let started = Instant::now();
        // Each user's key is localized here, once: a megabyte of hashing.
        let usm = Usm::new(&settings, local_addr, started);
        let table = MibTable {
            slots: BTreeMap::new(),
            objects: system
                .iter()
                .map(|instance| object_of(&instance.oid))
                .collect(),
        };
        let token = CancellationToken::new();
        let (state_tx, state) = watch::channel(true);
        Ok(Arc::new_cyclic(|agent| {
            let (agent, cancel) = (agent.clone(), token.clone());
            tokio::spawn(async move {
                // Declared before the socket moves into `serve`, so it drops
                // after it however the task ends: the socket is closed
                // before the agent reads as stopped.
                let _closed = ClosedOnDrop(state_tx);
                responder::serve(socket, agent, cancel).await;
            });
            SnmpAgent {
                local_addr,
                settings,
                started,
                usm,
                system,
                table: RwLock::new(table),
                uptime: Mutex::new(Vec::with_capacity(UPTIME_VARBIND_MAX)),
                counters: Counters::default(),
                too_big_warned: Mutex::new(None),
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

    /// The SNMP versions the agent answers, ascending.
    pub fn versions(&self) -> &[SnmpVersion] {
        &self.settings.versions
    }

    /// The agent's SNMPv3 engine ID: the configured one, or one derived
    /// from the address the socket bound.
    pub fn engine_id(&self) -> &[u8] {
        self.usm.engine().id()
    }

    /// The SNMPv3 engine and users.
    pub(crate) fn usm(&self) -> &Usm {
        &self.usm
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

    /// Add one to `counter`.
    pub(crate) fn count(&self, counter: Counter) {
        self.counters
            .counter(counter)
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Add one to `counter` and return its new value.
    pub(crate) fn count_and_read(&self, counter: Counter) -> u64 {
        self.counters
            .counter(counter)
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    /// Log at warn that a request whose first OID is `first` was answered
    /// `tooBig` under `budget`, unless that was logged less than
    /// [`WARN_INTERVAL`] ago.
    pub(crate) fn warn_too_big(&self, first: &dyn std::fmt::Display, budget: usize) {
        let now = Instant::now();
        {
            let mut warned = self
                .too_big_warned
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !warn_due(*warned, now) {
                return;
            }
            *warned = Some(now);
        }
        tracing::warn!(
            addr = %self.local_addr,
            oid = %first,
            max_message_size = budget,
            "snmp_agent answered tooBig: the answer to a request for {first} does not fit \
             max_message_size {budget}"
        );
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, MibTable> {
        self.table.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, MibTable> {
        self.table.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The read side, which answers requests from the table under its read lock.
impl SnmpAgent {
    /// Call `answer` with what the agent serves for `oid`: the instance's
    /// `VarBind`; else [`Found::NoSuchInstance`] when a registered object
    /// equals `oid` or is a prefix of it; else [`Found::NoSuchObject`].
    ///
    /// Takes the table's read lock, and holds it while `answer` runs.
    pub(crate) fn lookup<R>(&self, oid: &[u32], answer: impl FnOnce(Found<'_>) -> R) -> R {
        if let Some(instance) = self.system.iter().find(|i| i.oid.arcs() == oid) {
            return self.with_system_varbind(instance, |varbind| answer(Found::Value(varbind)));
        }
        let table = self.read();
        if let Some(slot) = table.slots.get(oid) {
            return answer(Found::Value(&slot.varbind));
        }
        if (2..=oid.len()).any(|len| table.objects.contains(&oid[..len])) {
            answer(Found::NoSuchInstance)
        } else {
            answer(Found::NoSuchObject)
        }
    }

    /// Call `emit` with each instance after `start`, and its `VarBind`, in
    /// OID order, among the published instances and the system group: at
    /// most `max` of them, and none after `emit` returns `false`. With
    /// `skip_counter64`, an instance whose value is a `Counter64` is passed
    /// over, as SNMPv1 requires, and does not count towards `max`.
    ///
    /// Takes the table's read lock once, and holds it until it returns.
    pub(crate) fn bulk(
        &self,
        start: &[u32],
        max: u32,
        skip_counter64: bool,
        mut emit: impl FnMut(&Oid, &[u8]) -> bool,
    ) {
        let table = self.read();
        let mut published = table
            .slots
            .range::<[u32], _>((Bound::Excluded(start), Bound::Unbounded))
            .peekable();
        let mut system = self
            .system
            .iter()
            .skip_while(|instance| instance.oid.arcs() <= start)
            .peekable();
        let mut emitted = 0;
        while emitted < max {
            // No published instance shares an OID with the system group.
            let from_system = match (published.peek(), system.peek()) {
                (Some((next, _)), Some(instance)) => instance.oid < **next,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                (None, None) => return,
            };
            let more = if from_system {
                let Some(instance) = system.next() else {
                    return;
                };
                // The system group holds no Counter64.
                self.with_system_varbind(instance, |varbind| emit(&instance.oid, varbind))
            } else {
                let Some((oid, slot)) = published.next() else {
                    return;
                };
                if skip_counter64 && is_counter64(&slot.varbind) {
                    continue;
                }
                emit(oid, &slot.varbind)
            };
            emitted += 1;
            if !more {
                return;
            }
        }
    }

    /// Hundredths of a second since the socket was bound, wrapping at
    /// 2³²: the value of `sysUpTime`.
    pub(crate) fn uptime_ticks(&self) -> u32 {
        let centiseconds = self.started.elapsed().as_millis() / 10;
        // TimeTicks wrap; keep the low 32 bits.
        (centiseconds % (u128::from(u32::MAX) + 1)) as u32
    }

    /// Call `f` with the `VarBind` of a system-group instance, `sysUpTime`
    /// read now.
    fn with_system_varbind<R>(&self, instance: &SystemInstance, f: impl FnOnce(&[u8]) -> R) -> R {
        if let Some(varbind) = &instance.varbind {
            return f(varbind);
        }
        let mut varbind = self.uptime.lock().unwrap_or_else(PoisonError::into_inner);
        varbind.clear();
        ber::write_tlv(&mut varbind, Tag::Sequence, |out| {
            ber::write_oid(out, &instance.oid);
            ber::write_timeticks(out, self.uptime_ticks());
        });
        f(&varbind)
    }
}

/// Owned forms of [`SnmpAgent::lookup`] and [`SnmpAgent::bulk`].
#[cfg(test)]
impl SnmpAgent {
    /// What the agent serves for `oid`, as [`SnmpAgent::lookup`] finds it.
    pub(crate) fn get(&self, oid: &Oid) -> Lookup {
        self.lookup(oid.arcs(), |found| match found {
            Found::Value(varbind) => Lookup::Value(Bytes::copy_from_slice(varbind)),
            Found::NoSuchInstance => Lookup::NoSuchInstance,
            Found::NoSuchObject => Lookup::NoSuchObject,
        })
    }

    /// The instance with the smallest OID strictly greater than `oid`, and
    /// its `VarBind`, among the published instances and the system group;
    /// `None` past the last.
    pub(crate) fn next_after(&self, oid: &Oid) -> Option<(Oid, Bytes)> {
        let mut next = None;
        self.bulk(oid.arcs(), 1, false, |oid, varbind| {
            next = Some((oid.clone(), Bytes::copy_from_slice(varbind)));
            true
        });
        next
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

/// Whether the `VarBind` `varbind`'s value is a `Counter64`, read from the
/// value's identifier octet without decoding it. `false` for bytes that are
/// not a `VarBind`.
pub(crate) fn is_counter64(varbind: &[u8]) -> bool {
    let Ok(body) = Reader::new(varbind).expect(Tag::Sequence) else {
        return false;
    };
    let mut fields = Reader::new(body);
    if fields.expect(Tag::Oid).is_err() {
        return false;
    }
    matches!(fields.raw_tlv(), Ok((tag, _)) if tag == Tag::Counter64.byte())
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

/// Reports the socket closed when dropped, however the responder task ends:
/// by returning, by panicking, or by being dropped with its runtime.
struct ClosedOnDrop(watch::Sender<bool>);

impl Drop for ClosedOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(false);
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
