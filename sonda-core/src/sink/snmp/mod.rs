//! SNMP agent sink: serves the metric events it receives to SNMP pollers.
//!
//! A [`SnmpAgentSink`] publishes every event into an [`SnmpAgent`], the OID
//! table and UDP socket for one listen address. Sinks that name the same
//! address share one agent through the process-wide [`registry`], so their
//! instances appear as one device.
//!
//! The sink takes the `snmp` encoder's publish records through
//! [`Sink::write_metric_event`] and never awaits: publishing is an upsert
//! into the agent's table under its write lock. Once the agent has taken a
//! series' first record, the sink also registers the object its instance
//! belongs to, which the agent keeps after the instance leaves.
//!
//! An instance leaves the agent when the last sink of its launch that
//! published it drops, and at no other time.
//!
//! Requires the `snmp` feature flag.

pub mod agent;
mod responder;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};

use crate::encoder::snmp::ber::{Reader, Tag};
use crate::encoder::snmp::oid::Oid;
use crate::encoder::snmp::RECORD_HEADER;
use crate::model::metric::{MetricEvent, MetricKey};
use crate::schedule::LaunchId;
use crate::sink::listener_registry::{canonical, ListenerRegistry};
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

use agent::Owner;
pub use agent::{RequestCounters, SnmpAgent};

/// `community` when it is not set.
pub(crate) const DEFAULT_COMMUNITY: &str = "public";

/// `max_message_size` when it is not set: the UDP payload of an Ethernet
/// frame.
pub(crate) const DEFAULT_MAX_MESSAGE_SIZE: usize = 1472;

/// Smallest accepted `max_message_size`: the message size every SNMP
/// entity must accept (RFC 3416).
pub(crate) const MIN_MAX_MESSAGE_SIZE: usize = 484;

/// Largest accepted `max_message_size`: the most a UDP datagram over IPv4
/// carries.
pub(crate) const MAX_MAX_MESSAGE_SIZE: usize = 65_507;

/// `system.object_id` when it is not set: net-snmp's Linux agent.
pub(crate) const DEFAULT_SYS_OBJECT_ID: &str = "1.3.6.1.4.1.8072.3.2.10";

/// `system.services` when it is not set: an end-to-end host running
/// applications (RFC 3418).
pub(crate) const DEFAULT_SYS_SERVICES: u8 = 72;

/// Largest accepted `system.services`: the seven layer bits RFC 3418
/// defines.
pub(crate) const MAX_SYS_SERVICES: u8 = 127;

/// Configuration for [`SnmpAgentSink`]; the body of
/// `sink: { type: snmp_agent, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `comunity:` is an
/// error rather than a silent fallback to the defaults. Every sink on one
/// address must use the same settings.
///
/// `Debug` prints `"***"` in place of the community.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SnmpAgentConfig {
    /// Address to listen on, e.g. `"0.0.0.0:1161"`. `"127.0.0.1:0"` binds an
    /// ephemeral port, readable from [`SnmpAgent::local_addr`]. A port below
    /// 1024, such as SNMP's 161, needs `CAP_NET_BIND_SERVICE`.
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub listen: String,
    /// The SNMPv2c community a request must carry. Default `"public"`.
    #[cfg_attr(
        feature = "config",
        serde(
            default = "default_community",
            deserialize_with = "crate::config::scalar_text::required"
        )
    )]
    pub community: String,
    /// The largest reply the agent sends, in bytes, 484 to 65507. Default
    /// 1472, the UDP payload of an Ethernet frame.
    #[cfg_attr(feature = "config", serde(default = "default_max_message_size"))]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = MIN_MAX_MESSAGE_SIZE, max = MAX_MAX_MESSAGE_SIZE))
    )]
    pub max_message_size: usize,
    /// The system group (`1.3.6.1.2.1.1`) the agent serves.
    #[cfg_attr(feature = "config", serde(default))]
    pub system: SystemGroup,
}

/// The `system:` block of an [`SnmpAgentConfig`]: the values of the system
/// group's scalars. `sysUpTime` is not configured: it counts from when the
/// agent bound its socket.
///
/// Every field is optional. The text fields default to the empty string.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SystemGroup {
    /// `sysName`: the device's name.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::required")
    )]
    pub name: String,
    /// `sysDescr`: a description of the device.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::required")
    )]
    pub descr: String,
    /// `sysContact`: who to contact about the device.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::required")
    )]
    pub contact: String,
    /// `sysLocation`: where the device is.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::required")
    )]
    pub location: String,
    /// `sysObjectID`: the device's vendor OID, in dotted decimal. A string;
    /// a YAML number is an error. Default `"1.3.6.1.4.1.8072.3.2.10"`.
    #[cfg_attr(
        feature = "config",
        serde(default = "default_object_id", deserialize_with = "object_id")
    )]
    #[cfg_attr(feature = "schema", schemars(extend("x-sonda-no-widening" = true)))]
    pub object_id: String,
    /// `sysServices`: the layers the device serves, 0 to 127. Default 72.
    #[cfg_attr(feature = "config", serde(default = "default_services"))]
    #[cfg_attr(feature = "schema", schemars(range(max = MAX_SYS_SERVICES)))]
    pub services: u8,
}

#[cfg(feature = "config")]
fn default_community() -> String {
    DEFAULT_COMMUNITY.to_string()
}

#[cfg(feature = "config")]
fn default_max_message_size() -> usize {
    DEFAULT_MAX_MESSAGE_SIZE
}

#[cfg(feature = "config")]
fn default_object_id() -> String {
    DEFAULT_SYS_OBJECT_ID.to_string()
}

#[cfg(feature = "config")]
fn default_services() -> u8 {
    DEFAULT_SYS_SERVICES
}

/// `deserialize_with` for `system.object_id`: a string only
/// ([`OidText`](crate::config::scalar_text::OidText)).
#[cfg(feature = "config")]
fn object_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de::DeserializeSeed;
    crate::config::scalar_text::OidText {
        field: "system.object_id",
    }
    .deserialize(deserializer)
}

impl Default for SystemGroup {
    /// Empty text fields, `object_id` `1.3.6.1.4.1.8072.3.2.10`, `services`
    /// 72.
    fn default() -> Self {
        Self {
            name: String::new(),
            descr: String::new(),
            contact: String::new(),
            location: String::new(),
            object_id: DEFAULT_SYS_OBJECT_ID.to_string(),
            services: DEFAULT_SYS_SERVICES,
        }
    }
}

/// Prints `"***"` in place of the community.
impl fmt::Debug for SnmpAgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnmpAgentConfig")
            .field("listen", &self.listen)
            .field("community", &"***")
            .field("max_message_size", &self.max_message_size)
            .field("system", &self.system)
            .finish()
    }
}

