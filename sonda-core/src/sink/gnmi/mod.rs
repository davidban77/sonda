//! gNMI target sink: serves the metric events it receives to gNMI clients.
//!
//! A [`GnmiTargetSink`] publishes every event into a [`GnmiTarget`], the
//! latest-value table and listener for one listen address. Sinks that name
//! the same address share one target through the process-wide
//! [`registry`](registry::registry), so their series appear as one device.
//!
//! The sink takes events through [`Sink::write_metric_event`] and never awaits
//! a client: publishing is a table upsert, a compare with the series' previous
//! value and, when the value changed, a non-blocking broadcast send.
//!
//! A series leaves the target when the last sink of its launch that published
//! it drops, and at no other time; a series that stops being updated stays.
//! When a series leaves, ON_CHANGE subscriptions that matched it, and SAMPLE
//! subscriptions with `suppress_redundant` that had sent it, receive one
//! notification deleting its path. A SAMPLE subscription with
//! `suppress_redundant` is sent a series only when its value changed since
//! the subscription last sent it, and its `heartbeat_interval` resends every
//! series it matches regardless.
//!
//! With `tls` set, every connection to the target is TLS, optionally
//! requiring client certificates; with `credentials` set, every RPC must
//! carry them as gRPC metadata. Every sink on one address uses the same.
//! Validating or binding a target with `tls` installs rustls's ring provider
//! as the process-wide default crypto provider when none is installed; an
//! application that installs its own must do so first.
//!
//! Requires the `gnmi` feature flag.

pub(crate) mod matcher;
pub(crate) mod notification;
pub mod registry;
pub mod service;
pub mod target;
#[cfg(test)]
pub(crate) mod test_tls;

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tonic::transport::{Certificate, Identity, ServerTlsConfig};

use crate::config::validate::{parse_duration, prepend_context};
use crate::model::metric::{MetricEvent, MetricKey};
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

use crate::schedule::LaunchId;
pub use notification::ValueKind;
pub use registry::{registry, GnmiTargetRegistry};
pub use target::{GnmiTarget, SeriesId, SeriesUpdate};
use target::{Owner, PathKey};

/// `max_subscribers` when it is not set.
pub const DEFAULT_MAX_SUBSCRIBERS: usize = 16;

/// Broadcast capacity per target when `subscriber_buffer` is not set.
pub const DEFAULT_SUBSCRIBER_BUFFER: usize = 1024;

/// Largest accepted `subscriber_buffer`. The broadcast ring is allocated up
/// front at this many slots.
pub const MAX_SUBSCRIBER_BUFFER: usize = 65_536;

/// `sample_floor` when it is not set.
pub const DEFAULT_SAMPLE_FLOOR: Duration = Duration::from_secs(1);

/// Configuration for [`GnmiTargetSink`]; the body of
/// `sink: { type: gnmi_target, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `listen_addr:` is an
/// error rather than a silent fallback to the defaults.
///
/// Histogram and summary entries emit `_sum` and `_count` series that carry
/// no `le` or `quantile` label, so they need a template without `{le}` or
/// `{quantile}`: either their own `paths:` entries (`<name>_sum`,
/// `<name>_count`), or `path` when the bucket or quantile series has a
/// `paths:` entry of its own. Validation names any series whose template uses
/// a key it does not carry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GnmiTargetConfig {
    /// Address to listen on, e.g. `"0.0.0.0:57400"`. `"127.0.0.1:0"` binds an
    /// ephemeral port, readable from [`GnmiTarget::local_addr`].
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub listen: String,
    /// Subscriptions the target serves at once, at least 1; one more is
    /// refused with `RESOURCE_EXHAUSTED`. Default 16. Every sink on one
    /// address must use the same value.
    #[cfg_attr(feature = "config", serde(default))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub max_subscribers: Option<usize>,
    /// Broadcast capacity per target, 1 to 65536: the number of changes an
    /// ON_CHANGE subscriber may fall behind before it is resynced with the
    /// latest value of every series it matches. Default 1024. Every sink on
    /// one address must use the same value.
    #[cfg_attr(feature = "config", serde(default))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = MAX_SUBSCRIBER_BUFFER)))]
    pub subscriber_buffer: Option<usize>,
    /// Shortest sample and heartbeat interval the target serves, a duration
    /// of at least one microsecond such as `"1s"`: a SAMPLE subscription
    /// asking for less, or for 0, is sampled at this interval, and a shorter
    /// heartbeat interval is raised to it. Default `"1s"`. Every sink on one
    /// address must use the same value.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::optional")
    )]
    pub sample_floor: Option<String>,
    /// Serve over TLS from these PEM files. Every sink on one address must
    /// name the same files.
    #[cfg_attr(feature = "config", serde(default))]
    pub tls: Option<GnmiTlsConfig>,
    /// Require every RPC to carry this username and password as gRPC
    /// metadata. Needs `tls`. Every sink on one address must use the same
    /// pair.
    #[cfg_attr(feature = "config", serde(default))]
    pub credentials: Option<GnmiCredentials>,
}

/// The `tls:` block of a [`GnmiTargetConfig`]: the PEM files a target
/// serves TLS from.
///
/// With `tls` set, every connection to the listen address is TLS, and a
/// plaintext client is refused at the handshake. Validation reads the files
/// to check them; a target serves the files as they were when it bound.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GnmiTlsConfig {
    /// PEM file holding the certificate chain the target presents, its own
    /// certificate first.
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub cert: String,
    /// PEM file holding the private key of `cert`.
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub key: String,
    /// PEM file of CA certificates. When set, every client must present a
    /// certificate signed by one of them; when unset, any client may
    /// connect.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::optional")
    )]
    pub client_ca: Option<String>,
}

/// The `credentials:` block of a [`GnmiTargetConfig`]: the username and
/// password every RPC must carry as gRPC metadata `username` and
/// `password`.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GnmiCredentials {
    /// The username clients must send.
    pub username: String,
    /// The password clients must send.
    #[cfg_attr(feature = "schema", schemars(extend("writeOnly" = true)))]
    pub password: String,
}

/// The fields of a `credentials:` block as written. The derive's errors name
/// this struct, `GnmiCredentials`.
#[cfg(feature = "config")]
mod credentials_fields {
    /// Each field a scalar read as text, and no other key.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct GnmiCredentials {
        #[serde(deserialize_with = "crate::config::scalar_text::required")]
        pub(super) username: String,
        #[serde(deserialize_with = "crate::config::scalar_text::required")]
        pub(super) password: String,
    }
}

/// The error for a `credentials:` value that is not a map. It names no part
/// of the value.
#[cfg(feature = "config")]
const CREDENTIALS_NOT_A_MAP: &str =
    "gnmi_target credentials must be a map with username and password";

