//! Metric pack engine: types and expansion logic.
//!
//! A metric pack is a reusable bundle of metric names and label schemas that
//! expands into a multi-metric scenario. Packs define *what metrics* to emit
//! (names, labels, default generators) but leave *how to deliver them* (rate,
//! duration, sink, encoder) to the user.
//!
//! This module provides the **engine** — the types and expansion function:
//!
//! - [`MetricPackDef`] and [`MetricSpec`]: the pack definition data model.
//! - [`PackScenarioConfig`]: the user-facing YAML config for referencing a pack.
//! - [`MetricOverride`]: per-metric overrides for generators and labels.
//! - [`expand_pack`]: the expansion function that produces `Vec<ScenarioEntry>`.
//!
//! This module is the engine only. The pack YAML the binary ships lives in
//! [`crate::catalog::builtin`], embedded with `include_str!` from the
//! repo-root `packs/` directory; user packs are read from `--catalog <dir>`.
//! [`crate::catalog::CatalogPackResolver`] chains the two.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::compiler::{AfterClause, DelayClause, WhileClause};
use crate::config::{BaseScheduleConfig, ScenarioConfig, ScenarioEntry};
use crate::encoder::EncoderConfig;
use crate::generator::GeneratorConfig;
use crate::sink::SinkConfig;
use crate::{ConfigError, SondaError};

// ---------------------------------------------------------------------------
// Pack definition types
// ---------------------------------------------------------------------------

/// A single metric within a pack definition.
///
/// Specifies the metric name and optionally per-metric labels and a default
/// generator. When the generator is absent, [`expand_pack`] uses a
/// `constant { value: 0.0 }` default.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricSpec {
    /// The metric name (e.g. `"ifHCInOctets"`, `"node_cpu_seconds_total"`).
    ///
    /// May not contain `.` — that is the selector separator, and
    /// [`validate_pack`] enforces the exclusion so `name.id` can never be
    /// ambushed by a dotted name.
    pub name: String,
    /// Disambiguator among specs sharing a [`name`](Self::name), and the
    /// second half of this spec's selector.
    ///
    /// Required — and unique — on every member of a repeated name; optional
    /// when the name occurs once. It stands for what the repetition is *by*:
    /// the CPU mode, the memory field, the SNMP column.
    #[cfg_attr(feature = "config", serde(default))]
    pub id: Option<String>,
    /// Labels specific to this metric, merged on top of the pack's shared labels.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
    /// Default value generator for this metric. When absent, a constant(0.0)
    /// generator is used.
    #[cfg_attr(feature = "config", serde(default))]
    pub generator: Option<GeneratorConfig>,
}

impl MetricSpec {
    /// The selector that addresses this spec: `name`, or `name.id` when the
    /// spec declares an id.
    pub fn selector(&self) -> String {
        match &self.id {
            Some(id) => format!("{}.{}", self.name, id),
            None => self.name.clone(),
        }
    }
}

/// A metric pack definition: a reusable bundle of metric names and label schemas.
///
/// Packs are templates — they contain no rate, duration, sink, or encoder.
/// Those come from the user via [`PackScenarioConfig`] at expansion time.
///
/// # YAML Schema
///
/// ```yaml
/// name: telegraf_snmp_interface
/// description: "Standard SNMP interface metrics (Telegraf-normalized)"
/// category: network
/// shared_labels:
///   device: ""
///   job: snmp
/// metrics:
///   - name: ifOperStatus
///     generator:
///       type: constant
///       value: 1.0
///   - name: ifHCInOctets
///     generator:
///       type: step
///       step_size: 125000.0
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricPackDef {
    /// Snake_case identifier for the pack (e.g. `"telegraf_snmp_interface"`).
    pub name: String,
    /// One-line human-readable description.
    pub description: String,
    /// Broad grouping (e.g. `"network"`, `"infrastructure"`).
    pub category: String,
    /// Labels shared across all metrics in the pack. Per-metric labels and
    /// user labels are merged on top (user wins on conflict).
    #[cfg_attr(feature = "config", serde(default))]
    pub shared_labels: Option<HashMap<String, String>>,
    /// The list of metric specifications in this pack.
    pub metrics: Vec<MetricSpec>,
}

