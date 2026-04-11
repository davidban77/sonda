//! YAML parsing, schema validation, and version detection for v2 scenario files.
//!
//! The primary entry point is [`parse_v2`], which deserializes a YAML string
//! into a [`V2ScenarioFile`] and runs structural validation (version check,
//! id uniqueness, signal type validity, generator/pack mutual exclusion).
//!
//! [`detect_version`] is a lightweight helper that peeks at the `version` field
//! without fully parsing the file. It will be used by the version dispatch layer
//! (PR 6) to route between v1 and v2 parsing paths.

use std::collections::HashSet;

use super::{V2Entry, V2ScenarioFile};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced during v2 scenario parsing and validation.
#[derive(Debug, thiserror::Error)]
pub enum V2ParseError {
    /// The YAML could not be deserialized into the expected structure.
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),

    /// The `version` field is present but is not `2`.
    #[error("version must be 2, got {0}")]
    InvalidVersion(u32),

    /// Two or more entries share the same `id`.
    #[error("duplicate entry id: '{0}'")]
    DuplicateId(String),

    /// An entry has an unrecognized `signal_type`.
    #[error("entry {index}: invalid signal_type '{signal_type}', must be one of: metrics, logs, histogram, summary")]
    InvalidSignalType {
        /// Zero-based index of the offending entry.
        index: usize,
        /// The invalid signal type string.
        signal_type: String,
    },

    /// An entry specifies both `generator` and `pack`.
    #[error("entry {index}: must have either 'generator' or 'pack', not both")]
    GeneratorAndPack {
        /// Zero-based index of the offending entry.
        index: usize,
    },

    /// An entry specifies neither `generator`/`distribution` nor `pack`.
    #[error("entry {index}: must have either 'generator' (or 'distribution' for histogram/summary) or 'pack'")]
    MissingGeneratorOrPack {
        /// Zero-based index of the offending entry.
        index: usize,
    },

    /// An inline entry (non-pack) is missing the required `name` field.
    #[error("entry {index}: inline signal must have 'name'")]
    MissingName {
        /// Zero-based index of the offending entry.
        index: usize,
    },

    /// A pack entry has a `signal_type` other than `"metrics"`.
    #[error("entry {index}: pack entries must have signal_type 'metrics'")]
    PackNotMetrics {
        /// Zero-based index of the offending entry.
        index: usize,
    },

    /// An entry `id` does not match the allowed pattern `[a-zA-Z_][a-zA-Z0-9_]*`.
    #[error("entry id '{0}' is invalid: must match [a-zA-Z_][a-zA-Z0-9_]*")]
    InvalidId(String),

    /// An `after.op` value is not `"<"` or `">"`.
    #[error("entry {index}: after.op must be '<' or '>', got '{op}'")]
    InvalidAfterOp {
        /// Zero-based index of the offending entry.
        index: usize,
        /// The invalid operator string.
        op: String,
    },
}

// ---------------------------------------------------------------------------
// Recognized signal types
// ---------------------------------------------------------------------------

/// The set of valid `signal_type` values in a v2 entry.
const VALID_SIGNAL_TYPES: &[&str] = &["metrics", "logs", "histogram", "summary"];

/// Signal types that support a `distribution` field instead of `generator`.
const DISTRIBUTION_SIGNAL_TYPES: &[&str] = &["histogram", "summary"];

// ---------------------------------------------------------------------------
// Version detection
// ---------------------------------------------------------------------------

/// Peek at the `version` field in a YAML string without fully parsing it.
///
/// Returns `Some(n)` when the top-level mapping contains a `version` key with
/// an integer value, or `None` when the field is absent or cannot be parsed.
/// This is intentionally cheap — it deserializes into a minimal struct.
///
/// # Examples
///
/// ```
/// use sonda_core::v2::parse::detect_version;
///
/// assert_eq!(detect_version("version: 2\nscenarios: []"), Some(2));
/// assert_eq!(detect_version("version: 1"), Some(1));
/// assert_eq!(detect_version("name: cpu_usage\nrate: 1"), None);
/// ```
pub fn detect_version(yaml: &str) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        version: Option<u32>,
    }

    let probe: VersionProbe = serde_yaml_ng::from_str(yaml).ok()?;
    probe.version
}