/// Reads a map with `username` and `password` (or the two as a sequence),
/// refusing any other key. Any other value is refused with
/// [`CREDENTIALS_NOT_A_MAP`], which does not repeat the value.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for GnmiCredentials {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
        use serde::de::{Error, MapAccess, SeqAccess, Visitor};

        struct CredentialsVisitor;
        impl CredentialsVisitor {
            fn refuse<E: Error>(self) -> Result<GnmiCredentials, E> {
                Err(E::custom(CREDENTIALS_NOT_A_MAP))
            }
        }
        impl<'de> Visitor<'de> for CredentialsVisitor {
            type Value = GnmiCredentials;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map with username and password")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                <credentials_fields::GnmiCredentials as serde::Deserialize>::deserialize(
                    MapAccessDeserializer::new(map),
                )
                .map(Into::into)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                <credentials_fields::GnmiCredentials as serde::Deserialize>::deserialize(
                    SeqAccessDeserializer::new(seq),
                )
                .map(Into::into)
            }
            fn visit_bool<E: Error>(self, _: bool) -> Result<Self::Value, E> {
                self.refuse()
            }
            fn visit_i64<E: Error>(self, _: i64) -> Result<Self::Value, E> {
                self.refuse()
            }
            fn visit_u64<E: Error>(self, _: u64) -> Result<Self::Value, E> {
                self.refuse()
            }
            fn visit_f64<E: Error>(self, _: f64) -> Result<Self::Value, E> {
                self.refuse()
            }
            fn visit_str<E: Error>(self, _: &str) -> Result<Self::Value, E> {
                self.refuse()
            }
            fn visit_bytes<E: Error>(self, _: &[u8]) -> Result<Self::Value, E> {
                self.refuse()
            }
        }
        // `deserialize_any`, because a buffered sink block answers
        // `deserialize_struct` for a scalar with an error quoting it.
        deserializer.deserialize_any(CredentialsVisitor)
    }
}

#[cfg(feature = "config")]
impl From<credentials_fields::GnmiCredentials> for GnmiCredentials {
    fn from(fields: credentials_fields::GnmiCredentials) -> Self {
        Self {
            username: fields.username,
            password: fields.password,
        }
    }
}

/// Prints the username, and `"***"` in place of the password.
impl fmt::Debug for GnmiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GnmiCredentials")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

impl GnmiTargetConfig {
    /// A config listening on `listen` with every other field at its default.
    pub fn new(listen: impl Into<String>) -> Self {
        Self {
            listen: listen.into(),
            max_subscribers: None,
            subscriber_buffer: None,
            sample_floor: None,
            tls: None,
            credentials: None,
        }
    }

    /// Resolve the config into the settings a target runs with.
    ///
    /// This is the one place the fields are checked; validation and
    /// [`GnmiTargetSink::new`] both call it. Reads the `tls` files. Returns
    /// [`SondaError::Config`] when `listen` is not a socket address,
    /// `sample_floor` is not a duration of at least one microsecond,
    /// `max_subscribers` is 0, `subscriber_buffer` is outside 1 to
    /// [`MAX_SUBSCRIBER_BUFFER`], `credentials` is set without `tls`, or a
    /// `tls` file cannot be read or does not hold what its field needs.
    pub(crate) fn settings(&self) -> Result<TargetSettings, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        let listen: SocketAddr = self.listen.parse().map_err(|_| {
            invalid(format!(
                "gnmi_target listen {:?} is not a socket address such as \"0.0.0.0:57400\"",
                self.listen
            ))
        })?;
        // An IPv4-mapped address binds the IPv4 socket it maps, so it is
        // that address everywhere a listen address is compared or keyed.
        let listen = canonical(listen);
        let max_subscribers = self.max_subscribers.unwrap_or(DEFAULT_MAX_SUBSCRIBERS);
        if max_subscribers == 0 {
            return Err(invalid(
                "gnmi_target max_subscribers must be at least 1".to_string(),
            ));
        }
        let subscriber_buffer = self.subscriber_buffer.unwrap_or(DEFAULT_SUBSCRIBER_BUFFER);
        if !(1..=MAX_SUBSCRIBER_BUFFER).contains(&subscriber_buffer) {
            return Err(invalid(format!(
                "gnmi_target subscriber_buffer must be between 1 and {MAX_SUBSCRIBER_BUFFER}, \
                 got {subscriber_buffer}"
            )));
        }
        let sample_floor = match self.sample_floor.as_deref() {
            None => DEFAULT_SAMPLE_FLOOR,
            Some(s) => {
                let floor = parse_duration(s)
                    .map_err(|e| prepend_context("invalid gnmi_target sample_floor", s, e))?;
                // parse_duration keeps whole microseconds, so a positive value
                // below one parses as zero.
                if floor.is_zero() {
                    return Err(invalid(format!(
                        "invalid gnmi_target sample_floor {s:?}: must be at least 0.001ms"
                    )));
                }
                floor
            }
        };
        if self.credentials.is_some() && self.tls.is_none() {
            return Err(invalid(
                "gnmi_target credentials need tls: without it the password would cross the \
                 wire in clear"
                    .to_string(),
            ));
        }
        let tls = self.tls.as_ref().map(TlsIdentity::read).transpose()?;
        Ok(TargetSettings {
            listen,
            max_subscribers,
            subscriber_buffer,
            sample_floor,
            tls,
            credentials: self.credentials.clone(),
            keepalive_interval: target::KEEPALIVE_INTERVAL,
            keepalive_timeout: target::KEEPALIVE_TIMEOUT,
            tls_handshake_timeout: target::TLS_HANDSHAKE_TIMEOUT,
            stall_floor: service::STALL_FLOOR,
            #[cfg(test)]
            probe: Arc::default(),
        })
    }
}

/// A [`GnmiTargetConfig`] with every field resolved and checked.
#[derive(Debug, Clone)]
pub(crate) struct TargetSettings {
    /// The address to bind.
    pub(crate) listen: SocketAddr,
    pub(crate) max_subscribers: usize,
    pub(crate) subscriber_buffer: usize,
    pub(crate) sample_floor: Duration,
    /// The TLS files, read and checked.
    pub(crate) tls: Option<TlsIdentity>,
    pub(crate) credentials: Option<GnmiCredentials>,
    /// How long an HTTP/2 connection may be silent before the server pings
    /// it. [`target::KEEPALIVE_INTERVAL`] except in tests; not configurable.
    pub(crate) keepalive_interval: Duration,
    /// How long the server waits for a ping's answer before it closes the
    /// connection. [`target::KEEPALIVE_TIMEOUT`] except in tests; not
    /// configurable.
    pub(crate) keepalive_timeout: Duration,
    /// How long a TLS target waits for a connection to finish its TLS
    /// handshake before it closes the connection.
    /// [`target::TLS_HANDSHAKE_TIMEOUT`] except in tests; not configurable.
    pub(crate) tls_handshake_timeout: Duration,
    /// The least time a subscription may go without its client draining a
    /// response before it is ended as stalled. [`service::STALL_FLOOR`]
    /// except in tests; not configurable.
    pub(crate) stall_floor: Duration,
    /// Counts what the target's subscriptions did.
    #[cfg(test)]
    pub(crate) probe: Arc<service::Probe>,
}

