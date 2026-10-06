//! gNMI encoder: one `MetricEvent` in, one prost-encoded `gnmi.Notification` out.
//!
//! The update path is rendered from a [`PathTemplate`] the first time a series
//! is seen and cached as encoded `Path` bytes; later events for that series
//! copy those bytes and encode only the timestamp, prefix and typed value.
//!
//! Requires the `gnmi` feature flag.

pub mod path;
pub mod proto;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::UNIX_EPOCH;

use prost::encoding::{self, encode_key, encode_varint, encoded_len_varint, key_len, WireType};
use prost::Message;

use crate::model::metric::MetricEvent;
use crate::schedule::stats::MetricKey;
use crate::{ConfigError, EncoderError, SondaError};

use super::Encoder;
use path::PathTemplate;

/// Default `Path.origin` on the notification prefix.
pub const DEFAULT_ORIGIN: &str = "openconfig";

/// Default label whose value becomes `Path.target` on the notification prefix.
pub const DEFAULT_TARGET_LABEL: &str = "device";

/// Upper bound on the `Display` length of an `f64`, sign included.
const F64_TEXT_CAPACITY: usize = 400;

/// The `TypedValue` variant a metric's value is encoded as.
///
/// YAML forms: `uint`, `int`, `double`, `bool`, `string`, and
/// `{ enum: { 1: UP, 2: DOWN } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "config",
    serde(from = "GnmiValueTypeWire", into = "GnmiValueTypeWire")
)]
pub enum GnmiValueType {
    /// `uint_val`: the value truncated toward zero, negatives clamped to 0.
    /// A non-finite value rejects the event.
    Uint,
    /// `int_val`: the value truncated toward zero. A non-finite value
    /// rejects the event.
    Int,
    /// `double_val` (field 14).
    Double,
    /// `bool_val`: `true` for any value other than `0.0`. A non-finite value
    /// rejects the event.
    Bool,
    /// `string_val`: the shortest decimal text that round-trips the `f64`.
    String,
    /// `string_val` looked up by the value rounded to the nearest `i64`, so
    /// float noise such as `0.9999999999999999` still selects code `1`. A
    /// value with no entry, or a non-finite value, rejects the event.
    Enum(BTreeMap<i64, String>),
}

/// YAML shape of [`GnmiValueType`]: a bare name, or a one-key `enum:` map.
///
/// `Deserialize` is written by hand so that enum keys may be integers or
/// integer strings (`1` or `"1"`, as JSON requires), and so that a mistake
/// names what is accepted instead of failing as an unmatched untagged enum.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
enum GnmiValueTypeWire {
    Name(GnmiScalarName),
    Enum(GnmiEnumWire),
}

/// The scalar value type names.
#[cfg(feature = "config")]
#[derive(Clone, Copy, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
enum GnmiScalarName {
    Uint,
    Int,
    Double,
    Bool,
    String,
}

/// `{ enum: { <i64>: <string>, ... } }`.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct GnmiEnumWire {
    #[serde(rename = "enum")]
    map: BTreeMap<i64, String>,
}

/// What a value type may be, for error messages.
#[cfg(feature = "config")]
const VALUE_TYPE_EXPECTED: &str =
    "one of uint, int, double, bool, string, or { enum: { <integer>: <label>, ... } }";

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for GnmiValueTypeWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueTypeVisitor)
    }
}

#[cfg(feature = "config")]
struct ValueTypeVisitor;

#[cfg(feature = "config")]
impl<'de> serde::de::Visitor<'de> for ValueTypeVisitor {
    type Value = GnmiValueTypeWire;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a gnmi value type: {VALUE_TYPE_EXPECTED}")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let name = match v {
            "uint" => GnmiScalarName::Uint,
            "int" => GnmiScalarName::Int,
            "double" => GnmiScalarName::Double,
            "bool" => GnmiScalarName::Bool,
            "string" => GnmiScalarName::String,
            other => {
                return Err(E::custom(format!(
                    "unknown gnmi value type {other:?}; expected {VALUE_TYPE_EXPECTED}"
                )))
            }
        };
        Ok(GnmiValueTypeWire::Name(name))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        let mut entries: Option<BTreeMap<i64, String>> = None;
        while let Some(key) = map.next_key::<String>()? {
            if key != "enum" {
                return Err(A::Error::custom(format!(
                    "unknown key {key:?} in a gnmi value type; expected {VALUE_TYPE_EXPECTED}"
                )));
            }
            if entries.is_some() {
                return Err(A::Error::custom(
                    "duplicate `enum` key in a gnmi value type",
                ));
            }
            entries = Some(map.next_value::<EnumEntries>()?.0);
        }
        entries
            .map(|map| GnmiValueTypeWire::Enum(GnmiEnumWire { map }))
            .ok_or_else(|| A::Error::custom(format!("empty map; expected {VALUE_TYPE_EXPECTED}")))
    }
}

/// The body of `enum:`: keys are integers, written bare or as strings.
#[cfg(feature = "config")]
struct EnumEntries(BTreeMap<i64, String>);

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for EnumEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
            type Value = EnumEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map from integer codes to labels, e.g. { 1: UP, 2: DOWN }")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(EnumCode(code)) = map.next_key()? {
                    out.insert(code, map.next_value::<String>()?);
                }
                Ok(EnumEntries(out))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

