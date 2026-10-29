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
//! belongs to, which the agent keeps after the instance leaves. A record's
//! companions, the label columns of its series, are published the same way
//! and held with the instance.
//!
//! An instance, or a label column's instance, leaves the agent when the last
//! sink of its launch that published it drops, and at no other time.
//!
//! Requires the `snmp` feature flag.

pub mod agent;
mod responder;
pub(crate) mod usm;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};

use crate::encoder::snmp::ber::{Reader, Tag};
use crate::encoder::snmp::oid::Oid;
use crate::encoder::snmp::{split_record, Companions};
use crate::model::metric::{MetricEvent, MetricKey};
use crate::schedule::LaunchId;
use crate::sink::listener_registry::{canonical, ListenerRegistry};
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

use agent::Owner;
pub use agent::{LastRequest, RequestCounters, SnmpAgent};

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

/// The shortest SNMPv3 engine ID, in octets (RFC 3411).
pub(crate) const MIN_ENGINE_ID: usize = 5;

/// The longest SNMPv3 engine ID, in octets (RFC 3411).
pub(crate) const MAX_ENGINE_ID: usize = 32;

/// The longest SNMPv3 user name, in octets (RFC 3414).
pub(crate) const MAX_USER_NAME: usize = 32;

/// The shortest SNMPv3 password, in octets (RFC 3414 §11.2).
pub(crate) const MIN_PASSWORD: usize = 8;

/// The schema's note on a password's `minLength`.
#[cfg(feature = "schema")]
const PASSWORD_LENGTH_COMMENT: &str = "minLength counts characters where sonda counts \
     octets, so the schema is the looser of the two: sonda refuses a password of fewer than 8 \
     octets that the schema may accept.";

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
    /// The SNMP versions the agent answers, at least one and each once.
    /// Default `[v2c]`. A request in another version is dropped unanswered.
    #[cfg_attr(
        feature = "config",
        serde(default = "default_versions", deserialize_with = "versions")
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1), extend("uniqueItems" = true))
    )]
    pub versions: Vec<SnmpVersion>,
    /// The SNMPv3 users the agent accepts, each name once. `v3` in
    /// `versions` needs at least one. Default none.
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub users: Vec<UsmUser>,
    /// The agent's SNMPv3 engine ID, 5 to 32 octets written as hex, such as
    /// `"80001f8804736f6e6461"`; a leading `0x` is allowed. A string; a YAML
    /// number is an error. Default: made from the host name and the address
    /// the agent binds (net-snmp's enterprise prefix `80001f88`, format `05`,
    /// then the first 16 octets of the SHA-256 of
    /// `sonda:<host name>:<bound address>`; a `listen` on port 0 gives the
    /// port it was bound to), so an agent has the same one on every run on
    /// one host and another on every other host.
    #[cfg_attr(
        feature = "config",
        serde(
            default,
            deserialize_with = "engine_id",
            skip_serializing_if = "Option::is_none"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(
            regex(pattern = r"^(0[xX])?([0-9a-fA-F]{2}){5,32}$"),
            extend("x-sonda-no-widening" = true)
        )
    )]
    pub engine_id: Option<String>,
}

/// An SNMPv3 user of the User-based Security Model (RFC 3414): an item of
/// an [`SnmpAgentConfig`]'s `users:`.
///
/// Without `auth` the user is `noAuthNoPriv`; with it, every request must
/// be authenticated with the user's key (`authNoPriv`); with `priv` as well,
/// every request must also be encrypted with the user's privacy key
/// (`authPriv`). A user's level is the only one it is answered at. `priv`
/// needs `auth`. Unknown keys are rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsmUser {
    /// The user name (`securityName`), 1 to 32 octets.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = MAX_USER_NAME)))]
    pub name: String,
    /// The user's authentication protocol and password, when its requests
    /// are authenticated.
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub auth: Option<UsmAuth>,
    /// The user's privacy protocol and password, `priv:` in a config, when
    /// its requests are encrypted. Needs `auth`.
    #[cfg_attr(
        feature = "config",
        serde(rename = "priv", skip_serializing_if = "Option::is_none")
    )]
    pub privacy: Option<UsmPriv>,
}

/// An SNMPv3 user's authentication: the `auth:` block of a [`UsmUser`].
///
/// `Debug` prints `"***"` in place of the password.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsmAuth {
    /// The HMAC the user's requests are authenticated with.
    pub protocol: AuthProtocol,
    /// The password the user's key is made from, at least 8 octets. A
    /// string; a YAML number is an error.
    #[cfg_attr(
        feature = "schema",
        schemars(
            length(min = MIN_PASSWORD),
            extend(
                "writeOnly" = true,
                "x-sonda-no-widening" = true,
                "$comment" = PASSWORD_LENGTH_COMMENT
            )
        )
    )]
    pub password: String,
}

/// An SNMPv3 user's privacy: the `priv:` block of a [`UsmUser`].
///
/// The privacy key is made from the password with the user's
/// authentication hash. `Debug` prints `"***"` in place of the password.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsmPriv {
    /// The cipher the user's scoped PDUs are encrypted with.
    pub protocol: PrivProtocol,
    /// The password the user's privacy key is made from, at least 8 octets.
    /// A string; a YAML number is an error.
    #[cfg_attr(
        feature = "schema",
        schemars(
            length(min = MIN_PASSWORD),
            extend(
                "writeOnly" = true,
                "x-sonda-no-widening" = true,
                "$comment" = PASSWORD_LENGTH_COMMENT
            )
        )
    )]
    pub password: String,
}

/// An SNMPv3 authentication protocol, named as Telegraf names it, in lower
/// case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[non_exhaustive]
pub enum AuthProtocol {
    /// HMAC-MD5-96 (RFC 3414), `md5`. Legacy; prefer a SHA-2 protocol.
    #[cfg_attr(feature = "config", serde(rename = "md5"))]
    Md5,
    /// HMAC-SHA-96 (RFC 3414), `sha1`, also read as `sha`.
    #[cfg_attr(feature = "config", serde(rename = "sha1"))]
    Sha1,
    /// HMAC-SHA-224 truncated to 128 bits (RFC 7860), `sha224`.
    #[cfg_attr(feature = "config", serde(rename = "sha224"))]
    Sha224,
    /// HMAC-SHA-256 truncated to 192 bits (RFC 7860), `sha256`.
    #[cfg_attr(feature = "config", serde(rename = "sha256"))]
    Sha256,
    /// HMAC-SHA-384 truncated to 256 bits (RFC 7860), `sha384`.
    #[cfg_attr(feature = "config", serde(rename = "sha384"))]
    Sha384,
    /// HMAC-SHA-512 truncated to 384 bits (RFC 7860), `sha512`.
    #[cfg_attr(feature = "config", serde(rename = "sha512"))]
    Sha512,
}

impl AuthProtocol {
    /// The protocol's name in a config: `md5`, `sha1`, `sha224`, `sha256`,
    /// `sha384` or `sha512`.
    pub fn name(self) -> &'static str {
        match self {
            AuthProtocol::Md5 => "md5",
            AuthProtocol::Sha1 => "sha1",
            AuthProtocol::Sha224 => "sha224",
            AuthProtocol::Sha256 => "sha256",
            AuthProtocol::Sha384 => "sha384",
            AuthProtocol::Sha512 => "sha512",
        }
    }

    /// The protocol a config name reads as, `sha` as `sha1`.
    #[cfg(feature = "config")]
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "md5" => AuthProtocol::Md5,
            "sha" | "sha1" => AuthProtocol::Sha1,
            "sha224" => AuthProtocol::Sha224,
            "sha256" => AuthProtocol::Sha256,
            "sha384" => AuthProtocol::Sha384,
            "sha512" => AuthProtocol::Sha512,
            _ => return None,
        })
    }
}

// Not a derive: the parser also reads `sha`, which no variant is named.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for AuthProtocol {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AuthProtocol".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "sonda_core::sink::snmp::AuthProtocol".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "An SNMPv3 authentication protocol, named as Telegraf names it, in \
                            lower case: md5 (legacy; prefer a SHA-2 protocol), sha1 (also \
                            read as sha), sha224, sha256, sha384 or sha512.",
            "type": "string",
            "enum": ["md5", "sha", "sha1", "sha224", "sha256", "sha384", "sha512"],
        })
    }
}

