//! SNMP encoder: one `MetricEvent` in, one publish record out.
//!
//! A publish record is one byte holding the number of index arcs at the end
//! of the instance OID, then a BER `VarBind` SEQUENCE { OID, value }. The
//! instance OID is rendered from the metric's [`OidTemplate`] the first time
//! a series is seen and cached as its encoded OBJECT IDENTIFIER; later events
//! copy those bytes and encode only the value. A series is its metric name
//! and the labels its template can see, so events that differ only in the
//! agent label or a `drop_labels` label share one entry. A series whose OID
//! cannot be rendered is cached with the reason, and its later events are
//! rejected without rendering again.
//!
//! Requires the `snmp` feature flag.

// The decoder half (`ber::Reader`, `ber::Message`, the OID body decoder) has
// no caller in the crate outside tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod ber;
#[cfg_attr(not(test), allow(dead_code))]
pub mod oid;
pub mod value;

pub use value::SnmpValueType;

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, PoisonError, RwLock};

use crate::model::metric::{is_valid_metric_name, Labels, MetricEvent, ValidatedMetricName};
use crate::{ConfigError, EncoderError, SondaError};

use super::Encoder;
use ber::Tag;
use oid::{Oid, OidTemplate};

/// Default label that names the device: excluded from the coverage rule and
/// never part of an OID.
pub const DEFAULT_AGENT_LABEL: &str = "device";

/// Length of a publish record's header: the one index-arc-count byte before
/// the `VarBind`.
pub(crate) const RECORD_HEADER: usize = 1;

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
    /// `"1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"`: the object's OID, then one
    /// `{label}` placeholder per index arc. Every metric the entry emits
    /// needs one.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "scalar_text_values")
    )]
    pub oids: HashMap<String, String>,
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

/// `deserialize_with` for a map of text values: each value may be any scalar.
#[cfg(feature = "config")]
fn scalar_text_values<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error> {
    use crate::config::scalar_text::ScalarText;
    let map: HashMap<String, ScalarText> = serde::Deserialize::deserialize(deserializer)?;
    Ok(map.into_iter().map(|(key, text)| (key, text.0)).collect())
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
    /// Default agent label, no templates, no types, no dropped labels.
    fn default() -> Self {
        Self {
            agent_label: default_agent_label(),
            oids: HashMap::new(),
            types: HashMap::new(),
            drop_labels: Vec::new(),
        }
    }
}

/// The first of `keys` that `template` does not cover: not one of its
/// placeholders, not the `agent_label`, and not listed in `drop_labels`.
/// `None` when every key is covered.
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
            && !template.placeholders().any(|p| p == *key)
    })
}

/// Encodes metric events as SNMP publish records.
///
/// Each call to [`Encoder::encode_metric`] appends one record: a byte
/// holding the template's [`index_arcs`](OidTemplate::index_arcs), then one
/// `VarBind` SEQUENCE { OBJECT IDENTIFIER, value }. Log events are not
/// supported.
pub struct SnmpEncoder {
    cfg: SnmpEncoderConfig,
    templates: HashMap<String, Template>,
    /// Each series' encoded OBJECT IDENTIFIER or rejection, filled on first
    /// sight, keyed by the series' [fingerprint](Template::fingerprint).
    oids: RwLock<HashMap<u64, Vec<CachedOid>>>,
    /// OIDs rendered, for tests.
    #[cfg(test)]
    renders: std::sync::atomic::AtomicUsize,
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
/// encoded OBJECT IDENTIFIER or the text it is rejected with.
struct CachedOid {
    name: ValidatedMetricName,
    labels: Arc<Labels>,
    oid: Result<Arc<[u8]>, String>,
}

impl CachedOid {
    /// Whether an event named `name` with `labels` is this entry's series
    /// under `template`.
    fn is_series(&self, template: &Template, name: &str, labels: &Labels) -> bool {
        *self.name == *name && template.seen(&self.labels).eq(template.seen(labels))
    }

    /// The cached OID, or the cached rejection as an
    /// [`EncoderError::EventRejected`].
    fn oid(&self) -> Result<Arc<[u8]>, SondaError> {
        self.oid
            .clone()
            .map_err(|reason| SondaError::Encoder(EncoderError::EventRejected(reason)))
    }
}

impl SnmpEncoder {
    /// Build an encoder, parsing every template in `cfg`.
    ///
    /// Returns [`SondaError::Config`] when an `oids:` or `types:` key is not
    /// a valid metric name, or an `oids:` entry is not a valid
    /// [`OidTemplate`]. Both maps are checked in key order, so the same
    /// config always reports the same first error.
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
                let ty = cfg.types.get(metric).copied().unwrap_or_default();
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