impl SnmpAgentConfig {
    /// A config listening on `listen` with every other field at its default.
    pub fn new(listen: impl Into<String>) -> Self {
        Self {
            listen: listen.into(),
            community: DEFAULT_COMMUNITY.to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            system: SystemGroup::default(),
        }
    }

    /// Resolve the config into the settings an agent runs with.
    ///
    /// This is the one place the fields are checked; validation and
    /// [`SnmpAgentSink::new`] both call it. Returns [`SondaError::Config`]
    /// when `listen` is not a socket address, `max_message_size` is outside
    /// 484 to 65507, `system.object_id` is not an OID, or `system.services`
    /// is above 127. An IPv4-mapped `listen` resolves to the IPv4 address it
    /// maps.
    pub(crate) fn settings(&self) -> Result<AgentSettings, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        let listen: SocketAddr = self.listen.parse().map_err(|_| {
            invalid(format!(
                "snmp_agent listen {:?} is not a socket address such as \"0.0.0.0:1161\"",
                self.listen
            ))
        })?;
        if !(MIN_MAX_MESSAGE_SIZE..=MAX_MAX_MESSAGE_SIZE).contains(&self.max_message_size) {
            return Err(invalid(format!(
                "snmp_agent max_message_size must be between {MIN_MAX_MESSAGE_SIZE} and \
                 {MAX_MAX_MESSAGE_SIZE}, got {}",
                self.max_message_size
            )));
        }
        let system = &self.system;
        let object_id = Oid::parse(&system.object_id)
            .map_err(|e| invalid(format!("snmp_agent system.object_id: {e}")))?;
        if system.services > MAX_SYS_SERVICES {
            return Err(invalid(format!(
                "snmp_agent system.services must be between 0 and {MAX_SYS_SERVICES}, got {}",
                system.services
            )));
        }
        Ok(AgentSettings {
            // An IPv4-mapped address binds the IPv4 socket it maps, so it is
            // that address everywhere a listen address is compared or keyed.
            listen: canonical(listen),
            community: self.community.clone(),
            max_message_size: self.max_message_size,
            system: SystemSettings {
                name: system.name.clone(),
                descr: system.descr.clone(),
                contact: system.contact.clone(),
                location: system.location.clone(),
                object_id,
                services: system.services,
            },
        })
    }
}

/// An [`SnmpAgentConfig`] with every field resolved and checked.
///
/// `Debug` prints `"***"` in place of the community.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgentSettings {
    /// The address to bind.
    pub(crate) listen: SocketAddr,
    pub(crate) community: String,
    pub(crate) max_message_size: usize,
    pub(crate) system: SystemSettings,
}

/// A [`SystemGroup`] with its OID parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemSettings {
    pub(crate) name: String,
    pub(crate) descr: String,
    pub(crate) contact: String,
    pub(crate) location: String,
    pub(crate) object_id: Oid,
    pub(crate) services: u8,
}

impl fmt::Debug for AgentSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentSettings")
            .field("listen", &self.listen)
            .field("community", &"***")
            .field("max_message_size", &self.max_message_size)
            .field("system", &self.system)
            .finish()
    }
}

impl AgentSettings {
    /// Whether an agent running with `self` can also serve `other`: every
    /// setting but the listen address is equal, the community compared in
    /// constant time.
    pub(crate) fn serves(&self, other: &AgentSettings) -> bool {
        crate::util::constant_time_eq(self.community.as_bytes(), other.community.as_bytes())
            && self.max_message_size == other.max_message_size
            && self.system == other.system
    }

    /// The settings in which `self` and `other` differ, `self`'s first, for
    /// an error saying they differ: `community and another community,
    /// max_message_size 1472 and 484, system.name "a" and "b"`. A community
    /// is never printed. Both address-sharing checks word the conflict with
    /// it.
    pub(crate) fn beside(&self, other: &AgentSettings) -> String {
        let (a, b) = (&self.system, &other.system);
        let mut differences = Vec::new();
        if !crate::util::constant_time_eq(self.community.as_bytes(), other.community.as_bytes()) {
            differences.push("community and another community".to_string());
        }
        if self.max_message_size != other.max_message_size {
            differences.push(format!(
                "max_message_size {} and {}",
                self.max_message_size, other.max_message_size
            ));
        }
        for (field, a, b) in [
            ("name", &a.name, &b.name),
            ("descr", &a.descr, &b.descr),
            ("contact", &a.contact, &b.contact),
            ("location", &a.location, &b.location),
        ] {
            if a != b {
                differences.push(format!("system.{field} {a:?} and {b:?}"));
            }
        }
        if a.object_id != b.object_id {
            differences.push(format!(
                "system.object_id {} and {}",
                a.object_id, b.object_id
            ));
        }
        if a.services != b.services {
            differences.push(format!("system.services {} and {}", a.services, b.services));
        }
        differences.join(", ")
    }
}

/// The registry every [`SnmpAgentSink`] acquires its agent from. Created
/// empty on first use; nothing binds until the first acquire.
pub fn registry() -> &'static SnmpAgentRegistry {
    static REGISTRY: OnceLock<SnmpAgentRegistry> = OnceLock::new();
    REGISTRY.get_or_init(SnmpAgentRegistry::new)
}

/// Shares one [`SnmpAgent`] between every acquire that names its address.
///
/// The map holds weak handles, so an agent lives exactly as long as some
/// sink holds it. Dropping the last handle stops the agent's responder and
/// its socket closes; an acquire of the same port waits for that before
/// binding, and the stale entry is pruned once it has. An SNMP agent and a
/// gNMI target on the same port number do not contend: one is UDP, the
/// other TCP.
pub struct SnmpAgentRegistry(ListenerRegistry<SnmpAgent>);

