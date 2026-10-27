//! SNMP encoder: one `MetricEvent` in, one publish record out.
//!
//! A publish record is
//! `[index_arcs: u8][companions: u8][VarBind]([index_arcs: u8][VarBind])*`:
//! the number of index arcs at the end of the instance OID, the number of
//! companion records after the instance, the instance's BER `VarBind`
//! SEQUENCE { OID, value }, then one companion record per label column the
//! series serves: its index-arc count and its `VarBind` { OID, OCTET STRING
//! holding the label's value }. The instance OID is rendered from the
//! metric's [`OidTemplate`] the first time a series is seen and cached as its
//! encoded OBJECT IDENTIFIER, with its companion records; later events copy
//! those bytes and encode only the value. Only the series' first record that
//! is written carries its companions; every other record carries none. A
//! series is its metric name and the labels its template can see, so events
//! that differ only in the agent label or a `drop_labels` label share one
//! entry. A series whose OID cannot be rendered is cached with the reason,
//! and its later events are rejected without rendering again.
//!
//! [`split_record`] is the one reader of the format.
//!
//! Requires the `snmp` feature flag.

// The decoder half (`ber::Reader`, `ber::Message`, the OID body decoder) has
// no caller in the crate outside tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod ber;
#[cfg_attr(not(test), allow(dead_code))]
pub mod oid;
pub(crate) mod v3;
pub mod value;

pub use value::SnmpValueType;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::model::metric::{
    is_valid_label_key, is_valid_metric_name, Labels, MetricEvent, ValidatedMetricName,
};
use crate::{ConfigError, EncoderError, SondaError};

use super::Encoder;
use ber::{BerError, Reader, Tag};
use oid::{Oid, OidTemplate};

/// Default label that names the device: excluded from the coverage rule and
/// never part of an OID.
pub const DEFAULT_AGENT_LABEL: &str = "device";

/// Length of a publish record's header before the instance's `VarBind`: its
/// index-arc count, then the number of companion records after it.
pub(crate) const RECORD_HEADER: usize = 2;

/// Configuration for [`SnmpEncoder`]; the body of `encoder: { type: snmp, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `oid:` is an error
/// rather than a silent fallback to the defaults.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SnmpEncoderConfig {
    /// Label that names the device. Default `"device"`. It needs no
    /// placeholder and no `drop_labels` entry, and never reaches the OID.
    #[cfg_attr(
        feature = "config",
        serde(
            default = "default_agent_label",
            deserialize_with = "crate::config::scalar_text::required"
        )
    )]
    pub agent_label: String,
    /// OID template per metric name, such as
    /// `"1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"`: the object's OID, then its
    /// index placeholders, `{label}` for one arc or `{label:str}` for a
    /// string index, or `.0` for a scalar. Every metric the entry emits
    /// needs one. Each is a string; a YAML number is an error.
    #[cfg_attr(feature = "config", serde(default, deserialize_with = "oid_templates"))]
    #[cfg_attr(feature = "schema", schemars(extend("x-sonda-no-widening" = true)))]
    pub oids: HashMap<String, String>,
    /// OID template per label whose value the agent serves as an OCTET
    /// STRING, such as `ifName: "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"`: a
    /// column beside the metrics' columns, one instance per index. A
    /// template uses only placeholders every `oids:` template uses. A label
    /// named here needs no placeholder and no `drop_labels` entry, and
    /// validation requires it to be a static label of the entry; a series
    /// without the label serves no instance of its column. Each is a string;
    /// a YAML number is an error.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "label_column_templates")
    )]
    #[cfg_attr(feature = "schema", schemars(extend("x-sonda-no-widening" = true)))]
    pub label_columns: HashMap<String, String>,
    /// Value type per metric name; default `gauge32`.
    #[cfg_attr(feature = "config", serde(default))]
    pub types: HashMap<String, SnmpValueType>,
    /// Labels deliberately left out of the OID.
    ///
    /// Validation requires every label key of an entry to be a placeholder
    /// of its template, the `agent_label`, or listed here; series that
    /// differ only in an unlisted, unreferenced label would otherwise share
    /// one OID.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "scalar_text_items")
    )]
    pub drop_labels: Vec<String>,
}

fn default_agent_label() -> String {
    DEFAULT_AGENT_LABEL.to_string()
}

/// `deserialize_with` for `oids:`: each template is a string
/// ([`OidText`](crate::config::scalar_text::OidText)), and an error names
/// its `oids.<metric>` field.
#[cfg(feature = "config")]
fn oid_templates<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error> {
    deserializer.deserialize_map(Templates {
        field: "oids",
        keys: "metric names",
    })
}

/// `deserialize_with` for `label_columns:`: each template is a string, and
/// an error names its `label_columns.<label>` field.
#[cfg(feature = "config")]
fn label_column_templates<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error> {
    deserializer.deserialize_map(Templates {
        field: "label_columns",
        keys: "labels",
    })
}

/// A map of OID templates read as strings
/// ([`OidText`](crate::config::scalar_text::OidText)), each error naming
/// `<field>.<key>`.
#[cfg(feature = "config")]
struct Templates {
    field: &'static str,
    keys: &'static str,
}

#[cfg(feature = "config")]
impl<'de> serde::de::Visitor<'de> for Templates {
    type Value = HashMap<String, String>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a map from {} to OID templates", self.keys)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use crate::config::scalar_text::OidText;
        let mut templates = HashMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let field = format!("{}.{key}", self.field);
            let template = map.next_value_seed(OidText { field: &field })?;
            templates.insert(key, template);
        }
        Ok(templates)
    }
}

/// `deserialize_with` for a list of text items: each item may be any scalar.
#[cfg(feature = "config")]
fn scalar_text_items<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    use crate::config::scalar_text::ScalarText;
    let items: Vec<ScalarText> = serde::Deserialize::deserialize(deserializer)?;
    Ok(items.into_iter().map(|text| text.0).collect())
}

impl Default for SnmpEncoderConfig {
    /// Default agent label, no templates, no label columns, no types, no
    /// dropped labels.
    fn default() -> Self {
        Self {
            agent_label: default_agent_label(),
            oids: HashMap::new(),
            label_columns: HashMap::new(),
            types: HashMap::new(),
            drop_labels: Vec::new(),
        }
    }
}

/// The first of `keys` that `template` does not cover: not one of its
/// placeholders, not a `label_columns` label, not the `agent_label`, and not
/// listed in `drop_labels`. `None` when every key is covered.
///
/// Validation and [`Encoder::encode_metric`] both apply this one rule, so a
/// configuration that validates never has an event rejected by it.
pub(crate) fn uncovered_label<'a>(
    cfg: &SnmpEncoderConfig,
    template: &OidTemplate,
    mut keys: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    keys.find(|key| {
        *key != cfg.agent_label
            && !cfg.drop_labels.iter().any(|dropped| dropped == key)
            && !cfg.label_columns.contains_key(*key)
            && !template.placeholders().any(|p| p == *key)
    })
}