/// One `enum:` key: an integer, or a string holding one.
#[cfg(feature = "config")]
struct EnumCode(i64);

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for EnumCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CodeVisitor;
        impl serde::de::Visitor<'_> for CodeVisitor {
            type Value = EnumCode;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an integer enum code")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(EnumCode(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                i64::try_from(v)
                    .map(EnumCode)
                    .map_err(|_| E::custom(format!("enum code {v} does not fit in i64")))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                v.trim()
                    .parse::<i64>()
                    .map(EnumCode)
                    .map_err(|_| E::custom(format!("enum key {v:?} is not an integer")))
            }
        }
        deserializer.deserialize_any(CodeVisitor)
    }
}

#[cfg(feature = "config")]
impl From<GnmiValueTypeWire> for GnmiValueType {
    fn from(wire: GnmiValueTypeWire) -> Self {
        match wire {
            GnmiValueTypeWire::Name(GnmiScalarName::Uint) => Self::Uint,
            GnmiValueTypeWire::Name(GnmiScalarName::Int) => Self::Int,
            GnmiValueTypeWire::Name(GnmiScalarName::Double) => Self::Double,
            GnmiValueTypeWire::Name(GnmiScalarName::Bool) => Self::Bool,
            GnmiValueTypeWire::Name(GnmiScalarName::String) => Self::String,
            GnmiValueTypeWire::Enum(e) => Self::Enum(e.map),
        }
    }
}

#[cfg(feature = "config")]
impl From<GnmiValueType> for GnmiValueTypeWire {
    fn from(value: GnmiValueType) -> Self {
        match value {
            GnmiValueType::Uint => Self::Name(GnmiScalarName::Uint),
            GnmiValueType::Int => Self::Name(GnmiScalarName::Int),
            GnmiValueType::Double => Self::Name(GnmiScalarName::Double),
            GnmiValueType::Bool => Self::Name(GnmiScalarName::Bool),
            GnmiValueType::String => Self::Name(GnmiScalarName::String),
            GnmiValueType::Enum(map) => Self::Enum(GnmiEnumWire { map }),
        }
    }
}

/// Configuration for [`GnmiEncoder`]; the body of `encoder: { type: gnmi, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `vaules:` is an
/// error rather than a silent fallback to the defaults.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GnmiEncoderConfig {
    /// `Path.origin` on every notification prefix. Default `"openconfig"`.
    #[cfg_attr(feature = "config", serde(default = "default_origin"))]
    pub origin: String,
    /// Label whose value becomes `Path.target` on the notification prefix. Default `"device"`.
    #[cfg_attr(feature = "config", serde(default = "default_target_label"))]
    pub target_label: String,
    /// Path template; `{name}` and `{<label>}` placeholders. Required unless every metric has a `paths:` entry.
    #[cfg_attr(feature = "config", serde(default))]
    pub path: Option<String>,
    /// Exact per-metric path overrides, keyed by metric name.
    #[cfg_attr(feature = "config", serde(default))]
    pub paths: HashMap<String, String>,
    /// Per-metric typed value; default `double`.
    #[cfg_attr(feature = "config", serde(default))]
    pub values: HashMap<String, GnmiValueType>,
    /// Labels deliberately left out of the path.
    ///
    /// Validation requires every key in an entry's `labels` and
    /// `dynamic_labels` to be referenced by its template, be the
    /// `target_label`, or be listed here; series that differ only in an
    /// unlisted, unreferenced label would otherwise share one path.
    #[cfg_attr(feature = "config", serde(default))]
    pub drop_labels: Vec<String>,
}

fn default_origin() -> String {
    DEFAULT_ORIGIN.to_string()
}

fn default_target_label() -> String {
    DEFAULT_TARGET_LABEL.to_string()
}

impl Default for GnmiEncoderConfig {
    /// Default origin and target label, no templates, no value overrides.
    fn default() -> Self {
        Self {
            origin: default_origin(),
            target_label: default_target_label(),
            path: None,
            paths: HashMap::new(),
            values: HashMap::new(),
            drop_labels: Vec::new(),
        }
    }
}

/// Encodes metric events as gNMI `Notification` messages.
///
/// Each call to [`Encoder::encode_metric`] appends one `Notification` with a
/// prefix carrying `origin` and `target`, the event timestamp in nanoseconds,
/// and exactly one `Update`. Log events are not supported.
pub struct GnmiEncoder {
    origin: String,
    target_label: String,
    template: Option<PathTemplate>,
    overrides: HashMap<String, PathTemplate>,
    values: HashMap<String, GnmiValueType>,
    drop_labels: Vec<String>,
    /// Encoded `Path` bytes per series, filled on first sight.
    paths: RwLock<HashMap<MetricKey, Arc<[u8]>>>,
}