// ---------------------------------------------------------------------------
// Single-signal shorthand support
// ---------------------------------------------------------------------------

/// A flat representation of a single-entry v2 file (no `scenarios:` key).
///
/// This is an internal deserialization target used to support the shorthand
/// format where the top-level YAML mapping contains entry fields directly.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct V2FlatFile {
    version: u32,

    // Defaults-level fields (also allowed at top level in shorthand)
    #[serde(default)]
    rate: Option<f64>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    encoder: Option<crate::encoder::EncoderConfig>,
    #[serde(default)]
    sink: Option<crate::sink::SinkConfig>,
    #[serde(default)]
    labels: Option<std::collections::BTreeMap<String, String>>,

    // Entry-level fields
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    signal_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    generator: Option<crate::generator::GeneratorConfig>,
    #[serde(default)]
    log_generator: Option<crate::generator::LogGeneratorConfig>,
    #[serde(default)]
    dynamic_labels: Option<Vec<crate::config::DynamicLabelConfig>>,
    #[serde(default)]
    jitter: Option<f64>,
    #[serde(default)]
    jitter_seed: Option<u64>,
    #[serde(default)]
    gaps: Option<crate::config::GapConfig>,
    #[serde(default)]
    bursts: Option<crate::config::BurstConfig>,
    #[serde(default)]
    cardinality_spikes: Option<Vec<crate::config::CardinalitySpikeConfig>>,
    #[serde(default)]
    phase_offset: Option<String>,
    #[serde(default)]
    clock_group: Option<String>,
    #[serde(default)]
    after: Option<super::AfterClause>,

    // Pack fields
    #[serde(default)]
    pack: Option<String>,
    #[serde(default)]
    overrides: Option<std::collections::BTreeMap<String, crate::packs::MetricOverride>>,

    // Histogram / summary fields
    #[serde(default)]
    distribution: Option<crate::config::DistributionConfig>,
    #[serde(default)]
    buckets: Option<Vec<f64>>,
    #[serde(default)]
    quantiles: Option<Vec<f64>>,
    #[serde(default)]
    observations_per_tick: Option<u32>,
    #[serde(default)]
    mean_shift_per_sec: Option<f64>,
    #[serde(default)]
    seed: Option<u64>,
}

impl V2FlatFile {
    /// Convert the flat representation into a [`V2ScenarioFile`] with a single entry.
    fn into_scenario_file(self) -> V2ScenarioFile {
        let signal_type = self.signal_type.unwrap_or_else(|| {
            if self.distribution.is_some() {
                if self.quantiles.is_some() {
                    "summary".to_string()
                } else {
                    "histogram".to_string()
                }
            } else if self.log_generator.is_some() {
                "logs".to_string()
            } else {
                "metrics".to_string()
            }
        });

        let entry = V2Entry {
            id: self.id,
            signal_type,
            name: self.name,
            rate: self.rate,
            duration: self.duration,
            generator: self.generator,
            log_generator: self.log_generator,
            labels: self.labels,
            dynamic_labels: self.dynamic_labels,
            encoder: self.encoder,
            sink: self.sink,
            jitter: self.jitter,
            jitter_seed: self.jitter_seed,
            gaps: self.gaps,
            bursts: self.bursts,
            cardinality_spikes: self.cardinality_spikes,
            phase_offset: self.phase_offset,
            clock_group: self.clock_group,
            after: self.after,
            pack: self.pack,
            overrides: self.overrides,
            distribution: self.distribution,
            buckets: self.buckets,
            quantiles: self.quantiles,
            observations_per_tick: self.observations_per_tick,
            mean_shift_per_sec: self.mean_shift_per_sec,
            seed: self.seed,
        };

        V2ScenarioFile {
            version: self.version,
            defaults: None,
            scenarios: vec![entry],
        }
    }
}