impl Default for SnmpAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SnmpAgentRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self(ListenerRegistry::new())
    }

    /// The live agent for `cfg.listen`, binding and starting one if there
    /// is none. A listen address with port 0 always binds a new agent. An
    /// agent whose responder task has exited is not live, even while handles
    /// to it remain. Before binding, waits up to two seconds in all for the
    /// socket of any agent that is no longer live, on an address that
    /// overlaps this one, to close.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, when a live agent on the
    /// address was bound with different settings, or when a live agent on
    /// another address contends for the same port (a wildcard address and an
    /// address it covers); [`SondaError::Sink`] when the address cannot be
    /// bound.
    pub async fn acquire(&self, cfg: &SnmpAgentConfig) -> Result<Arc<SnmpAgent>, SondaError> {
        let settings = cfg.settings()?;
        let listen = settings.listen;
        self.0.acquire(settings, listen).await
    }

    /// Whether [`acquire`](Self::acquire) of `cfg` would be refused by the
    /// agents running now, without binding anything: the config is invalid,
    /// a live agent on the address runs with different settings, or a live
    /// agent on another address contends for the port.
    ///
    /// # Errors
    ///
    /// The [`SondaError::Config`] that `acquire` would return for those
    /// cases.
    pub fn check(&self, cfg: &SnmpAgentConfig) -> Result<(), SondaError> {
        let settings = cfg.settings()?;
        self.0.check(&settings, settings.listen)
    }

    /// The live agent that serves `cfg.listen`, without ever binding: the
    /// agent on that address, or a wildcard agent on the same port that
    /// covers it. Of the settings, only the community is compared with the
    /// agent's, in constant time; `max_message_size` and `system` are left
    /// to the running agent.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, when no live agent
    /// serves the address, or when the agent's community is another; the
    /// error names the field and neither community.
    pub fn join(&self, cfg: &SnmpAgentConfig) -> Result<Arc<SnmpAgent>, SondaError> {
        let settings = cfg.settings()?;
        self.0.join(settings.listen, |agent| AgentSettings {
            community: settings.community.clone(),
            ..agent.settings().clone()
        })
    }

    /// Live agents.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no agent is live.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The live agents, sorted by the address each listens on.
    pub fn targets(&self) -> Vec<Arc<SnmpAgent>> {
        self.0.targets()
    }
}

/// Publishes the `snmp` encoder's records into the shared [`SnmpAgent`] for
/// its listen address.
///
/// Only [`Sink::write_metric_event`] carries data: the sink needs the
/// event's series identity, so it opts in through
/// [`Sink::wants_metric_events`] and [`Sink::write`] is an error. Each write
/// is one publish record: a byte holding the number of index arcs at the end
/// of the instance OID, then the instance's `VarBind`. The first record of
/// each series decodes the instance OID, publishes it and, once the agent
/// has taken it, registers the object it belongs to (the OID without its
/// index arcs); later records of the series reuse the decoded OID. An
/// instance is owned by the launch whose sink created it and held while any
/// sink of that launch has published it: the sinks of that launch share it,
/// a sink of another launch is refused it, and it leaves the agent when the
/// last of its publishers drops. Dropping the last sink on an address stops
/// its agent.
///
/// A sink made by joining a running agent publishes no instance of its own:
/// it replaces the latest value of an instance the agent already serves,
/// and refuses an event for any other OID.
pub struct SnmpAgentSink {
    agent: Arc<SnmpAgent>,
    owner: Owner,
    /// Each series key's entry in `published`.
    owned: HashMap<MetricKey, usize>,
    /// Every instance this sink has published, once each. Series keys
    /// rendering one OID share its entry.
    published: Vec<Publication>,
    /// Each instance's entry in `published`, looked up on a key's first
    /// event.
    instances: HashMap<Oid, usize>,
    last_delivered: bool,
    joined: bool,
}

/// An instance this sink has published: its OID and the buffers its
/// `VarBind`s are copied into.
struct Publication {
    oid: Oid,
    buffers: PublishBuffers,
}

/// Two buffers used in turn: each `VarBind` of an instance is copied into the
/// buffer used two publishes earlier and frozen into the `Bytes` the agent
/// stores. By then the agent has replaced the `Bytes` frozen from that
/// buffer, so the buffer holds its allocation alone and the copy reuses it.
/// A reader still holding that `Bytes` makes the copy allocate once.
#[derive(Default)]
struct PublishBuffers {
    buffers: [BytesMut; 2],
    next: usize,
}

impl PublishBuffers {
    /// `varbind`, copied into the next buffer and frozen.
    fn frozen(&mut self, varbind: &[u8]) -> Bytes {
        // The other buffer's `Bytes` is the one the agent holds now.
        let buffer = &mut self.buffers[self.next];
        self.next ^= 1;
        buffer.extend_from_slice(varbind);
        buffer.split().freeze()
    }
}

/// [`SondaError::Sink`]: `encoded` is not a publish record, for `detail`.
fn not_a_record(detail: impl fmt::Display) -> SondaError {
    SondaError::Sink(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the event is not an snmp publish record ({detail}); the snmp_agent sink takes \
             the snmp encoder's output"
        ),
    ))
}

/// The index-arc count and the `VarBind` of the publish record `encoded`: a
/// header byte, then exactly one SEQUENCE.
fn split_record(encoded: &[u8]) -> Result<(u8, &[u8]), SondaError> {
    let index_arcs = *encoded.first().ok_or_else(|| not_a_record("it is empty"))?;
    let varbind = &encoded[RECORD_HEADER..];
    let mut reader = Reader::new(varbind);
    reader.expect(Tag::Sequence).map_err(not_a_record)?;
    if !reader.is_empty() {
        return Err(not_a_record("bytes follow its VarBind"));
    }
    Ok((index_arcs, varbind))
}

/// The instance OID of `varbind`.
fn instance_oid(varbind: &[u8]) -> Result<Oid, SondaError> {
    let body = Reader::new(varbind)
        .expect(Tag::Sequence)
        .map_err(not_a_record)?;
    Reader::new(body).oid().map_err(not_a_record)
}

/// The object `instance` belongs to: the OID without its last `index_arcs`
/// arcs, which must leave at least two.
fn object_of(instance: &Oid, index_arcs: u8) -> Result<Oid, SondaError> {
    let arcs = instance.arcs();
    arcs.len()
        .checked_sub(usize::from(index_arcs))
        .and_then(|len| Oid::from_arcs(arcs[..len].to_vec()))
        .ok_or_else(|| {
            not_a_record(format!(
                "{index_arcs} index arcs leave no object of the OID {instance}"
            ))
        })
}

impl SnmpAgentSink {
    /// Join the agent for `cfg.listen`, binding it if no live agent has that
    /// address, as a launch of its own.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or a live agent on the
    /// same address with different settings; [`SondaError::Sink`] when the
    /// address cannot be bound.
    pub async fn new(cfg: &SnmpAgentConfig) -> Result<Self, SondaError> {
        Self::new_in(cfg, LaunchId::next()).await
    }

    /// [`new`](Self::new) for a scenario of `launch`.
    pub(crate) async fn new_in(
        cfg: &SnmpAgentConfig,
        launch: LaunchId,
    ) -> Result<Self, SondaError> {
        Ok(Self::from_agent(registry().acquire(cfg).await?, launch))
    }