/// The files of a `tls:` block, read and checked: each holds what its field
/// needs, and together they make a TLS server configuration.
///
/// `Debug` prints the paths only.
#[derive(Clone)]
pub(crate) struct TlsIdentity {
    /// The paths as configured. Two targets serve the same TLS when these
    /// are equal.
    pub(crate) paths: GnmiTlsConfig,
    cert: Vec<u8>,
    key: Vec<u8>,
    client_ca: Option<Vec<u8>>,
}

impl TlsIdentity {
    /// Read and check the files `paths` names.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] naming the field and the file when a file
    /// cannot be read, `cert` or `client_ca` holds no PEM certificate, `key`
    /// holds no PEM private key, or the three do not make a TLS server
    /// configuration (a key that is not `cert`'s, for one).
    fn read(paths: &GnmiTlsConfig) -> Result<Self, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        let read = |field: &str, path: &str| {
            std::fs::read(path).map_err(|e| {
                invalid(format!(
                    "gnmi_target tls {field} {path:?} cannot be read: {e}"
                ))
            })
        };
        let certificates = |field: &str, path: &str, pem: &[u8]| {
            let certificates = CertificateDer::pem_slice_iter(pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    invalid(format!(
                        "gnmi_target tls {field} {path:?} is not a PEM certificate file: {e}"
                    ))
                })?;
            if certificates.is_empty() {
                return Err(invalid(format!(
                    "gnmi_target tls {field} {path:?} holds no PEM certificate"
                )));
            }
            Ok(())
        };
        let cert = read("cert", &paths.cert)?;
        certificates("cert", &paths.cert, &cert)?;
        let key = read("key", &paths.key)?;
        if PrivateKeyDer::from_pem_slice(&key).is_err() {
            return Err(invalid(format!(
                "gnmi_target tls key {:?} holds no PEM private key",
                paths.key
            )));
        }
        let client_ca = match paths.client_ca.as_deref() {
            Some(path) => {
                let pem = read("client_ca", path)?;
                certificates("client_ca", path, &pem)?;
                Some(pem)
            }
            None => None,
        };
        let identity = Self {
            paths: paths.clone(),
            cert,
            key,
            client_ca,
        };
        tonic::transport::Server::builder()
            .tls_config(identity.server_config())
            .map_err(|e| {
                invalid(format!(
                    "gnmi_target tls files {} cannot serve TLS: {}",
                    describe_tls_paths(paths),
                    error_chain(&e)
                ))
            })?;
        Ok(identity)
    }

    /// The tonic server TLS configuration for these files: the identity
    /// from `cert` and `key`, and `client_ca` as the root client
    /// certificates must chain to, when set.
    ///
    /// tonic builds the server's rustls configuration from the process's
    /// default crypto provider; this installs the ring provider as that
    /// default when none is installed
    /// ([`ensure_crypto_provider`](crate::util::ensure_crypto_provider)).
    pub(crate) fn server_config(&self) -> ServerTlsConfig {
        crate::util::ensure_crypto_provider();
        let config = ServerTlsConfig::new().identity(Identity::from_pem(&self.cert, &self.key));
        match &self.client_ca {
            Some(ca) => config.client_ca_root(Certificate::from_pem(ca)),
            None => config,
        }
    }
}

impl fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsIdentity")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// `error` followed by each of its sources, separated by `": "`.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(e) = source {
        text.push_str(": ");
        text.push_str(&e.to_string());
        source = e.source();
    }
    text
}

/// A `tls:` block's files as messages name them:
/// `{cert "a.pem", key "b.pem"}`, with `client_ca` when set.
fn describe_tls_paths(paths: &GnmiTlsConfig) -> String {
    match &paths.client_ca {
        None => format!("{{cert {:?}, key {:?}}}", paths.cert, paths.key),
        Some(ca) => format!(
            "{{cert {:?}, key {:?}, client_ca {:?}}}",
            paths.cert, paths.key, ca
        ),
    }
}

/// Whether listeners on `a` and `b` would contend for one port: the same
/// address, or the same port with one of them a wildcard that covers the
/// other's family. An IPv4 wildcard covers IPv4 addresses; an IPv6 wildcard
/// covers both families. Port 0 never contends.
///
/// Treating `[::]` as covering IPv4 assumes a dual-stack socket, the Linux
/// default (`net.ipv6.bindv6only = 0`). Where it binds IPv6 only, an IPv4
/// address on the same port is refused although it could bind, and a join
/// of an IPv4 address finds the IPv6 target.
pub(crate) fn listen_overlap(a: SocketAddr, b: SocketAddr) -> bool {
    let (a, b) = (canonical(a), canonical(b));
    if a.port() == 0 || a.port() != b.port() {
        return false;
    }
    a == b || covers(a, b) || covers(b, a)
}

/// `addr` with an IPv4-mapped IPv6 address (`[::ffff:127.0.0.1]`) read as
/// the IPv4 address it maps, which is the socket it binds.
fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Whether `wild` is a wildcard listening on `other`'s port that accepts
/// `other`'s family, one way: `0.0.0.0` covers IPv4 addresses, `[::]` covers
/// both families (see [`listen_overlap`] for the dual-stack assumption).
pub(crate) fn covers(wild: SocketAddr, other: SocketAddr) -> bool {
    let (wild, other) = (canonical(wild), canonical(other));
    wild.port() != 0
        && wild.port() == other.port()
        && wild.ip().is_unspecified()
        && (wild.is_ipv6() || other.is_ipv4())
}

impl TargetSettings {
    /// Whether a target running with `self` can also serve `other`: every
    /// setting but the listen address is equal, the `tls` files by their
    /// paths.
    pub(crate) fn serves(&self, other: &TargetSettings) -> bool {
        self.max_subscribers == other.max_subscribers
            && self.subscriber_buffer == other.subscriber_buffer
            && self.sample_floor == other.sample_floor
            && self.tls.as_ref().map(|tls| &tls.paths) == other.tls.as_ref().map(|tls| &tls.paths)
            && self.credentials == other.credentials
    }