// ---------------------------------------------------------------------------
// Main parser
// ---------------------------------------------------------------------------

/// Parse a YAML string as a v2 scenario file.
///
/// Performs deserialization followed by structural validation:
///
/// 1. Version must be exactly `2`.
/// 2. Single-signal shorthand (no `scenarios:` key) is promoted to a
///    one-entry file.
/// 3. Entry `id` values must be unique and match `[a-zA-Z_][a-zA-Z0-9_]*`.
/// 4. `signal_type` must be one of `metrics`, `logs`, `histogram`, `summary`.
/// 5. Each entry has either `generator`/`distribution` or `pack`, not both.
/// 6. Pack entries must have `signal_type: metrics`.
/// 7. Inline (non-pack) entries must have `name`.
/// 8. `after.op` must be `"<"` or `">"`.
///
/// # Errors
///
/// Returns [`V2ParseError`] describing the first validation failure found.
pub fn parse_v2(yaml: &str) -> Result<V2ScenarioFile, V2ParseError> {
    let file = deserialize_v2(yaml)?;

    if file.version != 2 {
        return Err(V2ParseError::InvalidVersion(file.version));
    }

    validate_entries(&file.scenarios)?;
    Ok(file)
}

/// Attempt deserialization, falling back to single-signal shorthand.
fn deserialize_v2(yaml: &str) -> Result<V2ScenarioFile, V2ParseError> {
    // First, try the canonical multi-entry format.
    let canonical_result: Result<V2ScenarioFile, _> = serde_yaml_ng::from_str(yaml);
    if let Ok(file) = canonical_result {
        return Ok(file);
    }

    // If canonical parse fails, try the flat single-signal shorthand.
    let flat: V2FlatFile = serde_yaml_ng::from_str(yaml)?;
    Ok(flat.into_scenario_file())
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate all entries in a parsed scenario file.
fn validate_entries(entries: &[V2Entry]) -> Result<(), V2ParseError> {
    let mut seen_ids = HashSet::new();

    for (index, entry) in entries.iter().enumerate() {
        // Validate id format and uniqueness.
        if let Some(ref id) = entry.id {
            if !is_valid_id(id) {
                return Err(V2ParseError::InvalidId(id.clone()));
            }
            if !seen_ids.insert(id.clone()) {
                return Err(V2ParseError::DuplicateId(id.clone()));
            }
        }

        // Validate signal_type.
        if !VALID_SIGNAL_TYPES.contains(&entry.signal_type.as_str()) {
            return Err(V2ParseError::InvalidSignalType {
                index,
                signal_type: entry.signal_type.clone(),
            });
        }

        // Validate generator/pack mutual exclusion.
        let has_generator = entry.generator.is_some();
        let has_log_generator = entry.log_generator.is_some();
        let has_pack = entry.pack.is_some();
        let has_distribution = entry.distribution.is_some();
        let is_distribution_type = DISTRIBUTION_SIGNAL_TYPES.contains(&entry.signal_type.as_str());
        let is_logs = entry.signal_type == "logs";

        if (has_generator || has_log_generator || has_distribution) && has_pack {
            return Err(V2ParseError::GeneratorAndPack { index });
        }

        // For non-pack entries, validate the correct generator variant is present.
        if !has_pack {
            if is_distribution_type {
                if !has_distribution {
                    return Err(V2ParseError::MissingGeneratorOrPack { index });
                }
            } else if is_logs {
                if !has_log_generator {
                    return Err(V2ParseError::MissingGeneratorOrPack { index });
                }
            } else if !has_generator {
                return Err(V2ParseError::MissingGeneratorOrPack { index });
            }
        }

        // Pack entries must be metrics.
        if has_pack && entry.signal_type != "metrics" {
            return Err(V2ParseError::PackNotMetrics { index });
        }

        // Inline (non-pack) entries must have name.
        if !has_pack && entry.name.is_none() {
            return Err(V2ParseError::MissingName { index });
        }

        // Validate after.op.
        if let Some(ref after) = entry.after {
            if after.op != "<" && after.op != ">" {
                return Err(V2ParseError::InvalidAfterOp {
                    index,
                    op: after.op.clone(),
                });
            }
        }
    }

    Ok(())
}

/// Check whether an id string matches `[a-zA-Z_][a-zA-Z0-9_]*`.
fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::{AfterClause, V2Defaults};
    use super::*;

    // ======================================================================
    // Valid parse cases
    // ======================================================================

    #[test]
    fn multi_scenario_with_three_entries() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 1
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50

  - signal_type: logs
    name: syslog
    rate: 5
    log_generator:
      type: template
      templates:
        - message: "host={hostname} cpu={value}"
          field_pools:
            hostname: ["rtr-01", "rtr-02"]
            value: ["50", "90"]
      seed: 42

  - signal_type: metrics
    pack: telegraf_snmp_interface
    rate: 1
    labels:
      device: rtr-01
"#;

        let file = parse_v2(yaml).expect("must parse valid multi-scenario file");
        assert_eq!(file.version, 2);
        assert_eq!(file.scenarios.len(), 3);
        assert_eq!(file.scenarios[0].signal_type, "metrics");
        assert_eq!(file.scenarios[0].name.as_deref(), Some("cpu_usage"));
        assert_eq!(file.scenarios[1].signal_type, "logs");
        assert_eq!(
            file.scenarios[2].pack.as_deref(),
            Some("telegraf_snmp_interface")
        );
    }

    #[test]
    fn single_signal_shorthand_inline() {
        let yaml = r#"
version: 2
name: cpu_usage
signal_type: metrics
rate: 1
duration: 30s
generator:
  type: sine
  amplitude: 50
  period_secs: 60
  offset: 50
"#;

        let file = parse_v2(yaml).expect("must parse single-signal shorthand");
        assert_eq!(file.version, 2);
        assert!(file.defaults.is_none());
        assert_eq!(file.scenarios.len(), 1);

        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "metrics");
        assert_eq!(entry.name.as_deref(), Some("cpu_usage"));
        assert!(entry.generator.is_some());
        assert_eq!(entry.duration.as_deref(), Some("30s"));
    }

    #[test]
    fn single_signal_shorthand_pack() {
        let yaml = r#"
version: 2
pack: telegraf_snmp_interface
rate: 1
duration: 10s
labels:
  device: rtr-01
"#;

        let file = parse_v2(yaml).expect("must parse pack shorthand");
        assert_eq!(file.version, 2);
        assert_eq!(file.scenarios.len(), 1);

        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "metrics");
        assert_eq!(entry.pack.as_deref(), Some("telegraf_snmp_interface"));
        let labels = entry.labels.as_ref().expect("must have labels");
        assert_eq!(labels.get("device").map(String::as_str), Some("rtr-01"));
    }

    #[test]
    fn entry_with_after_clause() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu_usage
    id: cpu_signal
    rate: 1
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50

  - signal_type: metrics
    name: alert_metric
    rate: 1
    generator:
      type: constant
      value: 1.0
    after:
      ref: cpu_signal
      op: ">"
      value: 90.0