/// User-facing configuration for running a metric pack.
///
/// Combines a pack reference (name or file path) with the schedule and delivery
/// parameters needed to produce runnable scenarios.
///
/// # YAML Schema
///
/// ```yaml
/// pack: telegraf_snmp_interface
/// rate: 1
/// duration: 60s
/// labels:
///   device: rtr-edge-01
/// sink:
///   type: stdout
/// encoder:
///   type: prometheus_text
/// overrides:
///   ifOperStatus:
///     generator:
///       type: flap
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PackScenarioConfig {
    /// Pack reference: a snake_case name resolved via the CLI search path,
    /// or a file path to a user-defined pack YAML (detected by containing
    /// `/` or `.`).
    pub pack: String,
    /// Target event rate in events per second.
    pub rate: f64,
    /// Optional total run duration (e.g. `"30s"`, `"5m"`).
    #[cfg_attr(feature = "config", serde(default))]
    pub duration: Option<String>,
    /// Static labels applied to every metric in the expanded pack.
    /// Merged on top of pack shared and per-metric labels (user wins).
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
    /// Output sink. Defaults to `stdout`.
    #[cfg_attr(feature = "config", serde(default = "default_sink"))]
    pub sink: SinkConfig,
    /// Output encoder. Defaults to `prometheus_text`.
    #[cfg_attr(feature = "config", serde(default = "default_encoder"))]
    pub encoder: EncoderConfig,
    /// Per-metric overrides keyed by selector — `name`, or `name.id` for a
    /// spec whose name the pack repeats (see [`resolve_override_keys`]).
    /// Each override can replace the generator and/or add extra labels for
    /// the one metric its key addresses.
    #[cfg_attr(feature = "config", serde(default))]
    pub overrides: Option<HashMap<String, MetricOverride>>,
}

/// Per-metric override within a [`PackScenarioConfig`] or a v2 pack-backed
/// scenario entry.
///
/// Allows the user to customize the generator, add extra labels, or attach a
/// causal dependency (`after:`) for a specific metric without modifying the
/// pack definition. The v1 expansion path ([`expand_pack`]) consumes only
/// `generator` and `labels`; the v2 compiler additionally propagates `after`
/// onto the expanded signal (see
/// [`crate::compiler::expand`]).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricOverride {
    /// Replacement generator for this metric.
    #[cfg_attr(feature = "config", serde(default))]
    pub generator: Option<GeneratorConfig>,
    /// Additional labels merged on top of all other label sources.
    ///
    /// Uses `BTreeMap` for deterministic serialization order, consistent with
    /// the v2 AST label types.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<BTreeMap<String, String>>,
    /// Optional causal dependency (`after:`) attached specifically to this
    /// expanded metric.
    ///
    /// Per spec §2.4, a per-metric `after:` on a pack override sets a
    /// causal dependency for that specific expanded signal, overriding
    /// any entry-level `after` on the parent pack entry. The v2 compiler
    /// propagates this onto the resulting signal in Phase 3; v1 pack
    /// expansion ignores the field.
    #[cfg_attr(feature = "config", serde(default))]
    pub after: Option<AfterClause>,
    /// Per-metric `while:` clause; replaces any entry-level `while:` for
    /// this expanded signal.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "while", skip_serializing_if = "Option::is_none")
    )]
    pub while_clause: Option<WhileClause>,
    /// Per-metric `delay:` clause; replaces any entry-level `delay:` for
    /// this expanded signal.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "delay", skip_serializing_if = "Option::is_none")
    )]
    pub delay_clause: Option<DelayClause>,
}

#[cfg(feature = "config")]
fn default_sink() -> SinkConfig {
    SinkConfig::Stdout
}

#[cfg(feature = "config")]
fn default_encoder() -> EncoderConfig {
    EncoderConfig::PrometheusText { precision: None }
}

// ---------------------------------------------------------------------------
// Pack validation and metric selectors
// ---------------------------------------------------------------------------

/// A pack that cannot be addressed unambiguously.
///
/// These are authoring-time faults in the pack itself, raised when it loads.
/// Moving ambiguity here is the point: a selector can never meet an ambiguous
/// pack, because such a pack does not load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackValidationError {
    /// A metric name contains the selector separator.
    #[error(
        "pack '{pack_name}': metric name '{name}' contains '.', which separates \
         a selector's name from its id; Prometheus metric names cannot contain \
         '.' either"
    )]
    DottedMetricName { pack_name: String, name: String },

    /// A name occurs more than once and at least one of those specs has no id.
    #[error(
        "pack '{pack_name}': metric name '{name}' appears {count} times, so every \
         one of them needs a unique `id:` to be addressable; {without} declare none"
    )]
    RepeatedNameWithoutIds {
        pack_name: String,
        name: String,
        count: usize,
        without: usize,
    },

    /// Two specs sharing a name declare the same id.
    #[error("pack '{pack_name}': metric '{name}' declares id '{id}' more than once")]
    DuplicateMetricId {
        pack_name: String,
        name: String,
        id: String,
    },
}

