//! gNMI encoder: one `MetricEvent` in, one prost-encoded `gnmi.Notification` out.
//!
//! The update path is rendered from a [`PathTemplate`] the first time a series
//! is seen and cached as encoded `Path` bytes; later events for that series
//! copy those bytes and encode only the timestamp, prefix and typed value. A
//! series is its metric name and the labels its template can see, so events
//! that differ only in the target label or a `drop_labels` label share one
//! entry. A series whose path cannot be rendered is cached with the reason,
//! and its later events are rejected without rendering again.
//!
//! Requires the `gnmi` feature flag.

pub mod path;
pub mod proto;
pub mod value;

pub use value::GnmiValueType;

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::UNIX_EPOCH;

use prost::encoding::{self, encode_key, encode_varint, encoded_len_varint, key_len, WireType};
use prost::Message;

use crate::model::metric::{is_valid_metric_name, Labels, MetricEvent, ValidatedMetricName};
use crate::{ConfigError, EncoderError, SondaError};

use super::Encoder;
use path::PathTemplate;

/// Default `Path.origin` on the notification prefix.
pub const DEFAULT_ORIGIN: &str = "openconfig";

/// Default label whose value becomes `Path.target` on the notification prefix.
pub const DEFAULT_TARGET_LABEL: &str = "device";

/// Upper bound on the `Display` length of an `f64`, sign included.
const F64_TEXT_CAPACITY: usize = 400;

/// Configuration for [`GnmiEncoder`]; the body of `encoder: { type: gnmi, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `vaules:` is an
/// error rather than a silent fallback to the defaults.
#[derive(Debug, Clone, PartialEq)]
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
    /// `dynamic_labels`, and every `cardinality_spikes` label, to be
    /// referenced by its template, be the `target_label`, or be listed here;
    /// series that differ only in an unlisted, unreferenced label would
    /// otherwise share one path. A `cardinality_spikes` label cannot be a
    /// template placeholder, so unless it is the `target_label` or shares a
    /// key with a referenced label, it must be listed here.
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
    template: Option<Template>,
    overrides: HashMap<String, Template>,
    values: HashMap<String, GnmiValueType>,
    drop_labels: Vec<String>,
    /// Each series' encoded `Path` bytes or rejection, filled on first sight,
    /// keyed by the series' [fingerprint](Template::fingerprint).
    paths: RwLock<HashMap<u64, Vec<CachedPath>>>,
    /// Paths rendered, for tests.
    #[cfg(test)]
    renders: std::sync::atomic::AtomicUsize,
}

/// A path template and the labels it cannot see: the `target_label` and the
/// `drop_labels` labels it does not reference.
struct Template {
    template: PathTemplate,
    unseen: Vec<String>,
}

impl Template {
    /// `template`, seeing every label but `target_label` and `drop_labels`
    /// unless it references them.
    fn new(template: PathTemplate, target_label: &str, drop_labels: &[String]) -> Self {
        let referenced = |label: &str| {
            template
                .placeholders()
                .any(|p| p.name != path::NAME_PLACEHOLDER && p.name == label)
        };
        let unseen = std::iter::once(target_label)
            .chain(drop_labels.iter().map(String::as_str))
            .filter(|label| !referenced(label))
            .map(str::to_string)
            .collect();
        Self { template, unseen }
    }