"#;

        let file = parse_v2(yaml).expect("must parse after clause");
        assert_eq!(file.scenarios.len(), 2);

        let after = file.scenarios[1]
            .after
            .as_ref()
            .expect("second entry must have after clause");
        assert_eq!(after.ref_id, "cpu_signal");
        assert_eq!(after.op, ">");
        assert!((after.value - 90.0).abs() < f64::EPSILON);
        assert!(after.delay.is_none());
    }

    #[test]
    fn entry_with_after_clause_and_delay() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: source
    id: src
    rate: 1
    generator:
      type: constant
      value: 100.0

  - signal_type: metrics
    name: dependent
    rate: 1
    generator:
      type: constant
      value: 1.0
    after:
      ref: src
      op: "<"
      value: 50.0
      delay: "5s"
"#;

        let file = parse_v2(yaml).expect("must parse after with delay");
        let after = file.scenarios[1]
            .after
            .as_ref()
            .expect("must have after clause");
        assert_eq!(after.op, "<");
        assert_eq!(after.delay.as_deref(), Some("5s"));
    }

    #[test]
    fn histogram_entry_with_distribution_and_buckets() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: histogram
    name: http_request_duration_seconds
    rate: 1
    distribution:
      type: exponential
      rate: 10.0
    buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    observations_per_tick: 100
    seed: 42