    /// Join the live agent that serves `cfg.listen` without binding. The
    /// sink only replaces the latest value of instances the agent already
    /// serves; it owns none.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or when no live agent
    /// serves the address.
    pub(crate) fn join(cfg: &SnmpAgentConfig) -> Result<Self, SondaError> {
        let mut sink = Self::from_agent(registry().join(cfg)?, LaunchId::next());
        sink.joined = true;
        Ok(sink)
    }

    /// A sink of `launch` publishing into `agent`.
    pub(crate) fn from_agent(agent: Arc<SnmpAgent>, launch: LaunchId) -> Self {
        Self {
            agent,
            owner: Owner::from(launch),
            owned: HashMap::new(),
            published: Vec::new(),
            instances: HashMap::new(),
            last_delivered: false,
            joined: false,
        }
    }

    /// The agent this sink publishes into.
    pub fn agent(&self) -> &Arc<SnmpAgent> {
        &self.agent
    }
}

#[async_trait]
impl Sink for SnmpAgentSink {
    /// Always an error: this sink needs the event, which only
    /// [`Sink::write_metric_event`] carries.
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        Err(SondaError::Sink(io::Error::other(
            "the snmp_agent sink takes metric events from the snmp encoder, not raw bytes; \
             pair it with `encoder: { type: snmp }` on a metrics entry",
        )))
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }

    /// `true` once a write has stored its event in the agent.
    fn last_write_delivered(&self) -> bool {
        self.last_delivered
    }

    fn wants_metric_events(&self) -> bool {
        true
    }

    /// Publish the record `encoded` as the latest `VarBind` of the event's
    /// instance, copied into one of the [`PublishBuffers`] of that instance.
    /// Never awaits.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when the agent's responder task has exited, or
    /// when `encoded` is not a publish record; [`SondaError::Config`] when
    /// the instance belongs to another launch or is under the system group,
    /// or a joined sink's event names an instance the agent does not serve.
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        self.last_delivered = false;
        self.agent.check_serving()?;
        let (index_arcs, varbind) = split_record(encoded)?;
        if self.joined {
            let oid = instance_oid(varbind)?;
            if !self.agent.overwrite(&oid, Bytes::copy_from_slice(varbind)) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario serves the snmp OID {oid} on {}",
                    self.agent.local_addr()
                ))));
            }
            self.last_delivered = true;
            return Ok(());
        }
        let key = (event.name.clone(), Arc::clone(&event.labels));
        let entry = match self.owned.get(&key) {
            Some(&entry) => entry,
            None => {
                let oid = instance_oid(varbind)?;
                match self.instances.get(&oid) {
                    // Another of this sink's series renders the same OID.
                    Some(&entry) => {
                        self.owned.insert(key, entry);
                        entry
                    }
                    None => {
                        let object = object_of(&oid, index_arcs)?;
                        let mut buffers = PublishBuffers::default();
                        let stored = buffers.frozen(varbind);
                        self.agent.publish(self.owner, &oid, stored, true)?;
                        self.agent.register_object(&object);
                        let entry = self.published.len();
                        self.published.push(Publication {
                            oid: oid.clone(),
                            buffers,
                        });
                        self.instances.insert(oid, entry);
                        self.owned.insert(key, entry);
                        self.last_delivered = true;
                        return Ok(());
                    }
                }
            }
        };
        let publication = &mut self.published[entry];
        let stored = publication.buffers.frozen(varbind);
        self.agent
            .publish(self.owner, &publication.oid, stored, false)?;
        self.last_delivered = true;
        Ok(())
    }
}

