//! Metric pack catalog and expansion.
//!
//! A metric pack is a reusable bundle of metric names and label schemas that
//! expands into a multi-metric scenario. Packs define *what metrics* to emit
//! (names, labels, default generators) but leave *how to deliver them* (rate,
//! duration, sink, encoder) to the user.
//!
//! This module provides:
//! - [`MetricPackDef`] and [`MetricSpec`]: the pack definition data model.
//! - [`PackScenarioConfig`]: the user-facing YAML config for referencing a pack.
//! - A built-in catalog of packs compiled into the binary via [`include_str!`].
//! - [`expand_pack`]: the expansion function that produces `Vec<ScenarioEntry>`.
//!
//! # Built-in Packs
//!
//! ```
//! use sonda_core::packs;
//!
//! let all = packs::list();
//! assert!(!all.is_empty());
//!
//! let snmp = packs::get("telegraf_snmp_interface");
//! assert!(snmp.is_some());
//! ```

use std::collections::HashMap;

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
pub struct MetricSpec {
    /// The metric name (e.g. `"ifHCInOctets"`, `"node_cpu_seconds_total"`).
    pub name: String,
    /// Labels specific to this metric, merged on top of the pack's shared labels.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
    /// Default value generator for this metric. When absent, a constant(0.0)
    /// generator is used.
    #[cfg_attr(feature = "config", serde(default))]
    pub generator: Option<GeneratorConfig>,
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
/// Combines a pack reference (built-in name or file path) with the schedule
/// and delivery parameters needed to produce runnable scenarios.
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
pub struct PackScenarioConfig {
    /// Pack reference: a built-in snake_case name (e.g. `"telegraf_snmp_interface"`)
    /// or a file path to a user-defined pack YAML (detected by containing `/` or `.`).
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
    /// Per-metric overrides keyed by metric name. Each override can replace
    /// the generator and/or add extra labels for a specific metric.
    #[cfg_attr(feature = "config", serde(default))]
    pub overrides: Option<HashMap<String, MetricOverride>>,
}

/// Per-metric override within a [`PackScenarioConfig`].
///
/// Allows the user to customize the generator or add extra labels for a
/// specific metric without modifying the pack definition.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct MetricOverride {
    /// Replacement generator for this metric.
    #[cfg_attr(feature = "config", serde(default))]
    pub generator: Option<GeneratorConfig>,
    /// Additional labels merged on top of all other label sources.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
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
// Built-in pack catalog
// ---------------------------------------------------------------------------

/// A built-in metric pack definition embedded in the binary.
///
/// All fields are `&'static str` because the data is compiled in via
/// [`include_str!`]. The `yaml` field contains the full YAML content
/// that can be parsed into a [`MetricPackDef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinPack {
    /// Snake_case identifier (e.g. `"telegraf_snmp_interface"`).
    pub name: &'static str,
    /// Broad grouping: `"network"`, `"infrastructure"`, etc.
    pub category: &'static str,
    /// One-line human-readable description for list display.
    pub description: &'static str,
    /// Number of metric specs in this pack.
    pub metric_count: usize,
    /// The full embedded YAML content.
    pub yaml: &'static str,
}

/// The complete catalog of built-in metric packs.
///
/// This is a static array so there are zero heap allocations. The catalog
/// is small enough that linear scan is the right choice over a `HashMap`.
static CATALOG: &[BuiltinPack] = &[
    BuiltinPack {
        name: "telegraf_snmp_interface",
        category: "network",
        description: "Standard SNMP interface metrics (Telegraf-normalized)",
        metric_count: 5,
        yaml: include_str!("../../packs/telegraf-snmp-interface.yaml"),
    },
    BuiltinPack {
        name: "node_exporter_cpu",
        category: "infrastructure",
        description: "Per-CPU mode counters (node_exporter-compatible)",
        metric_count: 8,
        yaml: include_str!("../../packs/node-exporter-cpu.yaml"),
    },
    BuiltinPack {
        name: "node_exporter_memory",
        category: "infrastructure",
        description: "Memory gauge metrics (node_exporter-compatible)",
        metric_count: 5,
        yaml: include_str!("../../packs/node-exporter-memory.yaml"),
    },
];

/// Return the full catalog of built-in metric packs.
///
/// The returned slice is `&'static` — no allocation, no copying.
pub fn list() -> &'static [BuiltinPack] {
    CATALOG
}

/// Look up a built-in pack by its snake_case name.
///
/// Returns `None` if no pack with that name exists.
pub fn get(name: &str) -> Option<&'static BuiltinPack> {
    CATALOG.iter().find(|p| p.name == name)
}

/// Convenience function to get the raw YAML for a built-in pack.
///
/// Equivalent to `get(name).map(|p| p.yaml)`.
pub fn get_yaml(name: &str) -> Option<&'static str> {
    get(name).map(|p| p.yaml)
}

/// Return all built-in packs in a given category.
///
/// The category match is case-sensitive. Returns an empty `Vec` if no
/// packs belong to the requested category.
pub fn list_by_category(category: &str) -> Vec<&'static BuiltinPack> {
    CATALOG.iter().filter(|p| p.category == category).collect()
}