/// A publish record, split by [`split_record`].
#[derive(Debug, Clone)]
pub(crate) struct Record<'a> {
    /// How many arcs at the end of the instance OID are its index.
    pub(crate) index_arcs: u8,
    /// The instance's `VarBind`.
    pub(crate) varbind: &'a [u8],
    /// The companion records after the instance's `VarBind`, in order.
    pub(crate) companions: Companions<'a>,
}

/// One companion record of a [`Record`]: a label column's instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Companion<'a> {
    /// How many arcs at the end of the column instance's OID are its index.
    pub(crate) index_arcs: u8,
    /// The column instance's `VarBind`: its OID and the label's value as an
    /// OCTET STRING.
    pub(crate) varbind: &'a [u8],
}

/// The companion records of a [`Record`], which [`split_record`] has
/// checked.
#[derive(Debug, Clone)]
pub(crate) struct Companions<'a> {
    rest: &'a [u8],
    left: u8,
}

impl<'a> Iterator for Companions<'a> {
    type Item = Companion<'a>;

    fn next(&mut self) -> Option<Companion<'a>> {
        if self.left == 0 {
            return None;
        }
        let (&index_arcs, rest) = self.rest.split_first()?;
        let (varbind, rest) = split_varbind(rest).ok()?;
        self.rest = rest;
        self.left -= 1;
        Some(Companion {
            index_arcs,
            varbind,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (usize::from(self.left), Some(usize::from(self.left)))
    }
}

impl ExactSizeIterator for Companions<'_> {}

/// The SEQUENCE TLV at the start of `input`, and the bytes after it.
fn split_varbind(input: &[u8]) -> Result<(&[u8], &[u8]), BerError> {
    let body = Reader::new(input).expect(Tag::Sequence)?;
    // `body` borrows from `input`, so the TLV ends where it does.
    let end = body.as_ptr() as usize - input.as_ptr() as usize + body.len();
    Ok(input.split_at(end))
}

/// [`SondaError::Sink`]: the bytes are not a publish record, for `detail`.
fn not_a_record(detail: impl fmt::Display) -> SondaError {
    SondaError::Sink(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the event is not an snmp publish record ({detail}); the snmp_agent sink takes \
             the snmp encoder's output"
        ),
    ))
}

/// Split the publish record `encoded`: the index-arc count, the companion
/// count, one SEQUENCE, then exactly that many companion records, each a
/// byte and one SEQUENCE, and nothing after them.
///
/// Returns [`SondaError::Sink`] for bytes of any other shape. Allocates
/// nothing unless it returns an error.
pub(crate) fn split_record(encoded: &[u8]) -> Result<Record<'_>, SondaError> {
    let (&index_arcs, rest) = encoded
        .split_first()
        .ok_or_else(|| not_a_record("it is empty"))?;
    let (&count, rest) = rest
        .split_first()
        .ok_or_else(|| not_a_record("it has no companion count"))?;
    let (varbind, companions) = split_varbind(rest).map_err(not_a_record)?;
    let mut rest = companions;
    for _ in 0..count {
        let (_, after) = rest.split_first().ok_or_else(|| {
            not_a_record(format!(
                "it announces {count} companion records and carries fewer"
            ))
        })?;
        rest = split_varbind(after).map_err(not_a_record)?.1;
    }
    if !rest.is_empty() {
        return Err(not_a_record("bytes follow its last VarBind"));
    }
    Ok(Record {
        index_arcs,
        varbind,
        companions: Companions {
            rest: companions,
            left: count,
        },
    })
}

/// Encodes metric events as SNMP publish records.
///
/// Each call to [`Encoder::encode_metric`] appends one record:
/// `[index_arcs][companions][VarBind]([index_arcs][VarBind])*`. The first
/// byte is the number of index arcs the instance OID was rendered with, the
/// second the number of companion records that follow the instance's
/// `VarBind` SEQUENCE { OBJECT IDENTIFIER, value }. A companion record is a
/// label column's instance: its index-arc count, then a `VarBind` { the
/// column's template rendered for the event, OCTET STRING holding the
/// label's value }, one per `label_columns` label the event carries, in
/// label order. A series' companions ride its first record that is written;
/// every other record has none. Log events are not supported.
pub struct SnmpEncoder {
    cfg: SnmpEncoderConfig,
    templates: HashMap<String, Template>,
    /// The label columns, in label order.
    columns: Vec<LabelColumn>,
    /// Each series' encoded OBJECT IDENTIFIER or rejection, filled on first
    /// sight, keyed by the series' [fingerprint](Template::fingerprint).
    oids: RwLock<HashMap<u64, Vec<CachedOid>>>,
    /// OIDs rendered, for tests.
    #[cfg(test)]
    renders: std::sync::atomic::AtomicUsize,
}

/// A label column: the label whose value the agent serves, and the template
/// of its instances.
struct LabelColumn {
    label: String,
    template: OidTemplate,
}

/// A metric's template, its value type, and the labels the template cannot
/// see: the `agent_label` and the `drop_labels` labels it does not reference.
struct Template {
    template: OidTemplate,
    ty: SnmpValueType,
    unseen: Vec<String>,
}

impl Template {
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
/// rendered instance or the text it is rejected with.
struct CachedOid {
    name: ValidatedMetricName,
    labels: Arc<Labels>,
    oid: Result<Arc<Rendered>, String>,
}

/// A series' instance, rendered on first sight.
struct Rendered {
    /// The instance's encoded OBJECT IDENTIFIER.
    oid: Box<[u8]>,
    /// How many arcs at the end of the instance OID are its index, as
    /// rendered.
    index_arcs: u8,
    /// The series' companion records, one per label column it serves.
    companions: Box<[u8]>,
    /// How many records `companions` holds.
    companion_count: u8,
    /// Set until a record carrying the companions has been written.
    companions_pending: AtomicBool,
}

impl Rendered {
    /// Whether the record being written carries the companions: `true` once,
    /// for the first caller after they became pending, and never when there
    /// are none.
    fn take_companions(&self) -> bool {
        self.companions_pending.load(Ordering::Relaxed)
            && self
                .companions_pending
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
    }
}

impl CachedOid {
    /// Whether an event named `name` with `labels` is this entry's series
    /// under `template`.
    fn is_series(&self, template: &Template, name: &str, labels: &Labels) -> bool {
        *self.name == *name && template.seen(&self.labels).eq(template.seen(labels))
    }