/// Check that every spec in `pack` is addressable, and that no selector
/// addresses two of them.
///
/// One spec may still answer to two selectors — a unique name that also
/// declares an id is reachable as both `name` and `name.id`. That is
/// harmless for lookup and rejected where it would silently discard work
/// (see [`ExpandError::ConflictingOverrideKeys`](crate::compiler::expand::ExpandError)).
///
/// The rule, and the whole of it: a metric **name** is either unique within
/// the pack, or *every* spec sharing it declares an **id** unique among them.
/// Names may not contain `.`.
///
/// Pure, and deliberately the only definition of the rule — the selector
/// resolver below relies on it having passed and does not re-derive it.
pub fn validate_pack(pack: &MetricPackDef) -> Result<(), PackValidationError> {
    for spec in &pack.metrics {
        if spec.name.contains('.') {
            return Err(PackValidationError::DottedMetricName {
                pack_name: pack.name.clone(),
                name: spec.name.clone(),
            });
        }
    }

    let mut by_name: BTreeMap<&str, Vec<&MetricSpec>> = BTreeMap::new();
    for spec in &pack.metrics {
        by_name.entry(spec.name.as_str()).or_default().push(spec);
    }

    for (name, specs) in by_name {
        if specs.len() < 2 {
            continue;
        }
        let without = specs.iter().filter(|s| s.id.is_none()).count();
        if without > 0 {
            return Err(PackValidationError::RepeatedNameWithoutIds {
                pack_name: pack.name.clone(),
                name: name.to_string(),
                count: specs.len(),
                without,
            });
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for spec in &specs {
            let id = spec.id.as_deref().expect("checked above: none are None");
            if !seen.insert(id) {
                return Err(PackValidationError::DuplicateMetricId {
                    pack_name: pack.name.clone(),
                    name: name.to_string(),
                    id: id.to_string(),
                });
            }
        }
    }

    Ok(())
}

/// A reference to one metric spec: a bare `name`, or `name.id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricSelector {
    pub name: String,
    pub id: Option<String>,
}

impl MetricSelector {
    /// Split a selector string on its **first** `.`.
    ///
    /// First rather than last because a name cannot contain `.` (enforced by
    /// [`validate_pack`]) while an id may: `a.b.c` is unambiguously the name
    /// `a` with the id `b.c`.
    pub fn parse(raw: &str) -> Self {
        match raw.split_once('.') {
            Some((name, id)) => Self {
                name: name.to_string(),
                id: Some(id.to_string()),
            },
            None => Self {
                name: raw.to_string(),
                id: None,
            },
        }
    }
}

/// Why a selector did not address exactly one spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorError {
    /// Nothing in the pack matches.
    #[error(
        "no metric matches selector '{selector}' in pack '{pack_name}'; available: {available}"
    )]
    NoMatch {
        selector: String,
        pack_name: String,
        available: String,
    },

    /// A bare name was used where the pack repeats that name. This is the
    /// case that used to fan out silently across every spec sharing it.
    #[error(
        "selector '{selector}' is ambiguous in pack '{pack_name}': that name is \
         shared by {count} metrics; address one by id — {available}"
    )]
    Ambiguous {
        selector: String,
        pack_name: String,
        count: usize,
        available: String,
    },
}