"#;

        let file = parse_v2(yaml).expect("must parse histogram entry");
        assert_eq!(file.scenarios.len(), 1);

        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "histogram");
        assert!(entry.distribution.is_some());
        let buckets = entry.buckets.as_ref().expect("must have buckets");
        assert_eq!(buckets.len(), 11);
        assert_eq!(entry.observations_per_tick, Some(100));
        assert_eq!(entry.seed, Some(42));
    }

    #[test]
    fn summary_entry_with_distribution_and_quantiles() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: summary
    name: rpc_duration_seconds
    rate: 1
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    quantiles: [0.5, 0.9, 0.99]
    observations_per_tick: 200
    seed: 99
"#;

        let file = parse_v2(yaml).expect("must parse summary entry");
        assert_eq!(file.scenarios.len(), 1);

        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "summary");
        assert!(entry.distribution.is_some());
        let quantiles = entry.quantiles.as_ref().expect("must have quantiles");
        assert_eq!(quantiles.len(), 3);
    }

    #[test]
    fn file_with_defaults_block() {
        let yaml = r#"
version: 2
defaults:
  rate: 10
  duration: "60s"
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    env: staging
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 50.0
"#;

        let file = parse_v2(yaml).expect("must parse file with defaults");
        let defaults = file.defaults.as_ref().expect("must have defaults");
        assert!((defaults.rate.expect("must have rate") - 10.0).abs() < f64::EPSILON);
        assert_eq!(defaults.duration.as_deref(), Some("60s"));
        assert!(defaults.encoder.is_some());
        assert!(defaults.sink.is_some());
        let labels = defaults.labels.as_ref().expect("must have labels");
        assert_eq!(labels.get("env").map(String::as_str), Some("staging"));
    }

    #[test]
    fn entry_with_all_optional_fields() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    id: full_entry
    name: fully_loaded_metric
    rate: 5
    duration: "120s"
    generator:
      type: sine
      amplitude: 10
      period_secs: 30
      offset: 50
    labels:
      job: test
      env: dev
    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    jitter: 2.5
    jitter_seed: 12345
    gaps:
      every: "2m"
      for: "20s"
    bursts:
      every: "10s"
      for: "2s"
      multiplier: 3.0
    cardinality_spikes:
      - label: pod_name
        every: "2m"
        for: "30s"
        cardinality: 500
    phase_offset: "5s"
    clock_group: group_a
"#;

        let file = parse_v2(yaml).expect("must parse entry with all optional fields");
        let entry = &file.scenarios[0];

        assert_eq!(entry.id.as_deref(), Some("full_entry"));
        assert_eq!(entry.name.as_deref(), Some("fully_loaded_metric"));
        assert!(entry.rate.is_some());
        assert!(entry.duration.is_some());
        assert!(entry.generator.is_some());
        assert!(entry.labels.is_some());
        assert!(entry.dynamic_labels.is_some());
        assert!(entry.encoder.is_some());
        assert!(entry.sink.is_some());
        assert!(entry.jitter.is_some());
        assert!(entry.jitter_seed.is_some());
        assert!(entry.gaps.is_some());
        assert!(entry.bursts.is_some());
        assert!(entry.cardinality_spikes.is_some());
        assert_eq!(entry.phase_offset.as_deref(), Some("5s"));
        assert_eq!(entry.clock_group.as_deref(), Some("group_a"));
    }

    // ======================================================================
    // Invalid cases
    // ======================================================================

    #[test]
    fn version_1_returns_invalid_version() {
        let yaml = r#"
version: 1
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("version 1 must fail");
        assert!(
            matches!(err, V2ParseError::InvalidVersion(1)),
            "expected InvalidVersion(1), got: {err}"
        );
    }

    #[test]
    fn missing_version_returns_yaml_error() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("missing version must fail");
        assert!(
            matches!(err, V2ParseError::Yaml(_)),
            "expected Yaml error, got: {err}"
        );
    }

    #[test]
    fn duplicate_ids_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    id: same_id
    name: metric_a
    generator:
      type: constant
      value: 1.0
  - signal_type: metrics
    id: same_id
    name: metric_b
    generator:
      type: constant
      value: 2.0