impl fmt::Display for AuthProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An SNMPv3 privacy protocol, named as Telegraf names it, in lower case.
///
/// AES-192 and AES-256 need a longer key than SHA-1 and MD5 localize, and
/// two incompatible ways of extending it are in use: `aes192` and `aes256`
/// extend it as draft-blumenthal-aes-usm-04 does (Telegraf's `AES192` and
/// `AES256`), `aes192c` and `aes256c` as draft-reeder-snmpv3-usm-3desede-00
/// does (Telegraf's `AES192C` and `AES256C`, the extension Cisco devices
/// use). With an authentication hash at least as long as the key (SHA-256
/// and longer for AES-256) the two give the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[non_exhaustive]
pub enum PrivProtocol {
    /// DES-CBC (RFC 3414 §8), `des`. Legacy; prefer AES-128.
    #[cfg_attr(feature = "config", serde(rename = "des"))]
    Des,
    /// AES-128-CFB (RFC 3826), `aes128`, also read as `aes`.
    #[cfg_attr(feature = "config", serde(rename = "aes128"))]
    Aes128,
    /// AES-192-CFB with the Blumenthal key extension, `aes192`.
    #[cfg_attr(feature = "config", serde(rename = "aes192"))]
    Aes192,
    /// AES-192-CFB with the Reeder key extension, `aes192c`.
    #[cfg_attr(feature = "config", serde(rename = "aes192c"))]
    Aes192c,
    /// AES-256-CFB with the Blumenthal key extension, `aes256`.
    #[cfg_attr(feature = "config", serde(rename = "aes256"))]
    Aes256,
    /// AES-256-CFB with the Reeder key extension, `aes256c`.
    #[cfg_attr(feature = "config", serde(rename = "aes256c"))]
    Aes256c,
}

impl PrivProtocol {
    /// The protocol's name in a config: `des`, `aes128`, `aes192`,
    /// `aes192c`, `aes256` or `aes256c`.
    pub fn name(self) -> &'static str {
        match self {
            PrivProtocol::Des => "des",
            PrivProtocol::Aes128 => "aes128",
            PrivProtocol::Aes192 => "aes192",
            PrivProtocol::Aes192c => "aes192c",
            PrivProtocol::Aes256 => "aes256",
            PrivProtocol::Aes256c => "aes256c",
        }
    }

    /// The protocol a config name reads as, `aes` as `aes128`.
    #[cfg(feature = "config")]
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "des" => PrivProtocol::Des,
            "aes" | "aes128" => PrivProtocol::Aes128,
            "aes192" => PrivProtocol::Aes192,
            "aes192c" => PrivProtocol::Aes192c,
            "aes256" => PrivProtocol::Aes256,
            "aes256c" => PrivProtocol::Aes256c,
            _ => return None,
        })
    }
}

// Not a derive: the parser also reads `aes`, which no variant is named.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for PrivProtocol {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PrivProtocol".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "sonda_core::sink::snmp::PrivProtocol".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "An SNMPv3 privacy protocol, named as Telegraf names it, in lower \
                            case: des (legacy; prefer aes128), aes128 (also read as aes), \
                            aes192 and aes256 (the Blumenthal key extension), aes192c and \
                            aes256c (the Reeder key extension).",
            "type": "string",
            "enum": ["des", "aes", "aes128", "aes192", "aes192c", "aes256", "aes256c"],
        })
    }
}

impl fmt::Display for PrivProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Prints `"***"` in place of the password.
impl fmt::Debug for UsmAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsmAuth")
            .field("protocol", &self.protocol)
            .field("password", &"***")
            .finish()
    }
}

/// Prints `"***"` in place of the password.
impl fmt::Debug for UsmPriv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsmPriv")
            .field("protocol", &self.protocol)
            .field("password", &"***")
            .finish()
    }
}

/// The fields of a `users:` item and its `auth:` and `priv:` blocks as
/// written. The derives' errors name these structs, `UsmUser`, `UsmAuth`
/// and `UsmPriv`.
#[cfg(feature = "config")]
mod user_fields {
    /// A user: its name read as text, its `auth` and its `priv`.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct UsmUser {
        #[serde(deserialize_with = "crate::config::scalar_text::required")]
        pub(super) name: String,
        #[serde(default)]
        pub(super) auth: Option<super::UsmAuth>,
        #[serde(rename = "priv", default)]
        pub(super) privacy: Option<super::UsmPriv>,
    }

    /// An `auth:` block: the protocol, and the password as a string only.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct UsmAuth {
        pub(super) protocol: super::AuthProtocol,
        #[serde(deserialize_with = "super::auth_password")]
        pub(super) password: String,
    }

    /// A `priv:` block: the protocol, and the password as a string only.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct UsmPriv {
        pub(super) protocol: super::PrivProtocol,
        #[serde(deserialize_with = "super::priv_password")]
        pub(super) password: String,
    }
}

/// The error for an `auth:` value that is not a map. It names no part of
/// the value.
#[cfg(feature = "config")]
const AUTH_NOT_A_MAP: &str = "snmp_agent users auth must be a map with protocol and password";

/// The error for a `priv:` value that is not a map. It names no part of
/// the value.
#[cfg(feature = "config")]
const PRIV_NOT_A_MAP: &str = "snmp_agent users priv must be a map with protocol and password";

/// Reads a map with `name` and optionally `auth` and `priv`, refusing any
/// other key.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for UsmUser {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = user_fields::UsmUser::deserialize(deserializer)?;
        Ok(UsmUser {
            name: fields.name,
            auth: fields.auth,
            privacy: fields.privacy,
        })
    }
}

/// Reads a map with `protocol` and `password`, refusing any other key. Any
/// other value is refused with [`AUTH_NOT_A_MAP`], which does not repeat
/// the value.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for UsmAuth {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        map_only::<D, user_fields::UsmAuth, UsmAuth>(deserializer, AUTH_NOT_A_MAP)
    }
}

/// Reads a map with `protocol` and `password`, refusing any other key. Any
/// other value is refused with [`PRIV_NOT_A_MAP`], which does not repeat
/// the value.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for UsmPriv {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        map_only::<D, user_fields::UsmPriv, UsmPriv>(deserializer, PRIV_NOT_A_MAP)
    }
}

/// A `Fields` map read into a `T`; any value that is not a map is refused
/// with `refused`, which does not repeat the value.
#[cfg(feature = "config")]
fn map_only<'de, D, Fields, T>(deserializer: D, refused: &'static str) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    Fields: serde::Deserialize<'de> + Into<T>,
{
    use std::marker::PhantomData;

    use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
    use serde::de::{Error, MapAccess, SeqAccess, Visitor};

    struct MapOnly<Fields, T> {
        refused: &'static str,
        read: PhantomData<fn() -> (Fields, T)>,
    }
    impl<Fields, T> MapOnly<Fields, T> {
        fn refuse<E: Error>(self) -> Result<T, E> {
            Err(E::custom(self.refused))
        }
    }
    impl<'de, Fields, T> Visitor<'de> for MapOnly<Fields, T>
    where
        Fields: serde::Deserialize<'de> + Into<T>,
    {
        type Value = T;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map with protocol and password")
        }
        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            Fields::deserialize(MapAccessDeserializer::new(map)).map(Into::into)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Fields::deserialize(SeqAccessDeserializer::new(seq)).map(Into::into)
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
    deserializer.deserialize_any(MapOnly::<Fields, T> {
        refused,
        read: PhantomData,
    })
}

#[cfg(feature = "config")]
impl From<user_fields::UsmAuth> for UsmAuth {
    fn from(fields: user_fields::UsmAuth) -> Self {
        Self {
            protocol: fields.protocol,
            password: fields.password,
        }
    }
}

#[cfg(feature = "config")]
impl From<user_fields::UsmPriv> for UsmPriv {
    fn from(fields: user_fields::UsmPriv) -> Self {
        Self {
            protocol: fields.protocol,
            password: fields.password,
        }
    }
}

/// Reads a protocol name, `sha` as `sha1`; any other name or value is an
/// error naming the field.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for AuthProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        use crate::config::scalar_text::ScalarText;

        let ScalarText(name) = ScalarText::deserialize(deserializer)?;
        AuthProtocol::from_name(&name).ok_or_else(|| {
            D::Error::custom(format!(
                "snmp_agent users auth.protocol: unknown protocol {name:?}; expected md5, sha1 \
                 (or sha), sha224, sha256, sha384 or sha512"
            ))
        })
    }
}

/// Reads a protocol name, `aes` as `aes128`; any other name or value is an
/// error naming the field.
#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for PrivProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        use crate::config::scalar_text::ScalarText;

        let ScalarText(name) = ScalarText::deserialize(deserializer)?;
        PrivProtocol::from_name(&name).ok_or_else(|| {
            D::Error::custom(format!(
                "snmp_agent users priv.protocol: unknown protocol {name:?}; expected des, aes128 \
                 (or aes), aes192, aes192c, aes256 or aes256c"
            ))
        })
    }
}