/// Resolve a selector to the index of exactly one spec in `pack.metrics`.
///
/// `pack` must have passed [`validate_pack`]. Ambiguity is an error rather
/// than a fan-out: a bare name against a repeated name names no single metric,
/// and guessing which one is meant is how eight distinct generators became one.
pub fn resolve_selector(pack: &MetricPackDef, raw: &str) -> Result<usize, SelectorError> {
    let selector = MetricSelector::parse(raw);

    let matches: Vec<usize> = pack
        .metrics
        .iter()
        .enumerate()
        .filter(|(_, spec)| {
            spec.name == selector.name
                && match &selector.id {
                    Some(id) => spec.id.as_deref() == Some(id.as_str()),
                    None => true,
                }
        })
        .map(|(index, _)| index)
        .collect();

    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(SelectorError::NoMatch {
            selector: raw.to_string(),
            pack_name: pack.name.clone(),
            available: available_selectors(pack),
        }),
        count => Err(SelectorError::Ambiguous {
            selector: raw.to_string(),
            pack_name: pack.name.clone(),
            count,
            available: matches
                .iter()
                .map(|&i| pack.metrics[i].selector())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// Every selector the pack answers to, for diagnostics.
pub fn available_selectors(pack: &MetricPackDef) -> String {
    if pack.metrics.is_empty() {
        return "<none>".to_string();
    }
    pack.metrics
        .iter()
        .map(MetricSpec::selector)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why a set of override keys does not map cleanly onto a pack's specs.
#[derive(Debug, thiserror::Error)]
pub enum OverrideKeyError {
    /// One key addressed no spec, or more than one.
    #[error(transparent)]
    Unresolvable(#[from] SelectorError),

    /// Two keys addressed the same spec, so one would be discarded.
    #[error(
        "keys '{first}' and '{second}' both address metric '{selector}' in pack '{pack_name}'"
    )]
    Conflict {
        /// The pack being expanded.
        pack_name: String,
        /// The lexicographically earlier key.
        first: String,
        /// The key that collided with it.
        second: String,
        /// The canonical selector both keys resolve to.
        selector: String,
    },
}

/// Map each override key to the one spec index it addresses.
///
/// The only definition of override keying — both the v1 [`expand_pack`] path
/// and the v2 compiler go through it, so they cannot drift.
///
/// Two properties, and neither implies the other: every key addresses exactly
/// one spec, and no spec is addressed twice. A spec with a unique name that
/// also declares an `id:` answers to both `name` and `name.id`, so two
/// distinct keys can land on one index; writing both would silently discard
/// one of them. Keys are visited in sorted order, so the pair a conflict
/// names does not depend on the caller's map type.
///
/// # Errors
///
/// [`OverrideKeyError::Unresolvable`] for a key matching no spec or an
/// ambiguous one; [`OverrideKeyError::Conflict`] when two keys collide.
pub fn resolve_override_keys<'a>(
    pack: &MetricPackDef,
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<usize, &'a str>, OverrideKeyError> {
    let mut sorted: Vec<&str> = keys.into_iter().collect();
    sorted.sort_unstable();

    let mut by_spec: BTreeMap<usize, &str> = BTreeMap::new();
    for key in sorted {
        let index = resolve_selector(pack, key)?;
        if let Some(previous) = by_spec.insert(index, key) {
            return Err(OverrideKeyError::Conflict {
                pack_name: pack.name.clone(),
                first: previous.to_string(),
                second: key.to_string(),
                selector: pack.metrics[index].selector(),
            });
        }
    }
    Ok(by_spec)
}

// ---------------------------------------------------------------------------
// Pack expansion
// ---------------------------------------------------------------------------

/// Expand a [`MetricPackDef`] with user-provided schedule and delivery config
/// into a list of [`ScenarioEntry`] values — one per metric in the pack.
///
/// # Label merge order
///
/// For each metric, labels are merged in this order (later wins on conflict):
/// 1. Pack `shared_labels`
/// 2. Per-metric `MetricSpec::labels`
/// 3. User `labels` from [`PackScenarioConfig`]
/// 4. Per-metric override `labels` (from `overrides`)
///
/// # Generator selection
///
/// For each metric the generator is chosen as:
/// 1. Per-metric override generator (from `overrides`), if present.
/// 2. `MetricSpec::generator`, if present in the pack definition.
/// 3. `constant { value: 0.0 }` as a last-resort default.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if:
/// - The pack definition has no metrics.
/// - The pack is not addressable (see [`validate_pack`]).
/// - An override key does not address exactly one spec, or two keys address
///   the same one (see [`resolve_override_keys`]).
pub fn expand_pack(
    pack: &MetricPackDef,
    config: &PackScenarioConfig,
) -> Result<Vec<ScenarioEntry>, SondaError> {
    if pack.metrics.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "metric pack has no metrics defined",
        )));
    }

    validate_pack(pack).map_err(|e| SondaError::Config(ConfigError::invalid(e.to_string())))?;

    let override_keys = match config.overrides.as_ref() {
        Some(overrides) => resolve_override_keys(pack, overrides.keys().map(String::as_str))
            .map_err(|e| SondaError::Config(ConfigError::invalid(format!("override {e}"))))?,
        None => BTreeMap::new(),
    };

    let mut entries = Vec::with_capacity(pack.metrics.len());

    for (spec_index, spec) in pack.metrics.iter().enumerate() {
        // 1. Start with shared labels.
        let mut labels: HashMap<String, String> =
            pack.shared_labels.as_ref().cloned().unwrap_or_default();

        // 2. Merge per-metric labels.
        if let Some(ref metric_labels) = spec.labels {
            for (k, v) in metric_labels {
                labels.insert(k.clone(), v.clone());
            }
        }

        // 3. Merge user labels.
        if let Some(ref user_labels) = config.labels {
            for (k, v) in user_labels {
                labels.insert(k.clone(), v.clone());
            }
        }

        // Look up the override addressed at this spec, if any. Keyed by
        // resolved index rather than by name: a name the pack repeats
        // addresses no single spec, and `resolve_override_keys` has already
        // refused such a key rather than fanning one override across all of
        // them.
        let metric_override = override_keys
            .get(&spec_index)
            .and_then(|key| config.overrides.as_ref().and_then(|o| o.get(*key)));

        // 4. Merge override labels.
        if let Some(ov) = metric_override {
            if let Some(ref ov_labels) = ov.labels {
                for (k, v) in ov_labels {
                    labels.insert(k.clone(), v.clone());
                }
            }
        }

        // Generator: override > spec > constant(0.0)
        let generator = if let Some(ov) = metric_override {
            if let Some(ref gen) = ov.generator {
                gen.clone()
            } else {
                spec.generator
                    .clone()
                    .unwrap_or(GeneratorConfig::Constant { value: 0.0 })
            }
        } else {
            spec.generator
                .clone()
                .unwrap_or(GeneratorConfig::Constant { value: 0.0 })
        };

        let scenario = ScenarioConfig {
            base: BaseScheduleConfig {
                name: spec.name.clone(),
                rate: config.rate,
                duration: config.duration.clone(),
                gaps: None,
                gap_windows: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                sink: config.sink.clone(),
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator,
            encoder: config.encoder.clone(),
            metric_type: None,
            help: None,
        };

        entries.push(ScenarioEntry::Metrics(scenario));
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Expansion tests --------------------------------------------------------

    #[test]
    fn expand_pack_produces_one_entry_per_metric() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test pack".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![
                MetricSpec {
                    name: "metric_a".to_string(),
                    id: None,
                    labels: None,
                    generator: None,
                },
                MetricSpec {
                    name: "metric_b".to_string(),
                    id: None,
                    labels: None,
                    generator: None,
                },
            ],
        };

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: Some("10s".to_string()),
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        assert_eq!(entries.len(), 2);

        // Both must be Metrics entries.
        for entry in &entries {
            assert!(matches!(entry, ScenarioEntry::Metrics(_)));
        }

        // Check names.
        match &entries[0] {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "metric_a"),
            _ => panic!("expected Metrics"),
        }
        match &entries[1] {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "metric_b"),
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_merges_labels_in_correct_order() {
        let mut shared = HashMap::new();
        shared.insert("job".to_string(), "snmp".to_string());
        shared.insert("device".to_string(), "default".to_string());

        let mut metric_labels = HashMap::new();
        metric_labels.insert("ifName".to_string(), "eth0".to_string());
        metric_labels.insert("device".to_string(), "metric-override".to_string());

        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "ifOperStatus".to_string(),
                id: None,
                labels: Some(metric_labels),
                generator: None,
            }],
        };

        let mut user_labels = HashMap::new();
        user_labels.insert("device".to_string(), "rtr-edge-01".to_string());

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: Some(user_labels),
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        assert_eq!(entries.len(), 1);

        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                let labels = c.base.labels.as_ref().expect("must have labels");
                // User label wins over metric and shared.
                assert_eq!(
                    labels.get("device").map(String::as_str),
                    Some("rtr-edge-01")
                );
                // Shared label preserved.
                assert_eq!(labels.get("job").map(String::as_str), Some("snmp"));
                // Per-metric label preserved.
                assert_eq!(labels.get("ifName").map(String::as_str), Some("eth0"));
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_applies_generator_override() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "ifOperStatus".to_string(),
                id: None,
                labels: None,
                generator: Some(GeneratorConfig::Constant { value: 1.0 }),
            }],
        };

        let mut overrides = HashMap::new();
        overrides.insert(
            "ifOperStatus".to_string(),
            MetricOverride {
                generator: Some(GeneratorConfig::Constant { value: 42.0 }),
                labels: None,
                after: None,
                while_clause: None,
                delay_clause: None,
            },
        );

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: Some(overrides),
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                assert!(
                    matches!(c.generator, GeneratorConfig::Constant { value } if (value - 42.0).abs() < f64::EPSILON),
                    "override generator must be constant(42.0), got {:?}",
                    c.generator
                );
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_uses_default_generator_when_none() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "metric_a".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                assert!(
                    matches!(c.generator, GeneratorConfig::Constant { value } if value.abs() < f64::EPSILON),
                    "default generator must be constant(0.0), got {:?}",
                    c.generator
                );
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_propagates_rate_and_duration() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 5.0,
            duration: Some("30s".to_string()),
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                assert!((c.base.rate - 5.0).abs() < f64::EPSILON);
                assert_eq!(c.base.duration.as_deref(), Some("30s"));
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_propagates_sink_and_encoder() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::File {
                path: "/tmp/test.txt".to_string(),
            },
            encoder: EncoderConfig::JsonLines { precision: Some(2) },
            overrides: None,
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                assert!(matches!(c.base.sink, SinkConfig::File { .. }));
                assert!(matches!(
                    c.encoder,
                    EncoderConfig::JsonLines { precision: Some(2) }
                ));
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn expand_pack_errors_on_empty_metrics() {
        let pack = MetricPackDef {
            name: "empty".to_string(),
            description: "empty".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![],
        };

        let config = PackScenarioConfig {
            pack: "empty".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let err = expand_pack(&pack, &config).expect_err("empty metrics must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no metrics"),
            "error must mention empty metrics, got: {msg}"
        );
    }

    #[test]
    fn expand_pack_errors_on_unknown_override_key() {
        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "metric_a".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };

        let mut overrides = HashMap::new();
        overrides.insert(
            "nonexistent_metric".to_string(),
            MetricOverride {
                generator: None,
                labels: None,
                after: None,
                while_clause: None,
                delay_clause: None,
            },
        );

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: Some(overrides),
        };

        let err = expand_pack(&pack, &config).expect_err("unknown override must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent_metric"),
            "error must mention the unknown key, got: {msg}"
        );
    }

    #[test]
    fn expand_pack_override_labels_merge_on_top() {
        let mut shared = HashMap::new();
        shared.insert("job".to_string(), "snmp".to_string());

        let pack = MetricPackDef {
            name: "test".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "ifOperStatus".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };

        let mut override_labels = BTreeMap::new();
        override_labels.insert("extra".to_string(), "value".to_string());
        override_labels.insert("job".to_string(), "overridden".to_string());

        let mut overrides = HashMap::new();
        overrides.insert(
            "ifOperStatus".to_string(),
            MetricOverride {
                generator: None,
                labels: Some(override_labels),
                after: None,
                while_clause: None,
                delay_clause: None,
            },
        );

        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: Some(overrides),
        };

        let entries = expand_pack(&pack, &config).expect("must succeed");
        match &entries[0] {
            ScenarioEntry::Metrics(c) => {
                let labels = c.base.labels.as_ref().expect("must have labels");
                assert_eq!(
                    labels.get("job").map(String::as_str),
                    Some("overridden"),
                    "override label must win over shared"
                );
                assert_eq!(
                    labels.get("extra").map(String::as_str),
                    Some("value"),
                    "override extra label must be present"
                );
            }
            _ => panic!("expected Metrics"),
        }
    }

    // ---- Contract tests ---------------------------------------------------------

    #[test]
    fn metric_pack_def_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricPackDef>();
    }

    // ---- Deserialization tests (config feature) ---------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn pack_scenario_config_deserializes_from_yaml() {
        let yaml = r#"
