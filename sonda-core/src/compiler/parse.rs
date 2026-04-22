//! YAML parsing, schema validation, and version detection for v2 scenario files.
//!
//! The primary entry point is [`parse`], which deserializes a YAML string
//! into a [`ScenarioFile`] and runs structural validation (version check,
//! id uniqueness, signal type validity, generator/pack mutual exclusion).
//!
//! [`detect_version`] is a lightweight helper that peeks at the `version` field
//! without fully parsing the file. It will be used by the version dispatch layer
//! (PR 6) to route between v1 and v2 parsing paths.

use std::collections::HashSet;

use super::{Entry, ScenarioFile};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced during v2 scenario parsing and validation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
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

    /// An entry has a generator field that is incompatible with its `signal_type`.
    ///
    /// For example, a `signal_type: metrics` entry must not have `log_generator`
    /// or `distribution`.
    #[error("entry {index}: signal_type '{signal_type}' must not have '{field}' field")]
    UnexpectedField {
        /// Zero-based index of the offending entry.
        index: usize,
        /// The signal type of the entry.
        signal_type: String,
        /// The field name that is not allowed for this signal type.
        field: String,
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
/// use sonda_core::compiler::parse::detect_version;
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
struct FlatFile {
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

impl FlatFile {
    /// Convert the flat representation into a [`ScenarioFile`] with a single entry.
    fn into_scenario_file(self) -> ScenarioFile {
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

        let entry = Entry {
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

        // Flat-form files deliberately do NOT expose the top-level metadata
        // fields (`scenario_name` / `category` / `description`). The shorthand
        // is the terse single-signal authoring shape; metadata belongs on the
        // canonical `scenarios:` form consumed by the CLI catalog probe.
        ScenarioFile {
            version: self.version,
            scenario_name: None,
            category: None,
            description: None,
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
/// 6. Cross-generator mutual exclusion: each signal type may only carry its
///    expected generator field (`generator` for metrics, `log_generator` for
///    logs, `distribution` for histogram/summary). The other fields must be
///    absent.
/// 7. Pack entries must have `signal_type: metrics`.
/// 8. Inline (non-pack) entries must have `name`.
///
/// Note: `after.ref` references are not resolved during parsing. Reference
/// resolution, threshold validation, and timing computation happen during
/// compilation (see the `after` compiler).
///
/// Note: `after.op` is deserialized as an [`AfterOp`](super::AfterOp) enum.
/// Invalid operator values (anything other than `"<"` or `">"`) are rejected
/// by serde during deserialization.
///
/// # Errors
///
/// Returns [`ParseError`] describing the first validation failure found.
pub fn parse(yaml: &str) -> Result<ScenarioFile, ParseError> {
    let file = deserialize(yaml)?;

    if file.version != 2 {
        return Err(ParseError::InvalidVersion(file.version));
    }

    validate_entries(&file.scenarios)?;
    Ok(file)
}

/// Determine the file shape and deserialize accordingly.
///
/// Instead of trying canonical parsing and falling back to flat on failure (which
/// produces confusing errors when a canonical file has a structural mistake), we
/// peek for the `scenarios` key first. If present, we parse as canonical. If
/// absent, we parse as flat shorthand. No fallback.
fn deserialize(yaml: &str) -> Result<ScenarioFile, ParseError> {
    /// Minimal probe to detect whether the YAML contains a `scenarios` key.
    /// Intentionally does NOT use `deny_unknown_fields`.
    #[derive(serde::Deserialize)]
    struct ShapeProbe {
        scenarios: Option<serde_yaml_ng::Value>,
    }

    let probe: ShapeProbe = serde_yaml_ng::from_str(yaml)?;

    if probe.scenarios.is_some() {
        // Canonical format: top-level `scenarios` array.
        let file: ScenarioFile = serde_yaml_ng::from_str(yaml)?;
        Ok(file)
    } else {
        // Flat single-signal shorthand: no `scenarios` key.
        let flat: FlatFile = serde_yaml_ng::from_str(yaml)?;
        Ok(flat.into_scenario_file())
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate all entries in a parsed scenario file.
fn validate_entries(entries: &[Entry]) -> Result<(), ParseError> {
    let mut seen_ids = HashSet::new();

    for (index, entry) in entries.iter().enumerate() {
        // Validate id format and uniqueness.
        if let Some(ref id) = entry.id {
            if !is_valid_id(id) {
                return Err(ParseError::InvalidId(id.clone()));
            }
            if !seen_ids.insert(id.clone()) {
                return Err(ParseError::DuplicateId(id.clone()));
            }
        }

        // Validate signal_type.
        if !VALID_SIGNAL_TYPES.contains(&entry.signal_type.as_str()) {
            return Err(ParseError::InvalidSignalType {
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
            return Err(ParseError::GeneratorAndPack { index });
        }

        // For non-pack entries, validate the correct generator variant is present.
        if !has_pack {
            if is_distribution_type {
                if !has_distribution {
                    return Err(ParseError::MissingGeneratorOrPack { index });
                }
            } else if is_logs {
                if !has_log_generator {
                    return Err(ParseError::MissingGeneratorOrPack { index });
                }
            } else if !has_generator {
                return Err(ParseError::MissingGeneratorOrPack { index });
            }
        }

        // Cross-generator mutual exclusion: ensure only the expected generator
        // field is set for each signal_type. The wrong fields must be absent.
        validate_no_unexpected_generator_fields(entry, index)?;

        // Pack entries must be metrics.
        if has_pack && entry.signal_type != "metrics" {
            return Err(ParseError::PackNotMetrics { index });
        }

        // Inline (non-pack) entries must have name.
        if !has_pack && entry.name.is_none() {
            return Err(ParseError::MissingName { index });
        }
    }

    Ok(())
}

/// Ensure that an entry does not carry generator fields incompatible with its
/// `signal_type`.
///
/// - `metrics`: allows `generator`, forbids `log_generator` and `distribution`
/// - `logs`: allows `log_generator`, forbids `generator` and `distribution`
/// - `histogram`/`summary`: allows `distribution`, forbids `generator` and `log_generator`
/// - `pack` (any signal_type with `pack`): forbids all three generator fields
///   (already checked upstream, but pack entries also pass through here safely
///   since they must be `metrics` and having no extra fields is fine)
fn validate_no_unexpected_generator_fields(entry: &Entry, index: usize) -> Result<(), ParseError> {
    let st = entry.signal_type.as_str();

    // Build list of fields that must NOT be present for this signal_type.
    let forbidden: &[(&str, bool)] = match st {
        "metrics" => &[
            ("log_generator", entry.log_generator.is_some()),
            ("distribution", entry.distribution.is_some()),
        ],
        "logs" => &[
            ("generator", entry.generator.is_some()),
            ("distribution", entry.distribution.is_some()),
        ],
        "histogram" | "summary" => &[
            ("generator", entry.generator.is_some()),
            ("log_generator", entry.log_generator.is_some()),
        ],
        // Pack-only or unknown signal_type (caught by earlier validation) —
        // all three generator fields should be absent.
        _ => &[
            ("generator", entry.generator.is_some()),
            ("log_generator", entry.log_generator.is_some()),
            ("distribution", entry.distribution.is_some()),
        ],
    };

    for &(field, present) in forbidden {
        if present {
            return Err(ParseError::UnexpectedField {
                index,
                signal_type: entry.signal_type.clone(),
                field: field.to_string(),
            });
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
    use super::super::{AfterClause, AfterOp, Defaults};
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

        let file = parse(yaml).expect("must parse valid multi-scenario file");
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

        let file = parse(yaml).expect("must parse single-signal shorthand");
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

        let file = parse(yaml).expect("must parse pack shorthand");
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

        let file = parse(yaml).expect("must parse after clause");
        assert_eq!(file.scenarios.len(), 2);

        let after = file.scenarios[1]
            .after
            .as_ref()
            .expect("second entry must have after clause");
        assert_eq!(after.ref_id, "cpu_signal");
        assert_eq!(after.op, AfterOp::GreaterThan);
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

        let file = parse(yaml).expect("must parse after with delay");
        let after = file.scenarios[1]
            .after
            .as_ref()
            .expect("must have after clause");
        assert_eq!(after.op, AfterOp::LessThan);
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

        let file = parse(yaml).expect("must parse histogram entry");
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

        let file = parse(yaml).expect("must parse summary entry");
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

        let file = parse(yaml).expect("must parse file with defaults");
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

        let file = parse(yaml).expect("must parse entry with all optional fields");
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

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::version_1(r#"
version: 1
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
"#, 1)]
    #[case::version_0(r#"
version: 0
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
"#, 0)]
    fn unsupported_version_returns_invalid_version(#[case] yaml: &str, #[case] expected: u32) {
        let err = parse(yaml).expect_err("unsupported version must fail");
        assert!(
            matches!(err, ParseError::InvalidVersion(v) if v == expected),
            "expected InvalidVersion({expected}), got: {err}"
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

        let err = parse(yaml).expect_err("missing version must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
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

        let err = parse(yaml).expect_err("duplicate ids must fail");
        assert!(
            matches!(err, ParseError::DuplicateId(ref id) if id == "same_id"),
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

        let err = parse(yaml).expect_err("invalid signal_type must fail");
        assert!(
            matches!(err, ParseError::InvalidSignalType { index: 0, ref signal_type } if signal_type == "traces"),
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

        let err = parse(yaml).expect_err("generator + pack must fail");
        assert!(
            matches!(err, ParseError::GeneratorAndPack { index: 0 }),
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

        let err = parse(yaml).expect_err("missing generator/pack must fail");
        assert!(
            matches!(err, ParseError::MissingGeneratorOrPack { index: 0 }),
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

        let err = parse(yaml).expect_err("pack + logs must fail");
        assert!(
            matches!(err, ParseError::PackNotMetrics { index: 0 }),
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

        let err = parse(yaml).expect_err("logs without log_generator must fail");
        assert!(
            matches!(err, ParseError::MissingGeneratorOrPack { index: 0 }),
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

        let err = parse(yaml).expect_err("inline without name must fail");
        assert!(
            matches!(err, ParseError::MissingName { index: 0 }),
            "expected MissingName at index 0, got: {err}"
        );
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::starts_with_digit(r#"
version: 2
scenarios:
  - signal_type: metrics
    id: 123abc
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#, "123abc")]
    #[case::contains_dot(r#"
version: 2
scenarios:
  - signal_type: metrics
    id: my.id
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#, "my.id")]
    #[case::empty_string(r#"
version: 2
scenarios:
  - signal_type: metrics
    id: ""
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#, "")]
    fn invalid_id_returns_invalid_id_error(#[case] yaml: &str, #[case] expected_id: &str) {
        let err = parse(yaml).expect_err("invalid id must fail");
        assert!(
            matches!(err, ParseError::InvalidId(ref id) if id == expected_id),
            "expected InvalidId({expected_id:?}), got: {err}"
        );
    }

    #[test]
    fn invalid_after_op_returns_yaml_error() {
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

        let err = parse(yaml).expect_err("invalid after op must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
            "expected Yaml error for invalid op, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("=="),
            "error message should mention the invalid op '==', got: {msg}"
        );
    }

    // ======================================================================
    // Version detection tests
    // ======================================================================

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::v2("version: 2\nscenarios: []",  Some(2))]
    #[case::v1_explicit("version: 1\nname: test", Some(1))]
    #[case::absent("name: cpu_usage\nrate: 1",    None)]
    // Malformed YAML must surface as `None` rather than panicking — callers
    // rely on `detect_version` as a lightweight pre-flight probe.
    #[case::unparseable("not valid yaml {",       None)]
    fn detect_version_cases(#[case] yaml: &str, #[case] expected: Option<u32>) {
        assert_eq!(detect_version(yaml), expected);
    }

    // ======================================================================
    // ID validation unit tests
    // ======================================================================

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::simple_snake("cpu_signal",            true)]
    #[case::leading_underscore("_private",        true)]
    #[case::single_upper("A",                     true)]
    #[case::alphanumeric("a1b2c3",                true)]
    #[case::double_underscore("__double_underscore__", true)]
    #[case::empty("",                             false)]
    #[case::starts_with_digit("123abc",           false)]
    #[case::contains_dot("my.id",                 false)]
    #[case::contains_hyphen("has-hyphen",         false)]
    #[case::contains_space("has space",           false)]
    #[case::single_digit("0",                     false)]
    fn id_validation_cases(#[case] id: &str, #[case] expected: bool) {
        assert_eq!(is_valid_id(id), expected, "is_valid_id({id:?})");
    }

    // ======================================================================
    // Error display tests
    // ======================================================================

    #[test]
    fn error_display_messages() {
        let err = ParseError::InvalidVersion(3);
        assert_eq!(err.to_string(), "version must be 2, got 3");

        let err = ParseError::DuplicateId("foo".to_string());
        assert_eq!(err.to_string(), "duplicate entry id: 'foo'");

        let err = ParseError::InvalidSignalType {
            index: 2,
            signal_type: "traces".to_string(),
        };
        assert!(err.to_string().contains("entry 2"));
        assert!(err.to_string().contains("traces"));

        let err = ParseError::GeneratorAndPack { index: 0 };
        assert!(err.to_string().contains("entry 0"));
        assert!(err.to_string().contains("not both"));

        let err = ParseError::MissingName { index: 1 };
        assert!(err.to_string().contains("entry 1"));
        assert!(err.to_string().contains("name"));

        let err = ParseError::PackNotMetrics { index: 0 };
        assert!(err.to_string().contains("metrics"));

        let err = ParseError::InvalidId("bad.id".to_string());
        assert!(err.to_string().contains("bad.id"));
    }

    // ======================================================================
    // Contract tests
    // ======================================================================

    #[test]
    fn error_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ParseError>();
    }

    #[test]
    fn v2_scenario_file_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScenarioFile>();
        assert_send_sync::<Defaults>();
        assert_send_sync::<Entry>();
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

        let err = parse(yaml).expect_err("histogram without distribution must fail");
        assert!(
            matches!(err, ParseError::MissingGeneratorOrPack { index: 0 }),
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

        let file = parse(yaml).expect("must parse pack with overrides");
        let entry = &file.scenarios[0];
        let overrides = entry.overrides.as_ref().expect("must have overrides");
        assert!(overrides.contains_key("ifOperStatus"));
    }

    // ======================================================================
    // Cross-generator mutual exclusion tests
    // ======================================================================

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::metrics_with_log_generator(r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
    log_generator:
      type: template
      templates:
        - message: "hello"
      seed: 1
"#, "metrics", "log_generator")]
    #[case::metrics_with_distribution(r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
"#, "metrics", "distribution")]
    #[case::logs_with_generator(r#"
version: 2
scenarios:
  - signal_type: logs
    name: syslog
    log_generator:
      type: template
      templates:
        - message: "hello"
      seed: 1
    generator:
      type: constant
      value: 1.0
"#, "logs", "generator")]
    #[case::logs_with_distribution(r#"
version: 2
scenarios:
  - signal_type: logs
    name: syslog
    log_generator:
      type: template
      templates:
        - message: "hello"
      seed: 1
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
"#, "logs", "distribution")]
    #[case::histogram_with_generator(r#"
version: 2
scenarios:
  - signal_type: histogram
    name: request_duration
    distribution:
      type: exponential
      rate: 10.0
    buckets: [0.1, 0.5, 1.0]
    generator:
      type: constant
      value: 1.0
"#, "histogram", "generator")]
    #[case::histogram_with_log_generator(r#"
version: 2
scenarios:
  - signal_type: histogram
    name: request_duration
    distribution:
      type: exponential
      rate: 10.0
    buckets: [0.1, 0.5, 1.0]
    log_generator:
      type: template
      templates:
        - message: "hello"
      seed: 1
"#, "histogram", "log_generator")]
    #[case::summary_with_generator(r#"
version: 2
scenarios:
  - signal_type: summary
    name: rpc_duration
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    quantiles: [0.5, 0.9, 0.99]
    generator:
      type: constant
      value: 1.0
"#, "summary", "generator")]
    fn mismatched_generator_family_returns_unexpected_field(
        #[case] yaml: &str,
        #[case] expected_signal_type: &str,
        #[case] expected_field: &str,
    ) {
        let err = parse(yaml).expect_err("mismatched generator family must fail");
        assert!(
            matches!(
                err,
                ParseError::UnexpectedField { index: 0, ref signal_type, ref field }
                if signal_type == expected_signal_type && field == expected_field
            ),
            "expected UnexpectedField for {expected_field} on {expected_signal_type}, got: {err}"
        );
    }

    // ======================================================================
    // Fallback parse error clarity
    // ======================================================================

    #[test]
    fn malformed_canonical_file_does_not_produce_misleading_error() {
        // A canonical file (has `scenarios:`) with a structural error (unknown
        // field `bogus` inside an entry). The old fallback approach would try
        // flat parsing and produce a confusing "unknown field `scenarios`" error.
        // With the ShapeProbe approach, we should get a clear error about the
        // actual problem inside the canonical parse path.
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
    bogus: unexpected_field
"#;

        let err = parse(yaml).expect_err("malformed canonical file must fail");
        let msg = err.to_string();
        // The error should mention the actual problem (unknown field `bogus`),
        // not the misleading "unknown field `scenarios`".
        assert!(
            !msg.contains("unknown field `scenarios`"),
            "error must not mention 'unknown field scenarios', got: {msg}"
        );
        assert!(
            msg.contains("bogus"),
            "error should reference the actual unknown field 'bogus', got: {msg}"
        );
    }

    #[test]
    fn unexpected_field_error_display_message() {
        let err = ParseError::UnexpectedField {
            index: 1,
            signal_type: "metrics".to_string(),
            field: "log_generator".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "entry 1: signal_type 'metrics' must not have 'log_generator' field"
        );
    }

    // ======================================================================
    // Edge cases (NOTE 2)
    // ======================================================================

    #[test]
    fn empty_scenarios_list_parses_successfully() {
        // An empty scenarios array is syntactically valid at the parse level.
        // Semantic rejection (no runnable entries) is deferred to compilation.
        let yaml = r#"
version: 2
scenarios: []
"#;

        let file = parse(yaml).expect("empty scenarios list should parse");
        assert_eq!(file.version, 2);
        assert!(file.scenarios.is_empty());
    }

    #[test]
    fn deny_unknown_fields_rejects_typo() {
        // A misspelling of `signal_type` as `signal_typ` must produce a YAML
        // parse error (via deny_unknown_fields), not silently default to None.
        let yaml = r#"
version: 2
scenarios:
  - signal_typ: metrics
    name: cpu
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse(yaml).expect_err("typo in field name must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
            "expected Yaml error for unknown field, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("signal_typ"),
            "error should mention the typo 'signal_typ', got: {msg}"
        );
    }

    // ======================================================================
    // Shorthand signal_type inference tests
    //
    // When `signal_type` is omitted from a flat (single-signal) file,
    // `FlatFile::into_scenario_file` infers it from which generator-family
    // field is present:
    //   - `distribution` + `quantiles`   → "summary"
    //   - `distribution` (no quantiles)  → "histogram"
    //   - `log_generator`                → "logs"
    //   - else                           → "metrics"
    // ======================================================================

    #[test]
    fn shorthand_infers_histogram_from_distribution_and_buckets() {
        // No explicit `signal_type` — presence of `distribution` without
        // `quantiles` must infer `histogram`.
        let yaml = r#"
version: 2
name: http_request_duration_seconds
rate: 1
distribution:
  type: exponential
  rate: 10.0
buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]
observations_per_tick: 100
seed: 42
"#;

        let file = parse(yaml).expect("must parse histogram shorthand");
        assert_eq!(file.scenarios.len(), 1);
        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "histogram");
        assert_eq!(entry.name.as_deref(), Some("http_request_duration_seconds"));
        assert!(entry.distribution.is_some());
        assert!(entry.buckets.is_some());
        assert!(entry.quantiles.is_none());
    }

    #[test]
    fn shorthand_infers_summary_from_distribution_and_quantiles() {
        // No explicit `signal_type` — presence of `distribution` with
        // `quantiles` must infer `summary`.
        let yaml = r#"
version: 2
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

        let file = parse(yaml).expect("must parse summary shorthand");
        assert_eq!(file.scenarios.len(), 1);
        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "summary");
        assert!(entry.distribution.is_some());
        assert!(entry.quantiles.is_some());
    }

    #[test]
    fn shorthand_infers_logs_from_log_generator() {
        // No explicit `signal_type` — presence of `log_generator` must
        // infer `logs`.
        let yaml = r#"
version: 2
name: syslog
rate: 5
log_generator:
  type: template
  templates:
    - message: "host={hostname} value={value}"
      field_pools:
        hostname: ["rtr-01", "rtr-02"]
        value: ["50", "90"]
  seed: 42
"#;

        let file = parse(yaml).expect("must parse logs shorthand");
        assert_eq!(file.scenarios.len(), 1);
        let entry = &file.scenarios[0];
        assert_eq!(entry.signal_type, "logs");
        assert_eq!(entry.name.as_deref(), Some("syslog"));
        assert!(entry.log_generator.is_some());
        assert!(entry.generator.is_none());
    }

    #[test]
    fn shorthand_with_defaults_key_is_rejected() {
        // The flat (shorthand) format does not have a `defaults` field.
        // Since FlatFile uses deny_unknown_fields, including `defaults:`
        // in a flat file must produce a YAML parse error.
        let yaml = r#"
version: 2
name: cpu_usage
signal_type: metrics
generator:
  type: constant
  value: 1.0
defaults:
  rate: 10
"#;

        let err = parse(yaml).expect_err("defaults in shorthand must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
            "expected Yaml error for defaults in shorthand, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("defaults"),
            "error should mention 'defaults', got: {msg}"
        );
    }

    // ======================================================================
    // Catalog metadata roundtrip tests (Option 1 of ADR
    // `docs/refactor/adr-v2-catalog-metadata.md`)
    //
    // `scenario_name`, `category`, and `description` are optional top-level
    // fields on [`ScenarioFile`]. They are metadata consumed by the CLI
    // catalog probe (v1↔v2 parity) and ignored by every compiler phase.
    // The parser must preserve them verbatim.
    // ======================================================================

    #[test]
    fn metadata_all_fields_present_roundtrip() {
        // All three metadata fields at the root are preserved on the parsed
        // AST exactly as written in the YAML.
        let yaml = r#"
version: 2
scenario_name: steady-state
category: infrastructure
description: "Normal oscillating baseline (sine + jitter)"
scenarios:
  - signal_type: metrics
    name: node_cpu_usage_idle_percent
    rate: 1
    generator:
      type: constant
      value: 1.0
"#;

        let file = parse(yaml).expect("must parse file with full metadata");
        assert_eq!(file.scenario_name.as_deref(), Some("steady-state"));
        assert_eq!(file.category.as_deref(), Some("infrastructure"));
        assert_eq!(
            file.description.as_deref(),
            Some("Normal oscillating baseline (sine + jitter)")
        );
        // Compiler input remains untouched.
        assert_eq!(file.scenarios.len(), 1);
        assert_eq!(file.scenarios[0].signal_type, "metrics");
    }

    #[test]
    fn metadata_absent_leaves_fields_none() {
        // A v2 file without any metadata fields parses cleanly and the AST
        // reports `None` for all three. This is the shape every v2 fixture
        // and test file written before PR 8a will continue to produce, so
        // existing v2 callers are unaffected by the field additions.
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#;

        let file = parse(yaml).expect("must parse file without metadata");
        assert!(file.scenario_name.is_none());
        assert!(file.category.is_none());
        assert!(file.description.is_none());
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::only_scenario_name(r#"
version: 2
scenario_name: solo-name
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#, Some("solo-name"), None,                None)]
    #[case::only_category(r#"
version: 2
category: network
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#, None,              Some("network"),     None)]
    #[case::only_description(r#"
version: 2
description: "terse one-liner"
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#, None,              None,                Some("terse one-liner"))]
    #[case::name_and_category(r#"
version: 2
scenario_name: partial
category: application
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#, Some("partial"),   Some("application"), None)]
    fn metadata_partial_roundtrip(
        #[case] yaml: &str,
        #[case] expected_name: Option<&str>,
        #[case] expected_category: Option<&str>,
        #[case] expected_description: Option<&str>,
    ) {
        let file = parse(yaml).expect("must parse partial-metadata file");
        assert_eq!(file.scenario_name.as_deref(), expected_name);
        assert_eq!(file.category.as_deref(), expected_category);
        assert_eq!(file.description.as_deref(), expected_description);
    }

    #[test]
    fn metadata_unknown_field_is_rejected_by_deny_unknown_fields() {
        // `deny_unknown_fields` stays on `ScenarioFile` after the metadata
        // additions. A typo on an adjacent metadata key (e.g. `descripton`)
        // must still surface as a YAML parse error, not silently default
        // to `None`.
        let yaml = r#"
version: 2
scenario_name: typo-test
descripton: "misspelled — must be rejected"
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse(yaml).expect_err("unknown metadata field must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
            "expected Yaml error for unknown field, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("descripton"),
            "error should mention the misspelled field, got: {msg}"
        );
    }

    #[test]
    fn metadata_on_entry_is_rejected() {
        // Metadata lives at the top level only. Placing `category` inside a
        // scenario entry must be rejected by `Entry`'s
        // `deny_unknown_fields` — metadata is not per-entry and must not
        // silently leak through.
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    category: infrastructure
    generator:
      type: constant
      value: 1.0
"#;

        let err = parse(yaml).expect_err("metadata on entry must fail");
        assert!(
            matches!(err, ParseError::Yaml(_)),
            "expected Yaml error for entry-level metadata, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("category"),
            "error should mention the misplaced field, got: {msg}"
        );
    }
}