    /// The cached instance, or the cached rejection as an
    /// [`EncoderError::EventRejected`].
    fn oid(&self) -> Result<Arc<Rendered>, SondaError> {
        self.oid
            .clone()
            .map_err(|reason| SondaError::Encoder(EncoderError::EventRejected(reason)))
    }
}

impl SnmpEncoder {
    /// Build an encoder, parsing every template in `cfg`.
    ///
    /// Returns [`SondaError::Config`] when an `oids:` or `types:` key is not
    /// a valid metric name, a `label_columns:` key is not a valid label key,
    /// an `oids:` or `label_columns:` entry is not a valid [`OidTemplate`],
    /// or there are more than 255 label columns. Every map is checked in key
    /// order, so the same config always reports the same first error.
    pub fn new(cfg: &SnmpEncoderConfig) -> Result<Self, SondaError> {
        let check_key = |field: &str, metric: &str| {
            if is_valid_metric_name(metric) {
                Ok(())
            } else {
                Err(SondaError::Config(ConfigError::invalid(format!(
                    "snmp encoder {field} key {metric:?} is not a metric name; keys are \
                     metric names as the scenario writes them"
                ))))
            }
        };
        for metric in cfg.types.keys().collect::<std::collections::BTreeSet<_>>() {
            check_key("types", metric)?;
        }
        let templates = cfg
            .oids
            .iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(metric, text)| {
                check_key("oids", metric)?;
                let template = OidTemplate::parse(text).map_err(|e| {
                    SondaError::Config(ConfigError::invalid(format!(
                        "snmp encoder oids.{metric}: {e}"
                    )))
                })?;
                let referenced = |label: &str| template.placeholders().any(|p| p == label);
                let unseen = std::iter::once(cfg.agent_label.as_str())
                    .chain(cfg.drop_labels.iter().map(String::as_str))
                    .filter(|label| !referenced(label))
                    .map(str::to_string)
                    .collect();
                let ty = cfg.types.get(metric).cloned().unwrap_or_default();
                Ok((
                    metric.clone(),
                    Template {
                        template,
                        ty,
                        unseen,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, SondaError>>()?;
        let columns = cfg
            .label_columns
            .iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(label, text)| {
                if !is_valid_label_key(label) {
                    return Err(SondaError::Config(ConfigError::invalid(format!(
                        "snmp encoder label_columns key {label:?} is not a label key; keys \
                         are the labels whose values the agent serves"
                    ))));
                }
                let template = OidTemplate::parse(text).map_err(|e| {
                    SondaError::Config(ConfigError::invalid(format!(
                        "snmp encoder label_columns.{label}: {e}"
                    )))
                })?;
                Ok(LabelColumn {
                    label: label.clone(),
                    template,
                })
            })
            .collect::<Result<Vec<_>, SondaError>>()?;
        if u8::try_from(columns.len()).is_err() {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "snmp encoder has {} label_columns; a record carries at most 255",
                columns.len()
            ))));
        }

        Ok(Self {
            cfg: cfg.clone(),
            templates,
            columns,
            oids: RwLock::new(HashMap::new()),
            #[cfg(test)]
            renders: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// The template of `metric`. Returns [`SondaError::Config`] when `oids:`
    /// has no entry for it.
    pub(crate) fn template_for(&self, metric: &str) -> Result<&OidTemplate, SondaError> {
        self.template_of(metric).map(|t| &t.template)
    }

    /// [`template_for`](Self::template_for), with the value type and the
    /// labels it cannot see.
    fn template_of(&self, metric: &str) -> Result<&Template, SondaError> {
        self.templates.get(metric).ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "snmp encoder has no OID template for metric {metric:?}: set \
                 encoder.oids.{metric}"
            )))
        })
    }

    /// The event's series' instance under `template`, rendering and caching
    /// it on first sight.
    ///
    /// A hit allocates nothing: the entry is found by a hash of the name and
    /// the labels the template sees, then compared label by label. A series
    /// whose OID is rejected ([`EncoderError::EventRejected`]: a label the
    /// template does not cover, a placeholder whose label the event lacks,
    /// a label value that is not an index of its placeholder, or a label
    /// column that cannot be rendered) is cached with the rejection's text,
    /// and each later event of it gets that text without the OID being
    /// rendered again.
    fn rendered(
        &self,
        template: &Template,
        event: &MetricEvent,
    ) -> Result<Arc<Rendered>, SondaError> {
        let name: &str = &event.name;
        let fingerprint = template.fingerprint(name, &event.labels);
        let cached = |oids: &HashMap<u64, Vec<CachedOid>>| {
            oids.get(&fingerprint).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.is_series(template, name, &event.labels))
                    .map(CachedOid::oid)
            })
        };
        if let Some(oid) = cached(&self.oids.read().unwrap_or_else(PoisonError::into_inner)) {
            return oid;
        }

        let oid = self.render_oid(template, event);
        let mut oids = self.oids.write().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have cached the series since the read.
        if let Some(oid) = cached(&oids) {
            return oid;
        }
        let entry = CachedOid {
            name: event.name.clone(),
            labels: Arc::clone(&event.labels),
            oid,
        };
        let oid = entry.oid();
        oids.entry(fingerprint).or_default().push(entry);
        oid
    }

    /// The event's instance under `template`, with its OID BER-encoded and
    /// its companion records, or the text it is rejected with: the event
    /// carries a label the template does not cover, or a render fails.
    fn render_oid(
        &self,
        template: &Template,
        event: &MetricEvent,
    ) -> Result<Arc<Rendered>, String> {
        #[cfg(test)]
        self.renders.fetch_add(1, Ordering::Relaxed);
        let name: &str = &event.name;
        let template = &template.template;
        if let Some(label) =
            uncovered_label(&self.cfg, template, event.labels.iter().map(|(k, _)| k))
        {
            return Err(format!(
                "metric {name:?} carries label `{label}`, which the snmp OID template does \
                 not reference; reference it as an index placeholder, make it the \
                 agent_label, list it in label_columns, or list it in drop_labels"
            ));
        }
        let (oid, index_arcs) = render_instance(template, &event.labels)
            .map_err(|e| format!("metric {name:?}: {e}"))?;
        let mut companions = Vec::new();
        let mut companion_count = 0u8;
        for column in &self.columns {
            let Some(value) = label_value(&event.labels, &column.label) else {
                continue;
            };
            let (column_oid, column_arcs) = render_instance(&column.template, &event.labels)
                .map_err(|e| format!("metric {name:?}: label column {}: {e}", column.label))?;
            companions.push(column_arcs);
            ber::write_tlv(&mut companions, Tag::Sequence, |out| {
                ber::write_oid(out, &column_oid);
                ber::write_string(out, value.as_bytes());
            });
            // new() caps the label columns at 255.
            companion_count += 1;
        }
        let mut bytes = Vec::with_capacity(2 + oid.ber_body_len());
        ber::write_oid(&mut bytes, &oid);
        Ok(Arc::new(Rendered {
            oid: bytes.into(),
            index_arcs,
            companions: companions.into(),
            companion_count,
            companions_pending: AtomicBool::new(companion_count > 0),
        }))
    }

    /// How many series the OID cache holds.
    #[cfg(test)]
    fn cached_series(&self) -> usize {
        self.oids
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Vec::len)
            .sum()
    }
}