    /// The labels of `labels` this template can see, in key order.
    fn seen<'a>(&'a self, labels: &'a Labels) -> impl Iterator<Item = (&'a str, &'a str)> {
        labels
            .iter()
            .filter(|(key, _)| !self.unseen.iter().any(|unseen| unseen == key))
    }

    /// A hash of `name` and the labels of `labels` this template can see.
    fn fingerprint(&self, name: &str, labels: &Labels) -> u64 {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        for (key, value) in self.seen(labels) {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// One series' cache entry: the name and labels of its first event, and its
/// encoded `Path` bytes or the text it is rejected with.
struct CachedPath {
    name: ValidatedMetricName,
    labels: Arc<Labels>,
    path: Result<Arc<[u8]>, String>,
}

impl CachedPath {
    /// Whether an event named `name` with `labels` is this entry's series
    /// under `template`.
    fn is_series(&self, template: &Template, name: &str, labels: &Labels) -> bool {
        *self.name == *name && template.seen(&self.labels).eq(template.seen(labels))
    }

    /// The cached path, or the cached rejection as an
    /// [`EncoderError::EventRejected`].
    fn path(&self) -> Result<Arc<[u8]>, SondaError> {
        self.path
            .clone()
            .map_err(|reason| SondaError::Encoder(EncoderError::EventRejected(reason)))
    }
}

impl GnmiEncoder {
    /// Build an encoder, parsing every template in `cfg`.
    ///
    /// Returns [`SondaError::Config`] when `origin` is empty, a `values:` or
    /// `paths:` key is not a valid metric name, an `enum` value map is empty,
    /// or `path` or a `paths:` entry is not a valid template. `values` and
    /// `paths` are checked in key order, so the same config always reports
    /// the same first error.
    pub fn new(cfg: &GnmiEncoderConfig) -> Result<Self, SondaError> {
        if cfg.origin.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "gnmi encoder origin must not be empty",
            )));
        }
        let check_key = |field: &str, metric: &str| {
            if is_valid_metric_name(metric) {
                Ok(())
            } else {
                Err(SondaError::Config(ConfigError::invalid(format!(
                    "gnmi encoder {field} key {metric:?} is not a metric name; keys are \
                     metric names as the scenario writes them, with '_' (in_octets, not \
                     in-octets)"
                ))))
            }
        };
        for (metric, value) in cfg.values.iter().collect::<BTreeMap<_, _>>() {
            check_key("values", metric)?;
            if matches!(value, GnmiValueType::Enum(map) if map.is_empty()) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "gnmi encoder values.{metric}: enum map must not be empty"
                ))));
            }
        }
        let template_of = |template| Template::new(template, &cfg.target_label, &cfg.drop_labels);
        let template = cfg
            .path
            .as_deref()
            .map(PathTemplate::parse)
            .transpose()?
            .map(template_of);
        let overrides = cfg
            .paths
            .iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(metric, t)| {
                check_key("paths", metric)?;
                Ok((metric.clone(), template_of(PathTemplate::parse(t)?)))
            })
            .collect::<Result<HashMap<_, _>, SondaError>>()?;

        Ok(Self {
            origin: cfg.origin.clone(),
            target_label: cfg.target_label.clone(),
            template,
            overrides,
            values: cfg.values.clone(),
            drop_labels: cfg.drop_labels.clone(),
            paths: RwLock::new(HashMap::new()),
            #[cfg(test)]
            renders: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// The template used for `metric`: its `paths:` entry if present, else
    /// `path`. Returns [`SondaError::Config`] when neither covers the metric.
    pub(crate) fn template_for(&self, metric: &str) -> Result<&PathTemplate, SondaError> {
        self.template_of(metric).map(|t| &t.template)
    }

    /// [`template_for`](Self::template_for), with the labels it cannot see.
    fn template_of(&self, metric: &str) -> Result<&Template, SondaError> {
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

    /// Whether `key` is the `target_label` or listed in `drop_labels`, the two
    /// ways a label may stay out of the path on purpose.
    pub(crate) fn label_needs_no_reference(&self, key: &str) -> bool {
        key == self.target_label || self.drop_labels.iter().any(|dropped| dropped == key)
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
            !self.label_needs_no_reference(key)
                && !template
                    .placeholders()
                    .any(|p| p.name != path::NAME_PLACEHOLDER && p.name == *key)
        })
    }

    /// Encoded `Path` bytes for the event's series, rendering and caching them on first sight.
    ///
    /// A series is the event's metric name and the labels its template can
    /// see: every label but the `target_label` and those in `drop_labels`,
    /// unless the template references them. Events of one series share one
    /// entry whatever values those other labels take. A hit allocates
    /// nothing: the entry is found by a hash of the name and the labels the
    /// template sees, then compared label by label.
    ///
    /// A series whose path is rejected ([`EncoderError::EventRejected`]: a
    /// label the template does not cover, a placeholder whose label the
    /// event lacks, or an element name that is not valid) is cached with the
    /// rejection's text, and each later event of it gets that text without
    /// the path being rendered again. Returns [`SondaError::Config`], and
    /// caches nothing, when the metric has no template.
    fn path_bytes(&self, event: &MetricEvent) -> Result<Arc<[u8]>, SondaError> {
        let name: &str = &event.name;
        let template = self.template_of(name)?;
        let fingerprint = template.fingerprint(name, &event.labels);
        let cached = |paths: &HashMap<u64, Vec<CachedPath>>| {
            paths.get(&fingerprint).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.is_series(template, name, &event.labels))
                    .map(CachedPath::path)
            })
        };
        if let Some(path) = cached(&self.paths.read().unwrap_or_else(PoisonError::into_inner)) {
            return path;
        }

        let path = match self.render_path(template, event) {
            Ok(bytes) => Ok(bytes),
            Err(SondaError::Encoder(EncoderError::EventRejected(reason))) => Err(reason),
            Err(other) => return Err(other),
        };
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have cached the series since the read.
        if let Some(path) = cached(&paths) {
            return path;
        }
        let entry = CachedPath {
            name: event.name.clone(),
            labels: Arc::clone(&event.labels),
            path,
        };
        let path = entry.path();
        paths.entry(fingerprint).or_default().push(entry);
        path
    }

    /// The event's path under `template`, encoded: rejected when the event
    /// carries a label the template does not cover, or the render fails.
    fn render_path(
        &self,
        template: &Template,
        event: &MetricEvent,
    ) -> Result<Arc<[u8]>, SondaError> {
        #[cfg(test)]
        self.renders
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name: &str = &event.name;
        let template = &template.template;
        if let Some(label) = self.uncovered_label(template, event.labels.iter().map(|(k, _)| k)) {
            return Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                "metric {name:?} carries label `{label}`, which the gnmi path template does \
                 not reference; reference it, make it the target_label, or list it in \
                 drop_labels"
            ))));
        }
        Ok(template.render(name, &event.labels)?.encode_to_vec().into())
    }

    /// How many series the path cache holds.
    #[cfg(test)]
    fn cached_series(&self) -> usize {
        self.paths
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Vec::len)
            .sum()
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

