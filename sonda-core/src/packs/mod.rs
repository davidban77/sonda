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
//! Pack YAML files are **not embedded** in this crate. They live as standalone
//! files on the filesystem, discovered by the CLI via a search path. See the
//! `sonda` CLI crate for catalog/discovery logic.

use std::collections::{BTreeMap, HashMap};

use crate::compiler::AfterClause;
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
    /// Per-metric overrides keyed by metric name. Each override can replace
    /// the generator and/or add extra labels for a specific metric.
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
    /// Only used by the v2 compiler: when present, it replaces any
    /// entry-level `after` on the parent pack entry for this particular
    /// expanded signal. v1 pack expansion ignores this field.
    #[cfg_attr(feature = "config", serde(default))]
    pub after: Option<AfterClause>,
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
                after: None,
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
                after: None,
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
}