/// An SNMP version an agent can answer: the `versions:` items of an
/// [`SnmpAgentConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum SnmpVersion {
    /// SNMPv1 (RFC 1157), `v1`: `Get`, `GetNext` and `Set` without
    /// `Counter64`, every error `noSuchName` at its varbind.
    #[cfg_attr(feature = "config", serde(rename = "v1"))]
    V1,
    /// SNMPv2c (RFC 3416), `v2c`.
    #[cfg_attr(feature = "config", serde(rename = "v2c"))]
    V2c,
    /// SNMPv3 with the User-based Security Model (RFC 3414), `v3`: the
    /// SNMPv2c operations for the configured `users`, `noAuthNoPriv`,
    /// `authNoPriv` or `authPriv`.
    #[cfg_attr(feature = "config", serde(rename = "v3"))]
    V3,
}

impl SnmpVersion {
    /// The version's name in a config: `v1`, `v2c` or `v3`.
    pub fn name(self) -> &'static str {
        match self {
            SnmpVersion::V1 => "v1",
            SnmpVersion::V2c => "v2c",
            SnmpVersion::V3 => "v3",
        }
    }
}

impl fmt::Display for SnmpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
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
fn default_versions() -> Vec<SnmpVersion> {
    vec![SnmpVersion::V2c]
}

#[cfg(feature = "config")]
fn default_object_id() -> String {
    DEFAULT_SYS_OBJECT_ID.to_string()
}

#[cfg(feature = "config")]
fn default_services() -> u8 {
    DEFAULT_SYS_SERVICES
}

/// `deserialize_with` for `versions`: a sequence of version names, any
/// other value or name an error naming the field.
#[cfg(feature = "config")]
fn versions<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<SnmpVersion>, D::Error> {
    use serde::de::{Error, SeqAccess, Visitor};

    use crate::config::scalar_text::ScalarText;

    struct Versions;

    impl<'de> Visitor<'de> for Versions {
        type Value = Vec<SnmpVersion>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("snmp_agent versions as a list of v1, v2c and v3, such as [v1, v2c]")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut versions = Vec::new();
            while let Some(ScalarText(name)) = seq.next_element()? {
                versions.push(match name.as_str() {
                    "v1" => SnmpVersion::V1,
                    "v2c" => SnmpVersion::V2c,
                    "v3" => SnmpVersion::V3,
                    _ => {
                        return Err(A::Error::custom(format!(
                            "snmp_agent versions: unknown version {name:?}; expected v1, v2c \
                             or v3"
                        )))
                    }
                });
            }
            Ok(versions)
        }
    }

    deserializer.deserialize_seq(Versions)
}

/// `deserialize_with` for `engine_id`: a string only. A YAML number or
/// boolean is an error naming the field: read as a number, hex such as
/// `0000000002` would lose its leading zeros.
#[cfg(feature = "config")]
fn engine_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    use serde::de::{Error, Visitor};

    struct EngineId;
    impl EngineId {
        fn not_text<E: Error>(&self, kind: &str, value: &dyn fmt::Display) -> E {
            E::custom(format!(
                "snmp_agent engine_id: an engine ID is written as a hex string, and the YAML \
                 {kind} {value} is not one; quote it, as in \"80001f8804736f6e6461\""
            ))
        }
    }
    impl Visitor<'_> for EngineId {
        type Value = Option<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an snmp_agent engine_id as a hex string")
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
            Err(self.not_text("boolean", &v))
        }
        fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
            Err(self.not_text("number", &v))
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
            Err(self.not_text("number", &v))
        }
        fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
            Err(self.not_text("number", &v))
        }
    }
    deserializer.deserialize_any(EngineId)
}

/// `deserialize_with` for `auth.password`: [`password`] of that field.
#[cfg(feature = "config")]
fn auth_password<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    password(
        deserializer,
        "snmp_agent users auth.password must be a string; quote it",
    )
}

/// `deserialize_with` for `priv.password`: [`password`] of that field.
#[cfg(feature = "config")]
fn priv_password<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    password(
        deserializer,
        "snmp_agent users priv.password must be a string; quote it",
    )
}

/// A password: a string only. Any other value is refused with
/// `not_a_string`, which does not repeat it.
#[cfg(feature = "config")]
fn password<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    not_a_string: &'static str,
) -> Result<String, D::Error> {
    use serde::de::{Error, Visitor};

    struct Password(&'static str);
    impl Visitor<'_> for Password {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an snmp_agent password as a string")
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.to_string())
        }
        fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(v)
        }
        fn visit_bool<E: Error>(self, _: bool) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
        fn visit_i64<E: Error>(self, _: i64) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
        fn visit_u64<E: Error>(self, _: u64) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
        fn visit_f64<E: Error>(self, _: f64) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
        fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
        fn visit_bytes<E: Error>(self, _: &[u8]) -> Result<Self::Value, E> {
            Err(E::custom(self.0))
        }
    }
    deserializer.deserialize_any(Password(not_a_string))
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
            .field("versions", &self.versions)
            .field("users", &self.users)
            .field("engine_id", &self.engine_id)
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
            versions: vec![SnmpVersion::V2c],
            users: Vec::new(),
            engine_id: None,
        }
    }

    /// Resolve the config into the settings an agent runs with.
    ///
    /// This is the one place the fields are checked; validation and
    /// [`SnmpAgentSink::new`] both call it. Returns [`SondaError::Config`]
    /// when `listen` is not a socket address, `max_message_size` is outside
    /// 484 to 65507, `system.object_id` is not an OID, `system.services` is
    /// above 127, `versions` is empty or names a version twice, `versions`
    /// names `v3` and `users` is empty, a user name is empty, longer than 32
    /// octets or given twice, a user has `priv` without `auth`, a password is
    /// shorter than 8 octets, or `engine_id` is not 5 to 32 octets of hex.
    /// An IPv4-mapped `listen` resolves to the IPv4 address it maps;
    /// `versions` resolves to its versions in ascending order, `users` to its
    /// users ordered by name, and `engine_id` to its octets.
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
        let mut versions = self.versions.clone();
        versions.sort_unstable();
        if versions.is_empty() {
            return Err(invalid(
                "snmp_agent versions must name at least one version, such as [v2c]".to_string(),
            ));
        }
        if let Some(twice) = versions.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(invalid(format!(
                "snmp_agent versions names {} more than once",
                twice[0]
            )));
        }
        let users = self.resolved_users()?;
        if versions.contains(&SnmpVersion::V3) && users.is_empty() {
            return Err(invalid(
                "snmp_agent versions names v3, which needs at least one user under users"
                    .to_string(),
            ));
        }
        // An IPv4-mapped address binds the IPv4 socket it maps, so it is
        // that address everywhere a listen address is compared or keyed.
        let listen = canonical(listen);
        let engine_id = self.engine_id.as_deref().map(parse_engine_id).transpose()?;
        Ok(AgentSettings {
            listen,
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
            versions,
            users,
            engine_id,
        })
    }

    /// `users`, checked and ordered by name.
    fn resolved_users(&self) -> Result<Vec<UsmUser>, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        let mut users = self.users.clone();
        users.sort_by(|a, b| a.name.cmp(&b.name));
        for user in &users {
            if user.name.is_empty() || user.name.len() > MAX_USER_NAME {
                return Err(invalid(format!(
                    "snmp_agent users name {:?} must be 1 to {MAX_USER_NAME} bytes, got {}",
                    user.name,
                    user.name.len()
                )));
            }
            if let Some(auth) = &user.auth {
                if auth.password.len() < MIN_PASSWORD {
                    return Err(invalid(format!(
                        "snmp_agent users {:?} auth.password must be at least {MIN_PASSWORD} \
                         bytes (RFC 3414)",
                        user.name
                    )));
                }
            }
            if let Some(privacy) = &user.privacy {
                if user.auth.is_none() {
                    return Err(invalid(format!(
                        "snmp_agent users {:?} has priv without auth; privacy (authPriv) needs \
                         auth as well",
                        user.name
                    )));
                }
                if privacy.password.len() < MIN_PASSWORD {
                    return Err(invalid(format!(
                        "snmp_agent users {:?} priv.password must be at least {MIN_PASSWORD} \
                         bytes (RFC 3414)",
                        user.name
                    )));
                }
            }
        }
        if let Some(twice) = users.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(invalid(format!(
                "snmp_agent users names {:?} more than once",
                twice[0].name
            )));
        }
        Ok(users)
    }
}