        Ok(Self {
            cfg: cfg.clone(),
            templates,
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

    /// Encoded OBJECT IDENTIFIER of the event's series under `template`,
    /// rendering and caching it on first sight.
    ///
    /// A hit allocates nothing: the entry is found by a hash of the name and
    /// the labels the template sees, then compared label by label. A series
    /// whose OID is rejected ([`EncoderError::EventRejected`]: a label the
    /// template does not cover, a placeholder whose label the event lacks,
    /// or a label value that is not an arc) is cached with the rejection's
    /// text, and each later event of it gets that text without the OID being
    /// rendered again.
    fn oid_bytes(&self, template: &Template, event: &MetricEvent) -> Result<Arc<[u8]>, SondaError> {
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

    /// The event's instance OID under `template`, BER-encoded, or the text
    /// it is rejected with: the event carries a label the template does not
    /// cover, or the render fails.
    fn render_oid(&self, template: &Template, event: &MetricEvent) -> Result<Arc<[u8]>, String> {
        #[cfg(test)]
        self.renders
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name: &str = &event.name;
        let template = &template.template;
        if let Some(label) =
            uncovered_label(&self.cfg, template, event.labels.iter().map(|(k, _)| k))
        {
            return Err(format!(
                "metric {name:?} carries label `{label}`, which the snmp OID template does \
                 not reference; reference it as an index placeholder, make it the \
                 agent_label, or list it in drop_labels"
            ));
        }
        let mut arcs = Vec::new();
        template
            .render(&event.labels, &mut arcs)
            .map_err(|e| format!("metric {name:?}: {e}"))?;
        let oid = Oid::from_arcs(arcs)
            .ok_or_else(|| format!("metric {name:?}: the rendered OID cannot be encoded"))?;
        let mut bytes = Vec::with_capacity(2 + oid.ber_body_len());
        ber::write_oid(&mut bytes, &oid);
        Ok(bytes.into())
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
    /// Appends exactly one publish record into `buf`: one byte, the template's
    /// [`index_arcs`](OidTemplate::index_arcs), then a BER `VarBind`
    /// SEQUENCE { OBJECT IDENTIFIER, value }. The output is not a bare
    /// `VarBind` and not an SNMP message. `buf` is cleared by the caller, not
    /// here.
    ///
    /// Returns [`SondaError::Config`] when the metric has no `oids:` entry,
    /// which is a defect of the configuration rather than of this event.
    /// Returns [`EncoderError::EventRejected`], and writes nothing, when this
    /// event does not fit the configuration:
    ///
    /// - the event carries a label the template does not reference that is
    ///   neither the `agent_label` nor listed in `drop_labels`;
    /// - a template placeholder names a label the event lacks;
    /// - a placeholder's label value is not a canonical decimal `u32`;
    /// - the value is NaN or infinite and the type is not `string`;
    /// - the rounded value is outside the type's range.
    ///
    /// The label checks run only when a series is first seen and its OID is
    /// rendered and cached; later events for the same series skip them. A
    /// series rejected by them is rejected with the same text on every later
    /// event without being rendered again.
    ///
    /// Validation pairs this encoder with the `snmp_agent` sink only.
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let name: &str = &event.name;
        let template = self.template_of(name)?;
        let oid = self.oid_bytes(template, event)?;
        let start = buf.len();
        // The header byte, a SEQUENCE header, the OID, and the longest
        // numeric value TLV (a Counter64: tag, length, nine octets).
        buf.reserve(RECORD_HEADER + 2 + oid.len() + 11);
        buf.push(template.template.index_arcs());
        let mut written = Ok(());
        ber::write_tlv(buf, Tag::Sequence, |out| {
            out.extend_from_slice(&oid);
            written = template.ty.write(event.value, out);
        });
        if let Err(e) = written {
            buf.truncate(start);
            return Err(SondaError::Encoder(EncoderError::EventRejected(format!(
                "metric {name:?}: {e}"
            ))));
        }
        Ok(())
    }
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
        assert_eq!(&out[RECORD_HEADER..], &expected[..]);
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
        let encoder = encoder_of("ifHCInOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}", ty);
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
        assert_eq!(buf[2], 0x30);
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
        let check = |keys: &[&'static str]| uncovered_label(&cfg, &template, keys.iter().copied());
        assert_eq!(check(&["device", "ifIndex", "job"]), None);
        assert_eq!(check(&["device", "ifName", "ifIndex"]), Some("ifName"));
        assert_eq!(check(&[]), None);
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
    #[case::string_index(with(|c| { c.oids.insert("m".into(), "1.3.6.{ifName:str}".into()); }), "is not supported")]
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
        let empty: SnmpEncoderConfig = serde_yaml_ng::from_str("{}").expect("all fields default");
        assert_eq!(empty, SnmpEncoderConfig::default());
    }

    /// Inside the `type:`-tagged block a YAML number reaches a field as a
    /// number, not as text. The schema accepts any scalar for these fields,
    /// so each must parse.
    #[cfg(feature = "config")]
    #[test]
    fn numeric_scalars_parse_as_text_inside_the_encoder_block() {
        let parsed: crate::encoder::EncoderConfig = serde_yaml_ng::from_str(
            "type: snmp\nagent_label: 5\noids:\n  m: 1.3\ndrop_labels: [7, true]\n",
        )
        .expect("every scalar the schema accepts parses");
        let crate::encoder::EncoderConfig::Snmp(cfg) = parsed else {
            panic!("not the snmp variant: {parsed:?}");
        };
        assert_eq!(cfg.agent_label, "5");
        assert_eq!(cfg.oids["m"], "1.3");
        assert_eq!(cfg.drop_labels, ["7", "true"]);
    }

    #[cfg(feature = "config")]
    #[rstest]
    #[case::misspelt_oids("oid:\n  m: \"1.3.6.1\"\n", "oid")]
    #[case::misspelt_drop_labels("drop_label: [job]\n", "drop_label")]
    fn unknown_encoder_keys_are_rejected(#[case] yaml: &str, #[case] key: &str) {
        let err = serde_yaml_ng::from_str::<SnmpEncoderConfig>(yaml).expect_err("unknown key");
        let shown = err.to_string();
        assert!(shown.contains(&format!("unknown field `{key}`")), "{shown}");
    }
}