impl GnmiEncoder {
    /// Build an encoder, parsing every template in `cfg`.
    ///
    /// Returns [`SondaError::Config`] when `origin` is empty, an `enum` value
    /// map is empty, or `path` or a `paths:` entry is not a valid template.
    /// `values` and `paths` are checked in metric-name order, so the same
    /// config always reports the same first error.
    pub fn new(cfg: &GnmiEncoderConfig) -> Result<Self, SondaError> {
        if cfg.origin.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "gnmi encoder origin must not be empty",
            )));
        }
        for (metric, value) in cfg.values.iter().collect::<BTreeMap<_, _>>() {
            if matches!(value, GnmiValueType::Enum(map) if map.is_empty()) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "gnmi encoder values.{metric}: enum map must not be empty"
                ))));
            }
        }
        let template = cfg.path.as_deref().map(PathTemplate::parse).transpose()?;
        let overrides = cfg
            .paths
            .iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(metric, t)| Ok((metric.clone(), PathTemplate::parse(t)?)))
            .collect::<Result<HashMap<_, _>, SondaError>>()?;

        Ok(Self {
            origin: cfg.origin.clone(),
            target_label: cfg.target_label.clone(),
            template,
            overrides,
            values: cfg.values.clone(),
            drop_labels: cfg.drop_labels.clone(),
            paths: RwLock::new(HashMap::new()),
        })
    }

    /// The template used for `metric`: its `paths:` entry if present, else
    /// `path`. Returns [`SondaError::Config`] when neither covers the metric.
    pub(crate) fn template_for(&self, metric: &str) -> Result<&PathTemplate, SondaError> {
        self.overrides
            .get(metric)
            .or(self.template.as_ref())
            .ok_or_else(|| {
                SondaError::Config(ConfigError::invalid(format!(
                    "gnmi encoder has no path template for metric {metric:?}: set \
                     encoder.path or encoder.paths.{metric}"
                )))
            })
    }

    /// The first of `keys` that is not covered by `template`: not referenced by
    /// one of its placeholders (`{name}` is the metric name, never a label),
    /// not the `target_label`, and not listed in `drop_labels`. `None` when
    /// every key is covered.
    ///
    /// Validation and [`Encoder::encode_metric`] both apply this one rule, so
    /// a configuration that validates never has an event rejected by it.
    pub(crate) fn uncovered_label<'a>(
        &self,
        template: &PathTemplate,
        mut keys: impl Iterator<Item = &'a str>,
    ) -> Option<&'a str> {
        keys.find(|key| {
            *key != self.target_label
                && !self.drop_labels.iter().any(|dropped| dropped == key)
                && !template
                    .placeholders()
                    .any(|p| p != path::NAME_PLACEHOLDER && p == *key)
        })
    }

    /// Encoded `Path` bytes for the event's series, rendering and caching them on first sight.
    fn path_bytes(&self, event: &MetricEvent) -> Result<Arc<[u8]>, SondaError> {
        let key: MetricKey = (event.name.clone(), Arc::clone(&event.labels));
        if let Some(bytes) = self
            .paths
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Ok(Arc::clone(bytes));
        }

        let name: &str = &event.name;
        let template = self.template_for(name)?;
        if let Some(label) = self.uncovered_label(template, event.labels.iter().map(|(k, _)| k)) {
            return Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                "metric {name:?} carries label `{label}`, which the gnmi path template does \
                 not reference; reference it, make it the target_label, or list it in \
                 drop_labels"
            ))));
        }
        let bytes: Arc<[u8]> = template.render(name, &event.labels)?.encode_to_vec().into();
        self.paths
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::clone(&bytes));
        Ok(bytes)
    }
}

/// A `TypedValue` oneof payload borrowed for the duration of one encode.
enum Scalar<'a> {
    Str(&'a str),
    Int(i64),
    Uint(u64),
    Bool(bool),
    Double(f64),
}

impl Scalar<'_> {
    /// Encoded length of the `TypedValue` message holding this payload.
    ///
    /// `string_val` is a oneof member, so it is written even when empty;
    /// omitting it would decode as a `TypedValue` with no value at all.
    fn encoded_len(&self) -> usize {
        match self {
            Scalar::Str(s) => delimited_field_len(1, s.len()),
            Scalar::Int(v) => encoding::int64::encoded_len(2, v),
            Scalar::Uint(v) => encoding::uint64::encoded_len(3, v),
            Scalar::Bool(v) => encoding::bool::encoded_len(4, v),
            Scalar::Double(v) => encoding::double::encoded_len(14, v),
        }
    }

    /// Append the `TypedValue` message body.
    fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            Scalar::Str(s) => {
                encode_delimited_header(1, s.len(), buf);
                buf.extend_from_slice(s.as_bytes());
            }
            Scalar::Int(v) => encoding::int64::encode(2, v, buf),
            Scalar::Uint(v) => encoding::uint64::encode(3, v, buf),
            Scalar::Bool(v) => encoding::bool::encode(4, v, buf),
            Scalar::Double(v) => encoding::double::encode(14, v, buf),
        }
    }
}

/// Length of a length-delimited field holding `len` bytes.
fn delimited_field_len(tag: u32, len: usize) -> usize {
    key_len(tag) + encoded_len_varint(len as u64) + len
}

/// Length of a string field, or 0 when `s` is empty (proto3 omits it).
fn str_field_len(tag: u32, s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        delimited_field_len(tag, s.len())
    }
}

/// Append a length-delimited field header.
fn encode_delimited_header(tag: u32, len: usize, buf: &mut Vec<u8>) {
    encode_key(tag, WireType::LengthDelimited, buf);
    encode_varint(len as u64, buf);
}

/// Append a string field, or nothing when `s` is empty (proto3 omits it).
fn encode_str_field(tag: u32, s: &str, buf: &mut Vec<u8>) {
    if !s.is_empty() {
        encode_delimited_header(tag, s.len(), buf);
        buf.extend_from_slice(s.as_bytes());
    }
}

/// Format `value` with `Display` (shortest round-trip decimal) into `out` without allocating.
fn format_f64(value: f64, out: &mut [u8; F64_TEXT_CAPACITY]) -> Result<&str, SondaError> {
    use std::io::Write as _;
    let mut cursor = &mut out[..];
    write!(cursor, "{value}").map_err(|e| {
        SondaError::Encoder(EncoderError::Other(format!(
            "gnmi encoder could not format {value} as a string: {e}"
        )))
    })?;
    let written = F64_TEXT_CAPACITY - cursor.len();
    std::str::from_utf8(&out[..written])
        .map_err(|e| SondaError::Encoder(EncoderError::Other(e.to_string())))
}