impl Drop for SnmpAgentSink {
    /// Release this sink's claim on each instance it published, once per
    /// instance.
    fn drop(&mut self) {
        for publication in &self.published {
            self.agent.release(self.owner, &publication.oid);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::agent::Lookup;
    use super::*;
    use crate::config::{BaseScheduleConfig, ScenarioConfig};
    use crate::encoder::snmp::{SnmpEncoder, SnmpEncoderConfig, SnmpValueType};
    use crate::encoder::{Encoder, EncoderConfig};
    use crate::generator::GeneratorConfig;
    use crate::model::metric::Labels;
    use crate::schedule::stats::ScenarioStats;
    use crate::sink::SinkConfig;
    use crate::OnSinkError;

    pub(super) fn hex(text: &str) -> Vec<u8> {
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

    // ---- config and settings() --------------------------------------------

    #[test]
    fn settings_resolve_the_defaults() {
        let settings = SnmpAgentConfig::new("127.0.0.1:1161")
            .settings()
            .expect("the defaults are valid");
        assert_eq!(
            settings.listen,
            "127.0.0.1:1161".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(settings.community, "public");
        assert_eq!(settings.max_message_size, 1472);
        assert_eq!(settings.system.object_id, oid("1.3.6.1.4.1.8072.3.2.10"));
        assert_eq!(settings.system.services, 72);
        assert_eq!(
            (
                settings.system.name.as_str(),
                settings.system.descr.as_str()
            ),
            ("", "")
        );
    }

    /// `settings()` gives the address an IPv4-mapped listen binds: the IPv4
    /// address it maps.
    #[test]
    fn settings_canonicalise_an_ipv4_mapped_listen() {
        let settings = SnmpAgentConfig::new("[::ffff:127.0.0.1]:1161")
            .settings()
            .expect("a socket address");
        assert_eq!(
            settings.listen,
            "127.0.0.1:1161".parse::<SocketAddr>().unwrap()
        );
    }

    /// `settings()` takes `max_message_size` from 484 to 65507 and
    /// `system.services` to 127, and refuses a listen that is not an address
    /// or an `object_id` that is not an OID, naming the field.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::message_size_below_the_minimum(|c: &mut SnmpAgentConfig| c.max_message_size = 483,   Some("max_message_size must be between 484 and 65507, got 483"))]
    #[case::message_size_at_the_minimum(   |c: &mut SnmpAgentConfig| c.max_message_size = 484,   None)]
    #[case::message_size_at_the_maximum(   |c: &mut SnmpAgentConfig| c.max_message_size = 65507, None)]
    #[case::message_size_above_the_maximum(|c: &mut SnmpAgentConfig| c.max_message_size = 65508, Some("got 65508"))]
    #[case::services_at_the_maximum(       |c: &mut SnmpAgentConfig| c.system.services = 127,    None)]
    #[case::services_above_the_maximum(    |c: &mut SnmpAgentConfig| c.system.services = 128,    Some("system.services must be between 0 and 127, got 128"))]
    #[case::object_id_with_a_leading_dot(  |c: &mut SnmpAgentConfig| c.system.object_id = ".1.3.6.1".to_string(), Some("system.object_id"))]
    #[case::listen_not_an_address(         |c: &mut SnmpAgentConfig| c.listen = "1161".to_string(), Some("listen \"1161\" is not a socket address"))]
    fn settings_check_every_field(#[case] change: fn(&mut SnmpAgentConfig), #[case] refused: Option<&str>) {
        let mut cfg = SnmpAgentConfig::new("127.0.0.1:1161");
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
        let cfg = SnmpAgentConfig {
            community: "s3cret-community".to_string(),
            ..SnmpAgentConfig::new("127.0.0.1:1161")
        };
        let settings = cfg.settings().expect("valid settings");
        for debug in [format!("{cfg:?}"), format!("{settings:?}")] {
            assert!(debug.contains("127.0.0.1:1161"), "{debug}");
            assert!(!debug.contains("s3cret-community"), "{debug}");
        }
    }

    /// Every field of the sink block reads as written; `community` and the
    /// system text fields take any scalar as its text.
    #[cfg(feature = "config")]
    #[test]
    fn a_full_block_parses_and_numbers_are_read_as_text() {
        let yaml = "type: snmp_agent\nlisten: 0.0.0.0:1161\ncommunity: 12345\nmax_message_size: 1200\n\
                    system:\n  name: 42\n  descr: Sonda synthetic router\n  contact: noc@example.com\n  \
                    location: lab-1\n  object_id: 1.3.6.1.4.1.8072.3.2.10\n  services: 6\n";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("the block parses");
        let SinkConfig::SnmpAgent(cfg) = config else {
            panic!("an snmp_agent sink, got {config:?}");
        };
        assert_eq!(cfg.community, "12345", "a numeric community is its text");
        assert_eq!(cfg.max_message_size, 1200);
        assert_eq!(
            cfg.system,
            SystemGroup {
                name: "42".to_string(),
                descr: "Sonda synthetic router".to_string(),
                contact: "noc@example.com".to_string(),
                location: "lab-1".to_string(),
                object_id: "1.3.6.1.4.1.8072.3.2.10".to_string(),
                services: 6,
            }
        );
    }

    /// An unquoted `object_id` that YAML reads as a number is an error
    /// naming the field: read as a number, `1.30` would be `1.3`.
    #[cfg(feature = "config")]
    #[test]
    fn an_unquoted_numeric_object_id_is_refused_naming_the_field() {
        let parse = |object_id: &str| {
            serde_yaml_ng::from_str::<SinkConfig>(&format!(
                "type: snmp_agent\nlisten: 0.0.0.0:1161\nsystem:\n  object_id: {object_id}\n"
            ))
        };
        parse("\"1.30\"").expect("a quoted OID parses");
        let err = parse("1.30").expect_err("a YAML number is refused");
        let msg = err.to_string();
        assert!(
            msg.contains("system.object_id") && msg.contains("quote"),
            "{msg}"
        );
    }

    // ---- registry -----------------------------------------------------------

    /// Each test that counts agents uses its own registry, so agents other
    /// tests bind cannot change the counts asserted here.
    #[tokio::test]
    async fn same_address_shares_one_agent() {
        let registry = SnmpAgentRegistry::new();
        let first = registry
            .acquire(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr();
        assert_ne!(addr.port(), 0, "the bound port is read back");
        let second = registry
            .acquire(&SnmpAgentConfig::new(addr.to_string()))
            .await
            .expect("join the live agent");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.local_addr(), addr);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.targets().len(), 1);
    }

    /// Two sinks naming one address publish into one agent.
    #[tokio::test]
    async fn two_sinks_on_one_address_share_one_agent() {
        let first = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.agent().local_addr();
        let second = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.to_string()))
            .await
            .expect("join the live agent");
        assert!(Arc::ptr_eq(first.agent(), second.agent()));
        assert_eq!(second.agent().local_addr(), addr);
    }

    /// Sinks on one address that differ in `community` are refused by
    /// `acquire`, `check` and `join` alike, naming the field and neither
    /// value; equal settings share the agent. A join compares nothing else:
    /// one with the agent's community and another `max_message_size` and
    /// `system` joins.
    #[tokio::test]
    async fn differing_community_on_one_address_is_a_config_error_naming_the_field() {
        let registry = SnmpAgentRegistry::new();
        let with = |listen: &str, community: &str| SnmpAgentConfig {
            community: community.to_string(),
            ..SnmpAgentConfig::new(listen)
        };
        let first = registry
            .acquire(&with("127.0.0.1:0", "first-community"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr().to_string();
        let shared = registry
            .acquire(&with(&addr, "first-community"))
            .await
            .expect("equal settings share the agent");
        assert!(Arc::ptr_eq(&first, &shared));

        let other = with(&addr, "second-community");
        let refused = [
            registry.acquire(&other).await.map(|_| ()),
            registry.check(&other),
            registry.join(&other).map(|_| ()),
        ];
        for result in refused {
            let err = result.expect_err("another community on the address is refused");
            assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
            let msg = err.to_string();
            assert!(msg.contains("community and another community"), "{msg}");
            assert!(
                !msg.contains("first-community") && !msg.contains("second-community"),
                "{msg}"
            );
        }
        let elsewise = SnmpAgentConfig {
            max_message_size: 484,
            system: SystemGroup {
                name: "another-name".to_string(),
                ..SystemGroup::default()
            },
            ..with(&addr, "first-community")
        };
        registry
            .check(&elsewise)
            .expect_err("the premise: acquire would refuse these settings");
        let joined = registry
            .join(&elsewise)
            .expect("a join compares the community only");
        assert!(Arc::ptr_eq(&first, &joined));
        assert_eq!(registry.len(), 1, "nothing else was bound");
    }

    /// A gnmi target and an snmp agent on the same port number both bind:
    /// one is TCP, the other UDP, and each has its own registry.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn a_gnmi_target_and_an_snmp_agent_share_a_port_number() {
        use crate::sink::gnmi::{GnmiTargetConfig, GnmiTargetSink};

        let gnmi = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral TCP port");
        let addr = gnmi.target().local_addr();
        let snmp = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.to_string()))
            .await
            .expect("the UDP port of the same number binds");
        assert_eq!(snmp.agent().local_addr(), addr);
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("the gnmi target still listens");
        assert!(snmp.agent().is_serving());
    }

    /// `snmpget -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.1.3.0`, net-snmp 5.9.4.
    pub(super) const NET_SNMP_GET: &str =
        "302902010104067075626c6963a01c02043ac25c77020100020100300e300c06082b060102010103\
        000500";

    /// `snmpgetnext -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.31.1.1.1.6`, net-snmp 5.9.4.
    pub(super) const NET_SNMP_GETNEXT: &str =
        "302b02010104067075626c6963a11e02042db414d60201000201003010300e060a2b060102011f01\
        0101060500";