/// The octets of an `engine_id` written as hex, with or without a leading
/// `0x`.
///
/// # Errors
///
/// [`SondaError::Config`] naming the field when `text` is not an even number
/// of hex digits, or is not 5 to 32 octets.
fn parse_engine_id(text: &str) -> Result<Vec<u8>, SondaError> {
    let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if !digits.len().is_multiple_of(2) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(format!(
            "snmp_agent engine_id {text:?} is not hex, such as \"80001f8804736f6e6461\""
        )));
    }
    let octets: Vec<u8> = digits
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let nibble = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
            nibble(pair[0]) << 4 | nibble(pair[1])
        })
        .collect();
    if !(MIN_ENGINE_ID..=MAX_ENGINE_ID).contains(&octets.len()) {
        return Err(invalid(format!(
            "snmp_agent engine_id must be {MIN_ENGINE_ID} to {MAX_ENGINE_ID} bytes, got {}",
            octets.len()
        )));
    }
    Ok(octets)
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
    /// The versions answered, ascending, each once.
    pub(crate) versions: Vec<SnmpVersion>,
    /// The SNMPv3 users, ordered by name, each name once.
    pub(crate) users: Vec<UsmUser>,
    /// The configured SNMPv3 engine ID; without one, the agent makes its
    /// own from the host name and the address it binds.
    pub(crate) engine_id: Option<Vec<u8>>,
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
            .field("versions", &self.versions)
            .field("users", &self.users)
            .field("engine_id", &self.engine_id.as_deref().map(Hex))
            .finish()
    }
}

/// Octets displayed as lower-case hex.
struct Hex<'a>(&'a [u8]);

impl fmt::Debug for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// Whether `a` and `b` are the same users: each pair equal in name and in
/// both protocols, the passwords compared in constant time.
fn same_users(a: &[UsmUser], b: &[UsmUser]) -> bool {
    fn same_secret<P: PartialEq>(a: Option<(P, &str)>, b: Option<(P, &str)>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some((a, a_password)), Some((b, b_password))) => {
                a == b
                    && crate::util::constant_time_eq(a_password.as_bytes(), b_password.as_bytes())
            }
            _ => false,
        }
    }
    fn auth(user: &UsmUser) -> Option<(AuthProtocol, &str)> {
        user.auth
            .as_ref()
            .map(|auth| (auth.protocol, auth.password.as_str()))
    }
    fn privacy(user: &UsmUser) -> Option<(PrivProtocol, &str)> {
        user.privacy
            .as_ref()
            .map(|privacy| (privacy.protocol, privacy.password.as_str()))
    }
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.name == b.name && same_secret(auth(a), auth(b)) && same_secret(privacy(a), privacy(b))
        })
}

impl AgentSettings {
    /// Whether an agent running with `self` can also serve `other`: every
    /// setting but the listen address is equal, the community and the
    /// passwords compared in constant time.
    pub(crate) fn serves(&self, other: &AgentSettings) -> bool {
        crate::util::constant_time_eq(self.community.as_bytes(), other.community.as_bytes())
            && self.max_message_size == other.max_message_size
            && self.system == other.system
            && self.versions == other.versions
            && self.engine_id == other.engine_id
            && same_users(&self.users, &other.users)
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
        if self.versions != other.versions {
            let list = |versions: &[SnmpVersion]| {
                let names: Vec<&str> = versions.iter().map(|v| v.name()).collect();
                format!("[{}]", names.join(", "))
            };
            differences.push(format!(
                "versions {} and {}",
                list(&self.versions),
                list(&other.versions)
            ));
        }
        if self.engine_id != other.engine_id {
            let id = |id: &Option<Vec<u8>>| match id {
                Some(id) => Hex(id).to_string(),
                None => "derived".to_string(),
            };
            differences.push(format!(
                "engine_id {} and {}",
                id(&self.engine_id),
                id(&other.engine_id)
            ));
        }
        if !same_users(&self.users, &other.users) {
            let list = |users: &[UsmUser]| {
                let users: Vec<String> = users
                    .iter()
                    .map(|user| match (&user.auth, &user.privacy) {
                        (Some(auth), Some(privacy)) => {
                            format!("{} {} {}", user.name, auth.protocol, privacy.protocol)
                        }
                        (Some(auth), None) => format!("{} {}", user.name, auth.protocol),
                        (None, _) => format!("{} without auth", user.name),
                    })
                    .collect();
                format!("[{}]", users.join(", "))
            };
            let (a, b) = (list(&self.users), list(&other.users));
            if a != b {
                differences.push(format!("users {a} and {b}"));
            } else {
                differences.push("users with another password".to_string());
            }
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
/// is one publish record ([`split_record`]): the number of index arcs at the
/// end of the instance OID, the instance's `VarBind`, and the companion
/// records of label columns, which the encoder sends with a series' first
/// record only. The first record of each series decodes the instance OID,
/// publishes it and, once the agent has taken it, registers the object it
/// belongs to (the OID without its index arcs); later records of the series
/// reuse the decoded OID. Each companion is published under the same owner,
/// once per instance that carries it, and its object registered; the sink
/// remembers it with the instance and releases it after the instance. An
/// instance is owned by the launch whose sink created it and held while any
/// sink of that launch has published it: the sinks of that launch share it,
/// a sink of another launch is refused it, and it leaves the agent when the
/// last of its publishers drops. A label column's instance is held the same
/// way, so it stays while any instance carrying it lives. Dropping the last
/// sink on an address stops its agent.
///
/// A sink made by joining a running agent publishes no instance of its own:
/// it replaces the latest value of an instance the agent already serves,
/// and refuses an event for any other OID. It replaces a companion only
/// where the agent already serves its OID.
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

/// An instance this sink has published: its OID, the buffers its
/// `VarBind`s are copied into, and the label columns' instances published
/// with it, once each.
struct Publication {
    oid: Oid,
    buffers: PublishBuffers,
    companions: Vec<Oid>,
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
    /// instance, copied into one of the [`PublishBuffers`] of that instance,
    /// then each of its companions as a label column's instance held with
    /// it. Never awaits.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when the agent's responder task has exited, or
    /// when `encoded` is not a publish record; [`SondaError::Config`] when
    /// the instance or a companion belongs to another launch or is under the
    /// system group, or a joined sink's event names an instance the agent
    /// does not serve.
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        self.last_delivered = false;
        self.agent.check_serving()?;
        let record = split_record(encoded)?;
        let varbind = record.varbind;
        if self.joined {
            let oid = instance_oid(varbind)?;
            if !self.agent.overwrite(&oid, Bytes::copy_from_slice(varbind)) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario serves the snmp OID {oid} on {}",
                    self.agent.local_addr()
                ))));
            }
            self.last_delivered = true;
            for companion in record.companions {
                let oid = instance_oid(companion.varbind)?;
                self.agent
                    .overwrite(&oid, Bytes::copy_from_slice(companion.varbind));
            }
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
                        let object = object_of(&oid, record.index_arcs)?;
                        let mut buffers = PublishBuffers::default();
                        let stored = buffers.frozen(varbind);
                        self.agent.publish(self.owner, &oid, stored, true)?;
                        self.agent.register_object(&object);
                        let entry = self.published.len();
                        self.published.push(Publication {
                            oid: oid.clone(),
                            buffers,
                            companions: Vec::new(),
                        });
                        self.instances.insert(oid, entry);
                        self.owned.insert(key, entry);
                        self.last_delivered = true;
                        return self.publish_companions(entry, record.companions);
                    }
                }
            }
        };
        let publication = &mut self.published[entry];
        let stored = publication.buffers.frozen(varbind);
        self.agent
            .publish(self.owner, &publication.oid, stored, false)?;
        self.last_delivered = true;
        if record.companions.len() > 0 {
            return self.publish_companions(entry, record.companions);
        }
        Ok(())
    }
}

impl SnmpAgentSink {
    /// Publish each companion of a record as a label column's instance held
    /// with `published[entry]`: under this sink's owner, counted as a
    /// publisher once per instance that carries it, its object registered
    /// the first time.
    fn publish_companions(
        &mut self,
        entry: usize,
        companions: Companions<'_>,
    ) -> Result<(), SondaError> {
        for companion in companions {
            let oid = instance_oid(companion.varbind)?;
            let first = !self.published[entry].companions.contains(&oid);
            let object = if first {
                Some(object_of(&oid, companion.index_arcs)?)
            } else {
                None
            };
            self.agent.publish(
                self.owner,
                &oid,
                Bytes::copy_from_slice(companion.varbind),
                first,
            )?;
            if let Some(object) = object {
                self.agent.register_object(&object);
                self.published[entry].companions.push(oid);
            }
        }
        Ok(())
    }
}