    /// `self`'s settings beside `other`'s, for an error saying they differ:
    /// `max_subscribers 16 and 4, subscriber_buffer 1024 and 1024, sample_floor
    /// 1s and 1s, tls none and none, credentials none and none`. Credentials
    /// are named by username; a pair differing only in password reads
    /// `username "a" and username "a" with another password`. Both
    /// address-sharing checks word the conflict with it.
    pub(crate) fn beside(&self, other: &TargetSettings) -> String {
        let tls = |tls: &Option<TlsIdentity>| {
            tls.as_ref()
                .map_or_else(|| "none".to_string(), |tls| describe_tls_paths(&tls.paths))
        };
        let username = |credentials: &Option<GnmiCredentials>| {
            credentials.as_ref().map_or_else(
                || "none".to_string(),
                |c| format!("username {:?}", c.username),
            )
        };
        let password = match (&self.credentials, &other.credentials) {
            (Some(a), Some(b)) if a.username == b.username && a.password != b.password => {
                " with another password"
            }
            _ => "",
        };
        format!(
            "max_subscribers {} and {}, subscriber_buffer {} and {}, sample_floor {:?} and {:?}, \
             tls {} and {}, credentials {} and {}{password}",
            self.max_subscribers,
            other.max_subscribers,
            self.subscriber_buffer,
            other.subscriber_buffer,
            self.sample_floor,
            other.sample_floor,
            tls(&self.tls),
            tls(&other.tls),
            username(&self.credentials),
            username(&other.credentials),
        )
    }
}

/// Publishes metric events into the shared [`GnmiTarget`] for its listen address.
///
/// Only [`Sink::write_metric_event`] carries data: the sink needs the event's
/// series identity, so it opts in through [`Sink::wants_metric_events`] and
/// [`Sink::write`] is an error. The first event of each series decodes the
/// series' path from the encoded notification; later events of that series
/// reuse the id the target returned. A series is owned by the launch whose
/// sink created it and held while any sink of that launch has published it:
/// the sinks of that launch share it, a sink of another launch is refused
/// it, and it leaves the target, with a delete broadcast to its subscribers,
/// when the last of its publishers drops. A
/// path with no live publisher is free; a scenario refused it in that window
/// shows why in its stats. Dropping the last sink on an address stops its
/// listener.
///
/// A sink made by joining a running target publishes no series of its own:
/// it replaces the latest value of a series the target already serves, and
/// refuses an event for any other path.
pub struct GnmiTargetSink {
    target: Arc<GnmiTarget>,
    owner: Owner,
    /// Each series key's entry in `published`.
    owned: HashMap<MetricKey, usize>,
    /// Every path this sink has published, once each. Series keys rendering
    /// one path share its entry, so an id that changes changes for all of
    /// them.
    published: Vec<Publication>,
    /// Each path's entry in `published`, looked up on a key's first event.
    paths: HashMap<PathKey, usize>,
    last_delivered: bool,
    joined: bool,
}

/// A path this sink has published: the series id the target gave it, the
/// table's key for it, and the buffers its notifications are copied into.
struct Publication {
    id: SeriesId,
    path: PathKey,
    buffers: PublishBuffers,
}

/// Two buffers used in turn: each notification of a path is copied into the
/// buffer used two notifications earlier and frozen into the `Bytes` the
/// target stores. The table has dropped the `Bytes` it stored before the
/// latest one, so the copy reuses that buffer's allocation unless a
/// subscriber still holds it, and allocates once then.
#[derive(Default)]
struct PublishBuffers {
    buffers: [BytesMut; 2],
    next: usize,
}

impl PublishBuffers {
    /// `encoded`, copied into the next buffer and frozen.
    fn frozen(&mut self, encoded: &[u8]) -> Bytes {
        let buffer = &mut self.buffers[self.next];
        self.next ^= 1;
        // Rewinds to the start of the allocation when nothing else holds it;
        // otherwise the copy below allocates.
        let _ = buffer.try_reclaim(encoded.len());
        buffer.extend_from_slice(encoded);
        buffer.split().freeze()
    }
}

impl GnmiTargetSink {
    /// Join the target for `cfg.listen`, binding it if no live target has
    /// that address, as a launch of its own.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or a live target on the
    /// same address with different settings; [`SondaError::Sink`] when the
    /// address cannot be bound.
    pub async fn new(cfg: &GnmiTargetConfig) -> Result<Self, SondaError> {
        Self::new_in(cfg, LaunchId::next()).await
    }

    /// [`new`](Self::new) for a scenario of `launch`.
    pub(crate) async fn new_in(
        cfg: &GnmiTargetConfig,
        launch: LaunchId,
    ) -> Result<Self, SondaError> {
        Ok(Self::from_target(registry().acquire(cfg).await?, launch))
    }

    /// Join the live target that serves `cfg.listen` without binding. The
    /// sink only replaces the latest value of series the target already
    /// serves; it owns none.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] when no live target serves the address, or a
    /// setting `cfg` sets differs from the target's.
    pub(crate) fn join(cfg: &GnmiTargetConfig) -> Result<Self, SondaError> {
        Ok(Self {
            owner: Owner::from(LaunchId::next()),
            target: registry().join(cfg)?,
            owned: HashMap::new(),
            published: Vec::new(),
            paths: HashMap::new(),
            last_delivered: false,
            joined: true,
        })
    }

    /// A sink of `launch` publishing into `target`.
    pub(crate) fn from_target(target: Arc<GnmiTarget>, launch: LaunchId) -> Self {
        Self {
            owner: Owner::from(launch),
            target,
            owned: HashMap::new(),
            published: Vec::new(),
            paths: HashMap::new(),
            last_delivered: false,
            joined: false,
        }
    }

    /// The target this sink publishes into.
    pub fn target(&self) -> &Arc<GnmiTarget> {
        &self.target
    }
}