    /// `snmpbulkget -v2c -c public -Cr10 127.0.0.1:1161 1.3.6.1.2.1.31.1.1.1.6`, net-snmp 5.9.4.
    pub(super) const NET_SNMP_GETBULK: &str =
        "302b02010104067075626c6963a51e02041573e9b502010002010a3010300e060a2b060102011f01\
        0101060500";

    /// `snmpset -v2c -c public 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0 s rtr-2`, net-snmp 5.9.4.
    pub(super) const NET_SNMP_SET: &str =
        "302e02010104067075626c6963a32102047267bb3f0201000201003013301106082b060102010105\
        0004057274722d32";

    /// `snmpget -v3 -l noAuthNoPriv -u sonda 127.0.0.1:<port> 1.3.6.1.2.1.1.3.0`,
    /// net-snmp 5.9.4: its engine-discovery request.
    pub(super) const NET_SNMP_V3_DISCOVERY: &str =
        "303e020103301102044caaa96e020300ffe30401040201030410300e0400020100020100040004000400\
        301404000400a00e02046a3d08eb0201000201003000";

    /// Send `datagram` to `addr` from an ephemeral port.
    async fn send(addr: SocketAddr, datagram: &[u8]) {
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind a client socket");
        client.send_to(datagram, addr).await.expect("sent");
    }

    /// Wait up to two seconds for `agent`'s counters to satisfy `done`.
    async fn counted(
        agent: &SnmpAgent,
        done: impl Fn(&RequestCounters) -> bool,
    ) -> RequestCounters {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let requests = agent.requests();
            if done(&requests) || tokio::time::Instant::now() >= deadline {
                return requests;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The responder counts every datagram once: under its PDU type, as an
    /// unsupported version for SNMPv3, or as malformed.
    #[tokio::test]
    async fn the_responder_counts_each_datagram_under_its_pdu_type() {
        let sink = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let agent = sink.agent();
        let addr = agent.local_addr();
        assert_eq!(agent.requests(), RequestCounters::default());
        let datagrams = [
            NET_SNMP_GET,
            NET_SNMP_GETNEXT,
            NET_SNMP_GETBULK,
            NET_SNMP_SET,
            NET_SNMP_V3_DISCOVERY,
            "0102",
        ];
        for text in datagrams {
            let datagram = hex(text);
            assert!(!datagram.is_empty(), "fixture present");
            send(addr, &datagram).await;
        }
        let total = |r: &RequestCounters| {
            r.get + r.get_next + r.get_bulk + r.set_refused + r.malformed + r.unsupported_version
        };
        let requests = counted(agent, |r| total(r) == 6).await;
        assert_eq!(
            requests,
            RequestCounters {
                get: 1,
                get_next: 1,
                get_bulk: 1,
                set_refused: 1,
                malformed: 1,
                unsupported_version: 1,
                ..RequestCounters::default()
            }
        );
        assert!(agent.community_required());
    }

    /// Dropping the last sink on an address closes the socket within one
    /// second, after which the port binds again.
    #[tokio::test]
    async fn dropping_both_sinks_closes_the_socket_within_one_second() {
        let first = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.agent().local_addr();
        let second = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.to_string()))
            .await
            .expect("join the live agent");
        send(addr, &hex(NET_SNMP_GET)).await;
        let requests = counted(first.agent(), |r| r.get == 1).await;
        assert_eq!(requests.get, 1, "the socket received and counted a GET");

        let closed = first.agent().wait();
        drop(first);
        drop(second);
        tokio::time::timeout(Duration::from_secs(1), closed)
            .await
            .expect("the socket closes within 1 s of the last drop");
        tokio::net::UdpSocket::bind(addr)
            .await
            .expect("the port binds again");
    }

    /// A join never binds: with no agent on the address it is refused.
    #[tokio::test]
    async fn join_without_a_live_agent_binds_nothing() {
        let registry = SnmpAgentRegistry::new();
        let err = registry
            .join(&SnmpAgentConfig::new("127.0.0.1:1161"))
            .err()
            .expect("nothing to join");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(
            err.to_string()
                .contains("no running scenario listens on 127.0.0.1:1161"),
            "{err}"
        );
        assert!(registry.is_empty());
    }

    // ---- the sink -----------------------------------------------------------