/// Return a formatted list of all available pack names.
///
/// Useful for error messages that want to hint at valid names.
pub fn available_names() -> Vec<&'static str> {
    CATALOG.iter().map(|p| p.name).collect()
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
/// - An override references a metric name not present in the pack.
pub fn expand_pack(
    pack: &MetricPackDef,
    config: &PackScenarioConfig,
) -> Result<Vec<ScenarioEntry>, SondaError> {
    if pack.metrics.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "metric pack has no metrics defined",
        )));
    }

    // Validate that all override keys match a metric in the pack.
    if let Some(ref overrides) = config.overrides {
        let metric_names: Vec<&str> = pack.metrics.iter().map(|m| m.name.as_str()).collect();
        for key in overrides.keys() {
            if !metric_names.contains(&key.as_str()) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "override references unknown metric {:?}; pack {:?} contains: {}",
                    key,
                    pack.name,
                    metric_names.join(", ")
                ))));
            }
        }
    }

    let mut entries = Vec::with_capacity(pack.metrics.len());

    for spec in &pack.metrics {
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

        // Look up override for this metric (by name).
        // For packs like node_exporter_cpu where the same metric name appears
        // multiple times with different `mode` labels, the override applies to
        // all instances sharing that name. This is intentional — the override
        // replaces the generator/labels for every series of that metric.
        let metric_override = config.overrides.as_ref().and_then(|o| o.get(&spec.name));

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
                jitter: None,
                jitter_seed: None,
            },
            generator,
            encoder: config.encoder.clone(),
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

    // ---- Catalog structure tests ------------------------------------------------

    #[test]
    fn catalog_is_not_empty() {
        assert!(
            !list().is_empty(),
            "built-in pack catalog must contain at least one pack"
        );
    }

    #[test]
    fn all_names_are_unique() {
        let names: Vec<&str> = list().iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            names.len(),
            sorted.len(),
            "duplicate pack names found in catalog"
        );
    }

    #[test]
    fn all_names_are_snake_case() {
        for pack in list() {
            assert!(
                pack.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "pack name {:?} must be snake_case (lowercase + underscores)",
                pack.name
            );
            assert!(!pack.name.is_empty(), "pack name must not be empty");
        }
    }

    #[test]
    fn all_categories_are_known() {
        let known = ["infrastructure", "network", "application", "observability"];
        for pack in list() {
            assert!(
                known.contains(&pack.category),
                "pack {:?} has unknown category {:?}; expected one of {:?}",
                pack.name,
                pack.category,
                known
            );
        }
    }

    #[test]
    fn all_descriptions_are_non_empty() {
        for pack in list() {
            assert!(
                !pack.description.is_empty(),
                "pack {:?} must have a non-empty description",
                pack.name
            );
        }
    }

    #[test]
    fn all_yamls_are_non_empty() {
        for pack in list() {
            assert!(
                !pack.yaml.is_empty(),
                "pack {:?} must have non-empty YAML",
                pack.name
            );
        }
    }

    #[test]
    fn all_metric_counts_are_positive() {
        for pack in list() {
            assert!(
                pack.metric_count > 0,
                "pack {:?} must have at least one metric (metric_count = {})",
                pack.name,
                pack.metric_count
            );
        }
    }

    // ---- YAML parsing tests (require `config` feature) --------------------------

    #[cfg(feature = "config")]
    #[test]
    fn all_pack_yamls_parse_as_metric_pack_def() {
        for pack in list() {
            let result = serde_yaml_ng::from_str::<MetricPackDef>(pack.yaml);
            assert!(
                result.is_ok(),
                "pack {:?} failed to parse: {:?}",
                pack.name,
                result.err()
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_pack_yamls_have_matching_metric_count() {
        for pack in list() {
            let def: MetricPackDef = serde_yaml_ng::from_str(pack.yaml).expect("pack must parse");
            assert_eq!(
                def.metrics.len(),
                pack.metric_count,
                "pack {:?} metric_count ({}) does not match parsed metrics ({})",
                pack.name,
                pack.metric_count,
                def.metrics.len()
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_pack_yamls_have_matching_name() {
        for pack in list() {
            let def: MetricPackDef = serde_yaml_ng::from_str(pack.yaml).expect("pack must parse");
            assert_eq!(
                def.name, pack.name,
                "pack {:?} YAML name {:?} does not match catalog name",
                pack.name, def.name
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn telegraf_snmp_interface_has_correct_metrics() {
        let pack = get("telegraf_snmp_interface").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack.yaml).expect("must parse");
        let names: Vec<&str> = def.metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"ifOperStatus"));
        assert!(names.contains(&"ifHCInOctets"));
        assert!(names.contains(&"ifHCOutOctets"));
        assert!(names.contains(&"ifInErrors"));
        assert!(names.contains(&"ifOutErrors"));
    }

    #[cfg(feature = "config")]
    #[test]
    fn node_exporter_cpu_has_eight_modes() {
        let pack = get("node_exporter_cpu").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack.yaml).expect("must parse");
        assert_eq!(def.metrics.len(), 8, "node_exporter_cpu must have 8 modes");

        // All metrics have the same name but different mode labels.
        for spec in &def.metrics {
            assert_eq!(spec.name, "node_cpu_seconds_total");
            let mode = spec
                .labels
                .as_ref()
                .and_then(|l| l.get("mode"))
                .expect("each spec must have a mode label");
            assert!(!mode.is_empty());
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn node_exporter_memory_has_five_metrics() {
        let pack = get("node_exporter_memory").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack.yaml).expect("must parse");
        assert_eq!(
            def.metrics.len(),
            5,
            "node_exporter_memory must have 5 metrics"
        );
    }

    // ---- Lookup function tests --------------------------------------------------

    #[test]
    fn get_existing_pack_returns_some() {
        let pack = get("telegraf_snmp_interface");
        assert!(
            pack.is_some(),
            "telegraf_snmp_interface must exist in catalog"
        );
        let p = pack.expect("checked above");
        assert_eq!(p.name, "telegraf_snmp_interface");
        assert_eq!(p.category, "network");
    }

    #[test]
    fn get_nonexistent_pack_returns_none() {
        assert!(
            get("nonexistent_pack").is_none(),
            "nonexistent pack must return None"
        );
    }

    #[test]
    fn get_yaml_returns_yaml_content() {
        let yaml = get_yaml("telegraf_snmp_interface");
        assert!(yaml.is_some());
        let content = yaml.expect("checked above");
        assert!(content.contains("name:"), "YAML must contain a name field");
    }

    #[test]
    fn get_yaml_nonexistent_returns_none() {
        assert!(get_yaml("does_not_exist").is_none());
    }

    #[test]
    fn list_by_category_network() {
        let network = list_by_category("network");
        assert!(
            !network.is_empty(),
            "network category must have at least one pack"
        );
        for p in &network {
            assert_eq!(p.category, "network");
        }
    }

    #[test]
    fn list_by_category_infrastructure() {
        let infra = list_by_category("infrastructure");
        assert!(
            !infra.is_empty(),
            "infrastructure category must have at least one pack"
        );
        for p in &infra {
            assert_eq!(p.category, "infrastructure");
        }
    }

    #[test]
    fn list_by_category_unknown_returns_empty() {
        let unknown = list_by_category("nonexistent-category");
        assert!(
            unknown.is_empty(),
            "unknown category must return empty list"
        );
    }

    #[test]
    fn available_names_matches_catalog_count() {
        let names = available_names();
        assert_eq!(
            names.len(),
            list().len(),
            "available_names must return one name per catalog entry"
        );
    }

    #[test]
    fn available_names_contains_telegraf_snmp_interface() {
        let names = available_names();
        assert!(
            names.contains(&"telegraf_snmp_interface"),
            "available_names must include telegraf_snmp_interface"
        );
    }

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
                    labels: None,
                    generator: None,
                },
                MetricSpec {
                    name: "metric_b".to_string(),
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
                labels: None,
                generator: None,
            }],
        };

        let mut override_labels = HashMap::new();
        override_labels.insert("extra".to_string(), "value".to_string());
        override_labels.insert("job".to_string(), "overridden".to_string());

        let mut overrides = HashMap::new();
        overrides.insert(
            "ifOperStatus".to_string(),
            MetricOverride {
                generator: None,
                labels: Some(override_labels),
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
    fn builtin_pack_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BuiltinPack>();
    }

    #[test]
    fn metric_pack_def_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricPackDef>();
    }

    // ---- Expansion with built-in packs (config feature) -------------------------

    #[cfg(feature = "config")]
    #[test]
    fn expand_builtin_telegraf_snmp_produces_five_entries() {
        let pack_entry = get("telegraf_snmp_interface").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack_entry.yaml).expect("must parse");

        let config = PackScenarioConfig {
            pack: "telegraf_snmp_interface".to_string(),
            rate: 1.0,
            duration: Some("10s".to_string()),
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&def, &config).expect("must succeed");
        assert_eq!(entries.len(), 5);
    }

    #[cfg(feature = "config")]
    #[test]
    fn expand_builtin_node_cpu_produces_eight_entries() {
        let pack_entry = get("node_exporter_cpu").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack_entry.yaml).expect("must parse");

        let config = PackScenarioConfig {
            pack: "node_exporter_cpu".to_string(),
            rate: 1.0,
            duration: Some("10s".to_string()),
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&def, &config).expect("must succeed");
        assert_eq!(entries.len(), 8);
    }

    #[cfg(feature = "config")]
    #[test]
    fn expand_builtin_node_memory_produces_five_entries() {
        let pack_entry = get("node_exporter_memory").expect("must exist");
        let def: MetricPackDef = serde_yaml_ng::from_str(pack_entry.yaml).expect("must parse");

        let config = PackScenarioConfig {
            pack: "node_exporter_memory".to_string(),
            rate: 1.0,
            duration: Some("10s".to_string()),
            labels: None,
            sink: SinkConfig::Stdout,
            encoder: EncoderConfig::PrometheusText { precision: None },
            overrides: None,
        };

        let entries = expand_pack(&def, &config).expect("must succeed");
        assert_eq!(entries.len(), 5);
    }

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
}