pack: telegraf_snmp_interface
rate: 1
duration: 60s
labels:
  device: rtr-edge-01
  ifName: GigabitEthernet0/0/0
sink:
  type: stdout
encoder:
  type: prometheus_text
"#;
        let config: PackScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("pack config must parse");
        assert_eq!(config.pack, "telegraf_snmp_interface");
        assert!((config.rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.duration.as_deref(), Some("60s"));
        let labels = config.labels.as_ref().expect("must have labels");
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("rtr-edge-01")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn pack_scenario_config_with_overrides_deserializes() {
        let yaml = r#"
pack: telegraf_snmp_interface
rate: 1
duration: 60s
labels:
  device: rtr-edge-01
overrides:
  ifOperStatus:
    generator:
      type: constant
      value: 0.0
    labels:
      extra_label: extra_value
sink:
  type: stdout
"#;
        let config: PackScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("pack config with overrides must parse");
        let overrides = config.overrides.as_ref().expect("must have overrides");
        let ov = overrides
            .get("ifOperStatus")
            .expect("must have ifOperStatus");
        assert!(ov.generator.is_some());
        let labels = ov.labels.as_ref().expect("must have override labels");
        assert_eq!(
            labels.get("extra_label").map(String::as_str),
            Some("extra_value")
        );
    }

    // ---- Pack validation and selectors -----------------------------------------

    fn spec(name: &str, id: Option<&str>) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            id: id.map(str::to_string),
            labels: None,
            generator: None,
        }
    }

    fn pack_of(metrics: Vec<MetricSpec>) -> MetricPackDef {
        MetricPackDef {
            name: "p".to_string(),
            description: "d".to_string(),
            category: "network".to_string(),
            shared_labels: None,
            metrics,
        }
    }

    #[test]
    fn a_unique_name_needs_no_id() {
        let pack = pack_of(vec![spec("a", None), spec("b", None)]);
        assert_eq!(validate_pack(&pack), Ok(()));
    }

    /// The rule that moves ambiguity to authoring time. `node_exporter_cpu`
    /// is exactly this shape.
    #[test]
    fn a_repeated_name_without_ids_does_not_load() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", None)]);
        let err = validate_pack(&pack).expect_err("must refuse");
        assert_eq!(
            err,
            PackValidationError::RepeatedNameWithoutIds {
                pack_name: "p".to_string(),
                name: "cpu".to_string(),
                count: 2,
                without: 1,
            }
        );
        assert!(format!("{err}").contains("unique `id:`"), "got: {err}");
    }

    #[test]
    fn a_repeated_name_with_ids_on_all_of_them_loads() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", Some("idle"))]);
        assert_eq!(validate_pack(&pack), Ok(()));
    }

    #[test]
    fn two_specs_sharing_a_name_may_not_share_an_id() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", Some("user"))]);
        assert!(matches!(
            validate_pack(&pack),
            Err(PackValidationError::DuplicateMetricId { .. })
        ));
    }

    /// The separator must not be ambushable. Prometheus metric names cannot
    /// contain `.` either, so nothing legitimate is being refused.
    #[test]
    fn a_dotted_metric_name_does_not_load() {
        let pack = pack_of(vec![spec("if.OperStatus", None)]);
        let err = validate_pack(&pack).expect_err("must refuse");
        assert!(matches!(err, PackValidationError::DottedMetricName { .. }));
        assert!(format!("{err}").contains("if.OperStatus"), "got: {err}");
    }

    /// An id may repeat across *different* names — it only disambiguates
    /// within one name, exactly like an SNMP column index within a table.
    #[test]
    fn the_same_id_under_two_different_names_is_fine() {
        let pack = pack_of(vec![
            spec("cpu", Some("user")),
            spec("cpu", Some("idle")),
            spec("mem", Some("user")),
            spec("mem", Some("idle")),
        ]);
        assert_eq!(validate_pack(&pack), Ok(()));
    }

    #[test]
    fn selector_parse_splits_on_the_first_dot_so_an_id_may_contain_dots() {
        assert_eq!(
            MetricSelector::parse("ifOperStatus"),
            MetricSelector {
                name: "ifOperStatus".to_string(),
                id: None
            }
        );
        assert_eq!(
            MetricSelector::parse("cpu.user"),
            MetricSelector {
                name: "cpu".to_string(),
                id: Some("user".to_string())
            }
        );
        assert_eq!(
            MetricSelector::parse("cpu.a.b"),
            MetricSelector {
                name: "cpu".to_string(),
                id: Some("a.b".to_string())
            }
        );
    }

    #[test]
    fn selector_resolves_a_bare_unique_name() {
        let pack = pack_of(vec![spec("a", None), spec("b", None)]);
        assert_eq!(resolve_selector(&pack, "b"), Ok(1));
    }

    #[test]
    fn selector_resolves_a_name_dot_id() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", Some("idle"))]);
        assert_eq!(resolve_selector(&pack, "cpu.idle"), Ok(1));
    }

    /// The mirror of the fan-out: one spec addressed by two keys, one of
    /// which would be applied and the other dropped in silence.
    #[test]
    fn two_override_keys_reaching_one_spec_are_refused() {
        let pack = pack_of(vec![spec("ifOperStatus", Some("up"))]);
        for key in ["ifOperStatus", "ifOperStatus.up"] {
            resolve_override_keys(&pack, [key])
                .unwrap_or_else(|e| panic!("'{key}' alone must resolve, got {e}"));
        }

        let err = resolve_override_keys(&pack, ["ifOperStatus", "ifOperStatus.up"])
            .expect_err("both together must not silently pick one");
        match &err {
            OverrideKeyError::Conflict {
                first,
                second,
                selector,
                ..
            } => {
                assert_eq!(first, "ifOperStatus");
                assert_eq!(second, "ifOperStatus.up");
                assert_eq!(selector, "ifOperStatus.up");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// Keys are visited in sorted order, so the pair a conflict names does
    /// not depend on the caller handing them over in a particular order —
    /// `PackScenarioConfig` keeps its overrides in a `HashMap`.
    #[test]
    fn a_conflict_names_the_same_pair_whichever_order_the_keys_arrive_in() {
        let pack = pack_of(vec![spec("ifOperStatus", Some("up"))]);
        let forward = resolve_override_keys(&pack, ["ifOperStatus", "ifOperStatus.up"])
            .expect_err("must conflict")
            .to_string();
        let reverse = resolve_override_keys(&pack, ["ifOperStatus.up", "ifOperStatus"])
            .expect_err("must conflict")
            .to_string();
        assert_eq!(forward, reverse);
    }

    /// The v1 oracle used to fan a bare key across every spec sharing the
    /// name, with a comment saying so on purpose. It now goes through the
    /// same keying rule as the compiler.
    #[test]
    fn expand_pack_refuses_a_bare_key_against_a_repeated_name() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", Some("idle"))]);
        let mut overrides = HashMap::new();
        overrides.insert(
            "cpu".to_string(),
            MetricOverride {
                generator: Some(GeneratorConfig::Constant { value: 12345.0 }),
                labels: None,
                after: None,
                while_clause: None,
                delay_clause: None,
            },
        );
        let config = PackScenarioConfig {
            pack: "test".to_string(),
            rate: 1.0,
            duration: None,
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: Some(overrides),
        };

        let msg = expand_pack(&pack, &config)
            .expect_err("v1 must not fan one override across both specs")
            .to_string();
        assert!(msg.contains("ambiguous"), "got: {msg}");
        assert!(
            msg.contains("cpu.user") && msg.contains("cpu.idle"),
            "must list the ids to pick from: {msg}"
        );
    }

    /// The defect this whole change exists to kill: a bare name against a
    /// repeated one used to silently address all of them.
    #[test]
    fn a_bare_name_against_a_repeated_name_is_ambiguous_not_a_fan_out() {
        let pack = pack_of(vec![
            spec("cpu", Some("user")),
            spec("cpu", Some("idle")),
            spec("cpu", Some("steal")),
        ]);
        let err = resolve_selector(&pack, "cpu").expect_err("must refuse to guess");
        match &err {
            SelectorError::Ambiguous {
                count, available, ..
            } => {
                assert_eq!(*count, 3);
                assert!(
                    available.contains("cpu.user"),
                    "must list the ids: {available}"
                );
                assert!(
                    available.contains("cpu.steal"),
                    "must list the ids: {available}"
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_selector_names_what_the_pack_does_have() {
        let pack = pack_of(vec![spec("cpu", Some("user"))]);
        let err = resolve_selector(&pack, "nope").expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("nope"), "got: {msg}");
        assert!(
            msg.contains("cpu.user"),
            "must list the real selectors: {msg}"
        );
    }

    #[test]
    fn an_unknown_id_under_a_known_name_is_a_no_match() {
        let pack = pack_of(vec![spec("cpu", Some("user")), spec("cpu", Some("idle"))]);
        assert!(matches!(
            resolve_selector(&pack, "cpu.nope"),
            Err(SelectorError::NoMatch { .. })
        ));
    }

    #[test]
    fn spec_selector_round_trips_through_resolve() {
        let pack = pack_of(vec![
            spec("cpu", Some("user")),
            spec("cpu", Some("idle")),
            spec("mem", None),
        ]);
        assert_eq!(validate_pack(&pack), Ok(()));
        for (index, s) in pack.metrics.iter().enumerate() {
            assert_eq!(
                resolve_selector(&pack, &s.selector()),
                Ok(index),
                "every spec must be addressable by its own selector"
            );
        }
    }
}