    /// The encoder config the sink tests publish with: three columns of the
    /// interface tables and one scalar.
    fn encoder_config() -> SnmpEncoderConfig {
        let pairs = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>()
        };
        SnmpEncoderConfig {
            oids: pairs(&[
                ("ifHCInOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"),
                ("ifInErrors", "1.3.6.1.2.1.2.2.1.14.{ifIndex}"),
                ("playpenScalar", "1.3.6.1.4.1.8072.9999.1.1.0"),
            ]),
            types: HashMap::from([
                ("ifHCInOctets".to_string(), SnmpValueType::Counter64),
                ("ifInErrors".to_string(), SnmpValueType::Counter32),
            ]),
            drop_labels: vec!["job".to_string()],
            ..SnmpEncoderConfig::default()
        }
    }

    fn event(name: &str, value: f64, labels: &[(&str, &str)]) -> MetricEvent {
        MetricEvent::new(
            name.to_string(),
            value,
            Labels::from_pairs(labels).expect("valid labels"),
        )
        .expect("valid event")
    }

    /// `event` as the snmp encoder writes it: a publish record.
    fn record(event: &MetricEvent) -> Vec<u8> {
        let encoder = SnmpEncoder::new(&encoder_config()).expect("a valid encoder");
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    /// The `VarBind` of `event`'s record.
    fn varbind(event: &MetricEvent) -> Bytes {
        Bytes::copy_from_slice(&record(event)[RECORD_HEADER..])
    }

    async fn sink() -> SnmpAgentSink {
        SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port")
    }

    async fn publish(sink: &mut SnmpAgentSink, event: &MetricEvent) {
        sink.write_metric_event(event, &record(event))
            .await
            .expect("the write succeeds");
    }

    fn octets(if_index: &str, value: f64) -> MetricEvent {
        event("ifHCInOctets", value, &[("ifIndex", if_index)])
    }

    #[tokio::test]
    async fn write_without_an_event_is_an_error() {
        let mut sink = sink().await;
        let err = sink
            .write(b"bytes")
            .await
            .expect_err("raw bytes cannot be published");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(err.to_string().contains("snmp encoder"), "{err}");
        assert_eq!(sink.agent().instance_count(), 0);
    }

    /// A write is delivered once its `VarBind` is in the agent's table, and
    /// that `VarBind` is what the agent serves for the instance.
    #[tokio::test]
    async fn last_write_delivered_after_a_publish() {
        let mut sink = sink().await;
        assert!(!sink.last_write_delivered(), "nothing written yet");
        let event = octets("3", 1_250_000.0);
        publish(&mut sink, &event).await;
        assert!(sink.last_write_delivered());
        assert_eq!(
            sink.agent().get(&oid("1.3.6.1.2.1.31.1.1.1.6.3")),
            Lookup::Value(varbind(&event))
        );
        let later = octets("3", 2_500_000.0);
        publish(&mut sink, &later).await;
        assert!(sink.last_write_delivered());
        assert_eq!(
            sink.agent().get(&oid("1.3.6.1.2.1.31.1.1.1.6.3")),
            Lookup::Value(varbind(&later)),
            "the latest value is served"
        );
    }

    /// Bytes that are not a publish record are refused on a series' first
    /// event and on a later one, and the instance keeps its latest value.
    #[rstest::rstest]
    #[case::empty(&[])]
    #[case::header_only(&[1])]
    #[case::not_a_sequence(&[1, 0x04, 0x00])]
    #[case::bytes_after_the_varbind(&[1, 0x30, 0x00, 0x00])]
    #[tokio::test]
    async fn a_write_that_is_not_a_publish_record_is_refused(#[case] encoded: &[u8]) {
        let mut sink = sink().await;
        let first = octets("4", 1.0);
        let err = sink
            .write_metric_event(&first, encoded)
            .await
            .expect_err("a first event must be a record");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(
            err.to_string().contains("not an snmp publish record"),
            "{err}"
        );
        assert!(!sink.last_write_delivered());
        assert_eq!(sink.agent().instance_count(), 0);

        publish(&mut sink, &first).await;
        assert!(sink.last_write_delivered());
        let err = sink
            .write_metric_event(&first, encoded)
            .await
            .expect_err("every event must be a record");
        assert!(
            err.to_string().contains("not an snmp publish record"),
            "{err}"
        );
        assert!(!sink.last_write_delivered(), "the last write failed");
        assert_eq!(
            sink.agent().get(&oid("1.3.6.1.2.1.31.1.1.1.6.4")),
            Lookup::Value(varbind(&first))
        );
    }

    /// A record whose index-arc count leaves no object is refused.
    #[tokio::test]
    async fn a_record_with_too_many_index_arcs_is_refused() {
        let mut sink = sink().await;
        let event = octets("4", 1.0);
        let mut encoded = record(&event);
        encoded[0] = 11;
        let err = sink
            .write_metric_event(&event, &encoded)
            .await
            .expect_err("eleven index arcs of a twelve-arc OID leave one");
        assert!(err.to_string().contains("leave no object"), "{err}");
        assert_eq!(sink.agent().instance_count(), 0);
    }

    /// Dropping a sink removes the instances it published and leaves another
    /// sink's, on the same agent, in place.
    #[tokio::test]
    async fn drop_removes_the_sinks_instances() {
        let mut first = sink().await;
        let addr = first.agent().local_addr().to_string();
        let mut second = SnmpAgentSink::new(&SnmpAgentConfig::new(addr))
            .await
            .expect("join the live agent");
        let agent = Arc::clone(first.agent());
        publish(&mut first, &octets("1", 1.0)).await;
        publish(&mut first, &octets("2", 1.0)).await;
        publish(&mut second, &octets("3", 1.0)).await;
        assert_eq!(agent.instance_count(), 3);

        drop(first);
        assert_eq!(
            agent.instance_count(),
            1,
            "the first sink's two instances left"
        );
        assert!(matches!(
            agent.get(&oid("1.3.6.1.2.1.31.1.1.1.6.3")),
            Lookup::Value(_)
        ));
        drop(second);
        assert_eq!(agent.instance_count(), 0);
    }

    /// A second launch's first write of an instance another launch owns is
    /// a config error naming the OID and the address; the owner keeps it.
    #[tokio::test]
    async fn a_second_launch_on_an_owned_instance_is_refused() {
        let mut owner = sink().await;
        let addr = owner.agent().local_addr().to_string();
        let mut intruder = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.clone()))
            .await
            .expect("join the live agent");
        publish(&mut owner, &octets("1", 1.0)).await;
        let event = octets("1", 2.0);
        let err = intruder
            .write_metric_event(&event, &record(&event))
            .await
            .expect_err("the instance belongs to the first launch");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("1.3.6.1.2.1.31.1.1.1.6.1") && msg.contains(&addr),
            "{msg}"
        );
        assert!(!intruder.last_write_delivered());
        drop(intruder);
        publish(&mut owner, &octets("1", 3.0)).await;
        assert_eq!(owner.agent().instance_count(), 1, "the owner keeps it");
    }

    /// Two sinks of one launch share an instance. When one drops, the
    /// instance stays and another launch is still refused it; when the second
    /// drops, the instance leaves and the socket closes.
    #[tokio::test]
    async fn an_instance_shared_by_one_launch_leaves_with_its_last_publisher() {
        let launch = LaunchId::next();
        let mut first = SnmpAgentSink::new_in(&SnmpAgentConfig::new("127.0.0.1:0"), launch)
            .await
            .expect("bind an ephemeral port");
        let addr = first.agent().local_addr();
        let mut second = SnmpAgentSink::new_in(&SnmpAgentConfig::new(addr.to_string()), launch)
            .await
            .expect("join the live agent");
        let agent = Arc::clone(first.agent());
        publish(&mut first, &octets("1", 1.0)).await;
        publish(&mut second, &octets("1", 2.0)).await;
        assert_eq!(agent.instance_count(), 1, "one launch, one shared instance");

        drop(first);
        assert_eq!(agent.instance_count(), 1, "the instance outlives one sink");
        let mut intruder = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.to_string()))
            .await
            .expect("join the live agent");
        let event = octets("1", 3.0);
        let err = intruder
            .write_metric_event(&event, &record(&event))
            .await
            .expect_err("the instance still belongs to the launch");
        assert!(err.to_string().contains("already published"), "{err}");
        drop(intruder);
        publish(&mut second, &octets("1", 4.0)).await;

        drop(second);
        assert_eq!(
            agent.instance_count(),
            0,
            "the instance left with the launch"
        );
        let closed = agent.wait();
        drop(agent);
        tokio::time::timeout(Duration::from_secs(1), closed)
            .await
            .expect("the socket closes within 1 s");
    }

    /// Series of one sink that render one OID, once a label is dropped,
    /// share its entry and upsert one instance.
    #[tokio::test]
    async fn one_sink_publishing_an_instance_twice_upserts_it() {
        let mut sink = sink().await;
        for (job, value) in [("a", 1.0), ("b", 2.0)] {
            let event = event("ifHCInOctets", value, &[("ifIndex", "1"), ("job", job)]);
            publish(&mut sink, &event).await;
        }
        assert_eq!(sink.owned.len(), 2, "two series keys");
        assert_eq!(sink.published.len(), 1, "sharing one entry");
        assert_eq!(sink.agent().instance_count(), 1, "one instance");
        let agent = Arc::clone(sink.agent());
        drop(sink);
        assert_eq!(agent.instance_count(), 0, "released once");
    }

    /// A write through an agent whose responder stopped fails.
    #[tokio::test]
    async fn a_write_through_a_stopped_agent_fails() {
        let mut sink = sink().await;
        publish(&mut sink, &octets("1", 1.0)).await;
        sink.agent().stop_serving();
        tokio::time::timeout(Duration::from_secs(1), sink.agent().wait())
            .await
            .expect("the responder stops");
        let event = octets("1", 2.0);
        let err = sink
            .write_metric_event(&event, &record(&event))
            .await
            .expect_err("a stopped agent takes no write");
        assert!(err.to_string().contains("no longer serving"), "{err}");
        assert!(!sink.last_write_delivered());
    }

    /// The lookup rule: an instance answers its value; an OID that a
    /// registered object equals or prefixes answers "no such instance", even
    /// once every instance of the object has left; any other OID answers
    /// "no such object".
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::column_with_live_instances("1.3.6.1.2.1.31.1.1.1.6",          Lookup::NoSuchInstance)]
    #[case::missing_index(             "1.3.6.1.2.1.31.1.1.1.6.9",        Lookup::NoSuchInstance)]
    #[case::below_an_instance(         "1.3.6.1.2.1.31.1.1.1.6.1.5",      Lookup::NoSuchInstance)]
    #[case::scalar_without_zero(       "1.3.6.1.4.1.8072.9999.1.1",       Lookup::NoSuchInstance)]
    #[case::emptied_column(            "1.3.6.1.2.1.2.2.1.14.1",          Lookup::NoSuchInstance)]
    #[case::system_scalar_without_zero("1.3.6.1.2.1.1.5",                 Lookup::NoSuchInstance)]
    #[case::node_above_the_objects(    "1.3.6.1.2.1.31",                  Lookup::NoSuchObject)]
    #[case::unknown_enterprise(        "1.3.6.1.4.1.99999.1.0",           Lookup::NoSuchObject)]
    #[case::unserved_system_object(    "1.3.6.1.2.1.1.8.0",               Lookup::NoSuchObject)]
    #[case::under_a_refused_publish(   "1.3.6.1.2.1.1.9.1.2.5",           Lookup::NoSuchObject)]
    #[tokio::test]
    async fn get_answers_by_the_object_set(#[case] request: &str, #[case] expected: Lookup) {
        let mut sink = sink().await;
        let agent = Arc::clone(sink.agent());
        // A column under the system group: the agent refuses its publish,
        // so its object `1.3.6.1.2.1.1.9.1.2` is not registered.
        let refused = event("sysORID", 1.0, &[("ifIndex", "1")]);
        let encoder = SnmpEncoder::new(&SnmpEncoderConfig {
            oids: HashMap::from([(
                "sysORID".to_string(),
                "1.3.6.1.2.1.1.9.1.2.{ifIndex}".to_string(),
            )]),
            ..SnmpEncoderConfig::default()
        })
        .expect("a valid encoder");
        let mut encoded = Vec::new();
        encoder.encode_metric(&refused, &mut encoded).expect("encodes");
        let err = sink
            .write_metric_event(&refused, &encoded)
            .await
            .expect_err("the system group is the agent's");
        assert!(err.to_string().contains("system group"), "{err}");
        publish(&mut sink, &octets("1", 1.0)).await;
        publish(&mut sink, &octets("2", 1.0)).await;
        publish(&mut sink, &event("playpenScalar", 7.0, &[])).await;
        let mut emptied = SnmpAgentSink::new(&SnmpAgentConfig::new(agent.local_addr().to_string()))
            .await
            .expect("join the live agent");
        let errors = event("ifInErrors", 3.0, &[("ifIndex", "1")]);
        publish(&mut emptied, &errors).await;
        assert_eq!(agent.get(&oid("1.3.6.1.2.1.2.2.1.14.1")), Lookup::Value(varbind(&errors)));
        drop(emptied);

        // Guard: the agent answers its live instances before a miss is asserted.
        let live = [("1.3.6.1.2.1.31.1.1.1.6.1", octets("1", 1.0)), ("1.3.6.1.4.1.8072.9999.1.1.0", event("playpenScalar", 7.0, &[]))];
        for (instance, event) in live {
            assert_eq!(agent.get(&oid(instance)), Lookup::Value(varbind(&event)), "{instance}");
        }
        assert!(matches!(agent.get(&oid("1.3.6.1.2.1.1.5.0")), Lookup::Value(_)));
        assert_eq!(agent.get(&oid(request)), expected, "{request}");
    }

    fn base(name: &str, rate: f64, duration: &str) -> BaseScheduleConfig {
        BaseScheduleConfig {
            gap_windows: None,
            name: name.to_string(),
            rate,
            duration: Some(duration.to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: Some(HashMap::from([("ifIndex".to_string(), "1".to_string())])),
            sink: SinkConfig::SnmpAgent(SnmpAgentConfig::new("127.0.0.1:0")),
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        }
    }

    /// Through the runner, every event reaches the agent as one instance,
    /// holding the last event's value.
    #[tokio::test]
    async fn a_scenario_publishes_every_event_into_the_agent() {
        let inner = sink().await;
        let agent = Arc::clone(inner.agent());
        let mut sink: Box<dyn Sink> = Box::new(inner);
        let config = ScenarioConfig {
            base: base("ifHCInOctets", 1_000.0, "250ms"),
            generator: GeneratorConfig::Step {
                start: Some(0.0),
                step_size: 1.0,
                max: None,
            },
            encoder: EncoderConfig::Snmp(encoder_config()),
            metric_type: None,
            help: None,
        };
        crate::config::validate::validate_config(&config).expect("the entry validates");
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");

        let stats = stats.read().expect("stats lock");
        assert!(
            stats.total_events >= 100,
            "only {} ticks ran",
            stats.total_events
        );
        assert_eq!(stats.errors, 0);
        assert_eq!(agent.instance_count(), 1);
        let last = octets("1", (stats.total_events - 1) as f64);
        assert_eq!(
            agent.get(&oid("1.3.6.1.2.1.31.1.1.1.6.1")),
            Lookup::Value(varbind(&last)),
            "the latest of {} events",
            stats.total_events
        );
    }
}