impl Drop for SnmpAgentSink {
    /// Release this sink's claim on each instance it published, once per
    /// instance, then on the label columns' instances published with it.
    fn drop(&mut self) {
        for publication in &self.published {
            self.agent.release(self.owner, &publication.oid);
            for companion in &publication.companions {
                self.agent.release(self.owner, companion);
            }
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
    use crate::encoder::snmp::ber;
    use crate::encoder::snmp::{SnmpEncoder, SnmpEncoderConfig, SnmpValueType, RECORD_HEADER};
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

    /// `versions` is `[v2c]` when absent, reads `v1` and `v2c` in any order,
    /// and resolves to its versions ascending.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::absent(       "",                       &[SnmpVersion::V2c],                   &[SnmpVersion::V2c])]
    #[case::v1_alone(     "versions: [v1]\n",       &[SnmpVersion::V1],                    &[SnmpVersion::V1])]
    #[case::both(         "versions: [v1, v2c]\n",  &[SnmpVersion::V1, SnmpVersion::V2c],  &[SnmpVersion::V1, SnmpVersion::V2c])]
    #[case::both_reversed("versions: [v2c, v1]\n",  &[SnmpVersion::V2c, SnmpVersion::V1],  &[SnmpVersion::V1, SnmpVersion::V2c])]
    fn versions_default_to_v2c_and_resolve_ascending(
        #[case] line: &str,
        #[case] parsed: &[SnmpVersion],
        #[case] resolved: &[SnmpVersion],
    ) {
        let yaml = format!("type: snmp_agent\nlisten: 0.0.0.0:1161\n{line}");
        let config: SinkConfig = serde_yaml_ng::from_str(&yaml).expect("the block parses");
        let SinkConfig::SnmpAgent(cfg) = config else {
            panic!("an snmp_agent sink, got {config:?}");
        };
        assert_eq!(cfg.versions, parsed);
        let settings = cfg.settings().expect("valid settings");
        assert_eq!(settings.versions, resolved);
        assert_eq!(SnmpAgentConfig::new("0.0.0.0:1161").versions, [SnmpVersion::V2c]);
    }

    /// `settings()` refuses an empty `versions` and one naming a version
    /// twice, naming the field.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::empty(    &[],                                     "snmp_agent versions must name at least one version")]
    #[case::duplicate(&[SnmpVersion::V2c, SnmpVersion::V2c],   "snmp_agent versions names v2c more than once")]
    #[case::duplicate_among_two(&[SnmpVersion::V1, SnmpVersion::V2c, SnmpVersion::V1], "snmp_agent versions names v1 more than once")]
    fn settings_refuse_an_empty_or_repeated_versions_list(
        #[case] versions: &[SnmpVersion],
        #[case] refused: &str,
    ) {
        let mut cfg = SnmpAgentConfig::new("127.0.0.1:1161");
        cfg.settings().expect("the baseline is valid");
        cfg.versions = versions.to_vec();
        match cfg.settings() {
            Err(SondaError::Config(e)) => assert!(e.to_string().contains(refused), "{e}"),
            other => panic!("expected {refused:?}, got {other:?}"),
        }
    }

    /// A version name other than `v1`, `v2c` and `v3`, a value that is not a list,
    /// and a misspelt `version:` key are refused at parse time, naming the
    /// field.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::v4(          "versions: [v4]",       "snmp_agent versions: unknown version \"v4\"; expected v1, v2c or v3")]
    #[case::upper_case(  "versions: [V1]",       "snmp_agent versions: unknown version \"V1\"")]
    #[case::a_number(    "versions: [1]",        "snmp_agent versions: unknown version \"1\"")]
    #[case::not_a_list(  "versions: v1",         "expected snmp_agent versions as a list of v1, v2c and v3")]
    #[case::misspelt_key("version: [v1]",        "unknown field `version`")]
    fn an_unknown_version_is_refused_naming_the_field(#[case] line: &str, #[case] refused: &str) {
        let parse = |line: &str| {
            serde_yaml_ng::from_str::<SinkConfig>(&format!(
                "type: snmp_agent\nlisten: 0.0.0.0:1161\n{line}\n"
            ))
        };
        parse("versions: [v1, v2c]").expect("the baseline parses");
        let msg = parse(line).expect_err("refused").to_string();
        assert!(msg.contains(refused), "{msg}");
    }

    /// Two agent configs on one address that differ in `versions` do not
    /// serve each other, and the difference names both lists; the same
    /// versions in another order do.
    #[test]
    fn versions_are_compared_as_a_set() {
        let with = |versions: &[SnmpVersion]| {
            SnmpAgentConfig {
                versions: versions.to_vec(),
                ..SnmpAgentConfig::new("127.0.0.1:1161")
            }
            .settings()
            .expect("valid settings")
        };
        let both = with(&[SnmpVersion::V1, SnmpVersion::V2c]);
        assert!(both.serves(&with(&[SnmpVersion::V2c, SnmpVersion::V1])));
        assert_eq!(both.beside(&with(&[SnmpVersion::V2c, SnmpVersion::V1])), "");
        let v2c = with(&[SnmpVersion::V2c]);
        assert!(!both.serves(&v2c));
        assert_eq!(both.beside(&v2c), "versions [v1, v2c] and [v2c]");
    }

    // ---- SNMPv3 config ------------------------------------------------------

    /// A config answering v2c and v3 for `users`.
    fn with_users(users: Vec<UsmUser>) -> SnmpAgentConfig {
        SnmpAgentConfig {
            versions: vec![SnmpVersion::V2c, SnmpVersion::V3],
            users,
            ..SnmpAgentConfig::new("127.0.0.1:1161")
        }
    }

    fn user(name: &str, protocol: AuthProtocol, password: &str) -> UsmUser {
        UsmUser {
            name: name.to_string(),
            auth: Some(UsmAuth {
                protocol,
                password: password.to_string(),
            }),
            privacy: None,
        }
    }

    /// The YAML sink block with `rest` after `versions: [v2c, v3]`.
    #[cfg(feature = "config")]
    fn v3_block(rest: &str) -> String {
        format!("type: snmp_agent\nlisten: 0.0.0.0:1161\nversions: [v2c, v3]\n{rest}")
    }

    #[cfg(feature = "config")]
    fn parse_block(yaml: &str) -> Result<SnmpAgentConfig, String> {
        match serde_yaml_ng::from_str::<SinkConfig>(yaml) {
            Ok(SinkConfig::SnmpAgent(cfg)) => Ok(cfg),
            Ok(other) => panic!("an snmp_agent sink, got {other:?}"),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A full v3 block parses: each protocol by its name, `sha` as `sha1`, a
    /// user without auth, the engine ID with and without `0x`; and resolves
    /// with the users ordered by name.
    #[cfg(feature = "config")]
    #[test]
    fn a_v3_block_parses_every_protocol_and_sha_as_sha1() {
        let yaml = v3_block(
            "engine_id: \"0x80001F8804736f6e6461\"\nusers:\n\
             \x20 - { name: telegraf, auth: { protocol: sha256, password: lab-only-password } }\n\
             \x20 - { name: a-md5, auth: { protocol: md5, password: lab-only-password } }\n\
             \x20 - { name: b-sha, auth: { protocol: sha, password: lab-only-password } }\n\
             \x20 - { name: c-sha1, auth: { protocol: sha1, password: lab-only-password } }\n\
             \x20 - { name: d-224, auth: { protocol: sha224, password: lab-only-password } }\n\
             \x20 - { name: e-384, auth: { protocol: sha384, password: lab-only-password } }\n\
             \x20 - { name: f-512, auth: { protocol: sha512, password: lab-only-password } }\n\
             \x20 - { name: 12345 }\n",
        );
        let cfg = parse_block(&yaml).expect("the block parses");
        let protocols: Vec<Option<AuthProtocol>> = cfg
            .users
            .iter()
            .map(|user| user.auth.as_ref().map(|auth| auth.protocol))
            .collect();
        use AuthProtocol::*;
        assert_eq!(
            protocols,
            [
                Some(Sha256),
                Some(Md5),
                Some(Sha1),
                Some(Sha1),
                Some(Sha224),
                Some(Sha384),
                Some(Sha512),
                None
            ]
        );
        assert_eq!(
            cfg.users[7].name, "12345",
            "a numeric user name is its text"
        );
        let settings = cfg.settings().expect("valid settings");
        assert_eq!(settings.engine_id, Some(hex("80001f8804736f6e6461")));
        let names: Vec<&str> = settings.users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            ["12345", "a-md5", "b-sha", "c-sha1", "d-224", "e-384", "f-512", "telegraf"]
        );
    }

    /// Each refused v3 setting names its field: v3 without users, a user
    /// named twice, an empty or 33-byte name, a 7-byte password, an engine
    /// ID of 4 or 33 bytes or not hex. The baseline is valid.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::v3_without_users(   |c: &mut SnmpAgentConfig| c.users.clear(),                                                     "snmp_agent versions names v3, which needs at least one user under users")]
    #[case::duplicate_user(     |c: &mut SnmpAgentConfig| c.users.push(user("telegraf", AuthProtocol::Md5, "other-password")), "snmp_agent users names \"telegraf\" more than once")]
    #[case::empty_name(         |c: &mut SnmpAgentConfig| c.users[0].name.clear(),                                             "snmp_agent users name \"\" must be 1 to 32 bytes, got 0")]
    #[case::name_of_33_bytes(   |c: &mut SnmpAgentConfig| c.users[0].name = "u".repeat(33),                                    "must be 1 to 32 bytes, got 33")]
    #[case::password_of_7_bytes(|c: &mut SnmpAgentConfig| c.users[0] = user("telegraf", AuthProtocol::Sha256, "1234567"),      "snmp_agent users \"telegraf\" auth.password must be at least 8 bytes")]
    #[case::engine_id_of_4(     |c: &mut SnmpAgentConfig| c.engine_id = Some("80001f88".to_string()),                          "snmp_agent engine_id must be 5 to 32 bytes, got 4")]
    #[case::engine_id_of_33(    |c: &mut SnmpAgentConfig| c.engine_id = Some("ab".repeat(33)),                                 "snmp_agent engine_id must be 5 to 32 bytes, got 33")]
    #[case::engine_id_not_hex(  |c: &mut SnmpAgentConfig| c.engine_id = Some("80001f88zz".to_string()),                        "snmp_agent engine_id \"80001f88zz\" is not hex")]
    #[case::engine_id_odd(      |c: &mut SnmpAgentConfig| c.engine_id = Some("80001f8804a".to_string()),                       "is not hex")]
    fn settings_refuse_each_bad_v3_setting_naming_it(
        #[case] change: fn(&mut SnmpAgentConfig),
        #[case] refused: &str,
    ) {
        let mut cfg = with_users(vec![user("telegraf", AuthProtocol::Sha256, "12345678")]);
        cfg.engine_id = Some("80001f8804736f6e6461".to_string());
        cfg.settings().expect("the baseline is valid: an 8-byte password, a 10-byte engine ID");
        change(&mut cfg);
        match cfg.settings() {
            Err(SondaError::Config(e)) => assert!(e.to_string().contains(refused), "{e}"),
            other => panic!("expected {refused:?}, got {other:?}"),
        }
    }

    /// Refused at parse time, naming the field: an unknown protocol, a
    /// `priv` that is not a map, a misspelt key in a user and in its auth, a
    /// numeric engine ID, and a numeric password or an auth that is not a
    /// map, both without repeating the value.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::unknown_protocol( "users: [{ name: t, auth: { protocol: sha999, password: lab-only-password } }]", "snmp_agent users auth.protocol: unknown protocol \"sha999\"")]
    #[case::priv_not_a_map(   "users: [{ name: t, auth: { protocol: sha256, password: lab-only-password }, priv: lab-only-secret }]", "snmp_agent users priv must be a map with protocol and password")]
    #[case::misspelt_user_key("users: [{ nmae: t }]",                                                         "unknown field `nmae`")]
    #[case::misspelt_auth_key("users: [{ name: t, auth: { protocol: md5, pasword: lab-only-password } }]",    "unknown field `pasword`")]
    #[case::numeric_engine_id("users: [{ name: t }]\nengine_id: 8000000002",                                  "snmp_agent engine_id: an engine ID is written as a hex string")]
    #[case::numeric_password( "users: [{ name: t, auth: { protocol: md5, password: 987654321 } }]",           "snmp_agent users auth.password must be a string")]
    #[case::auth_not_a_map(   "users: [{ name: t, auth: hunter2-secret }]",                                   "snmp_agent users auth must be a map with protocol and password")]
    fn a_bad_v3_block_is_refused_naming_the_field(#[case] rest: &str, #[case] refused: &str) {
        parse_block(&v3_block("users: [{ name: t, auth: { protocol: md5, password: lab-only-password } }]"))
            .expect("the baseline parses");
        let msg = parse_block(&v3_block(rest)).expect_err("refused");
        assert!(msg.contains(refused), "{msg}");
        for secret in ["987654321", "hunter2-secret", "lab-only-secret"] {
            assert!(!msg.contains(secret), "the error repeats a secret: {msg}");
        }
    }

    /// `v3` in `versions` parses; without users it is refused by
    /// `settings()`, with one it resolves.
    #[cfg(feature = "config")]
    #[test]
    fn v3_without_users_is_refused() {
        let cfg = parse_block(&v3_block("")).expect("the block parses");
        assert_eq!(cfg.versions, [SnmpVersion::V2c, SnmpVersion::V3]);
        let err = cfg.settings().expect_err("refused");
        assert!(
            err.to_string()
                .contains("versions names v3, which needs at least one user"),
            "{err}"
        );
        parse_block(&v3_block("users: [{ name: watcher }]"))
            .expect("parses")
            .settings()
            .expect("one user without auth is enough");
    }

    /// Debug of a config and of its settings prints no password.
    #[test]
    fn debug_prints_no_password() {
        let cfg = with_users(vec![user(
            "telegraf",
            AuthProtocol::Sha256,
            "s3cret-password",
        )]);
        let settings = cfg.settings().expect("valid settings");
        for debug in [format!("{cfg:?}"), format!("{settings:?}")] {
            assert!(
                debug.contains("telegraf") && debug.contains("Sha256"),
                "{debug}"
            );
            assert!(!debug.contains("s3cret-password"), "{debug}");
        }
    }

    /// Two configs on one address with different users do not serve each
    /// other: another user, another protocol, another password. The
    /// difference never names a password. The same users in another order
    /// do serve each other.
    #[test]
    fn different_users_do_not_serve_each_other() {
        let settings = |users: Vec<UsmUser>| with_users(users).settings().expect("valid settings");
        let telegraf = user("telegraf", AuthProtocol::Sha256, "first-password");
        let watcher = UsmUser {
            name: "watcher".to_string(),
            auth: None,
            privacy: None,
        };
        let both = settings(vec![telegraf.clone(), watcher.clone()]);
        assert!(
            both.serves(&settings(vec![watcher.clone(), telegraf.clone()])),
            "users compare as a set"
        );
        for (other, difference) in [
            (vec![telegraf.clone()], "users [telegraf sha256, watcher without auth] and [telegraf sha256]"),
            (vec![user("telegraf", AuthProtocol::Sha512, "first-password"), watcher.clone()], "users [telegraf sha256, watcher without auth] and [telegraf sha512, watcher without auth]"),
            (vec![user("telegraf", AuthProtocol::Sha256, "other-password"), watcher.clone()], "users with another password"),
        ] {
            let other = settings(other);
            assert!(!both.serves(&other));
            let beside = both.beside(&other);
            assert_eq!(beside, difference);
            assert!(!beside.contains("password") || beside == "users with another password");
        }
    }

    /// Two configs with different engine IDs, or one configured and one
    /// derived, do not serve each other.
    #[test]
    fn different_engine_ids_do_not_serve_each_other() {
        let with = |engine_id: Option<&str>| {
            SnmpAgentConfig {
                engine_id: engine_id.map(str::to_string),
                ..SnmpAgentConfig::new("127.0.0.1:1161")
            }
            .settings()
            .expect("valid settings")
        };
        let a = with(Some("80001f8804616161"));
        assert!(a.serves(&with(Some("0x80001F8804616161"))));
        assert_eq!(
            a.beside(&with(Some("80001f8804626262"))),
            "engine_id 80001f8804616161 and 80001f8804626262"
        );
        assert_eq!(
            a.beside(&with(None)),
            "engine_id 80001f8804616161 and derived"
        );
    }

    /// An agent configured with an engine ID reports it; one without makes
    /// its own from this host's name and the address it bound, the port it
    /// was given rather than 0.
    #[tokio::test]
    async fn an_agent_reports_its_engine_id() {
        let configured = SnmpAgentConfig {
            engine_id: Some("80001f8804616161".to_string()),
            ..SnmpAgentConfig::new("127.0.0.1:0")
        };
        let agent = SnmpAgent::bind(configured.settings().expect("valid"))
            .await
            .expect("binds");
        assert_eq!(agent.engine_id(), hex("80001f8804616161"));
        let derived = SnmpAgent::bind(
            SnmpAgentConfig::new("127.0.0.1:0")
                .settings()
                .expect("valid"),
        )
        .await
        .expect("binds");
        let expected = usm::default_engine_id(&usm::hostname(), derived.local_addr());
        assert_eq!(derived.engine_id(), expected.as_slice());
        let unbound =
            usm::default_engine_id(&usm::hostname(), "127.0.0.1:0".parse().expect("an address"));
        assert_ne!(
            derived.engine_id(),
            unbound.as_slice(),
            "the bound port, not 0"
        );
    }

    // ---- SNMPv3 privacy config ----------------------------------------------

    fn private_user(name: &str, protocol: PrivProtocol, password: &str) -> UsmUser {
        UsmUser {
            privacy: Some(UsmPriv {
                protocol,
                password: password.to_string(),
            }),
            ..user(name, AuthProtocol::Sha256, "lab-auth-password")
        }
    }

    /// A `priv` block parses with each protocol by its name, `aes` as
    /// `aes128`, and resolves.
    #[cfg(feature = "config")]
    #[test]
    fn a_priv_block_parses_every_protocol_and_aes_as_aes128() {
        let yaml = v3_block(
            "users:\n\
             \x20 - { name: a, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: des, password: lab-priv-pass } }\n\
             \x20 - { name: b, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes, password: lab-priv-pass } }\n\
             \x20 - { name: c, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes128, password: lab-priv-pass } }\n\
             \x20 - { name: d, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes192, password: lab-priv-pass } }\n\
             \x20 - { name: e, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes192c, password: lab-priv-pass } }\n\
             \x20 - { name: f, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes256, password: lab-priv-pass } }\n\
             \x20 - { name: g, auth: { protocol: sha1, password: lab-auth-pass }, priv: { protocol: aes256c, password: lab-priv-pass } }\n\
             \x20 - { name: h, auth: { protocol: sha1, password: lab-auth-pass } }\n",
        );
        let cfg = parse_block(&yaml).expect("the block parses");
        let protocols: Vec<Option<PrivProtocol>> = cfg
            .users
            .iter()
            .map(|user| user.privacy.as_ref().map(|privacy| privacy.protocol))
            .collect();
        use PrivProtocol::*;
        assert_eq!(
            protocols,
            [
                Some(Des),
                Some(Aes128),
                Some(Aes128),
                Some(Aes192),
                Some(Aes192c),
                Some(Aes256),
                Some(Aes256c),
                None
            ]
        );
        cfg.settings().expect("valid settings");
    }

    /// Each refused privacy setting names its field: `priv` without `auth`
    /// names both, a 7-byte privacy password names `priv.password`. The
    /// baseline, an 8-byte privacy password, is valid.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::priv_without_auth(  |c: &mut SnmpAgentConfig| c.users[0].auth = None,                                            "snmp_agent users \"telegraf\" has priv without auth; privacy (authPriv) needs auth as well")]
    #[case::password_of_7_bytes(|c: &mut SnmpAgentConfig| c.users[0] = private_user("telegraf", PrivProtocol::Aes128, "1234567"), "snmp_agent users \"telegraf\" priv.password must be at least 8 bytes")]
    fn settings_refuse_each_bad_priv_setting_naming_it(
        #[case] change: fn(&mut SnmpAgentConfig),
        #[case] refused: &str,
    ) {
        let mut cfg = with_users(vec![private_user("telegraf", PrivProtocol::Aes128, "12345678")]);
        cfg.settings().expect("the baseline is valid: an 8-byte privacy password");
        change(&mut cfg);
        match cfg.settings() {
            Err(SondaError::Config(e)) => assert!(e.to_string().contains(refused), "{e}"),
            other => panic!("expected {refused:?}, got {other:?}"),
        }
    }

    /// Refused at parse time, naming the field: an unknown privacy protocol,
    /// a misspelt key in `priv`, and a numeric privacy password, without
    /// repeating the value.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::unknown_protocol( "priv: { protocol: aes512, password: lab-only-secret }", "snmp_agent users priv.protocol: unknown protocol \"aes512\"; expected des, aes128 (or aes), aes192, aes192c, aes256 or aes256c")]
    #[case::misspelt_key(     "priv: { protocol: aes, pasword: lab-only-secret }",    "unknown field `pasword`")]
    #[case::numeric_password( "priv: { protocol: aes, password: 987654321 }",          "snmp_agent users priv.password must be a string")]
    fn a_bad_priv_block_is_refused_naming_the_field(#[case] block: &str, #[case] refused: &str) {
        let with = |block: &str| {
            v3_block(&format!(
                "users: [{{ name: t, auth: {{ protocol: sha256, password: lab-only-password }}, {block} }}]"
            ))
        };
        parse_block(&with("priv: { protocol: aes, password: lab-only-secret }")).expect("the baseline parses");
        let msg = parse_block(&with(block)).expect_err("refused");
        assert!(msg.contains(refused), "{msg}");
        for secret in ["987654321", "lab-only-secret"] {
            assert!(!msg.contains(secret), "the error repeats a secret: {msg}");
        }
    }

    /// Debug of a config and of its settings prints neither password.
    #[test]
    fn debug_prints_neither_password() {
        let cfg = with_users(vec![UsmUser {
            privacy: Some(UsmPriv {
                protocol: PrivProtocol::Aes256c,
                password: "s3cret-privacy".to_string(),
            }),
            ..user("telegraf", AuthProtocol::Sha256, "s3cret-password")
        }]);
        let settings = cfg.settings().expect("valid settings");
        for debug in [format!("{cfg:?}"), format!("{settings:?}")] {
            assert!(debug.contains("Aes256c"), "{debug}");
            assert!(
                !debug.contains("s3cret-password") && !debug.contains("s3cret-privacy"),
                "{debug}"
            );
        }
    }

    /// Two configs on one address whose users differ only in privacy do
    /// not serve each other: another protocol, another password, none. The
    /// difference never names a password.
    #[test]
    fn different_privacy_does_not_serve() {
        let settings = |users: Vec<UsmUser>| with_users(users).settings().expect("valid settings");
        let aes = settings(vec![private_user(
            "telegraf",
            PrivProtocol::Aes128,
            "first-privacy",
        )]);
        assert!(aes.serves(&settings(vec![private_user(
            "telegraf",
            PrivProtocol::Aes128,
            "first-privacy"
        )])));
        for (other, difference) in [
            (
                private_user("telegraf", PrivProtocol::Des, "first-privacy"),
                "users [telegraf sha256 aes128] and [telegraf sha256 des]",
            ),
            (
                private_user("telegraf", PrivProtocol::Aes128, "other-privacy"),
                "users with another password",
            ),
            (
                user("telegraf", AuthProtocol::Sha256, "lab-auth-password"),
                "users [telegraf sha256 aes128] and [telegraf sha256]",
            ),
        ] {
            let other = settings(vec![other]);
            assert!(!aes.serves(&other));
            assert_eq!(aes.beside(&other), difference);
        }
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

    /// A second agent config on a live agent's address with other
    /// `versions` is refused by `acquire` and `check`, naming both lists;
    /// the same versions in another order share the agent, and a join takes
    /// the agent's versions.
    #[tokio::test]
    async fn differing_versions_on_one_address_are_a_config_error() {
        let registry = SnmpAgentRegistry::new();
        let with = |listen: &str, versions: &[SnmpVersion]| SnmpAgentConfig {
            versions: versions.to_vec(),
            ..SnmpAgentConfig::new(listen)
        };
        let first = registry
            .acquire(&with("127.0.0.1:0", &[SnmpVersion::V1, SnmpVersion::V2c]))
            .await
            .expect("bind an ephemeral port");
        assert_eq!(first.versions(), [SnmpVersion::V1, SnmpVersion::V2c]);
        let addr = first.local_addr().to_string();
        let shared = registry
            .acquire(&with(&addr, &[SnmpVersion::V2c, SnmpVersion::V1]))
            .await
            .expect("the same versions share the agent");
        assert!(Arc::ptr_eq(&first, &shared));

        let other = with(&addr, &[SnmpVersion::V2c]);
        for result in [
            registry.acquire(&other).await.map(|_| ()),
            registry.check(&other),
        ] {
            let msg = result.expect_err("other versions are refused").to_string();
            assert!(msg.contains("versions [v1, v2c] and [v2c]"), "{msg}");
        }
        let joined = registry
            .join(&other)
            .expect("a join compares the community only");
        assert_eq!(joined.versions(), [SnmpVersion::V1, SnmpVersion::V2c]);
        assert_eq!(registry.len(), 1, "nothing else was bound");
    }

    /// A second agent on one address with other users is refused by the
    /// registry, naming the users and never a password.
    #[tokio::test]
    async fn differing_users_on_one_address_are_a_config_error() {
        let registry = SnmpAgentRegistry::new();
        let with = |listen: &str, password: &str| SnmpAgentConfig {
            listen: listen.to_string(),
            ..with_users(vec![user("telegraf", AuthProtocol::Sha256, password)])
        };
        let first = registry
            .acquire(&with("127.0.0.1:0", "first-password"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr().to_string();
        let shared = registry
            .acquire(&with(&addr, "first-password"))
            .await
            .expect("the same users share the agent");
        assert!(Arc::ptr_eq(&first, &shared));
        let other = with(&addr, "other-password");
        for result in [
            registry.acquire(&other).await.map(|_| ()),
            registry.check(&other),
        ] {
            let msg = result.expect_err("other users are refused").to_string();
            assert!(msg.contains("users with another password"), "{msg}");
            assert!(
                !msg.contains("first-password") && !msg.contains("other-password"),
                "{msg}"
            );
        }
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
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::empty(&[])]
    #[case::no_companion_count(&[1])]
    #[case::no_varbind(&[1, 0])]
    #[case::not_a_sequence(&[1, 0, 0x04, 0x00])]
    #[case::bytes_after_the_varbind(&[1, 0, 0x30, 0x00, 0x00])]
    #[case::fewer_companions_than_announced(&[1, 1, 0x30, 0x00])]
    #[case::companion_without_varbind(&[1, 1, 0x30, 0x00, 1])]
    #[case::companion_not_a_sequence(&[1, 1, 0x30, 0x00, 1, 0x04, 0x00])]
    #[case::bytes_after_the_last_companion(&[1, 1, 0x30, 0x00, 1, 0x30, 0x00, 0x00])]
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

    // ---- label columns --------------------------------------------------------

    const IF_NAME: &str = "1.3.6.1.2.1.31.1.1.1.1";
    const IF_ALIAS: &str = "1.3.6.1.2.1.31.1.1.1.18";

    /// [`encoder_config`] with `ifName` and `ifAlias` served as columns.
    fn column_encoder() -> SnmpEncoder {
        let mut cfg = encoder_config();
        cfg.label_columns = HashMap::from([
            ("ifName".to_string(), format!("{IF_NAME}.{{ifIndex}}")),
            ("ifAlias".to_string(), format!("{IF_ALIAS}.{{ifIndex}}")),
        ]);
        SnmpEncoder::new(&cfg).expect("a valid encoder")
    }

    /// `event` as `encoder` writes it.
    fn encoded(encoder: &SnmpEncoder, event: &MetricEvent) -> Vec<u8> {
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    async fn write(sink: &mut SnmpAgentSink, encoder: &SnmpEncoder, event: &MetricEvent) {
        sink.write_metric_event(event, &encoded(encoder, event))
            .await
            .expect("the write succeeds");
    }

    /// An event of `metric` on interface `if_index`, named `if_name`.
    fn named(metric: &str, if_index: &str, if_name: &str) -> MetricEvent {
        event(
            metric,
            1.0,
            &[("ifIndex", if_index), ("ifName", if_name), ("job", "snmp")],
        )
    }

    /// The `VarBind` serving `text` at `instance`, built by hand.
    fn string_varbind(instance: &str, text: &str) -> Bytes {
        let mut value = Vec::new();
        ber::write_string(&mut value, text.as_bytes());
        let mut varbind = Vec::new();
        ber::write_varbind(&mut varbind, &oid(instance), &value);
        varbind.into()
    }

    /// Every instance under `column`, in walk order.
    fn walk(agent: &SnmpAgent, column: &str) -> Vec<(Oid, Bytes)> {
        let column = oid(column);
        let mut walked = Vec::new();
        let mut at = column.clone();
        while let Some((next, varbind)) = agent.next_after(&at) {
            if !column.is_prefix_of(&next) {
                break;
            }
            at = next.clone();
            walked.push((next, varbind));
        }
        walked
    }

    /// Two series of index 1 carry `ifName.1`, one per sink of a launch, and
    /// one series of index 2 carries `ifName.2`. The column walks one string
    /// per live index; `ifName.1` outlives either series of index 1 alone
    /// and leaves with the second, its object still registered.
    #[tokio::test]
    async fn a_label_column_stays_while_any_series_of_its_index_lives() {
        let launch = LaunchId::next();
        let mut octets = SnmpAgentSink::new_in(&SnmpAgentConfig::new("127.0.0.1:0"), launch)
            .await
            .expect("bind an ephemeral port");
        let addr = octets.agent().local_addr().to_string();
        let mut errors = SnmpAgentSink::new_in(&SnmpAgentConfig::new(addr), launch)
            .await
            .expect("join the live agent");
        let agent = Arc::clone(octets.agent());
        let (octets_encoder, errors_encoder) = (column_encoder(), column_encoder());
        for _ in 0..3 {
            write(
                &mut octets,
                &octets_encoder,
                &named("ifHCInOctets", "1", "Gi0/0/0"),
            )
            .await;
            write(
                &mut errors,
                &errors_encoder,
                &named("ifInErrors", "1", "Gi0/0/0"),
            )
            .await;
            write(
                &mut octets,
                &octets_encoder,
                &named("ifHCInOctets", "2", "Gi0/0/1"),
            )
            .await;
        }
        let name_1 = string_varbind(&format!("{IF_NAME}.1"), "Gi0/0/0");
        let name_2 = string_varbind(&format!("{IF_NAME}.2"), "Gi0/0/1");
        assert_eq!(
            walk(&agent, IF_NAME),
            [
                (oid(&format!("{IF_NAME}.1")), name_1.clone()),
                (oid(&format!("{IF_NAME}.2")), name_2.clone()),
            ],
            "one string per live index, in index order"
        );
        assert!(
            walk(&agent, IF_ALIAS).is_empty(),
            "no series carries ifAlias"
        );
        assert_eq!(agent.instance_count(), 5, "three series and two names");

        drop(octets);
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.1"))),
            Lookup::Value(name_1),
            "the errors series of index 1 still carries it"
        );
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.2"))),
            Lookup::NoSuchInstance,
            "index 2's only series left"
        );
        drop(errors);
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.1"))),
            Lookup::NoSuchInstance,
            "gone with the last series of index 1, its object still registered"
        );
        assert_eq!(agent.instance_count(), 0);
    }

    /// A second series key on an instance's OID publishes its own label
    /// value over the first's, holds the column once, and releases it once.
    #[tokio::test]
    async fn a_series_sharing_an_instance_replaces_its_label_column() {
        let mut sink = sink().await;
        let agent = Arc::clone(sink.agent());
        let encoder = column_encoder();
        write(&mut sink, &encoder, &named("ifHCInOctets", "1", "Gi0/0/0")).await;
        write(&mut sink, &encoder, &named("ifHCInOctets", "1", "Gi0/0/9")).await;
        assert_eq!(sink.owned.len(), 2, "two series keys");
        assert_eq!(sink.published.len(), 1, "sharing one entry");
        assert_eq!(sink.published[0].companions, [oid(&format!("{IF_NAME}.1"))]);
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.1"))),
            Lookup::Value(string_varbind(&format!("{IF_NAME}.1"), "Gi0/0/9")),
            "the last writer wins"
        );
        drop(sink);
        assert_eq!(agent.instance_count(), 0, "each released once");
    }

    /// A label column's instance belongs to its launch like any other: a
    /// second launch's series carrying it is refused, the owner keeps it,
    /// and the refused sink still releases the instance it did publish.
    #[tokio::test]
    async fn another_launchs_label_column_is_refused() {
        let mut owner = sink().await;
        let agent = Arc::clone(owner.agent());
        let mut intruder =
            SnmpAgentSink::new(&SnmpAgentConfig::new(agent.local_addr().to_string()))
                .await
                .expect("join the live agent");
        write(
            &mut owner,
            &column_encoder(),
            &named("ifHCInOctets", "1", "Gi0/0/0"),
        )
        .await;
        let event = named("ifInErrors", "1", "Gi0/0/7");
        let err = intruder
            .write_metric_event(&event, &encoded(&column_encoder(), &event))
            .await
            .expect_err("ifName.1 belongs to the first launch");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(
            err.to_string().contains(&format!("{IF_NAME}.1")),
            "names the column's instance: {err}"
        );
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.1"))),
            Lookup::Value(string_varbind(&format!("{IF_NAME}.1"), "Gi0/0/0"))
        );
        assert!(matches!(
            agent.get(&oid("1.3.6.1.2.1.2.2.1.14.1")),
            Lookup::Value(_)
        ));
        drop(intruder);
        assert_eq!(
            agent.get(&oid("1.3.6.1.2.1.2.2.1.14.1")),
            Lookup::NoSuchInstance,
            "the intruder released what it published"
        );
        assert_eq!(agent.instance_count(), 2, "the owner's instance and name");
    }

    /// A joined sink replaces a label column the agent serves and creates
    /// none it does not.
    #[tokio::test]
    async fn a_joined_sink_replaces_only_a_served_label_column() {
        let mut owner = sink().await;
        let agent = Arc::clone(owner.agent());
        write(
            &mut owner,
            &column_encoder(),
            &named("ifHCInOctets", "1", "Gi0/0/0"),
        )
        .await;
        let mut joined = SnmpAgentSink::join(&SnmpAgentConfig::new(agent.local_addr().to_string()))
            .expect("join the live agent");
        let event = event(
            "ifHCInOctets",
            2.0,
            &[
                ("ifIndex", "1"),
                ("ifName", "uplink"),
                ("ifAlias", "to core"),
            ],
        );
        joined
            .write_metric_event(&event, &encoded(&column_encoder(), &event))
            .await
            .expect("the instance is served");
        assert_eq!(
            agent.get(&oid(&format!("{IF_NAME}.1"))),
            Lookup::Value(string_varbind(&format!("{IF_NAME}.1"), "uplink"))
        );
        assert_eq!(
            agent.get(&oid(&format!("{IF_ALIAS}.1"))),
            Lookup::NoSuchObject,
            "no ifAlias was ever published"
        );
        assert_eq!(agent.instance_count(), 2);
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