/// The rounded values that fit an `i64`: from -2^63 up to, not including,
/// 2^63. Both bounds are exact `f64`s.
const I64_RANGE: std::ops::Range<f64> = -9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0;

/// The rounded values that fit a `u64`: from 0 up to, not including, 2^64.
/// `-0.0`, which `-0.4` rounds to, is in range.
const U64_RANGE: std::ops::Range<f64> = 0.0..18_446_744_073_709_551_616.0;

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
    /// event. Returns [`EncoderError::EventRejected`] when this event does not
    /// fit the configuration:
    ///
    /// - a template placeholder names a label the event lacks;
    /// - the event carries a label the template does not reference that is
    ///   neither the `target_label` nor listed in `drop_labels`;
    /// - a placeholder in element-name position renders empty or containing
    ///   `/`, `[` or `]`;
    /// - the value is NaN or infinite and the metric is `uint`, `int`, `bool`
    ///   or `enum`;
    /// - the rounded value is outside the `i64` range for `int`, or the `u64`
    ///   range for `uint` (so any value at or below -0.5);
    /// - the rounded value has no `enum` mapping.
    ///
    /// Returns [`EncoderError::TimestampBeforeEpoch`] or
    /// [`EncoderError::Other`] when the timestamp predates the epoch or exceeds
    /// `i64` nanoseconds.
    ///
    /// The label checks run only when a series is first seen and its path is
    /// rendered and cached; later events for the same series skip them, so
    /// they cost nothing in steady state. A series rejected by them, or by
    /// its render, is rejected with the same text on every later event
    /// without being rendered again.
    ///
    /// The output is not length-prefixed, so it is only usable by a sink that
    /// takes one notification per write. Validation rejects pairing this
    /// encoder with any sink other than `gnmi_target`.
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
        // Every integer-shaped type rounds the same way.
        let rounded = |kind: &str| {
            if value.is_finite() {
                Ok(value.round())
            } else {
                Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                    "value {value} of metric {name:?} cannot be encoded as {kind}"
                ))))
            }
        };
        let out_of_range = |kind: &str| {
            SondaError::Encoder(EncoderError::EventRejected(format!(
                "value {value} of metric {name:?} is outside the {kind} range"
            )))
        };
        let scalar = match self.values.get(name) {
            None | Some(GnmiValueType::Double) => Scalar::Double(value),
            Some(GnmiValueType::Uint) => {
                let r = rounded("uint")?;
                if !U64_RANGE.contains(&r) {
                    return Err(out_of_range("uint"));
                }
                Scalar::Uint(r as u64)
            }
            Some(GnmiValueType::Int) => {
                let r = rounded("int")?;
                if !I64_RANGE.contains(&r) {
                    return Err(out_of_range("int"));
                }
                Scalar::Int(r as i64)
            }
            Some(GnmiValueType::Bool) => Scalar::Bool(rounded("bool")? != 0.0),
            Some(GnmiValueType::String) => {
                text = [0u8; F64_TEXT_CAPACITY];
                Scalar::Str(format_f64(value, &mut text)?)
            }
            Some(GnmiValueType::Enum(map)) => Scalar::Str(
                Some(rounded("enum")?)
                    .filter(|r| I64_RANGE.contains(r))
                    .and_then(|r| map.get(&(r as i64)))
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

/// Append one prost-encoded `Notification` deleting the series at `path` —
/// NOT length-prefixed — into `buf`.
///
/// `path` is a series' full path, as the sink's table keys it: its origin
/// and target go on the notification prefix and its elements on `delete[0]`,
/// split as [`GnmiEncoder`] splits an update's prefix and path. A timestamp of
/// 0 is omitted.
pub(crate) fn encode_delete(buf: &mut Vec<u8>, path: &proto::Path, timestamp_ns: i64) {
    // Notification { timestamp = 1, prefix = 2, delete = 5 }
    //   prefix: Path { origin = 2, target = 4 }
    //   delete: Path { elem = 3 }
    let prefix_len = str_field_len(2, &path.origin) + str_field_len(4, &path.target);
    let delete_len: usize = path
        .elem
        .iter()
        .map(|elem| encoding::message::encoded_len(3, elem))
        .sum();
    buf.reserve(
        encoding::int64::encoded_len(1, &timestamp_ns)
            + delimited_field_len(2, prefix_len)
            + delimited_field_len(5, delete_len),
    );
    if timestamp_ns != 0 {
        encoding::int64::encode(1, &timestamp_ns, buf);
    }
    encode_delimited_header(2, prefix_len, buf);
    encode_str_field(2, &path.origin, buf);
    encode_str_field(4, &path.target, buf);
    encode_delimited_header(5, delete_len, buf);
    for elem in &path.elem {
        encoding::message::encode(3, elem, buf);
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

    /// A delete for a series the encoder rendered carries the update's
    /// prefix and, as its one deleted path, the update's path: the full path
    /// joined from the update the way the sink's table joins it splits back
    /// the same way.
    #[test]
    fn a_delete_splits_its_path_as_the_update_does() {
        let custom = GnmiEncoderConfig {
            origin: "openconfig-interfaces".to_string(),
            ..config()
        };
        let cases = [
            (config(), event("in_octets", 1.5)),
            (
                config(),
                event_at("in_octets", 2.0, &[("ifName", "Gi0/0/1")], 7),
            ),
            (custom, event("out_octets", 3.0)),
        ];
        for (cfg, e) in cases {
            let encoder = GnmiEncoder::new(&cfg).unwrap();
            let mut update_bytes = Vec::new();
            encoder.encode_metric(&e, &mut update_bytes).unwrap();
            let update = proto::Notification::decode(update_bytes.as_slice()).unwrap();
            let full = crate::sink::gnmi::target::PathKey::decode(&update_bytes).unwrap();

            let mut delete_bytes = Vec::new();
            encode_delete(&mut delete_bytes, full.path(), 1_700_000_000_000_000_001);
            let delete = proto::Notification::decode(delete_bytes.as_slice()).unwrap();

            assert_eq!(delete.timestamp, 1_700_000_000_000_000_001);
            assert_eq!(delete.prefix, update.prefix, "{}", e.name);
            assert_eq!(delete.delete.len(), 1, "{}", e.name);
            assert_eq!(Some(&delete.delete[0]), update.update[0].path.as_ref());
            assert!(delete.update.is_empty(), "{}", e.name);
        }
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
        assert_eq!(encoder.cached_series(), 1);
        encoder.encode_metric(&event("n", 1.0), &mut buf).unwrap();
        encoder
            .encode_metric(
                &event_at("m", 1.0, &[("device", "rtr-1"), ("ifName", "Gi0/0/1")], 1),
                &mut buf,
            )
            .unwrap();
        assert_eq!(encoder.cached_series(), 3);
    }

    /// The encoded path of `event`'s update.
    fn path_of(encoder: &GnmiEncoder, event: &MetricEvent) -> String {
        encode(encoder, event).update[0]
            .path
            .clone()
            .expect("a path")
            .to_string()
    }

    /// An encoder on [`TEMPLATE`] that drops `spike`.
    fn dropping_spike() -> GnmiEncoder {
        GnmiEncoder::new(&GnmiEncoderConfig {
            drop_labels: vec!["spike".to_string()],
            ..config()
        })
        .unwrap()
    }

    /// A spike of 1 000 values on a label in `drop_labels` leaves one cache
    /// entry: every event renders the one path the template writes.
    #[test]
    fn a_dropped_label_spike_leaves_one_cache_entry() {
        let encoder = dropping_spike();
        let mut paths = std::collections::BTreeSet::new();
        for i in 0..1_000 {
            let value = format!("spike-{i}");
            let event = event_at(
                "in_octets",
                1.0,
                &[
                    ("device", "rtr-1"),
                    ("ifName", "Gi0/0/0"),
                    ("spike", &value),
                ],
                1,
            );
            paths.insert(path_of(&encoder, &event));
        }
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            ["/interfaces/interface[name=Gi0/0/0]/state/counters/in-octets"],
            "the premise: every event renders one path"
        );
        assert_eq!(encoder.cached_series(), 1);
        assert_eq!(
            encoder.renders.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    /// Two events that differ only in a dropped label get the cache's one
    /// copy of the path bytes.
    #[test]
    fn keys_differing_only_in_a_dropped_label_share_the_path_bytes() {
        let encoder = dropping_spike();
        let pairs = |spike| [("device", "rtr-1"), ("ifName", "Gi0/0/0"), ("spike", spike)];
        let a = encoder
            .path_bytes(&event_at("in_octets", 1.0, &pairs("a"), 1))
            .unwrap();
        let b = encoder
            .path_bytes(&event_at("in_octets", 1.0, &pairs("b"), 1))
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "one cached path for both");
        assert_eq!(encoder.cached_series(), 1);
    }

    /// Two events that differ in a label the template references render
    /// their own paths and never share an entry.
    #[test]
    fn keys_differing_in_a_kept_label_never_share_an_entry() {
        let encoder = dropping_spike();
        let on = |if_name| {
            event_at(
                "in_octets",
                1.0,
                &[("device", "rtr-1"), ("ifName", if_name)],
                1,
            )
        };
        let (a, b) = (on("Gi0/0/0"), on("Gi0/0/1"));
        assert_ne!(path_of(&encoder, &a), path_of(&encoder, &b));
        assert!(!Arc::ptr_eq(
            &encoder.path_bytes(&a).unwrap(),
            &encoder.path_bytes(&b).unwrap()
        ));
        assert_eq!(encoder.cached_series(), 2);
    }

    /// The target label goes on the prefix, not the path: events that
    /// differ only in it share the entry and keep their own targets.
    #[test]
    fn a_target_label_change_alone_shares_the_entry() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        let on = |device| {
            event_at(
                "in_octets",
                1.0,
                &[("device", device), ("ifName", "Gi0/0/0")],
                1,
            )
        };
        let (a, b) = (on("rtr-1"), on("rtr-2"));
        assert!(Arc::ptr_eq(
            &encoder.path_bytes(&a).unwrap(),
            &encoder.path_bytes(&b).unwrap()
        ));
        assert_eq!(encoder.cached_series(), 1);
        let target = |event| encode(&encoder, event).prefix.expect("a prefix").target;
        assert_eq!(
            (target(&a), target(&b)),
            ("rtr-1".to_string(), "rtr-2".to_string())
        );
    }

    /// A target label or dropped label the template references is part of
    /// the series: events that differ in it render their own paths.
    #[rustfmt::skip]
    #[rstest]
    #[case::target_label(GnmiEncoderConfig { path: Some("/devices/device[name={device}]/{name}".into()), ..GnmiEncoderConfig::default() }, "device")]
    #[case::dropped_label(GnmiEncoderConfig { path: Some("/sites/site[name={site}]/{name}".into()), drop_labels: vec!["site".into()], ..GnmiEncoderConfig::default() }, "site")]
    fn a_label_the_template_references_stays_in_the_series(
        #[case] cfg: GnmiEncoderConfig,
        #[case] label: &str,
    ) {
        let encoder = GnmiEncoder::new(&cfg).unwrap();
        let first = path_of(&encoder, &event_at("up", 1.0, &[(label, "one")], 1));
        let second = path_of(&encoder, &event_at("up", 1.0, &[(label, "two")], 1));
        assert!(first.contains("[name=one]"), "{first}");
        assert!(second.contains("[name=two]"), "{second}");
        assert_eq!(encoder.cached_series(), 2);
    }

    /// A series rejected on first sight is rejected with the same text on
    /// its 1 000th event, and its path is rendered once.
    #[test]
    fn a_rejected_series_is_rendered_once() {
        let encoder = GnmiEncoder::new(&config()).unwrap();
        // The template's {ifName} names a label this series lacks.
        let event = event_at("in_octets", 1.0, &[("device", "rtr-1")], 1);
        let mut buf = Vec::new();
        let mut reasons = std::collections::BTreeSet::new();
        for _ in 0..1_000 {
            match encoder.encode_metric(&event, &mut buf) {
                Err(SondaError::Encoder(EncoderError::EventRejected(reason))) => {
                    reasons.insert(reason);
                }
                other => panic!("expected a rejection, got {other:?}"),
            }
        }
        assert!(buf.is_empty(), "a rejected event writes nothing");
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(
            reasons.iter().all(|reason| reason.contains("{ifName}")),
            "{reasons:?}"
        );
        assert_eq!(
            encoder.renders.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(encoder.cached_series(), 1);
    }

    /// A rejection of one event's value is not the series': the series'
    /// next event with a value in range is encoded.
    #[test]
    fn a_rejected_value_is_not_cached() {
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            values: HashMap::from([("in_octets".to_string(), GnmiValueType::Uint)]),
            ..config()
        })
        .unwrap();
        let mut buf = Vec::new();
        assert!(matches!(
            encoder.encode_metric(&event("in_octets", -5.0), &mut buf),
            Err(SondaError::Encoder(EncoderError::EventRejected(_)))
        ));
        assert_eq!(
            only_value(&encode(&encoder, &event("in_octets", 5.0))),
            Value::UintVal(5)
        );
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
    #[case::uint_rounds_up(       Some(GnmiValueType::Uint),   3.7,    Ok(Value::UintVal(4)))]
    #[case::uint_rounds_down(     Some(GnmiValueType::Uint),   3.4,    Ok(Value::UintVal(3)))]
    #[case::uint_rounds_noise(    Some(GnmiValueType::Uint),   0.9999999999999999, Ok(Value::UintVal(1)))]
    #[case::uint_rejects_negative(Some(GnmiValueType::Uint),   -5.0,   Err("outside the uint range"))]
    #[case::uint_rejects_minus_06(Some(GnmiValueType::Uint),   -0.6,   Err("outside the uint range"))]
    #[case::uint_rejects_minus_05(Some(GnmiValueType::Uint),   -0.5,   Err("outside the uint range"))]
    #[case::int_rounds(           Some(GnmiValueType::Int),    -2.9,   Ok(Value::IntVal(-3)))]
    #[case::int_half_away(        Some(GnmiValueType::Int),    2.5,    Ok(Value::IntVal(3)))]
    #[case::int_rounds_noise(     Some(GnmiValueType::Int),    0.9999999999999999, Ok(Value::IntVal(1)))]
    #[case::double(               Some(GnmiValueType::Double), 1.25,   Ok(Value::DoubleVal(1.25)))]
    #[case::default_is_double(    None,                        0.5,    Ok(Value::DoubleVal(0.5)))]
    #[case::bool_zero_is_false(   Some(GnmiValueType::Bool),   0.0,    Ok(Value::BoolVal(false)))]
    #[case::bool_nonzero_is_true( Some(GnmiValueType::Bool),   -0.6,   Ok(Value::BoolVal(true)))]
    #[case::bool_rounds_to_false( Some(GnmiValueType::Bool),   0.4,    Ok(Value::BoolVal(false)))]
    #[case::bool_sin_pi_is_false( Some(GnmiValueType::Bool),   std::f64::consts::PI.sin(), Ok(Value::BoolVal(false)))]
    #[case::bool_rounds_noise(    Some(GnmiValueType::Bool),   0.9999999999999999, Ok(Value::BoolVal(true)))]
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
    #[case::double_keeps_infinity(Some(GnmiValueType::Double), f64::INFINITY, Ok(Value::DoubleVal(f64::INFINITY)))]
    #[case::double_keeps_nan(     Some(GnmiValueType::Double), f64::NAN, Ok(Value::DoubleVal(f64::NAN)))]
    #[case::bool_small_negative(  Some(GnmiValueType::Bool),   -0.4,   Ok(Value::BoolVal(false)))]
    #[case::uint_small_negative(  Some(GnmiValueType::Uint),   -0.4,   Ok(Value::UintVal(0)))]
    #[case::int_small_negative(   Some(GnmiValueType::Int),    -0.4,   Ok(Value::IntVal(0)))]
    #[case::int_rejects_high(     Some(GnmiValueType::Int),    1e20,   Err("outside the int range"))]
    #[case::int_rejects_low(      Some(GnmiValueType::Int),    -1e20,  Err("outside the int range"))]
    #[case::int_rejects_2_pow_63( Some(GnmiValueType::Int),    9_223_372_036_854_775_808.0, Err("outside the int range"))]
    #[case::int_accepts_min(      Some(GnmiValueType::Int),    -9_223_372_036_854_775_808.0, Ok(Value::IntVal(i64::MIN)))]
    #[case::uint_rejects_high(    Some(GnmiValueType::Uint),   1e25,   Err("outside the uint range"))]
    #[case::uint_rejects_2_pow_64(Some(GnmiValueType::Uint),   18_446_744_073_709_551_616.0, Err("outside the uint range"))]
    #[case::uint_accepts_largest( Some(GnmiValueType::Uint),   18_446_744_073_709_549_568.0, Ok(Value::UintVal(18_446_744_073_709_549_568)))]
    #[case::enum_out_of_i64_range(Some(GnmiValueType::Enum(BTreeMap::from([(i64::MAX, "MAX".into())]))), 1e30, Err("no gnmi enum mapping"))]
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
                match (only_value(&n), want) {
                    // Bit equality, so NaN matches NaN and -0.0 differs from 0.0.
                    (Value::DoubleVal(got), Value::DoubleVal(want)) => {
                        assert_eq!(got.to_bits(), want.to_bits(), "got {got}, want {want}")
                    }
                    (got, want) => assert_eq!(got, want),
                }
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
    #[case::values_key_dash( with(|c| { c.values.insert("in-octets".into(), GnmiValueType::Uint); }),       "values key \"in-octets\" is not a metric name")]
    #[case::paths_key_dash(  with(|c| { c.paths.insert("in-octets".into(), "/a".into()); }),                "paths key \"in-octets\" is not a metric name")]
    fn new_rejects(#[case] cfg: GnmiEncoderConfig, #[case] needle: &str) {
        let err = GnmiEncoder::new(&cfg).err().expect("must be rejected");
        assert!(matches!(err, SondaError::Config(_)));
        assert!(err.to_string().contains(needle), "{err}");
    }

    /// The positive control for the key cases of `new_rejects`: the same keys
    /// in metric-name form are accepted and used.
    #[test]
    fn values_and_paths_keys_in_metric_name_form_are_accepted() {
        let cfg = with(|c| {
            c.values.insert("in_octets".into(), GnmiValueType::Uint);
            c.path = None;
            c.paths.insert("in_octets".into(), TEMPLATE.into());
        });
        let encoder = GnmiEncoder::new(&cfg).expect("metric-name keys are valid");
        let mut buf = Vec::new();
        encoder
            .encode_metric(&event("in_octets", 3.0), &mut buf)
            .unwrap();
        let n = proto::Notification::decode(buf.as_slice()).unwrap();
        assert_eq!(only_value(&n), Value::UintVal(3));
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

    /// Labels written as numbers or booleans are stored as their text, from
    /// YAML and from JSON alike.
    #[cfg(feature = "config")]
    #[test]
    fn enum_labels_may_be_numbers_or_booleans() {
        let want = GnmiValueType::Enum(BTreeMap::from([
            (0, "false".into()),
            (1, "10".into()),
            (2, "1.5".into()),
            (3, "UP".into()),
            (4, "1".into()),
        ]));
        let yaml = "type: gnmi\npath: /a/{name}\nvalues:\n  m:\n    enum: { 0: false, 1: 10, 2: 1.5, 3: UP, 4: 1.0 }\n";
        let json = r#"{"type":"gnmi","path":"/a/{name}","values":{"m":{"enum":{"0":false,"1":10,"2":1.5,"3":"UP","4":1.0}}}}"#;
        for cfg in [
            serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(yaml).unwrap(),
            serde_json::from_str::<crate::encoder::EncoderConfig>(json).unwrap(),
        ] {
            let crate::encoder::EncoderConfig::Gnmi(cfg) = cfg else {
                panic!("not the gnmi variant")
            };
            assert_eq!(cfg.values["m"], want);
        }
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
    #[case::plus_sign(
        "values:\n  m:\n    enum: { \"+1\": UP }\n",
        "enum key \"+1\" is not an integer"
    )]
    #[case::padded(
        "values:\n  m:\n    enum: { \" 1\": UP }\n",
        "enum key \" 1\" is not an integer"
    )]
    #[case::decimal(
        "values:\n  m:\n    enum: { \"1.0\": UP }\n",
        "enum key \"1.0\" is not an integer"
    )]
    #[case::same_code_twice(
        "values:\n  m:\n    enum: { 1: UP, \"01\": DOWN }\n",
        "enum code 1 is given more than once"
    )]
    fn value_type_errors_say_what_is_accepted(#[case] yaml: &str, #[case] needle: &str) {
        let err = serde_yaml_ng::from_str::<GnmiEncoderConfig>(yaml)
            .expect_err("must be rejected")
            .to_string();
        assert!(err.contains(needle), "{err}");
        if needle.starts_with("unknown") {
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