impl Encoder for GnmiEncoder {
    /// Writes exactly one prost-encoded `Notification` — NOT length-prefixed — into `buf`.
    /// One call = one notification with one `Update`. `buf` is cleared by the caller, not here.
    ///
    /// The prefix carries `origin` and, when the event has the target label,
    /// `target`. Returns [`SondaError::Config`] when the metric has no
    /// template, which is a defect of the configuration rather than of this
    /// event. Returns [`EncoderError::EventRejected`] when a template
    /// placeholder names a label this event lacks, or the event carries a
    /// label the template does not reference that is neither the
    /// `target_label` nor listed in `drop_labels`. Returns another
    /// [`SondaError::Encoder`] variant when the timestamp predates the epoch or
    /// exceeds `i64` nanoseconds, or an `enum` value has no mapping.
    ///
    /// The label checks run only when a series is first seen and its path is
    /// rendered and cached; later events for the same series skip them, so
    /// they cost nothing in steady state.
    ///
    /// The output is not length-prefixed, so it is only usable by a sink that
    /// takes one notification per write. Pairing with any other sink is not
    /// rejected by validation.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let since_epoch = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SondaError::Encoder(EncoderError::TimestampBeforeEpoch(e)))?;
        let timestamp = i64::try_from(since_epoch.as_nanos()).map_err(|_| {
            SondaError::Encoder(EncoderError::Other(
                "gnmi encoder: timestamp does not fit in i64 nanoseconds".to_string(),
            ))
        })?;

        let path = self.path_bytes(event)?;
        let name: &str = &event.name;
        let target = event
            .labels
            .iter()
            .find(|(k, _)| *k == self.target_label)
            .map_or("", |(_, v)| v);