"#;

        let err = parse_v2(yaml).expect_err("duplicate ids must fail");
        assert!(
            matches!(err, V2ParseError::DuplicateId(ref id) if id == "same_id"),
            "expected DuplicateId('same_id'), got: {err}"
        );
    }

    #[test]
    fn invalid_signal_type_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: traces
    name: some_trace
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("invalid signal_type must fail");
        assert!(
            matches!(err, V2ParseError::InvalidSignalType { index: 0, ref signal_type } if signal_type == "traces"),
            "expected InvalidSignalType at index 0, got: {err}"
        );
    }

    #[test]
    fn both_generator_and_pack_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: mixed
    generator:
      type: constant
      value: 1.0
    pack: some_pack
"#;

        let err = parse_v2(yaml).expect_err("generator + pack must fail");
        assert!(
            matches!(err, V2ParseError::GeneratorAndPack { index: 0 }),
            "expected GeneratorAndPack at index 0, got: {err}"
        );
    }

    #[test]
    fn neither_generator_nor_pack_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: bare_entry
"#;

        let err = parse_v2(yaml).expect_err("missing generator/pack must fail");
        assert!(
            matches!(err, V2ParseError::MissingGeneratorOrPack { index: 0 }),
            "expected MissingGeneratorOrPack at index 0, got: {err}"
        );
    }

    #[test]
    fn pack_with_logs_signal_type_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: logs
    pack: some_log_pack
"#;

        let err = parse_v2(yaml).expect_err("pack + logs must fail");
        assert!(
            matches!(err, V2ParseError::PackNotMetrics { index: 0 }),
            "expected PackNotMetrics at index 0, got: {err}"
        );
    }

    #[test]
    fn logs_without_log_generator_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: logs
    name: bare_log
"#;

        let err = parse_v2(yaml).expect_err("logs without log_generator must fail");
        assert!(
            matches!(err, V2ParseError::MissingGeneratorOrPack { index: 0 }),
            "expected MissingGeneratorOrPack at index 0, got: {err}"
        );
    }

    #[test]
    fn inline_without_name_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("inline without name must fail");
        assert!(
            matches!(err, V2ParseError::MissingName { index: 0 }),
            "expected MissingName at index 0, got: {err}"
        );
    }

    #[test]
    fn invalid_id_starting_with_digit() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    id: 123abc
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("id starting with digit must fail");
        assert!(
            matches!(err, V2ParseError::InvalidId(ref id) if id == "123abc"),
            "expected InvalidId('123abc'), got: {err}"
        );
    }

    #[test]
    fn invalid_id_with_dot() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    id: my.id
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("id with dot must fail");
        assert!(
            matches!(err, V2ParseError::InvalidId(ref id) if id == "my.id"),
            "expected InvalidId('my.id'), got: {err}"
        );
    }

    #[test]
    fn invalid_after_op_returns_error() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: source
    id: src
    generator:
      type: constant
      value: 1.0

  - signal_type: metrics
    name: dependent
    generator:
      type: constant
      value: 1.0
    after:
      ref: src
      op: "=="
      value: 50.0