#[async_trait]
impl Sink for GnmiTargetSink {
    /// Always an error: this sink needs the event, which only
    /// [`Sink::write_metric_event`] carries.
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        Err(SondaError::Sink(std::io::Error::other(
            "the gnmi_target sink takes metric events from the gnmi encoder, not raw \
             bytes; pair it with `encoder: { type: gnmi }` on a metrics, histogram or \
             summary entry",
        )))
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }

    /// `true` once a write has upserted its event into the target.
    fn last_write_delivered(&self) -> bool {
        self.last_delivered
    }

    fn wants_metric_events(&self) -> bool {
        true
    }

    /// Publish `encoded` as the latest value of the event's series, copied
    /// into one of the [`PublishBuffers`] of the series' path. Never awaits.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when the target's listener task has exited, or
    /// when an event is not a gnmi notification whose update has a value;
    /// [`SondaError::Config`] when the event's path belongs to another launch,
    /// or a joined sink's event names a path the target does not serve.
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        self.last_delivered = false;
        self.target.check_serving()?;
        if self.joined {
            let path = PathKey::decode(encoded)?;
            if self
                .target
                .overwrite(&path, Bytes::copy_from_slice(encoded))?
                .is_none()
            {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario serves the gnmi path `{}` on {}",
                    path.path(),
                    self.target.local_addr()
                ))));
            }
            self.last_delivered = true;
            return Ok(());
        }
        let key = (event.name.clone(), Arc::clone(&event.labels));
        let entry = match self.owned.get(&key) {
            Some(&entry) => entry,
            None => {
                let path = PathKey::decode(encoded)?;
                match self.paths.get(&path) {
                    // Another of this sink's series renders the same path.
                    Some(&entry) => {
                        self.owned.insert(key, entry);
                        entry
                    }
                    None => {
                        let mut buffers = PublishBuffers::default();
                        let encoded = buffers.frozen(encoded);
                        let (id, _) = self.target.publish(self.owner, None, &path, encoded)?;
                        // Cache the table's own key, which a sibling may have
                        // made, so later publishes match it by pointer.
                        let path = self.target.path_of(id).unwrap_or(path);
                        let entry = self.published.len();
                        self.published.push(Publication {
                            id,
                            path: path.clone(),
                            buffers,
                        });
                        self.paths.insert(path, entry);
                        self.owned.insert(key, entry);
                        self.last_delivered = true;
                        return Ok(());
                    }
                }
            }
        };
        let publication = &mut self.published[entry];
        let encoded = publication.buffers.frozen(encoded);
        let (id, _) =
            self.target
                .publish(self.owner, Some(publication.id), &publication.path, encoded)?;
        publication.id = id;
        self.last_delivered = true;
        Ok(())
    }
}