        // Initialised only on the `String` path; every other type skips it.
        let mut text: [u8; F64_TEXT_CAPACITY];
        let value = event.value;
        let finite = |kind: &str| {
            if value.is_finite() {
                Ok(value)
            } else {
                Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                    "value {value} of metric {name:?} cannot be encoded as {kind}"
                ))))
            }
        };
        let scalar = match self.values.get(name) {
            None | Some(GnmiValueType::Double) => Scalar::Double(value),
            Some(GnmiValueType::Uint) => Scalar::Uint(finite("uint")?.max(0.0) as u64),
            Some(GnmiValueType::Int) => Scalar::Int(finite("int")? as i64),
            Some(GnmiValueType::Bool) => Scalar::Bool(finite("bool")? != 0.0),
            Some(GnmiValueType::String) => {
                text = [0u8; F64_TEXT_CAPACITY];
                Scalar::Str(format_f64(value, &mut text)?)
            }
            Some(GnmiValueType::Enum(map)) => Scalar::Str(
                map.get(&(finite("enum")?.round() as i64))
                    .map(String::as_str)
                    .ok_or_else(|| {
                        SondaError::Encoder(EncoderError::EventRejected(format!(
                            "value {value} of metric {name:?} has no gnmi enum mapping"
                        )))
                    })?,
            ),
        };

        // Notification { timestamp = 1, prefix = 2, update = 4 }
        //   prefix: Path { origin = 2, target = 4 }
        //   update: Update { path = 1 (cached bytes), val = 3 }
        let prefix_len = str_field_len(2, &self.origin) + str_field_len(4, target);
        let value_len = scalar.encoded_len();
        let update_len = delimited_field_len(1, path.len()) + delimited_field_len(3, value_len);

        buf.reserve(
            encoding::int64::encoded_len(1, &timestamp)
                + delimited_field_len(2, prefix_len)
                + delimited_field_len(4, update_len),
        );
        if timestamp != 0 {
            encoding::int64::encode(1, &timestamp, buf);
        }
        encode_delimited_header(2, prefix_len, buf);
        encode_str_field(2, &self.origin, buf);
        encode_str_field(4, target, buf);
        encode_delimited_header(4, update_len, buf);
        encode_delimited_header(1, path.len(), buf);
        buf.extend_from_slice(&path);
        encode_delimited_header(3, value_len, buf);
        scalar.encode(buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::uniform::UniformRandom;
    use crate::generator::ValueGenerator;
    use crate::model::metric::Labels;
    use proto::typed_value::Value;
    use rstest::rstest;
    use std::time::{Duration, SystemTime};

    const TEMPLATE: &str = "/interfaces/interface[name={ifName}]/state/counters/{name}";

    fn config() -> GnmiEncoderConfig {
        GnmiEncoderConfig {
            path: Some(TEMPLATE.to_string()),
            ..GnmiEncoderConfig::default()
        }
    }

    fn event_at(name: &str, value: f64, pairs: &[(&str, &str)], nanos: u64) -> MetricEvent {
        MetricEvent::with_timestamp(
            name.to_string(),
            value,
            Labels::from_pairs(pairs).unwrap(),
            UNIX_EPOCH + Duration::from_nanos(nanos),
        )
        .unwrap()
    }

    fn event(name: &str, value: f64) -> MetricEvent {
        event_at(
            name,
            value,
            &[("device", "rtr-1"), ("ifName", "Gi0/0/0")],
            1_700_000_000_123_456_789,
        )
    }

    fn encode(encoder: &GnmiEncoder, event: &MetricEvent) -> proto::Notification {
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).unwrap();
        proto::Notification::decode(buf.as_slice()).unwrap()
    }

    fn only_value(n: &proto::Notification) -> Value {
        assert_eq!(n.update.len(), 1, "exactly one update per notification");
        n.update[0].val.clone().unwrap().value.unwrap()
    }

    #[test]
    fn notification_carries_prefix_timestamp_path_and_value() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let n = encode(&encoder, &event("in_octets", 1.5));

        assert_eq!(n.timestamp, 1_700_000_000_123_456_789);
        let prefix = n.prefix.clone().unwrap();
        assert_eq!(prefix.origin, "openconfig");
        assert_eq!(prefix.target, "rtr-1");
        assert!(prefix.elem.is_empty());
        assert!(n.delete.is_empty());
        assert!(!n.atomic);

        let path = n.update[0].path.clone().unwrap();
        assert_eq!(
            path.to_string(),
            "/interfaces/interface[name=Gi0/0/0]/state/counters/in-octets"
        );
        assert_eq!(only_value(&n), Value::DoubleVal(1.5));
    }

    #[test]
    fn hand_encoding_is_byte_identical_to_prost() {
        let mut cfg = config();
        cfg.values.insert("up".into(), GnmiValueType::Bool);
        cfg.values.insert(
            "state".into(),
            GnmiValueType::Enum(BTreeMap::from([(1, String::new())])),
        );
        let encoder = GnmiEncoder::new(&cfg).unwrap();
        for e in [
            event("in_octets", 42.0),
            event("up", 1.0),
            event("state", 1.0),
            event_at("in_octets", 1.0, &[("ifName", "x")], 0),
        ] {
            let mut buf = Vec::new();
            encoder.encode_metric(&e, &mut buf).unwrap();
            let decoded = proto::Notification::decode(buf.as_slice()).unwrap();
            // Re-encoding only proves the bytes are well formed; a value
            // dropped before decoding round-trips identically. Assert it is there.
            assert!(
                decoded.update[0]
                    .val
                    .as_ref()
                    .and_then(|v| v.value.as_ref())
                    .is_some(),
                "decoded update has no value: {e:?}"
            );
            assert_eq!(decoded.encode_to_vec(), buf, "{e:?}");
        }
    }

    #[test]
    fn target_is_empty_when_the_label_is_absent() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let n = encode(
            &encoder,
            &event_at("in_octets", 1.0, &[("ifName", "Gi0")], 1),
        );
        let prefix = n.prefix.unwrap();
        assert_eq!(prefix.origin, "openconfig");
        assert_eq!(prefix.target, "");
    }

    #[test]
    fn origin_and_target_label_are_configurable() {
        let cfg = GnmiEncoderConfig {
            origin: "openconfig-interfaces".into(),
            target_label: "host".into(),
            ..config()
        };
        let encoder = GnmiEncoder::new(&cfg).unwrap();
        let n = encode(
            &encoder,
            &event_at("m", 1.0, &[("host", "h1"), ("ifName", "e1")], 1),
        );
        let prefix = n.prefix.unwrap();
        assert_eq!(prefix.origin, "openconfig-interfaces");
        assert_eq!(prefix.target, "h1");
    }

    #[test]
    fn paths_entry_wins_over_path_for_its_metric() {
        let mut cfg = config();
        cfg.paths.insert(
            "oper_status".into(),
            "/interfaces/interface[name={ifName}]/state/oper-status".into(),
        );
        let encoder = GnmiEncoder::new(&cfg).unwrap();

        let n = encode(&encoder, &event("oper_status", 1.0));
        assert_eq!(
            n.update[0].path.clone().unwrap().to_string(),
            "/interfaces/interface[name=Gi0/0/0]/state/oper-status"
        );
        let n = encode(&encoder, &event("in_octets", 1.0));
        assert_eq!(
            n.update[0].path.clone().unwrap().to_string(),
            "/interfaces/interface[name=Gi0/0/0]/state/counters/in-octets"
        );
    }

    #[test]
    fn metric_without_a_template_is_a_config_error() {
        let mut cfg = GnmiEncoderConfig::default();
        cfg.paths.insert("a".into(), "/a[if={ifName}]".into());
        let encoder = GnmiEncoder::new(&cfg).unwrap();
        // The covered metric encodes, so the failure below is the template
        // lookup and nothing else.
        encoder
            .encode_metric(&event("a", 1.0), &mut Vec::new())
            .expect("a metric with a template must encode");
        let err = encoder
            .encode_metric(&event("b", 1.0), &mut Vec::new())
            .unwrap_err();
        assert!(matches!(err, SondaError::Config(_)), "{err:?}");
        assert!(err.to_string().contains("no path template"), "{err}");
    }

    #[test]
    fn placeholder_naming_a_missing_label_rejects_the_event() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let err = encoder
            .encode_metric(&event_at("m", 1.0, &[("device", "d")], 1), &mut Vec::new())
            .unwrap_err();
        assert!(
            matches!(err, SondaError::Encoder(EncoderError::EventRejected(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("{ifName}"), "{err}");
    }

    /// The baseline event (`device` is the target, `ifName` is referenced)
    /// encodes, so each rejection below comes from the one label added.
    #[test]
    fn event_with_an_uncovered_label_is_rejected_unless_dropped() {
        let labels = [("device", "rtr-1"), ("ifName", "Gi0/0/0"), ("job", "edge")];
        let encoder = GnmiEncoder::new(&config()).unwrap();
        encoder
            .encode_metric(&event("m", 1.0), &mut Vec::new())
            .expect("the baseline event must encode");

        let err = encoder
            .encode_metric(&event_at("m", 1.0, &labels, 1), &mut Vec::new())
            .unwrap_err();
        assert!(
            matches!(err, SondaError::Encoder(EncoderError::EventRejected(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("label `job`"), "{err}");

        let dropping = GnmiEncoder::new(&with(|c| c.drop_labels.push("job".into()))).unwrap();
        dropping
            .encode_metric(&event_at("m", 1.0, &labels, 1), &mut Vec::new())
            .expect("a label listed in drop_labels is allowed");
    }

    /// `device` passes only as the target label: move the target and the
    /// same event is rejected for carrying it.
    #[test]
    fn target_label_is_the_only_reason_device_is_allowed() {
        GnmiEncoder::new(&config())
            .unwrap()
            .encode_metric(&event("m", 1.0), &mut Vec::new())
            .expect("`device` is allowed while it is the target label");

        let encoder = GnmiEncoder::new(&with(|c| c.target_label = "host".into())).unwrap();
        let err = encoder
            .encode_metric(&event("m", 1.0), &mut Vec::new())
            .unwrap_err();
        assert!(err.to_string().contains("label `device`"), "{err}");
    }

    #[test]
    fn timestamp_before_epoch_is_an_encoder_error() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let mut e = event("m", 1.0);
        e.timestamp = UNIX_EPOCH - Duration::from_secs(1);
        let err = encoder.encode_metric(&e, &mut Vec::new()).unwrap_err();
        assert!(matches!(
            err,
            SondaError::Encoder(EncoderError::TimestampBeforeEpoch(_))
        ));
    }

    #[test]
    fn encode_appends_without_clearing_the_buffer() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let mut alone = Vec::new();
        encoder.encode_metric(&event("m", 1.0), &mut alone).unwrap();
        let mut buf = b"head".to_vec();
        encoder.encode_metric(&event("m", 1.0), &mut buf).unwrap();
        assert_eq!(&buf[..4], b"head");
        assert_eq!(&buf[4..], alone.as_slice());
    }

    #[test]
    fn path_bytes_are_cached_once_per_series() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let mut buf = Vec::new();
        for _ in 0..3 {
            encoder.encode_metric(&event("m", 1.0), &mut buf).unwrap();
        }
        assert_eq!(encoder.paths.read().unwrap().len(), 1);
        encoder.encode_metric(&event("n", 1.0), &mut buf).unwrap();
        encoder
            .encode_metric(
                &event_at("m", 1.0, &[("device", "rtr-1"), ("ifName", "Gi0/0/1")], 1),
                &mut buf,
            )
            .unwrap();
        assert_eq!(encoder.paths.read().unwrap().len(), 3);
    }

    #[test]
    fn encode_log_is_not_supported() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let log = crate::model::log::LogEvent::new(
            crate::model::log::Severity::Info,
            "x".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        let mut buf = Vec::new();
        assert!(matches!(
            encoder.encode_log(&log, &mut buf),
            Err(SondaError::Encoder(EncoderError::NotSupported(_)))
        ));
        assert!(buf.is_empty());
    }

    fn empty_enum_label() -> GnmiValueType {
        GnmiValueType::Enum(BTreeMap::from([(1, String::new())]))
    }

    fn enum_map() -> GnmiValueType {
        GnmiValueType::Enum(BTreeMap::from([
            (1, "UP".to_string()),
            (2, "DOWN".to_string()),
        ]))
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::uint_truncates(       Some(GnmiValueType::Uint),   3.7,    Ok(Value::UintVal(3)))]
    #[case::uint_clamps_negative( Some(GnmiValueType::Uint),   -5.0,   Ok(Value::UintVal(0)))]
    #[case::int_truncates(        Some(GnmiValueType::Int),    -2.9,   Ok(Value::IntVal(-2)))]
    #[case::double(               Some(GnmiValueType::Double), 1.25,   Ok(Value::DoubleVal(1.25)))]
    #[case::default_is_double(    None,                        0.5,    Ok(Value::DoubleVal(0.5)))]
    #[case::bool_zero_is_false(   Some(GnmiValueType::Bool),   0.0,    Ok(Value::BoolVal(false)))]
    #[case::bool_nonzero_is_true( Some(GnmiValueType::Bool),   -0.1,   Ok(Value::BoolVal(true)))]
    #[case::string_shortest(      Some(GnmiValueType::String), 0.1,    Ok(Value::StringVal("0.1".into())))]
    #[case::string_integral(      Some(GnmiValueType::String), 3.0,    Ok(Value::StringVal("3".into())))]
    #[case::string_huge(          Some(GnmiValueType::String), -1e300, Ok(Value::StringVal(format!("{}", -1e300))))]
    #[case::enum_mapped(          Some(enum_map()),            2.0,    Ok(Value::StringVal("DOWN".into())))]
    #[case::enum_unmapped(        Some(enum_map()),            3.0,    Err("no gnmi enum mapping"))]
    #[case::enum_empty_string(    Some(empty_enum_label()),    1.0,    Ok(Value::StringVal(String::new())))]
    #[case::enum_rounds_noise(    Some(enum_map()),            0.9999999999999999, Ok(Value::StringVal("UP".into())))]
    #[case::enum_rounds_down(     Some(enum_map()),            2.4,    Ok(Value::StringVal("DOWN".into())))]
    #[case::enum_nan(             Some(enum_map()),            f64::NAN, Err("cannot be encoded as enum"))]
    #[case::uint_nan(             Some(GnmiValueType::Uint),   f64::NAN, Err("cannot be encoded as uint"))]
    #[case::int_infinite(         Some(GnmiValueType::Int),    f64::INFINITY, Err("cannot be encoded as int"))]
    #[case::bool_nan(             Some(GnmiValueType::Bool),   f64::NAN, Err("cannot be encoded as bool"))]
    #[case::double_keeps_nan(     Some(GnmiValueType::Double), f64::INFINITY, Ok(Value::DoubleVal(f64::INFINITY)))]
    fn value_mapping(
        #[case] value_type: Option<GnmiValueType>,
        #[case] value: f64,
        #[case] expected: Result<Value, &str>,
    ) {
        let mut cfg = config();
        if let Some(t) = value_type {
            cfg.values.insert("m".into(), t);
        }
        let encoder = GnmiEncoder::new(&cfg).unwrap();
        let mut buf = Vec::new();
        match (encoder.encode_metric(&event("m", value), &mut buf), expected) {
            (Ok(()), Ok(want)) => {
                let n = proto::Notification::decode(buf.as_slice()).unwrap();
                assert_eq!(only_value(&n), want);
            }
            (Err(e), Err(needle)) => {
                assert!(
                    matches!(e, SondaError::Encoder(EncoderError::EventRejected(_))),
                    "{e:?}"
                );
                assert!(e.to_string().contains(needle), "{e}");
            }
            (got, want) => panic!("got {got:?}, want {want:?}"),
        }
    }

    #[test]
    fn extreme_values_format_within_the_stack_buffer() {
        let mut text = [0u8; F64_TEXT_CAPACITY];
        for v in [
            f64::MIN_POSITIVE,
            -5e-324,
            f64::MAX,
            f64::MIN,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert_eq!(format_f64(v, &mut text).unwrap(), format!("{v}"));
        }
    }

    /// `config()` with one change applied.
    fn with(change: impl FnOnce(&mut GnmiEncoderConfig)) -> GnmiEncoderConfig {
        let mut cfg = config();
        change(&mut cfg);
        cfg
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::empty_origin(    with(|c| c.origin = String::new()),                                          "origin")]
    #[case::empty_enum(      with(|c| { c.values.insert("m".into(), GnmiValueType::Enum(BTreeMap::new())); }), "enum map")]
    #[case::bad_path(        with(|c| c.path = Some("/a//b".into())),                                     "empty path segment")]
    #[case::bad_paths_entry( with(|c| { c.paths.insert("m".into(), "/a[k".into()); }),                     "unclosed")]
    #[case::origin_prefix(   with(|c| c.path = Some("openconfig:/a/{name}".into())),                      "origin prefix")]
    fn new_rejects(#[case] cfg: GnmiEncoderConfig, #[case] needle: &str) {
        let err = GnmiEncoder::new(&cfg).err().expect("must be rejected");
        assert!(matches!(err, SondaError::Config(_)));
        assert!(err.to_string().contains(needle), "{err}");
    }

    /// Two bad entries, reported in metric-name order. Each `HashMap` gets
    /// its own random iteration order, so building the config many times
    /// would report both errors if `new` walked the maps unsorted.
    #[test]
    fn new_reports_the_same_error_for_the_same_config() {
        for _ in 0..64 {
            let cfg = with(|c| {
                c.values
                    .insert("b".into(), GnmiValueType::Enum(BTreeMap::new()));
                c.values
                    .insert("a".into(), GnmiValueType::Enum(BTreeMap::new()));
                c.paths.insert("z".into(), "/z[k".into());
                c.paths.insert("y".into(), "/y//x".into());
            });
            let err = GnmiEncoder::new(&cfg).err().expect("must be rejected");
            assert!(err.to_string().contains("values.a:"), "{err}");

            let cfg = with(|c| {
                c.paths.insert("z".into(), "/z[k".into());
                c.paths.insert("y".into(), "/y//x".into());
            });
            let err = GnmiEncoder::new(&cfg).err().expect("must be rejected");
            assert!(err.to_string().contains("\"/y//x\""), "{err}");
        }
    }

    /// Three events from a seeded generator, encoded twice by two encoders,
    /// must produce the same bytes, and those bytes are pinned by a snapshot.
    #[test]
    fn snapshot_sequence_is_deterministic() {
        let mut cfg = config();
        cfg.values.insert("in_octets".into(), GnmiValueType::Uint);
        let generator = UniformRandom::new(0.0, 1_000_000.0, 42);
        let events: Vec<MetricEvent> = (0..3u64)
            .map(|tick| {
                event_at(
                    "in_octets",
                    generator.value(tick),
                    &[("device", "rtr-1"), ("ifName", "Gi0/0/0")],
                    1_700_000_000_000_000_000 + tick * 1_000_000_000,
                )
            })
            .collect();

        let run = || -> Vec<Vec<u8>> {
            let encoder = GnmiEncoder::new(&cfg).unwrap();
            events
                .iter()
                .map(|e| {
                    let mut buf = Vec::new();
                    encoder.encode_metric(e, &mut buf).unwrap();
                    buf
                })
                .collect()
        };
        let first = run();

        // Vacuity guard: every buffer is a real notification with one update.
        assert_eq!(first.len(), 3);
        for buf in &first {
            let n = proto::Notification::decode(buf.as_slice()).unwrap();
            assert_eq!(n.update.len(), 1);
            assert!(n.update[0].path.is_some());
            assert!(matches!(only_value(&n), Value::UintVal(_)));
        }

        assert_eq!(
            first,
            run(),
            "same seed must give byte-identical notifications"
        );

        let hex: Vec<String> = first
            .iter()
            .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
            .collect();
        insta::with_settings!({ snapshot_path => "../../../tests/snapshots" }, {
            insta::assert_snapshot!(hex.join("\n"));
        });
    }

    #[cfg(feature = "config")]
    #[test]
    fn config_deserializes_with_defaults_and_value_types() {
        let yaml = r#"
path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
paths:
  oper_status: "/interfaces/interface[name={ifName}]/state/oper-status"
values:
  in_octets: uint
  errors: int
  ratio: double
  up: bool
  label: string
  oper_status:
    enum: { 1: UP, 2: DOWN }
"#;
        let cfg: GnmiEncoderConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(cfg.origin, DEFAULT_ORIGIN);
        assert_eq!(cfg.target_label, DEFAULT_TARGET_LABEL);
        assert_eq!(cfg.values["in_octets"], GnmiValueType::Uint);
        assert_eq!(cfg.values["errors"], GnmiValueType::Int);
        assert_eq!(cfg.values["ratio"], GnmiValueType::Double);
        assert_eq!(cfg.values["up"], GnmiValueType::Bool);
        assert_eq!(cfg.values["label"], GnmiValueType::String);
        assert_eq!(cfg.values["oper_status"], enum_map());
        assert_eq!(cfg.paths.len(), 1);
    }

    #[cfg(feature = "config")]
    #[test]
    fn value_types_serialize_to_the_yaml_they_parse_from() {
        for value in [
            GnmiValueType::Uint,
            GnmiValueType::Int,
            GnmiValueType::Double,
            GnmiValueType::Bool,
            GnmiValueType::String,
            enum_map(),
        ] {
            let yaml = serde_yaml_ng::to_string(&value).unwrap();
            let back: GnmiValueType = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(back, value, "{yaml}");
        }
        assert_eq!(
            serde_yaml_ng::to_string(&GnmiValueType::Uint).unwrap(),
            "uint\n"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn enum_with_an_extra_key_fails_to_deserialize() {
        let yaml = "values:\n  m:\n    enum: { 1: UP }\n    extra: 1\n";
        assert!(serde_yaml_ng::from_str::<GnmiEncoderConfig>(yaml).is_err());
    }

    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest]
    #[case::values_typo("type: gnmi\npath: /a/{name}\nvaules:\n  m: uint\n", "vaules")]
    #[case::origin_typo("type: gnmi\npath: /a/{name}\norgin: x\n", "orgin")]
    #[case::drop_label_typo("type: gnmi\npath: /a/{name}\ndrop_label: [job]\n", "drop_label")]
    fn unknown_encoder_keys_are_rejected(#[case] yaml: &str, #[case] key: &str) {
        // Positive control: the same block without the typo parses, so the
        // rejection below can only come from the misspelt key — and `type`
        // itself is not treated as unknown.
        let valid =
            "type: gnmi\npath: /a/{name}\norigin: x\nvalues:\n  m: uint\ndrop_labels: [job]\n";
        serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(valid)
            .expect("a well-formed gnmi block must parse");

        let err = serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(yaml)
            .expect_err("a misspelt key must be rejected")
            .to_string();
        assert!(err.contains(key), "error should name {key:?}: {err}");
    }

    /// JSON object keys are always strings, so `POST /events` and
    /// `POST /scenarios` can only send `"1"`. Quoted YAML keys are the same.
    /// The unquoted YAML form must keep working alongside them.
    #[cfg(feature = "config")]
    #[rstest]
    #[case::yaml_integer_keys(
        "type: gnmi\npath: /a/{name}\nvalues:\n  m:\n    enum: { 1: UP, -2: DOWN }\n"
    )]
    #[case::yaml_quoted_keys(
        "type: gnmi\npath: /a/{name}\nvalues:\n  m:\n    enum: { \"1\": UP, \"-2\": DOWN }\n"
    )]
    fn enum_codes_parse_bare_or_quoted_from_yaml(#[case] yaml: &str) {
        let cfg = serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(yaml).unwrap();
        let crate::encoder::EncoderConfig::Gnmi(cfg) = cfg else {
            panic!("not the gnmi variant")
        };
        assert_eq!(
            cfg.values["m"],
            GnmiValueType::Enum(BTreeMap::from([(1, "UP".into()), (-2, "DOWN".into())]))
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn enum_codes_parse_from_json_string_keys() {
        let json =
            r#"{"type":"gnmi","path":"/a/{name}","values":{"m":{"enum":{"1":"UP","2":"DOWN"}}}}"#;
        let cfg = serde_json::from_str::<crate::encoder::EncoderConfig>(json).unwrap();
        let crate::encoder::EncoderConfig::Gnmi(cfg) = cfg else {
            panic!("not the gnmi variant")
        };
        assert_eq!(cfg.values["m"], enum_map());
    }

    #[cfg(feature = "config")]
    #[rstest]
    #[case::misspelt_name("values:\n  m: unit\n", "unknown gnmi value type \"unit\"")]
    #[case::non_integer_key(
        "values:\n  m:\n    enum: { UP: 1 }\n",
        "enum key \"UP\" is not an integer"
    )]
    #[case::wrong_map_key("values:\n  m:\n    enums: { 1: UP }\n", "unknown key \"enums\"")]
    fn value_type_errors_say_what_is_accepted(#[case] yaml: &str, #[case] needle: &str) {
        let err = serde_yaml_ng::from_str::<GnmiEncoderConfig>(yaml)
            .expect_err("must be rejected")
            .to_string();
        assert!(err.contains(needle), "{err}");
        if !needle.starts_with("enum key") {
            assert!(err.contains("uint, int, double, bool, string"), "{err}");
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn unknown_value_type_fails_to_deserialize() {
        let yaml = "values:\n  m: float\n";
        assert!(serde_yaml_ng::from_str::<GnmiEncoderConfig>(yaml).is_err());
    }

    #[test]
    fn encoded_time_is_nanoseconds_since_epoch() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let ts = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 5);
        let mut e = event("m", 1.0);
        e.timestamp = ts;
        assert_eq!(encode(&encoder, &e).timestamp, 1_700_000_000_000_000_005);
    }
}