"#;

        let err = parse_v2(yaml).expect_err("invalid after op must fail");
        assert!(
            matches!(err, V2ParseError::InvalidAfterOp { index: 1, ref op } if op == "=="),
            "expected InvalidAfterOp at index 1 with op '==', got: {err}"
        );
    }

    // ======================================================================
    // Version detection tests
    // ======================================================================

    #[test]
    fn detect_version_v2() {
        let yaml = "version: 2\nscenarios: []";
        assert_eq!(detect_version(yaml), Some(2));
    }

    #[test]
    fn detect_version_v1_explicit() {
        let yaml = "version: 1\nname: test";
        assert_eq!(detect_version(yaml), Some(1));
    }

    #[test]
    fn detect_version_absent() {
        let yaml = "name: cpu_usage\nrate: 1";
        assert_eq!(detect_version(yaml), None);
    }

    // ======================================================================
    // ID validation unit tests
    // ======================================================================

    #[test]
    fn valid_ids() {
        assert!(is_valid_id("cpu_signal"));
        assert!(is_valid_id("_private"));
        assert!(is_valid_id("A"));
        assert!(is_valid_id("a1b2c3"));
        assert!(is_valid_id("__double_underscore__"));
    }

    #[test]
    fn invalid_ids() {
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("123abc"));
        assert!(!is_valid_id("my.id"));
        assert!(!is_valid_id("has-hyphen"));
        assert!(!is_valid_id("has space"));
        assert!(!is_valid_id("0"));
    }

    // ======================================================================
    // Error display tests
    // ======================================================================

    #[test]
    fn error_display_messages() {
        let err = V2ParseError::InvalidVersion(3);
        assert_eq!(err.to_string(), "version must be 2, got 3");

        let err = V2ParseError::DuplicateId("foo".to_string());
        assert_eq!(err.to_string(), "duplicate entry id: 'foo'");

        let err = V2ParseError::InvalidSignalType {
            index: 2,
            signal_type: "traces".to_string(),
        };
        assert!(err.to_string().contains("entry 2"));
        assert!(err.to_string().contains("traces"));

        let err = V2ParseError::GeneratorAndPack { index: 0 };
        assert!(err.to_string().contains("entry 0"));
        assert!(err.to_string().contains("not both"));

        let err = V2ParseError::MissingName { index: 1 };
        assert!(err.to_string().contains("entry 1"));
        assert!(err.to_string().contains("name"));

        let err = V2ParseError::PackNotMetrics { index: 0 };
        assert!(err.to_string().contains("metrics"));

        let err = V2ParseError::InvalidId("bad.id".to_string());
        assert!(err.to_string().contains("bad.id"));

        let err = V2ParseError::InvalidAfterOp {
            index: 1,
            op: "==".to_string(),
        };
        assert!(err.to_string().contains("=="));
    }

    // ======================================================================
    // Contract tests
    // ======================================================================

    #[test]
    fn error_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<V2ParseError>();
    }

    #[test]
    fn v2_scenario_file_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<V2ScenarioFile>();
        assert_send_sync::<V2Defaults>();
        assert_send_sync::<V2Entry>();
        assert_send_sync::<AfterClause>();
    }

    // ======================================================================
    // Histogram without distribution fails
    // ======================================================================

    #[test]
    fn histogram_without_distribution_fails() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: histogram
    name: bad_histogram
    buckets: [0.1, 0.5, 1.0]
"#;

        let err = parse_v2(yaml).expect_err("histogram without distribution must fail");
        assert!(
            matches!(err, V2ParseError::MissingGeneratorOrPack { index: 0 }),
            "expected MissingGeneratorOrPack, got: {err}"
        );
    }

    // ======================================================================
    // Pack with overrides
    // ======================================================================

    #[test]
    fn pack_entry_with_overrides() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    rate: 1
    overrides:
      ifOperStatus:
        generator:
          type: constant
          value: 0.0
        labels:
          alert: down
"#;

        let file = parse_v2(yaml).expect("must parse pack with overrides");
        let entry = &file.scenarios[0];
        let overrides = entry.overrides.as_ref().expect("must have overrides");
        assert!(overrides.contains_key("ifOperStatus"));
    }

    // ======================================================================
    // Edge case: empty id string
    // ======================================================================

    #[test]
    fn empty_id_string_returns_invalid_id() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    id: ""
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse_v2(yaml).expect_err("empty id must fail");
        assert!(
            matches!(err, V2ParseError::InvalidId(ref id) if id.is_empty()),
            "expected InvalidId(''), got: {err}"
        );
    }
}