impl Encoder for SnmpEncoder {
    /// Appends exactly one publish record into `buf`: the number of index
    /// arcs the instance OID was rendered with, the number of companion
    /// records, a BER `VarBind` SEQUENCE { OBJECT IDENTIFIER, value }, then
    /// the companion records (see [`SnmpEncoder`]), which only the series'
    /// first written record carries. The output is not a bare `VarBind` and
    /// not an SNMP message. `buf` is cleared by the caller, not here.
    ///
    /// Returns [`SondaError::Config`] when the metric has no `oids:` entry,
    /// which is a defect of the configuration rather than of this event.
    /// Returns [`EncoderError::EventRejected`], and writes nothing, when this
    /// event does not fit the configuration:
    ///
    /// - the event carries a label the template does not reference that is
    ///   neither the `agent_label` nor listed in `drop_labels`;
    /// - a template placeholder names a label the event lacks;
    /// - a `{label}` placeholder's label value is not a canonical decimal
    ///   `u32`, or a `{label:str}` placeholder's is empty or would make the
    ///   OID longer than 128 arcs;
    /// - a label column the event carries cannot be rendered, for the same
    ///   reasons;
    /// - the value is NaN or infinite and the type is not `string`;
    /// - the rounded value is outside the type's range, or an
    ///   `opaque_float` value is beyond the `f32` range;
    /// - the type is an enum map with no entry for the rounded value.
    ///
    /// The label checks run only when a series is first seen and its OID is
    /// rendered and cached; later events for the same series skip them. A
    /// series rejected by them is rejected with the same text on every later
    /// event without being rendered again. A record rejected for its value
    /// carries no companions, and the series' next written record does.
    ///
    /// Validation pairs this encoder with the `snmp_agent` sink only.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let name: &str = &event.name;
        let template = self.template_of(name)?;
        let rendered = self.rendered(template, event)?;
        let companions = rendered.take_companions();
        let start = buf.len();
        // The header, a SEQUENCE header, the OID, the longest fixed-size
        // value TLV (an Opaque double: tag, length, eleven octets), and the
        // companions.
        let companion_len = if companions {
            rendered.companions.len()
        } else {
            0
        };
        buf.reserve(RECORD_HEADER + 2 + rendered.oid.len() + 13 + companion_len);
        buf.push(rendered.index_arcs);
        buf.push(if companions {
            rendered.companion_count
        } else {
            0
        });
        let mut written = Ok(());
        ber::write_tlv(buf, Tag::Sequence, |out| {
            out.extend_from_slice(&rendered.oid);
            written = template.ty.write(event.value, out);
        });
        if let Err(e) = written {
            buf.truncate(start);
            if companions {
                rendered.companions_pending.store(true, Ordering::Relaxed);
            }
            return Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                "metric {name:?}: {e}"
            ))));
        }
        if companions {
            buf.extend_from_slice(&rendered.companions);
        }
        Ok(())
    }
}

/// The value of `label` in `labels`, if it is there.
fn label_value<'a>(labels: &'a Labels, label: &str) -> Option<&'a str> {
    labels
        .iter()
        .find_map(|(key, value)| (key == label).then_some(value))
}