impl Drop for GnmiTargetSink {
    /// Release this sink's claim on each path it published, once per path.
    fn drop(&mut self) {
        if !self.published.is_empty() {
            self.target
                .release(self.owner, self.published.iter().map(|p| p.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::RwLock;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{
        BaseScheduleConfig, DistributionConfig, HistogramScenarioConfig, ScenarioConfig,
        SummaryScenarioConfig,
    };
    use crate::encoder::gnmi::GnmiEncoderConfig;
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    use crate::model::metric::Labels;
    use crate::schedule::stats::ScenarioStats;
    use crate::sink::SinkConfig;
    use crate::OnSinkError;

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::same_address("127.0.0.1:57400", "127.0.0.1:57400", true)]
    #[case::v4_wildcard_and_v4("0.0.0.0:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_and_v4("[::]:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_and_v4_wildcard("[::]:57400", "0.0.0.0:57400", true)]
    #[case::v6_wildcard_and_v6("[::]:57400", "[::1]:57400", true)]
    #[case::v4_wildcard_and_v6("0.0.0.0:57400", "[::1]:57400", false)]
    #[case::two_specific_addresses("127.0.0.1:57400", "127.0.0.2:57400", false)]
    #[case::different_ports("0.0.0.0:57400", "127.0.0.1:57401", false)]
    #[case::port_zero("0.0.0.0:0", "127.0.0.1:0", false)]
    #[case::v4_mapped_and_v4("[::ffff:127.0.0.1]:57400", "127.0.0.1:57400", true)]
    #[case::v4_mapped_and_v4_wildcard("[::ffff:127.0.0.1]:57400", "0.0.0.0:57400", true)]
    fn listen_overlap_is_per_family(#[case] a: &str, #[case] b: &str, #[case] overlap: bool) {
        let (a, b): (SocketAddr, SocketAddr) = (a.parse().unwrap(), b.parse().unwrap());
        assert_eq!(listen_overlap(a, b), overlap, "{a} and {b}");
        assert_eq!(listen_overlap(b, a), overlap, "{b} and {a}");
    }

    /// `settings()` gives the address an IPv4-mapped listen binds: the IPv4
    /// address it maps.
    #[test]
    fn settings_canonicalise_an_ipv4_mapped_listen() {
        let settings = GnmiTargetConfig::new("[::ffff:127.0.0.1]:57400")
            .settings()
            .expect("a socket address");
        assert_eq!(
            settings.listen,
            "127.0.0.1:57400".parse::<SocketAddr>().unwrap()
        );
    }

    /// The credentials `username` / `password`.
    fn credentials(username: &str, password: &str) -> GnmiCredentials {
        GnmiCredentials {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    #[test]
    fn credentials_debug_prints_the_username_and_not_the_password() {
        let debug = format!("{:?}", credentials("sonda", "hunter2"));
        assert_eq!(
            debug,
            r#"GnmiCredentials { username: "sonda", password: "***" }"#
        );
        let config = GnmiTargetConfig {
            credentials: Some(credentials("sonda", "hunter2")),
            ..GnmiTargetConfig::new("127.0.0.1:0")
        };
        let debug = format!("{config:?}");
        assert!(
            debug.contains("sonda") && !debug.contains("hunter2"),
            "{debug}"
        );
    }

    /// Settings with `credentials` set directly: [`GnmiTargetConfig::settings`]
    /// refuses credentials without TLS, and these tests need neither.
    fn settings_with(credentials: Option<GnmiCredentials>) -> TargetSettings {
        let mut settings = GnmiTargetConfig::new("127.0.0.1:57400")
            .settings()
            .expect("valid settings");
        settings.credentials = credentials;
        settings
    }

    /// `beside` names differing credentials by username, says when only
    /// the passwords differ, and prints neither password.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::other_password(Some(credentials("sonda", "first-secret")), Some(credentials("sonda", "second-secret")), r#"credentials username "sonda" and username "sonda" with another password"#)]
    #[case::other_user(Some(credentials("sonda", "first-secret")), Some(credentials("admin", "second-secret")), r#"credentials username "sonda" and username "admin""#)]
    #[case::none_and_some(None, Some(credentials("sonda", "second-secret")), r#"credentials none and username "sonda""#)]
    fn beside_names_credentials_without_their_passwords(
        #[case] a: Option<GnmiCredentials>,
        #[case] b: Option<GnmiCredentials>,
        #[case] expected: &str,
    ) {
        let (a, b) = (settings_with(a), settings_with(b));
        assert!(!a.serves(&b) && !b.serves(&a), "the premise: they differ");
        let text = a.beside(&b);
        assert!(text.ends_with(expected), "{text}");
        assert!(
            !text.contains("first-secret") && !text.contains("second-secret"),
            "{text}"
        );
    }

    /// `serves` compares `tls` by its paths and `credentials` by both
    /// fields; settings equal in both serve each other.
    #[test]
    fn serves_compares_tls_paths_and_credentials() {
        let files = test_tls::TlsFiles::new();
        let tls = |tls: GnmiTlsConfig| GnmiTargetConfig {
            tls: Some(tls),
            credentials: Some(credentials("sonda", "secret")),
            ..GnmiTargetConfig::new("127.0.0.1:57400")
        };
        let base = tls(files.tls()).settings().expect("valid settings");
        let same = tls(files.tls()).settings().expect("valid settings");
        assert!(base.serves(&same));

        let copy = files.write(
            "copy.pem",
            &std::fs::read_to_string(files.path("server.pem")).expect("cert"),
        );
        let moved = tls(GnmiTlsConfig {
            cert: copy,
            ..files.tls()
        })
        .settings()
        .expect("valid settings");
        assert!(!base.serves(&moved), "another cert path");
        let mutual = tls(files.mutual_tls()).settings().expect("valid settings");
        assert!(!base.serves(&mutual), "a client_ca");
        let mut other_password = same.clone();
        other_password.credentials = Some(credentials("sonda", "other"));
        assert!(!base.serves(&other_password), "another password");
        let mut plain = same;
        plain.tls = None;
        assert!(!base.serves(&plain), "no tls");
        assert!(base
            .beside(&mutual)
            .contains(&format!("client_ca {:?}", files.path("ca.pem"))));
    }

    /// `covers` is one way: a wildcard covers the addresses of the families
    /// it accepts, never the other way round.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::v4_wildcard_covers_v4("0.0.0.0:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_covers_v4("[::]:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_covers_v4_wildcard("[::]:57400", "0.0.0.0:57400", true)]
    #[case::v4_wildcard_not_v6_wildcard("0.0.0.0:57400", "[::]:57400", false)]
    #[case::v4_wildcard_not_v6("0.0.0.0:57400", "[::1]:57400", false)]
    #[case::specific_covers_nothing("127.0.0.1:57400", "127.0.0.1:57400", false)]
    #[case::other_port("0.0.0.0:57400", "127.0.0.1:57401", false)]
    #[case::port_zero("0.0.0.0:0", "127.0.0.1:0", false)]
    fn covers_is_one_way(#[case] wild: &str, #[case] other: &str, #[case] covered: bool) {
        let (wild, other): (SocketAddr, SocketAddr) = (wild.parse().unwrap(), other.parse().unwrap());
        assert_eq!(covers(wild, other), covered, "{wild} covers {other}");
    }

    async fn sink() -> GnmiTargetSink {
        GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port")
    }

    fn event(name: &str) -> MetricEvent {
        MetricEvent::new(name.to_string(), 1.0, Labels::default()).expect("valid event")
    }

    /// `event` encoded by a gnmi encoder rendering `/a/{name}`.
    fn notification(event: &MetricEvent) -> Vec<u8> {
        use crate::encoder::gnmi::GnmiEncoder;
        use crate::encoder::Encoder;

        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            ..Default::default()
        })
        .expect("valid encoder");
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    async fn publish(sink: &mut GnmiTargetSink, name: &str) {
        let event = event(name);
        sink.write_metric_event(&event, &notification(&event))
            .await
            .expect("the write succeeds");
    }

    /// Counts calls on the way through to the wrapped sink, delegating
    /// `wants_metric_events` so the runner routes exactly as for the real sink.
    struct Counting {
        inner: GnmiTargetSink,
        writes: Arc<AtomicUsize>,
        events: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Sink for Counting {
        async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.write(data).await
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            self.inner.flush().await
        }
        fn last_write_delivered(&self) -> bool {
            self.inner.last_write_delivered()
        }
        fn wants_metric_events(&self) -> bool {
            self.inner.wants_metric_events()
        }
        async fn write_metric_event(
            &mut self,
            event: &MetricEvent,
            encoded: &[u8],
        ) -> Result<(), SondaError> {
            self.events.fetch_add(1, Ordering::SeqCst);
            self.inner.write_metric_event(event, encoded).await
        }
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
            labels: None,
            sink: SinkConfig::GnmiTarget(GnmiTargetConfig::new("127.0.0.1:0")),
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        }
    }

    fn encoder(path: &str, paths: &[(&str, &str)]) -> EncoderConfig {
        EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some(path.to_string()),
            paths: paths
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn write_without_event_is_an_error() {
        let mut sink = sink().await;
        let err = sink
            .write(b"bytes")
            .await
            .expect_err("raw bytes cannot be published");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(err.to_string().contains("gnmi encoder"), "got {err}");
        assert_eq!(sink.target().series_count(), 0);
    }

    /// A write is delivered once its event is in the target's table,
    /// whether or not anyone is subscribed.
    #[tokio::test]
    async fn last_write_delivered_after_an_upsert() {
        let mut sink = sink().await;
        assert!(!sink.last_write_delivered(), "nothing written yet");

        publish(&mut sink, "up").await;
        assert_eq!(sink.target().subscriber_count(), 0);
        assert!(
            sink.last_write_delivered(),
            "upserted with nobody subscribed"
        );

        let receiver = sink.target().subscribe();
        publish(&mut sink, "up").await;
        assert!(sink.last_write_delivered());
        drop(receiver);
        assert_eq!(sink.target().series_count(), 1);
    }

    /// A failed write is not delivered, even right after one that was.
    #[tokio::test]
    async fn a_failed_write_clears_last_write_delivered() {
        let mut sink = sink().await;
        publish(&mut sink, "up").await;
        assert!(sink.last_write_delivered());
        sink.write_metric_event(&event("down"), b"not protobuf")
            .await
            .expect_err("a series needs a path");
        assert!(!sink.last_write_delivered(), "the last write failed");
    }

    #[tokio::test]
    async fn first_event_that_is_not_a_notification_is_an_error() {
        let mut sink = sink().await;
        let err = sink
            .write_metric_event(&event("up"), b"not protobuf")
            .await
            .expect_err("a series needs a path");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(!sink.last_write_delivered());
        assert_eq!(sink.target().series_count(), 0);
    }

    /// A series' later event that is not a notification with a value is
    /// refused too, and the series keeps its latest value.
    #[tokio::test]
    async fn a_later_event_that_is_not_a_notification_is_an_error() {
        let mut sink = sink().await;
        let up = event("up");
        let first = notification(&up);
        sink.write_metric_event(&up, &first)
            .await
            .expect("the first write succeeds");
        let err = sink
            .write_metric_event(&up, b"not protobuf")
            .await
            .expect_err("every event needs a value");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(
            err.to_string().contains("not a gnmi notification"),
            "got {err}"
        );
        assert!(!sink.last_write_delivered());
        assert_eq!(sink.target().snapshot()[0].latest, first.as_slice());
    }

    /// Dropping a sink removes the series it published and leaves another
    /// sink's series, on the same target, in place.
    #[tokio::test]
    async fn drop_removes_the_sinks_series() {
        let mut first = sink().await;
        let addr = first.target().local_addr().to_string();
        let mut second = GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
            .await
            .expect("join the live target");
        let target = Arc::clone(first.target());
        assert!(Arc::ptr_eq(&target, second.target()));

        publish(&mut first, "in_octets").await;
        publish(&mut first, "out_octets").await;
        publish(&mut second, "in_errors").await;
        assert_eq!(target.series_count(), 3);

        drop(first);
        assert_eq!(target.series_count(), 1, "the first sink's two series left");
        let remaining: Vec<String> = target
            .snapshot()
            .into_iter()
            .map(|s| s.path.path().to_string())
            .collect();
        assert_eq!(remaining, ["openconfig:/a/in-errors"]);

        drop(second);
        assert_eq!(target.series_count(), 0);
    }

    /// Two sinks on one target, the first of which has published `/a/up`,
    /// and the error the second gets for its first write of the same path.
    async fn second_sink_on_an_owned_path() -> (GnmiTargetSink, GnmiTargetSink, SondaError) {
        let mut first = sink().await;
        let addr = first.target().local_addr().to_string();
        let mut second = GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
            .await
            .expect("acquire the live target");
        publish(&mut first, "up").await;
        let event = event("up");
        let err = second
            .write_metric_event(&event, &notification(&event))
            .await
            .expect_err("the path belongs to the first sink");
        (first, second, err)
    }

    /// A second sink's first write of a path another sink owns is a config
    /// error naming the path and the address.
    #[tokio::test]
    async fn a_second_sink_on_an_owned_path_is_refused() {
        let (first, second, err) = second_sink_on_an_owned_path().await;
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains(":/a/up`"), "names the path: {msg}");
        let addr = first.target().local_addr().to_string();
        assert!(msg.contains(&addr), "names the address: {msg}");
        assert!(!second.last_write_delivered());
    }

    /// The owner of a path keeps publishing it after another sink was
    /// refused.
    #[tokio::test]
    async fn the_owner_is_untouched_by_a_refused_sink() {
        let (mut first, _second, _) = second_sink_on_an_owned_path().await;
        publish(&mut first, "up").await;
        assert!(first.last_write_delivered());
        assert_eq!(first.target().series_count(), 1);
    }

    /// Dropping the refused sink leaves the owner's series in the table.
    #[tokio::test]
    async fn dropping_a_refused_sink_leaves_the_series() {
        let (first, second, _) = second_sink_on_an_owned_path().await;
        drop(second);
        assert_eq!(first.target().series_count(), 1);
        let target = Arc::clone(first.target());
        drop(first);
        assert_eq!(target.series_count(), 0, "the series leaves with its owner");
    }

    /// Two sinks of one launch share a path. When one drops, the series
    /// stays and another launch is still refused it; when the second drops,
    /// the series leaves and the port closes within the teardown bound.
    #[tokio::test]
    async fn a_shared_leaf_leaves_with_its_last_publisher() {
        let launch = LaunchId::next();
        let mut first = GnmiTargetSink::new_in(&GnmiTargetConfig::new("127.0.0.1:0"), launch)
            .await
            .expect("bind an ephemeral port");
        let addr = first.target().local_addr();
        let mut second = GnmiTargetSink::new_in(&GnmiTargetConfig::new(addr.to_string()), launch)
            .await
            .expect("acquire the live target");
        let target = Arc::clone(first.target());

        publish(&mut first, "up").await;
        publish(&mut second, "up").await;
        assert_eq!(target.series_count(), 1, "one launch, one shared series");
        let cached = |sink: &GnmiTargetSink| {
            let publication = sink.published.first().expect("cached");
            (publication.id, publication.path.clone())
        };
        let ((id, first_key), (_, second_key)) = (cached(&first), cached(&second));
        let held = target.path_of(id).expect("the series is served");
        assert!(
            first_key.is_same_key(&held) && second_key.is_same_key(&held),
            "both sinks cache the table's key"
        );

        drop(first);
        let paths: Vec<String> = target
            .snapshot()
            .iter()
            .map(|series| format!("{:?}", series.path))
            .collect();
        assert_eq!(paths.len(), 1, "the series outlives one of its sinks");
        assert!(paths[0].contains("\"up\""), "got {paths:?}");
        let mut intruder = GnmiTargetSink::new(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .expect("acquire the live target");
        let event = event("up");
        let err = intruder
            .write_metric_event(&event, &notification(&event))
            .await
            .expect_err("the path still belongs to the launch");
        assert!(err.to_string().contains("already published"), "got {err}");
        drop(intruder);
        publish(&mut second, "up").await;
        assert!(second.last_write_delivered());

        drop(second);
        assert_eq!(target.series_count(), 0, "the series left with the launch");
        let stopped = target.wait();
        drop(target);
        tokio::time::timeout(std::time::Duration::from_secs(2), stopped)
            .await
            .expect("the listener stops within the teardown bound");
        tokio::net::TcpListener::bind(addr)
            .await
            .expect("the port is free again");
    }

    /// Two sinks of one launch on different leaves: when one drops, its own
    /// leaf leaves while the other's is still served, and a new launch may
    /// then publish the freed leaf.
    #[tokio::test]
    async fn a_sinks_own_leaf_leaves_with_it() {
        let launch = LaunchId::next();
        let mut first = GnmiTargetSink::new_in(&GnmiTargetConfig::new("127.0.0.1:0"), launch)
            .await
            .expect("bind an ephemeral port");
        let addr = first.target().local_addr().to_string();
        let mut second = GnmiTargetSink::new_in(&GnmiTargetConfig::new(addr.clone()), launch)
            .await
            .expect("acquire the live target");
        let target = Arc::clone(first.target());
        publish(&mut first, "only_a").await;
        publish(&mut second, "only_b").await;
        assert_eq!(target.series_count(), 2);

        drop(first);
        let served: Vec<String> = target
            .snapshot()
            .iter()
            .map(|series| format!("{:?}", series.path))
            .collect();
        assert_eq!(served.len(), 1, "the dropped sink's leaf left with it");
        assert!(
            served[0].contains("only-b"),
            "the other leaf is served: {served:?}"
        );

        let mut reposted = GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
            .await
            .expect("acquire the live target");
        publish(&mut reposted, "only_a").await;
        assert!(
            reposted.last_write_delivered(),
            "a new launch takes the freed leaf"
        );
        assert_eq!(target.series_count(), 2);
    }

    /// One sink whose series render one leaf, once a label is dropped,
    /// upserts that leaf.
    #[tokio::test]
    async fn one_sink_publishing_a_leaf_twice_upserts_it() {
        use crate::encoder::gnmi::GnmiEncoder;
        use crate::encoder::Encoder;

        let mut sink = sink().await;
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            drop_labels: vec!["job".to_string()],
            ..Default::default()
        })
        .expect("valid encoder");
        for (job, value) in [("a", 1.0), ("b", 2.0)] {
            let event = MetricEvent::new(
                "up".to_string(),
                value,
                Labels::from_pairs(&[("job", job)]).expect("valid labels"),
            )
            .expect("valid event");
            let mut buf = Vec::new();
            encoder.encode_metric(&event, &mut buf).expect("encodes");
            sink.write_metric_event(&event, &buf)
                .await
                .expect("the sink's own leaf");
        }
        assert_eq!(sink.owned.len(), 2, "two series keys");
        assert_eq!(sink.published.len(), 1, "sharing one entry");
        assert_eq!(sink.target().series_count(), 1, "one leaf");
        let target = Arc::clone(sink.target());
        drop(sink);
        assert_eq!(target.series_count(), 0);
    }

    #[tokio::test]
    async fn scenario_delivers_every_event_through_the_hook() {
        let writes = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let inner = sink().await;
        let target = Arc::clone(inner.target());
        let mut sink: Box<dyn Sink> = Box::new(Counting {
            inner,
            writes: Arc::clone(&writes),
            events: Arc::clone(&events),
        });
        let config = ScenarioConfig {
            base: base("in_octets", 1_000.0, "250ms"),
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: encoder("/a/{name}", &[]),
            metric_type: None,
            help: None,
        };
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");

        let ticks = stats.read().unwrap().total_events as usize;
        assert!(ticks >= 100, "only {ticks} ticks ran");
        assert_eq!(
            events.load(Ordering::SeqCst),
            ticks,
            "every event used the hook"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "nothing fell back to write"
        );
        assert_eq!(target.series_count(), 1);
    }

    /// A scenario whose first write lands on a path another sink owns ends
    /// with that error recorded in its stats, and the owner keeps the path.
    #[tokio::test]
    async fn a_scenario_refused_a_path_records_why_it_stopped() {
        let mut owner = sink().await;
        publish(&mut owner, "in_octets").await;
        let addr = owner.target().local_addr().to_string();
        let mut sink: Box<dyn Sink> = Box::new(
            GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
                .await
                .expect("acquire the live target"),
        );
        let config = ScenarioConfig {
            base: base("in_octets", 100.0, "1s"),
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: encoder("/a/{name}", &[]),
            metric_type: None,
            help: None,
        };
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let err = crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect_err("the path belongs to the owner");

        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let st = stats.read().expect("stats lock");
        assert_eq!(st.errors, 1, "the refusal is counted");
        assert_eq!(st.total_events, 0);
        let reason = st.last_sink_error.clone().unwrap_or_default();
        assert!(reason.contains(":/a/in-octets`"), "got {reason:?}");
        drop(st);
        drop(sink);
        assert_eq!(owner.target().series_count(), 1, "the owner keeps the path");
    }

    /// The `le` / `quantile` key of every bucket or quantile path, and the
    /// names of the series that have none.
    fn keyed_paths(target: &GnmiTarget, key: &str) -> (Vec<String>, Vec<String>) {
        let mut keyed = Vec::new();
        let mut plain = Vec::new();
        for series in target.snapshot() {
            let last = series.path.path().elem.last().expect("a path has elements");
            match series.path.path().elem.iter().find_map(|e| e.key.get(key)) {
                Some(value) => keyed.push(value.clone()),
                None => plain.push(last.name.clone()),
            }
        }
        keyed.sort();
        plain.sort();
        (keyed, plain)
    }

    /// The entry kinds whose runner injects a label per series.
    enum Distribution {
        Histogram,
        Summary,
    }

    /// One tick of a histogram (4 buckets) or summary (3 quantiles) lands
    /// one series per bucket or quantile, keyed by `le` or `quantile`, plus
    /// `_sum` and `_count` on their own paths.
    #[rstest::rstest]
    #[case::histogram(Distribution::Histogram, "le", &["+Inf", "0.1", "0.5", "1", "5"])]
    #[case::summary(Distribution::Summary, "quantile", &["0.5", "0.9", "0.99"])]
    #[tokio::test]
    async fn histogram_scenario_lands_one_series_per_bucket(
        #[case] kind: Distribution,
        #[case] key: &str,
        #[case] expected: &[&str],
    ) {
        let inner = sink().await;
        let target = Arc::clone(inner.target());
        let mut sink: Box<dyn Sink> = Box::new(inner);
        let encoder = encoder(
            &format!("/latency[{key}={{{key}}}]/{{name}}"),
            &[
                ("latency_sum", "/latency/{name}"),
                ("latency_count", "/latency/{name}"),
            ],
        );
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let cancel = CancellationToken::new();
        match kind {
            Distribution::Histogram => {
                let config = HistogramScenarioConfig {
                    base: base("latency", 10.0, "150ms"),
                    buckets: Some(vec![0.1, 0.5, 1.0, 5.0]),
                    distribution: DistributionConfig::Exponential { rate: 10.0 },
                    observations_per_tick: Some(100),
                    mean_shift_per_sec: None,
                    seed: Some(42),
                    encoder,
                    metric_type: None,
                    help: None,
                };
                crate::config::validate::validate_histogram_config(&config)
                    .expect("the entry validates");
                crate::schedule::histogram_runner::run_with_sink(
                    &config,
                    &mut sink,
                    &cancel,
                    Some(Arc::clone(&stats)),
                )
                .await
            }
            Distribution::Summary => {
                let config = SummaryScenarioConfig {
                    base: base("latency", 10.0, "150ms"),
                    quantiles: Some(vec![0.5, 0.9, 0.99]),
                    distribution: DistributionConfig::Exponential { rate: 10.0 },
                    observations_per_tick: Some(100),
                    mean_shift_per_sec: None,
                    seed: Some(42),
                    encoder,
                    metric_type: None,
                    help: None,
                };
                crate::config::validate::validate_summary_config(&config)
                    .expect("the entry validates");
                crate::schedule::summary_runner::run_with_sink(
                    &config,
                    &mut sink,
                    &cancel,
                    Some(Arc::clone(&stats)),
                )
                .await
            }
        }
        .expect("the scenario runs to completion");
        assert_eq!(stats.read().unwrap().rejected_events, 0);

        assert_eq!(
            target.series_count(),
            expected.len() + 2,
            "one series per {key} value, plus sum and count"
        );
        let (keyed, plain) = keyed_paths(&target, key);
        assert_eq!(keyed, expected);
        assert_eq!(plain, ["latency-count", "latency-sum"]);
    }
}