/// `template` rendered for `labels`: the instance OID and how many of its
/// arcs are the index.
fn render_instance(template: &OidTemplate, labels: &Labels) -> Result<(Oid, u8), String> {
    let mut arcs = Vec::new();
    template
        .render(labels, &mut arcs)
        .map_err(|e| e.to_string())?;
    // render() caps the OID at 128 arcs and the object has at least two.
    let index_arcs = (arcs.len() - template.object().arcs().len()) as u8;
    let oid = Oid::from_arcs(arcs).ok_or("the rendered OID cannot be encoded")?;
    Ok((oid, index_arcs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, UNIX_EPOCH};

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digits"))
            .collect()
    }

    fn encoder_of(name: &str, template: &str, ty: SnmpValueType) -> SnmpEncoder {
        SnmpEncoder::new(&SnmpEncoderConfig {
            oids: HashMap::from([(name.to_string(), template.to_string())]),
            types: HashMap::from([(name.to_string(), ty)]),
            ..SnmpEncoderConfig::default()
        })
        .expect("a valid encoder")
    }

    fn event(name: &str, value: f64, pairs: &[(&str, &str)]) -> MetricEvent {
        MetricEvent::with_timestamp(
            name.to_string(),
            value,
            Labels::from_pairs(pairs).expect("valid labels"),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .expect("valid metric name")
    }

    fn record(encoder: &SnmpEncoder, event: &MetricEvent) -> Vec<u8> {
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    fn rejection(encoder: &SnmpEncoder, event: &MetricEvent) -> String {
        let mut buf = Vec::new();
        match encoder.encode_metric(event, &mut buf) {
            Err(SondaError::Encoder(EncoderError::EventRejected(reason))) => {
                assert!(buf.is_empty(), "a rejected event writes nothing");
                reason
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    /// Each expected `VarBind` is net-snmp 5.9.4's encoding of the same OID
    /// and value, from `snmptrap -v2c -c public 127.0.0.1:<port> 0
    /// 1.3.6.1.4.1.8072.9999.0.2` with the varbinds listed on the
    /// `NET_SNMP_TRAP_TYPES` fixture in `ber.rs` (the type letter is the
    /// last-but-one column of each case's name).
    #[rustfmt::skip]
    #[rstest]
    #[case::integer_one_index(    "1.3.6.1.2.1.2.2.1.8.{ifIndex}",       &[("ifIndex", "4")],          SnmpValueType::Integer,    2.0,              1, "300f060a2b060102010202010804020102")]
    #[case::counter64_one_index(  "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}",    &[("ifIndex", "3")],          SnmpValueType::Counter64,  1_250_000.0,      1, "3012060b2b060102011f010101060346031312d0")]
    #[case::counter32_one_index(  "1.3.6.1.2.1.2.2.1.14.{ifIndex}",      &[("ifIndex", "2")],          SnmpValueType::Counter32,  7.0,              1, "300f060a2b060102010202010e02410107")]
    #[case::gauge32_one_index(    "1.3.6.1.2.1.31.1.1.1.15.{ifIndex}",   &[("ifIndex", "2")],          SnmpValueType::Gauge32,    10_000.0,         1, "3011060b2b060102011f0101010f0242022710")]
    #[case::unsigned32_one_index( "1.3.6.1.2.1.31.1.1.1.15.{ifIndex}",   &[("ifIndex", "2")],          SnmpValueType::Unsigned32, 10_000.0,         1, "3011060b2b060102011f0101010f0242022710")]
    #[case::timeticks_scalar(     "1.3.6.1.4.1.8072.9999.1.10.0",        &[],                          SnmpValueType::TimeTicks,  4_294_967_295.0,  1, "3015060c2b06010401bf08ce0f010a00430500ffffffff")]
    #[case::string_scalar(        "1.3.6.1.4.1.8072.9999.1.20.0",        &[],                          SnmpValueType::String,     12.5,             1, "3014060c2b06010401bf08ce0f011400040431322e35")]
    #[case::integer_two_index(    "1.3.6.1.4.1.8072.9999.2.1.{a}.{b}",   &[("a", "7"), ("b", "3")],    SnmpValueType::Integer,    -129.0,           2, "3013060d2b06010401bf08ce0f020107030202ff7f")]
    #[case::integer_i32_min(      "1.3.6.1.4.1.8072.9999.1.4.0",         &[],                          SnmpValueType::Integer,    -2_147_483_648.0, 1, "3014060c2b06010401bf08ce0f010400020480000000")]
    fn record_is_index_arcs_then_the_varbind_net_snmp_encodes(
        #[case] template: &str,
        #[case] pairs: &[(&str, &str)],
        #[case] ty: SnmpValueType,
        #[case] value: f64,
        #[case] index_arcs: u8,
        #[case] varbind: &str,
    ) {
        let expected = hex(varbind);
        assert_eq!(expected.first(), Some(&0x30), "fixture present");
        let encoder = encoder_of("m", template, ty);
        let mut pairs = pairs.to_vec();
        pairs.push(("device", "rtr-1"));
        let out = record(&encoder, &event("m", value, &pairs));
        assert_eq!(out[0], index_arcs);
        assert_eq!(out[0], encoder.template_for("m").expect("template").index_arcs());
        assert_eq!(out[1], 0, "no label columns, no companions");
        assert_eq!(&out[RECORD_HEADER..], &expected[..]);
    }

    /// The record's `VarBind` for an Opaque float, an Opaque double, an
    /// IpAddress and an enum string is, byte for byte, the `VarBind` net-snmp
    /// 5.9.4 sent for the same OID and value (`snmpset` with `F`, `D`, `a`
    /// and `s`: the `NET_SNMP_SET_TYPES` fixture in `ber.rs`).
    #[rustfmt::skip]
    #[rstest]
    #[case::opaque_float(      "1.3.6.1.4.1.2021.10.1.6.{laIndex}", &[("laIndex", "1")], SnmpValueType::OpaqueFloat,  0.15,            "1.3.6.1.4.1.2021.10.1.6.1")]
    #[case::opaque_double(     "1.3.6.1.4.1.8072.9999.1.1.0",       &[],                 SnmpValueType::OpaqueDouble, 0.15,            "1.3.6.1.4.1.8072.9999.1.1.0")]
    #[case::ipaddress(         "1.3.6.1.4.1.8072.9999.1.2.0",       &[],                 SnmpValueType::IpAddress,    167_772_161.0,   "1.3.6.1.4.1.8072.9999.1.2.0")]
    #[case::ipaddress_u32_max( "1.3.6.1.4.1.8072.9999.1.5.0",       &[],                 SnmpValueType::IpAddress,    4_294_967_295.0, "1.3.6.1.4.1.8072.9999.1.5.0")]
    #[case::enum_string(       "1.3.6.1.4.1.8072.9999.1.3.0",       &[],                 SnmpValueType::Enum(BTreeMap::from([(1, "up".to_string()), (2, "down".to_string())])), 1.0, "1.3.6.1.4.1.8072.9999.1.3.0")]
    fn record_varbind_is_the_one_net_snmp_sent(
        #[case] template: &str,
        #[case] pairs: &[(&str, &str)],
        #[case] ty: SnmpValueType,
        #[case] value: f64,
        #[case] oid: &str,
    ) {
        let expected = ber::tests::captured_varbind(ber::tests::NET_SNMP_SET_TYPES, oid);
        assert!(expected.len() > 16, "the fixture carries {oid}");
        let encoder = encoder_of("m", template, ty);
        let out = record(&encoder, &event("m", value, pairs));
        assert_eq!(&out[..RECORD_HEADER], &[1, 0], "one index arc, no companions");
        assert_eq!(&out[RECORD_HEADER..], &expected[..]);
    }

    /// `snmptrap -v2c -c public 127.0.0.1:<port> 0 1.3.6.1.4.1.8072.9999.0.2`
    /// with `1.3.6.1.2.1.31.1.1.1.6.1 C 1250000`,
    /// `1.3.6.1.2.1.31.1.1.1.1.1 s GigabitEthernet0/0/0`,
    /// `1.3.6.1.2.1.31.1.1.1.18.1 s "uplink to core"` and
    /// `1.3.6.1.4.1.8072.9999.3.1.20.71.105.103.97.98.105.116.69.116.104.101.114.110.101.116.48.47.48.47.48 i 2`
    /// as varbinds, net-snmp 5.9.4: each `VarBind` as net-snmp encodes it.
    const NET_SNMP_IF_HC_IN_OCTETS_1: &str = "3012060b2b060102011f010101060146031312d0";
    const NET_SNMP_IF_NAME_1: &str =
        "3023060b2b060102011f010101010104144769676162697445746865726e6574302f302f30";
    const NET_SNMP_IF_ALIAS_1: &str =
        "301d060b2b060102011f0101011201040e75706c696e6b20746f20636f7265";
    const NET_SNMP_STRING_INDEX: &str =
        "302506202b06010401bf08ce0f0301144769676162697445746865726e6574302f302f30020102";

    /// ifHCInOctets with ifName and ifAlias served as columns.
    fn column_encoder() -> SnmpEncoder {
        column_encoder_of(SnmpValueType::Counter64)
    }

    /// [`column_encoder`] with ifHCInOctets encoded as `ty`.
    fn column_encoder_of(ty: SnmpValueType) -> SnmpEncoder {
        SnmpEncoder::new(&SnmpEncoderConfig {
            oids: HashMap::from([(
                "ifHCInOctets".to_string(),
                "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
            )]),
            types: HashMap::from([("ifHCInOctets".to_string(), ty)]),
            label_columns: HashMap::from([
                (
                    "ifName".to_string(),
                    "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}".to_string(),
                ),
                (
                    "ifAlias".to_string(),
                    "1.3.6.1.2.1.31.1.1.1.18.{ifIndex}".to_string(),
                ),
            ]),
            drop_labels: vec!["job".to_string()],
            ..SnmpEncoderConfig::default()
        })
        .expect("a valid encoder")
    }

    fn interface_one(value: f64) -> MetricEvent {
        event(
            "ifHCInOctets",
            value,
            &[
                ("device", "rtr-1"),
                ("ifIndex", "1"),
                ("ifName", "GigabitEthernet0/0/0"),
                ("ifAlias", "uplink to core"),
                ("job", "snmp"),
            ],
        )
    }

    /// A series' first record carries one companion per label column, in
    /// label order, each its index-arc count and the `VarBind` net-snmp
    /// writes for the label's value; its second record carries none.
    #[test]
    fn the_first_record_of_a_series_carries_its_label_columns() {
        let (instance, if_alias, if_name) = (
            hex(NET_SNMP_IF_HC_IN_OCTETS_1),
            hex(NET_SNMP_IF_ALIAS_1),
            hex(NET_SNMP_IF_NAME_1),
        );
        for fixture in [&instance, &if_alias, &if_name] {
            assert_eq!(fixture.first(), Some(&0x30), "fixture present");
        }
        let encoder = column_encoder();
        let first = record(&encoder, &interface_one(1_250_000.0));
        assert_eq!(first[0], 1, "the instance's rendered index arcs");
        assert_eq!(first[1], 2, "two companions");
        let mut expected = vec![1, 2];
        expected.extend_from_slice(&instance);
        expected.push(1);
        expected.extend_from_slice(&if_alias);
        expected.push(1);
        expected.extend_from_slice(&if_name);
        assert_eq!(first, expected);

        let second = record(&encoder, &interface_one(1_250_000.0));
        assert_eq!(second[1], 0, "a later record carries no companions");
        let mut expected = vec![1, 0];
        expected.extend_from_slice(&instance);
        assert_eq!(second, expected);

        let split = split_record(&first).expect("a record");
        assert_eq!(split.varbind, &instance[..]);
        assert_eq!(
            split.companions.collect::<Vec<_>>(),
            [
                Companion {
                    index_arcs: 1,
                    varbind: &if_alias[..]
                },
                Companion {
                    index_arcs: 1,
                    varbind: &if_name[..]
                },
            ]
        );
    }

    #[test]
    fn record_snapshot_of_a_first_sight_with_label_columns() {
        let out = record(&column_encoder(), &interface_one(1_250_000.0));
        assert_eq!(out[0], 1, "the instance's rendered index arcs");
        let shown: Vec<String> = out.iter().map(|b| format!("{b:02x}")).collect();
        insta::assert_snapshot!("snmp_record_first_sight_label_columns", shown.join(" "));
    }

    /// A record's first byte is the index arcs the OID was rendered with: a
    /// 20-byte string index is 21 arcs, which the template alone cannot
    /// say.
    #[test]
    fn a_string_index_record_carries_its_rendered_index_arcs() {
        let expected = hex(NET_SNMP_STRING_INDEX);
        assert_eq!(expected.first(), Some(&0x30), "fixture present");
        let encoder = encoder_of(
            "m",
            "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}",
            SnmpValueType::Integer,
        );
        let out = record(
            &encoder,
            &event("m", 2.0, &[("ifName", "GigabitEthernet0/0/0")]),
        );
        assert_eq!(out[0], 21);
        assert_eq!(
            encoder.template_for("m").expect("template").index_arcs(),
            2,
            "the fewest"
        );
        assert_eq!(out[1], 0);
        assert_eq!(&out[RECORD_HEADER..], &expected[..]);
    }

    #[test]
    fn an_empty_string_index_rejects_the_series() {
        let encoder = encoder_of(
            "m",
            "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}",
            SnmpValueType::Integer,
        );
        assert!(!record(&encoder, &event("m", 2.0, &[("ifName", "x")])).is_empty());
        let reason = rejection(&encoder, &event("m", 2.0, &[("ifName", "")]));
        assert!(reason.contains("label `ifName` is empty"), "{reason}");
    }

    /// The companions ride the series' first record that is written: a first
    /// event refused for its value carries none away with it.
    #[rstest]
    #[case::negative_counter64(SnmpValueType::Counter64, -1.0, "value -1 is outside the counter64 range")]
    #[case::enum_without_an_entry(
        SnmpValueType::Enum(BTreeMap::from([(1, "up".to_string()), (2, "down".to_string())])),
        3.0,
        "value 3 rounds to 3, which has no entry in the enum map"
    )]
    fn companions_wait_for_a_written_record(
        #[case] ty: SnmpValueType,
        #[case] refused: f64,
        #[case] reason: &str,
    ) {
        let encoder = column_encoder_of(ty);
        let mut buf = Vec::new();
        let err = encoder
            .encode_metric(&interface_one(refused), &mut buf)
            .expect_err("refused for its value");
        assert!(
            matches!(&err, SondaError::Encoder(EncoderError::EventRejected(text)) if text.contains(reason)),
            "{err}"
        );
        assert!(buf.is_empty());
        assert_eq!(encoder.cached_series(), 1, "the OID was cached");
        assert_eq!(record(&encoder, &interface_one(1.0))[1], 2);
        assert_eq!(record(&encoder, &interface_one(1.0))[1], 0);
    }

    /// A series without a column's label serves no instance of that column.
    #[test]
    fn a_series_without_the_label_serves_no_column() {
        let encoder = column_encoder();
        let out = record(
            &encoder,
            &event(
                "ifHCInOctets",
                1_250_000.0,
                &[("ifIndex", "1"), ("ifName", "GigabitEthernet0/0/0")],
            ),
        );
        let split = split_record(&out).expect("a record");
        let companions: Vec<_> = split.companions.collect();
        assert_eq!(companions.len(), 1);
        assert_eq!(companions[0].varbind, &hex(NET_SNMP_IF_NAME_1)[..]);
    }

    #[test]
    fn a_label_column_that_cannot_render_rejects_the_series() {
        let mut cfg = with(|_| {});
        cfg.label_columns.insert(
            "ifName".to_string(),
            "1.3.6.1.4.1.8072.9999.3.1.{slot}".to_string(),
        );
        let encoder = SnmpEncoder::new(&cfg).expect("valid");
        let reason = rejection(
            &encoder,
            &event("m", 1.0, &[("ifIndex", "1"), ("ifName", "Gi0")]),
        );
        assert!(reason.contains("label column ifName"), "{reason}");
        assert!(reason.contains("{slot}"), "{reason}");
    }

    /// Series that differ only in a label-column label are two series.
    #[test]
    fn each_label_value_is_its_own_series() {
        let encoder = column_encoder();
        for name in ["Gi0", "Gi1", "Gi0"] {
            let event = event("ifHCInOctets", 1.0, &[("ifIndex", "1"), ("ifName", name)]);
            record(&encoder, &event);
        }
        assert_eq!(encoder.cached_series(), 2);
    }

    /// One record per value type, from one event on a one-index column.
    #[rstest]
    fn record_snapshot_per_value_type(
        #[values(
            SnmpValueType::Integer,
            SnmpValueType::Counter32,
            SnmpValueType::Counter64,
            SnmpValueType::Gauge32,
            SnmpValueType::Unsigned32,
            SnmpValueType::TimeTicks,
            SnmpValueType::String
        )]
        ty: SnmpValueType,
    ) {
        let encoder = encoder_of(
            "ifHCInOctets",
            "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}",
            ty.clone(),
        );
        let out = record(
            &encoder,
            &event(
                "ifHCInOctets",
                1_250_000.0,
                &[("device", "rtr-1"), ("ifIndex", "3")],
            ),
        );
        assert_eq!(out[0], 1, "the template's index_arcs");
        let shown: Vec<String> = out.iter().map(|b| format!("{b:02x}")).collect();
        insta::assert_snapshot!(format!("snmp_record_{ty}"), shown.join(" "));
    }

    #[test]
    fn encode_appends_without_clearing_the_buffer() {
        let encoder = encoder_of("m", "1.3.6.1.4.1.8072.9999.1.1.0", SnmpValueType::Integer);
        let mut buf = vec![0xAA];
        encoder
            .encode_metric(&event("m", 1.0, &[]), &mut buf)
            .expect("encodes");
        assert_eq!(buf[0], 0xAA);
        assert_eq!(buf[1], 1, "a scalar's one index arc, its .0");
        assert_eq!(buf[2], 0, "no companions");
        assert_eq!(buf[3], 0x30);
    }

    #[test]
    fn a_metric_without_a_template_is_a_config_error() {
        let encoder = encoder_of("m", "1.3.6.1.4.1.8072.9999.1.1.0", SnmpValueType::Integer);
        let err = encoder
            .encode_metric(&event("other", 1.0, &[]), &mut Vec::new())
            .expect_err("no template");
        assert!(
            matches!(&err, SondaError::Config(_)) && err.to_string().contains("encoder.oids.other"),
            "{err}"
        );
    }

    #[test]
    fn the_default_type_is_gauge32() {
        let encoder = SnmpEncoder::new(&SnmpEncoderConfig {
            oids: HashMap::from([("m".to_string(), "1.3.6.1.4.1.8072.9999.1.1.0".to_string())]),
            ..SnmpEncoderConfig::default()
        })
        .expect("valid");
        let out = record(&encoder, &event("m", 7.0, &[]));
        assert_eq!(&out[out.len() - 3..], &[0x42, 0x01, 0x07]);
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::missing_placeholder_label( &[("device", "rtr-1")],                    "names a label the event does not carry")]
    #[case::not_an_arc(                &[("ifIndex", "eth0")],                    "label `ifIndex` has value \"eth0\"")]
    #[case::leading_zero(              &[("ifIndex", "01")],                      "label `ifIndex` has value \"01\"")]
    #[case::uncovered_label(           &[("ifIndex", "1"), ("job", "snmp")],      "carries label `job`")]
    fn a_series_that_does_not_fit_is_rejected(#[case] pairs: &[(&str, &str)], #[case] fragment: &str) {
        let encoder = encoder_of("m", "1.3.6.1.2.1.2.2.1.8.{ifIndex}", SnmpValueType::Integer);
        let reason = rejection(&encoder, &event("m", 1.0, pairs));
        assert!(reason.contains(fragment), "{reason}");
        assert!(reason.contains("\"m\""), "the rejection names the metric: {reason}");
    }

    #[test]
    fn the_agent_label_and_dropped_labels_need_no_placeholder() {
        let mut cfg = SnmpEncoderConfig {
            oids: HashMap::from([("m".to_string(), "1.3.6.1.2.1.2.2.1.8.{ifIndex}".to_string())]),
            drop_labels: vec!["job".to_string()],
            ..SnmpEncoderConfig::default()
        };
        let pairs = [("device", "rtr-1"), ("ifIndex", "1"), ("job", "snmp")];
        let encoder = SnmpEncoder::new(&cfg).expect("valid");
        assert_eq!(record(&encoder, &event("m", 1.0, &pairs))[0], 1);

        cfg.agent_label = "host".to_string();
        let encoder = SnmpEncoder::new(&cfg).expect("valid");
        let reason = rejection(&encoder, &event("m", 1.0, &pairs));
        assert!(reason.contains("carries label `device`"), "{reason}");
    }

    #[rstest]
    #[case::out_of_range(4_294_967_296.0, "outside the counter32 range")]
    #[case::negative(-1.0, "outside the counter32 range")]
    #[case::non_finite(f64::NAN, "cannot be encoded as counter32")]
    fn a_value_that_does_not_fit_is_rejected_and_not_cached(
        #[case] value: f64,
        #[case] fragment: &str,
    ) {
        let encoder = encoder_of(
            "m",
            "1.3.6.1.2.1.2.2.1.14.{ifIndex}",
            SnmpValueType::Counter32,
        );
        let pairs = [("ifIndex", "2")];
        let mut buf = vec![0xAA];
        let err = encoder
            .encode_metric(&event("m", value, &pairs), &mut buf)
            .expect_err("rejected");
        assert!(err.to_string().contains(fragment), "{err}");
        assert_eq!(buf, [0xAA], "nothing of the record is left in the buffer");
        // The series' OID was cached and serves the next, valid value.
        assert_eq!(encoder.cached_series(), 1);
        assert!(!record(&encoder, &event("m", 7.0, &pairs)).is_empty());
        assert_eq!(encoder.renders.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_oid_is_rendered_once_per_series() {
        let encoder = encoder_of(
            "m",
            "1.3.6.1.2.1.2.2.1.14.{ifIndex}",
            SnmpValueType::Counter32,
        );
        for i in 0..10 {
            for index in ["1", "2"] {
                record(
                    &encoder,
                    &event(
                        "m",
                        f64::from(i),
                        &[("device", "rtr-1"), ("ifIndex", index)],
                    ),
                );
            }
        }
        assert_eq!(encoder.cached_series(), 2);
        assert_eq!(encoder.renders.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn values_of_a_dropped_or_agent_label_share_one_entry() {
        let encoder = SnmpEncoder::new(&SnmpEncoderConfig {
            oids: HashMap::from([(
                "m".to_string(),
                "1.3.6.1.2.1.2.2.1.14.{ifIndex}".to_string(),
            )]),
            drop_labels: vec!["spike".to_string()],
            ..SnmpEncoderConfig::default()
        })
        .expect("valid");
        let first = record(&encoder, &event("m", 1.0, &[("ifIndex", "1")]));
        for i in 0..20 {
            let spike = format!("spike-{i}");
            let device = format!("rtr-{i}");
            let pairs = [
                ("ifIndex", "1"),
                ("spike", spike.as_str()),
                ("device", device.as_str()),
            ];
            assert_eq!(record(&encoder, &event("m", 1.0, &pairs)), first);
        }
        assert_eq!(encoder.cached_series(), 1);
        assert_eq!(encoder.renders.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_rejected_series_is_rendered_once() {
        let encoder = encoder_of(
            "m",
            "1.3.6.1.2.1.2.2.1.14.{ifIndex}",
            SnmpValueType::Counter32,
        );
        let bad = event("m", 1.0, &[("ifIndex", "eth0")]);
        let first = rejection(&encoder, &bad);
        for _ in 0..10 {
            assert_eq!(rejection(&encoder, &bad), first);
        }
        assert_eq!(encoder.cached_series(), 1);
        assert_eq!(encoder.renders.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn encode_log_is_not_supported() {
        let encoder = encoder_of("m", "1.3.6.1.4.1.8072.9999.1.1.0", SnmpValueType::Integer);
        let log = crate::model::log::LogEvent::new(
            crate::model::log::Severity::Info,
            "hello".to_string(),
            Labels::default(),
            std::collections::BTreeMap::new(),
        );
        let err = encoder
            .encode_log(&log, &mut Vec::new())
            .expect_err("logs are not encoded");
        assert!(matches!(
            err,
            SondaError::Encoder(EncoderError::NotSupported(_))
        ));
    }

    #[test]
    fn uncovered_label_is_the_coverage_rule() {
        let cfg = SnmpEncoderConfig {
            drop_labels: vec!["job".to_string()],
            ..SnmpEncoderConfig::default()
        };
        let template = OidTemplate::parse("1.3.6.1.2.1.2.2.1.8.{ifIndex}").expect("valid");
        let check = |cfg: &SnmpEncoderConfig, keys: &[&'static str]| {
            uncovered_label(cfg, &template, keys.iter().copied())
        };
        assert_eq!(check(&cfg, &["device", "ifIndex", "job"]), None);
        assert_eq!(
            check(&cfg, &["device", "ifName", "ifIndex"]),
            Some("ifName")
        );
        assert_eq!(check(&cfg, &[]), None);
        let mut columns = cfg.clone();
        columns.label_columns.insert(
            "ifName".to_string(),
            "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}".to_string(),
        );
        assert_eq!(
            check(&columns, &["device", "ifName", "ifIndex"]),
            None,
            "a label column is covered"
        );
    }

    fn with(change: impl FnOnce(&mut SnmpEncoderConfig)) -> SnmpEncoderConfig {
        let mut cfg = SnmpEncoderConfig {
            oids: HashMap::from([("m".to_string(), "1.3.6.1.2.1.2.2.1.8.{ifIndex}".to_string())]),
            ..SnmpEncoderConfig::default()
        };
        change(&mut cfg);
        cfg
    }

    #[rstest]
    #[case::bad_template(with(|c| { c.oids.insert("m".into(), "1.{x}".into()); }), "oids.m: OID template \"1.{x}\"")]
    #[case::unknown_placeholder_form(with(|c| { c.oids.insert("m".into(), "1.3.6.{ifName:x}".into()); }), "is not supported")]
    #[case::bad_label_column(with(|c| { c.label_columns.insert("ifName".into(), "1.3.6.1.2.1.31.1.1.1.1".into()); }), "label_columns.ifName: OID template")]
    #[case::label_column_key_not_a_label(with(|c| { c.label_columns.insert("if-name".into(), "1.3.6.{ifIndex}".into()); }), "label_columns key \"if-name\"")]
    #[case::oids_key_not_a_metric(with(|c| { c.oids.insert("in-octets".into(), "1.3.6.1".into()); }), "oids key \"in-octets\"")]
    #[case::types_key_not_a_metric(with(|c| { c.types.insert("in-octets".into(), SnmpValueType::Integer); }), "types key \"in-octets\"")]
    fn new_rejects(#[case] cfg: SnmpEncoderConfig, #[case] fragment: &str) {
        let err = SnmpEncoder::new(&cfg).err().expect("must not build");
        assert!(matches!(err, SondaError::Config(_)), "{err}");
        assert!(err.to_string().contains(fragment), "{err}");
    }

    #[cfg(feature = "config")]
    #[test]
    fn config_deserializes_with_defaults() {
        let cfg: SnmpEncoderConfig = serde_yaml_ng::from_str(
            "oids:\n  ifHCInOctets: \"1.3.6.1.2.1.31.1.1.1.6.{ifIndex}\"\ntypes:\n  ifHCInOctets: counter64\n",
        )
        .expect("parses");
        assert_eq!(cfg.agent_label, "device");
        assert_eq!(cfg.types["ifHCInOctets"], SnmpValueType::Counter64);
        assert!(cfg.drop_labels.is_empty());
        assert!(cfg.label_columns.is_empty());
        let columns: SnmpEncoderConfig = serde_yaml_ng::from_str(
            "label_columns:\n  ifName: \"1.3.6.1.2.1.31.1.1.1.1.{ifIndex}\"\n",
        )
        .expect("parses");
        assert_eq!(
            columns.label_columns["ifName"],
            "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"
        );
        let empty: SnmpEncoderConfig = serde_yaml_ng::from_str("{}").expect("all fields default");
        assert_eq!(empty, SnmpEncoderConfig::default());
    }

    /// Every value type parses as a `types:` entry, inside the
    /// `type:`-tagged block and outside it.
    #[cfg(feature = "config")]
    #[rstest]
    #[case::config_alone("")]
    #[case::encoder_block("type: snmp\n")]
    fn a_types_entry_of_each_new_type_parses(#[case] prefix: &str) {
        let yaml = format!(
            "{prefix}types:\n  a: opaque_float\n  b: opaque_double\n  c: ipaddress\n  \
             d: {{ enum: {{ 1: up, \"2\": down }} }}\n"
        );
        let types = if prefix.is_empty() {
            serde_yaml_ng::from_str::<SnmpEncoderConfig>(&yaml)
                .expect("parses")
                .types
        } else {
            match serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(&yaml).expect("parses") {
                crate::encoder::EncoderConfig::Snmp(cfg) => cfg.types,
                other => panic!("not the snmp variant: {other:?}"),
            }
        };
        let expected = HashMap::from([
            ("a".to_string(), SnmpValueType::OpaqueFloat),
            ("b".to_string(), SnmpValueType::OpaqueDouble),
            ("c".to_string(), SnmpValueType::IpAddress),
            (
                "d".to_string(),
                SnmpValueType::Enum(BTreeMap::from([
                    (1, "up".to_string()),
                    (2, "down".to_string()),
                ])),
            ),
        ]);
        assert_eq!(types, expected);
    }

    /// Inside the `type:`-tagged block a YAML number reaches a field as a
    /// number, not as text. The schema accepts any scalar for these fields,
    /// so each must parse.
    #[cfg(feature = "config")]
    #[test]
    fn numeric_scalars_parse_as_text_inside_the_encoder_block() {
        let parsed: crate::encoder::EncoderConfig = serde_yaml_ng::from_str(
            "type: snmp\nagent_label: 5\noids:\n  m: 1.3.6.1.2.1.1.3.0\ndrop_labels: [7, true]\n",
        )
        .expect("every scalar the schema accepts parses");
        let crate::encoder::EncoderConfig::Snmp(cfg) = parsed else {
            panic!("not the snmp variant: {parsed:?}");
        };
        assert_eq!(cfg.agent_label, "5");
        assert_eq!(
            cfg.oids["m"], "1.3.6.1.2.1.1.3.0",
            "a dotted OID is a YAML string"
        );
        assert_eq!(cfg.drop_labels, ["7", "true"]);
    }

    /// A plain `1.30` is a YAML number, which would read as `1.3`, so an
    /// `oids:` value must be a string, inside the `type:`-tagged block and
    /// outside it.
    #[cfg(feature = "config")]
    #[rstest]
    #[case::encoder_block("type: snmp\noids:\n  ifHCInOctets: ")]
    #[case::config_alone("oids:\n  ifHCInOctets: ")]
    #[case::label_column_in_the_encoder_block("type: snmp\nlabel_columns:\n  ifHCInOctets: ")]
    #[case::label_column_alone("label_columns:\n  ifHCInOctets: ")]
    fn an_unquoted_oid_is_an_error_naming_the_field(#[case] prefix: &str) {
        let parse = |value: &str| {
            let yaml = format!("{prefix}{value}\n");
            if prefix.starts_with("type:") {
                serde_yaml_ng::from_str::<crate::encoder::EncoderConfig>(&yaml).map(|_| ())
            } else {
                serde_yaml_ng::from_str::<SnmpEncoderConfig>(&yaml).map(|_| ())
            }
        };
        parse("\"1.30\"").expect("quoted parses");
        for value in ["1.30", "7", "true"] {
            let shown = parse(value).expect_err("not a string").to_string();
            let field = if prefix.contains("label_columns") {
                "label_columns.ifHCInOctets"
            } else {
                "oids.ifHCInOctets"
            };
            assert!(shown.contains(field), "names the field: {shown}");
            assert!(shown.contains("quote it"), "says to quote it: {shown}");
        }
    }

    #[cfg(feature = "config")]
    #[rstest]
    #[case::misspelt_oids("oid:\n  m: \"1.3.6.1\"\n", "oid")]
    #[case::misspelt_drop_labels("drop_label: [job]\n", "drop_label")]
    #[case::misspelt_label_columns("label_column:\n  ifName: \"1.3.6.1\"\n", "label_column")]
    fn unknown_encoder_keys_are_rejected(#[case] yaml: &str, #[case] key: &str) {
        let err = serde_yaml_ng::from_str::<SnmpEncoderConfig>(yaml).expect_err("unknown key");
        let shown = err.to_string();
        assert!(shown.contains(&format!("unknown field `{key}`")), "{shown}");
    }
}
