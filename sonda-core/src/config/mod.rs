//! Scenario configuration types and validation.
//!
//! The `Deserialize` impls on all config types are available only when the
//! `config` Cargo feature is enabled (active by default). Without the feature,
//! configs can still be constructed in code — only YAML/JSON parsing is gated.

pub mod validate;

use std::collections::HashMap;

use crate::encoder::EncoderConfig;
use crate::generator::{CsvColumnSpec, GeneratorConfig, LogGeneratorConfig};
use crate::sink::SinkConfig;
use crate::{ConfigError, SondaError};

/// Gap window configuration — a recurring silent period within a scenario.
///
/// During a gap the scheduler emits no events. The gap repeats on a fixed
/// cycle defined by `every`, and each instance lasts for `for`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct GapConfig {
    /// How often the gap recurs (e.g. `"2m"`).
    pub every: String,
    /// How long each gap lasts (e.g. `"20s"`). Must be less than `every`.
    pub r#for: String,
}

/// Strategy for generating unique label values during a cardinality spike.
///
/// Determines how the spike window produces distinct values for the injected
/// label key on each tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "config", serde(rename_all = "snake_case"))]
pub enum SpikeStrategy {
    /// Sequential counter: `prefix + (tick % cardinality)`.
    ///
    /// Produces deterministic, predictable label values without needing a seed.
    #[default]
    Counter,
    /// Deterministic random: SplitMix64 hash of `seed ^ tick`, formatted as hex.
    ///
    /// Produces label values that look random but are reproducible given the
    /// same seed.
    Random,
}

/// Configuration for a cardinality spike — a recurring window that injects
/// dynamic label values to simulate cardinality explosions.
///
/// During the spike window, a label key is injected with one of `cardinality`
/// unique values per tick. Outside the window, the label key is absent.
///
/// # Example YAML
///
/// ```yaml
/// cardinality_spikes:
///   - label: pod_name
///     every: 2m
///     for: 30s
///     cardinality: 500
///     strategy: counter
///     prefix: "pod-"
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct CardinalitySpikeConfig {
    /// The label key to inject during the spike window.
    ///
    /// Must be a valid Prometheus label key: `[a-zA-Z_][a-zA-Z0-9_]*`.
    pub label: String,
    /// How often the spike recurs (e.g. `"2m"`).
    pub every: String,
    /// How long each spike lasts (e.g. `"30s"`). Must be less than `every`.
    pub r#for: String,
    /// Number of unique label values generated during the spike.
    ///
    /// Must be greater than zero.
    pub cardinality: u64,
    /// Strategy for generating unique label values.
    ///
    /// Defaults to `counter` if not specified.
    #[cfg_attr(feature = "config", serde(default))]
    pub strategy: SpikeStrategy,
    /// Optional prefix for generated label values.
    ///
    /// Defaults to `"{label}_"` when not specified.
    #[cfg_attr(feature = "config", serde(default))]
    pub prefix: Option<String>,
    /// Optional RNG seed for the `random` strategy.
    ///
    /// Ignored for the `counter` strategy.
    #[cfg_attr(feature = "config", serde(default))]
    pub seed: Option<u64>,
}

/// Strategy for generating dynamic label values.
///
/// Determines how a [`DynamicLabelConfig`] produces per-tick values for the
/// label key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "config", serde(untagged))]
pub enum DynamicLabelStrategy {
    /// Cycle through an explicit list of values.
    ///
    /// The label value at each tick is `values[tick % values.len()]`.
    /// Cardinality is implicit (length of the list).
    ValuesList {
        /// The explicit list of label values to cycle through.
        values: Vec<String>,
    },
    /// Sequential counter: `"{prefix}{tick % cardinality}"`.
    ///
    /// Produces deterministic, predictable label values that cycle through
    /// `cardinality` distinct values indefinitely.
    Counter {
        /// Prefix prepended to the counter index (e.g. `"host-"` produces
        /// `"host-0"`, `"host-1"`, ...).
        #[cfg_attr(feature = "config", serde(default))]
        prefix: Option<String>,
        /// Number of unique label values in the cycle. Must be greater than zero.
        cardinality: u64,
    },
}

/// Configuration for a dynamic label — an always-on rotating label value
/// attached to every emitted event.
///
/// Unlike [`CardinalitySpikeConfig`], dynamic labels are not time-windowed:
/// they appear in every event for the lifetime of the scenario. This enables
/// simulating a stable fleet of N distinct sources (e.g., 10 hostnames, 5 pod
/// names) without a spike/window concept.
///
/// # Example YAML (counter strategy)
///
/// ```yaml
/// dynamic_labels:
///   - key: hostname
///     prefix: "host-"
///     cardinality: 10
/// ```
///
/// # Example YAML (values list strategy)
///
/// ```yaml
/// dynamic_labels:
///   - key: region
///     values: [us-east-1, us-west-2, eu-west-1]
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct DynamicLabelConfig {
    /// The label key to attach to every event.
    ///
    /// Must be a valid Prometheus label key: `[a-zA-Z_][a-zA-Z0-9_]*`.
    pub key: String,
    /// The strategy for generating per-tick label values.
    ///
    /// Deserialized via untagged enum: provide either `values: [...]` or
    /// `prefix: / cardinality:` fields directly alongside `key:`.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub strategy: DynamicLabelStrategy,
}

/// Burst window configuration — a recurring high-rate period within a scenario.
///
/// During a burst the event rate is multiplied by `multiplier`. The burst
/// repeats on a fixed cycle defined by `every`, and each instance lasts for `for`.
///
/// If a gap and burst overlap in time, the gap takes priority and no events
/// are emitted.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct BurstConfig {
    /// How often the burst recurs (e.g. `"10s"`).
    pub every: String,
    /// How long each burst lasts (e.g. `"2s"`). Must be less than `every`.
    pub r#for: String,
    /// Rate multiplier during the burst (must be strictly positive).
    pub multiplier: f64,
}

#[cfg(feature = "config")]
fn default_encoder() -> EncoderConfig {
    EncoderConfig::PrometheusText { precision: None }
}

#[cfg(feature = "config")]
fn default_log_encoder() -> EncoderConfig {
    EncoderConfig::JsonLines { precision: None }
}

#[cfg(feature = "config")]
fn default_sink() -> SinkConfig {
    SinkConfig::Stdout
}

/// Shared schedule and delivery fields common to all signal types.
///
/// Both [`ScenarioConfig`] (metrics) and [`LogScenarioConfig`] (logs) embed
/// this struct via `#[serde(flatten)]`. It contains every field that is
/// identical across signal types — everything except the generator
/// configuration and the encoder default.
///
/// New schedule-level fields (rate control, windows, labels, sink, phase
/// offset) should be added here once and automatically propagate to both
/// signal types.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct BaseScheduleConfig {
    /// Scenario name (metric name for metrics, identifier for logs).
    pub name: String,
    /// Target event rate in events per second. Must be strictly positive.
    pub rate: f64,
    /// Optional total run duration (e.g. `"30s"`, `"5m"`). `None` means run indefinitely.
    #[cfg_attr(feature = "config", serde(default))]
    pub duration: Option<String>,
    /// Optional gap window: recurring silent periods in the event stream.
    #[cfg_attr(feature = "config", serde(default))]
    pub gaps: Option<GapConfig>,
    /// Optional burst window: recurring high-rate periods in the event stream.
    ///
    /// When both a gap and a burst overlap in time, the gap takes priority.
    #[cfg_attr(feature = "config", serde(default))]
    pub bursts: Option<BurstConfig>,
    /// Optional cardinality spikes: recurring windows that inject dynamic
    /// labels to simulate cardinality explosions.
    #[cfg_attr(feature = "config", serde(default))]
    pub cardinality_spikes: Option<Vec<CardinalitySpikeConfig>>,
    /// Optional dynamic labels: always-on rotating label values that cycle
    /// through a fixed set of values on every tick.
    ///
    /// Unlike [`CardinalitySpikeConfig`], dynamic labels are never gated by a
    /// time window — they appear in every emitted event. Use this to simulate
    /// a fleet of N hosts, pods, or regions.
    #[cfg_attr(feature = "config", serde(default))]
    pub dynamic_labels: Option<Vec<DynamicLabelConfig>>,
    /// Static labels attached to every emitted event.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
    /// Output sink. Defaults to `stdout`.
    #[cfg_attr(feature = "config", serde(default = "default_sink"))]
    pub sink: SinkConfig,
    /// Delay before starting this scenario, relative to the group start time.
    ///
    /// Only meaningful in multi-scenario mode. Enables temporal correlation
    /// between scenarios: "metric A starts immediately, metric B starts 30s
    /// later". Accepts a duration string (e.g. `"30s"`, `"1m"`, `"500ms"`).
    #[cfg_attr(feature = "config", serde(default))]
    pub phase_offset: Option<String>,
    /// Clock group identifier for multi-scenario correlation.
    ///
    /// Scenarios with the same `clock_group` value share a common start time
    /// reference. For MVP this provides a shared start reference only; advanced
    /// cross-scenario signaling is deferred to a future phase.
    #[cfg_attr(feature = "config", serde(default))]
    pub clock_group: Option<String>,
    /// Optional jitter amplitude. When set, adds uniform noise in
    /// `[-jitter, +jitter]` to every generated value. Defaults to `None` (no jitter).
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter: Option<f64>,
    /// Optional seed for jitter noise. Defaults to `0` when absent.
    /// Different seeds produce different noise sequences.
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter_seed: Option<u64>,
}

/// Full configuration for a single metric scenario run.
///
/// Embeds [`BaseScheduleConfig`] for the shared schedule and delivery fields,
/// adding only the metric-specific value generator and a Prometheus-defaulting
/// encoder.
///
/// Fields from [`BaseScheduleConfig`] are accessible directly via `Deref` (e.g.
/// `config.name`, `config.rate`) for ergonomic read access. Struct construction
/// uses the explicit `base` field.
///
/// # Example YAML
///
/// ```yaml
/// name: interface_oper_state
/// rate: 1000
/// duration: 30s
/// generator:
///   type: sine
///   amplitude: 5.0
///   period_secs: 30
///   offset: 10.0
/// gaps:
///   every: 2m
///   for: 20s
/// labels:
///   hostname: t0-a1
///   zone: eu1
/// encoder:
///   type: prometheus_text
/// sink:
///   type: stdout
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct ScenarioConfig {
    /// Shared schedule and delivery fields.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub base: BaseScheduleConfig,
    /// Value generator configuration.
    pub generator: GeneratorConfig,
    /// Output encoder. Defaults to `prometheus_text`.
    #[cfg_attr(feature = "config", serde(default = "default_encoder"))]
    pub encoder: EncoderConfig,
}

impl std::ops::Deref for ScenarioConfig {
    type Target = BaseScheduleConfig;

    fn deref(&self) -> &BaseScheduleConfig {
        &self.base
    }
}

impl std::ops::DerefMut for ScenarioConfig {
    fn deref_mut(&mut self) -> &mut BaseScheduleConfig {
        &mut self.base
    }
}

/// A single entry in a multi-scenario configuration.
///
/// The `signal_type` tag selects whether this entry is a metrics or logs scenario.
/// Deserialized from a YAML multi-scenario file where each element of the
/// `scenarios` list carries a `signal_type: metrics` or `signal_type: logs` key.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "config", serde(tag = "signal_type"))]
pub enum ScenarioEntry {
    /// A metrics scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "metrics"))]
    Metrics(ScenarioConfig),
    /// A logs scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "logs"))]
    Logs(LogScenarioConfig),
}

impl ScenarioEntry {
    /// Return a reference to the shared [`BaseScheduleConfig`].
    ///
    /// Useful when only schedule-level fields (name, rate, duration, gaps,
    /// labels, sink, etc.) are needed regardless of signal type.
    pub fn base(&self) -> &BaseScheduleConfig {
        match self {
            ScenarioEntry::Metrics(c) => &c.base,
            ScenarioEntry::Logs(c) => &c.base,
        }
    }

    /// Return the `phase_offset` duration string, if set on the inner config.
    pub fn phase_offset(&self) -> Option<&str> {
        self.base().phase_offset.as_deref()
    }

    /// Return the `clock_group` identifier, if set on the inner config.
    pub fn clock_group(&self) -> Option<&str> {
        self.base().clock_group.as_deref()
    }
}

/// Full configuration for running multiple concurrent scenarios.
///
/// Deserialized from a multi-scenario YAML file that contains a top-level
/// `scenarios:` list. Each entry specifies its `signal_type` (either `metrics`
/// or `logs`) along with the scenario-specific fields.
///
/// # Example YAML
///
/// ```yaml
/// scenarios:
///   - signal_type: metrics
///     name: cpu_usage
///     rate: 100
///     duration: 30s
///     generator: { type: sine, amplitude: 50, period_secs: 60, offset: 50 }
///     encoder:
///       type: prometheus_text
///     sink:
///       type: stdout
///   - signal_type: logs
///     name: app_logs
///     rate: 10
///     duration: 30s
///     generator:
///       type: template
///       templates: [{ message: "event", field_pools: {} }]
///     encoder:
///       type: json_lines
///     sink:
///       type: file
///       path: /tmp/logs.json
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct MultiScenarioConfig {
    /// The list of scenarios to run concurrently.
    pub scenarios: Vec<ScenarioEntry>,
}

/// Validate the `column` / `columns` fields of a `CsvReplay` generator config.
///
/// Returns an error when:
/// - Both `column` and `columns` are set (mutually exclusive).
/// - `columns` is `Some` but the list is empty.
///
/// This validation is called before expansion so that invalid configs are
/// rejected early with a clear error message.
///
/// # Errors
///
/// Returns [`SondaError::Config`] with a descriptive message.
fn validate_csv_columns(
    column: &Option<usize>,
    columns: &Option<Vec<CsvColumnSpec>>,
) -> Result<(), SondaError> {
    if let Some(ref cols) = columns {
        if column.is_some() {
            return Err(SondaError::Config(ConfigError::invalid(
                "csv_replay: 'column' and 'columns' are mutually exclusive; use one or the other",
            )));
        }
        if cols.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "csv_replay: 'columns' must not be empty; provide at least one column spec or omit the field",
            )));
        }
    }
    Ok(())
}

/// Expand a [`ScenarioConfig`] that uses multi-column `csv_replay` into N
/// independent single-column scenarios.
///
/// When the `generator` is `CsvReplay` with `columns: Some(specs)`, this
/// function returns one `ScenarioConfig` per column spec. Each expanded config
/// has:
/// - `name` set to the column spec's `name`.
/// - `generator.column` set to `Some(spec.index)`.
/// - `generator.columns` set to `None`.
/// - All other fields (rate, duration, labels, sink, encoder, gaps, bursts,
///   jitter, etc.) cloned from the parent.
///
/// When `columns` is `None`, returns `vec![config]` unchanged.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if:
/// - Both `column` and `columns` are set.
/// - `columns` is an empty list.
pub fn expand_scenario(config: ScenarioConfig) -> Result<Vec<ScenarioConfig>, SondaError> {
    // Only the CsvReplay variant can have `columns`.
    let columns = match &config.generator {
        GeneratorConfig::CsvReplay {
            columns, column, ..
        } => {
            validate_csv_columns(column, columns)?;
            columns.clone()
        }
        _ => None,
    };

    let specs = match columns {
        Some(specs) => specs,
        None => return Ok(vec![config]),
    };

    let expanded = specs
        .into_iter()
        .map(|spec| {
            let mut child = config.clone();
            child.base.name = spec.name;
            // Replace the generator's column/columns fields.
            if let GeneratorConfig::CsvReplay {
                ref mut column,
                ref mut columns,
                ..
            } = child.generator
            {
                *column = Some(spec.index);
                *columns = None;
            }
            child
        })
        .collect();

    Ok(expanded)
}

/// Expand a [`ScenarioEntry`] that uses multi-column `csv_replay`.
///
/// For `ScenarioEntry::Metrics`, delegates to [`expand_scenario`] and wraps
/// the results back in `ScenarioEntry::Metrics`. For `ScenarioEntry::Logs`,
/// returns the entry unchanged (log scenarios do not use `csv_replay`).
///
/// # Errors
///
/// Propagates errors from [`expand_scenario`].
pub fn expand_entry(entry: ScenarioEntry) -> Result<Vec<ScenarioEntry>, SondaError> {
    match entry {
        ScenarioEntry::Metrics(config) => {
            let expanded = expand_scenario(config)?;
            Ok(expanded.into_iter().map(ScenarioEntry::Metrics).collect())
        }
        other => Ok(vec![other]),
    }
}

/// Full configuration for a single log scenario run.
///
/// Embeds [`BaseScheduleConfig`] for the shared schedule and delivery fields,
/// adding only the log-specific generator and a JSON-Lines-defaulting encoder.
///
/// Fields from [`BaseScheduleConfig`] are accessible directly via `Deref` (e.g.
/// `config.name`, `config.rate`) for ergonomic read access. Struct construction
/// uses the explicit `base` field.
///
/// # Example YAML
///
/// ```yaml
/// name: app_logs
/// rate: 10
/// duration: 60s
/// generator:
///   type: template
///   templates:
///     - message: "Request from {ip} to {endpoint}"
///       field_pools:
///         ip: ["10.0.0.1", "10.0.0.2"]
///         endpoint: ["/api", "/health"]
///   severity_weights:
///     info: 0.7
///     warn: 0.2
///     error: 0.1
/// encoder:
///   type: json_lines
/// sink:
///   type: stdout
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct LogScenarioConfig {
    /// Shared schedule and delivery fields.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub base: BaseScheduleConfig,
    /// Log generator configuration.
    pub generator: LogGeneratorConfig,
    /// Output encoder. Defaults to `json_lines`.
    #[cfg_attr(feature = "config", serde(default = "default_log_encoder"))]
    pub encoder: EncoderConfig,
}

impl std::ops::Deref for LogScenarioConfig {
    type Target = BaseScheduleConfig;

    fn deref(&self) -> &BaseScheduleConfig {
        &self.base
    }
}

impl std::ops::DerefMut for LogScenarioConfig {
    fn deref_mut(&mut self) -> &mut BaseScheduleConfig {
        &mut self.base
    }
}

#[cfg(all(test, feature = "config"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    // -----------------------------------------------------------------------
    // phase_offset deserialization: ScenarioConfig
    // -----------------------------------------------------------------------

    /// phase_offset deserializes from YAML on ScenarioConfig.
    #[test]
    fn scenario_config_phase_offset_deserializes_from_yaml() {
        let yaml = r#"
name: test_metric
rate: 10
generator:
  type: constant
  value: 1.0
phase_offset: "5s"
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.phase_offset.as_deref(), Some("5s"));
    }

    /// phase_offset defaults to None when not specified in YAML.
    #[test]
    fn scenario_config_phase_offset_defaults_to_none() {
        let yaml = r#"
name: test_metric
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.phase_offset.is_none());
    }

    /// phase_offset with milliseconds deserializes correctly.
    #[test]
    fn scenario_config_phase_offset_milliseconds() {
        let yaml = r#"
name: ms_test
rate: 10
generator:
  type: constant
  value: 1.0
phase_offset: "500ms"
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.phase_offset.as_deref(), Some("500ms"));
    }

    /// phase_offset with minutes deserializes correctly.
    #[test]
    fn scenario_config_phase_offset_minutes() {
        let yaml = r#"
name: min_test
rate: 10
generator:
  type: constant
  value: 1.0
phase_offset: "2m"
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.phase_offset.as_deref(), Some("2m"));
    }

    // -----------------------------------------------------------------------
    // phase_offset deserialization: LogScenarioConfig
    // -----------------------------------------------------------------------

    /// phase_offset deserializes from YAML on LogScenarioConfig.
    #[test]
    fn log_scenario_config_phase_offset_deserializes_from_yaml() {
        let yaml = r#"
name: log_test
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
phase_offset: "3s"
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.phase_offset.as_deref(), Some("3s"));
    }

    /// phase_offset defaults to None for LogScenarioConfig.
    #[test]
    fn log_scenario_config_phase_offset_defaults_to_none() {
        let yaml = r#"
name: log_test
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.phase_offset.is_none());
    }

    // -----------------------------------------------------------------------
    // clock_group deserialization
    // -----------------------------------------------------------------------

    /// clock_group deserializes from YAML on ScenarioConfig.
    #[test]
    fn scenario_config_clock_group_deserializes_from_yaml() {
        let yaml = r#"
name: group_test
rate: 10
generator:
  type: constant
  value: 1.0
clock_group: alert-test
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.clock_group.as_deref(), Some("alert-test"));
    }

    /// clock_group defaults to None when absent.
    #[test]
    fn scenario_config_clock_group_defaults_to_none() {
        let yaml = r#"
name: no_group
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.clock_group.is_none());
    }

    /// clock_group deserializes from YAML on LogScenarioConfig.
    #[test]
    fn log_scenario_config_clock_group_deserializes_from_yaml() {
        let yaml = r#"
name: log_group
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
clock_group: log-sync
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.clock_group.as_deref(), Some("log-sync"));
    }

    /// clock_group defaults to None for LogScenarioConfig.
    #[test]
    fn log_scenario_config_clock_group_defaults_to_none() {
        let yaml = r#"
name: log_no_group
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.clock_group.is_none());
    }

    // -----------------------------------------------------------------------
    // jitter deserialization
    // -----------------------------------------------------------------------

    /// jitter and jitter_seed deserialize from YAML on ScenarioConfig.
    #[test]
    fn scenario_config_jitter_deserializes_from_yaml() {
        let yaml = r#"
name: jitter_test
rate: 10
generator:
  type: constant
  value: 1.0
jitter: 3.5
jitter_seed: 42
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.jitter, Some(3.5));
        assert_eq!(config.jitter_seed, Some(42));
    }

    /// jitter defaults to None when not specified in YAML.
    #[test]
    fn scenario_config_jitter_defaults_to_none() {
        let yaml = r#"
name: no_jitter
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.jitter.is_none());
        assert!(config.jitter_seed.is_none());
    }

    /// jitter_seed defaults to None when only jitter is specified.
    #[test]
    fn scenario_config_jitter_without_seed() {
        let yaml = r#"
name: jitter_no_seed
rate: 10
generator:
  type: sine
  amplitude: 20
  period_secs: 60
  offset: 50
jitter: 5.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.jitter, Some(5.0));
        assert!(config.jitter_seed.is_none());
    }

    /// jitter deserializes from YAML on LogScenarioConfig.
    #[test]
    fn log_scenario_config_jitter_deserializes_from_yaml() {
        let yaml = r#"
name: log_jitter
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
jitter: 2.0
jitter_seed: 99
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.jitter, Some(2.0));
        assert_eq!(config.jitter_seed, Some(99));
    }

    // -----------------------------------------------------------------------
    // LogScenarioConfig: labels deserialization
    // -----------------------------------------------------------------------

    /// YAML with labels section deserializes into Some(HashMap).
    #[test]
    fn log_scenario_config_labels_deserialize_from_yaml() {
        let yaml = r#"
name: labeled_logs
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
labels:
  device: wlan0
  hostname: router-01
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let labels = config.labels.as_ref().expect("labels must be Some");
        assert_eq!(labels.get("device").map(String::as_str), Some("wlan0"));
        assert_eq!(
            labels.get("hostname").map(String::as_str),
            Some("router-01")
        );
        assert_eq!(labels.len(), 2);
    }

    /// YAML without labels field deserializes with labels: None.
    #[test]
    fn log_scenario_config_labels_default_to_none() {
        let yaml = r#"
name: no_labels_logs
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            config.labels.is_none(),
            "labels must default to None when not in YAML"
        );
    }

    /// YAML with empty labels section deserializes as Some(empty HashMap).
    #[test]
    fn log_scenario_config_empty_labels_deserializes_as_some_empty_map() {
        let yaml = r#"
name: empty_labels
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
labels: {}
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let labels = config
            .labels
            .as_ref()
            .expect("labels must be Some for explicit empty map");
        assert!(labels.is_empty(), "labels must be an empty map");
    }

    /// ScenarioConfig (metrics) also supports labels deserialization.
    #[test]
    fn scenario_config_labels_deserialize_from_yaml() {
        let yaml = r#"
name: metric_with_labels
rate: 10
generator:
  type: constant
  value: 1.0
labels:
  zone: eu1
  env: production
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let labels = config.labels.as_ref().expect("labels must be Some");
        assert_eq!(labels.get("zone").map(String::as_str), Some("eu1"));
        assert_eq!(labels.get("env").map(String::as_str), Some("production"));
    }

    // -----------------------------------------------------------------------
    // Both phase_offset and clock_group together
    // -----------------------------------------------------------------------

    /// Both phase_offset and clock_group set on ScenarioConfig.
    #[test]
    fn scenario_config_both_phase_offset_and_clock_group() {
        let yaml = r#"
name: both_fields
rate: 10
generator:
  type: constant
  value: 1.0
phase_offset: "30s"
clock_group: compound-alert
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.phase_offset.as_deref(), Some("30s"));
        assert_eq!(config.clock_group.as_deref(), Some("compound-alert"));
    }

    // -----------------------------------------------------------------------
    // ScenarioEntry::phase_offset() accessor
    // -----------------------------------------------------------------------

    /// ScenarioEntry::phase_offset() returns the phase_offset for a Metrics entry.
    #[test]
    fn scenario_entry_phase_offset_returns_value_for_metrics() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "accessor_test".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("5s".to_string()),
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        assert_eq!(entry.phase_offset(), Some("5s"));
    }

    /// ScenarioEntry::phase_offset() returns None when not set on Metrics.
    #[test]
    fn scenario_entry_phase_offset_returns_none_for_metrics_without_offset() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "no_offset".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        assert_eq!(entry.phase_offset(), None);
    }

    /// ScenarioEntry::phase_offset() returns the phase_offset for a Logs entry.
    #[test]
    fn scenario_entry_phase_offset_returns_value_for_logs() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "log_accessor".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("10s".to_string()),
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        assert_eq!(entry.phase_offset(), Some("10s"));
    }

    // -----------------------------------------------------------------------
    // ScenarioEntry::clock_group() accessor
    // -----------------------------------------------------------------------

    /// ScenarioEntry::clock_group() returns the value for a Metrics entry.
    #[test]
    fn scenario_entry_clock_group_returns_value_for_metrics() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "group_accessor".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: Some("my-group".to_string()),
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        assert_eq!(entry.clock_group(), Some("my-group"));
    }

    /// ScenarioEntry::clock_group() returns None when not set.
    #[test]
    fn scenario_entry_clock_group_returns_none_when_absent() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "no_group_acc".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        assert_eq!(entry.clock_group(), None);
    }

    // -----------------------------------------------------------------------
    // ScenarioEntry::base() accessor
    // -----------------------------------------------------------------------

    /// ScenarioEntry::base() returns the shared config for a Metrics entry.
    #[test]
    fn scenario_entry_base_returns_shared_config_for_metrics() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "base_test".to_string(),
                rate: 42.0,
                duration: Some("5s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        assert_eq!(entry.base().name, "base_test");
        assert_eq!(entry.base().rate, 42.0);
    }

    /// ScenarioEntry::base() returns the shared config for a Logs entry.
    #[test]
    fn scenario_entry_base_returns_shared_config_for_logs() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "log_base".to_string(),
                rate: 99.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        assert_eq!(entry.base().name, "log_base");
        assert_eq!(entry.base().rate, 99.0);
    }

    // -----------------------------------------------------------------------
    // Multi-scenario YAML with phase_offset and clock_group
    // -----------------------------------------------------------------------

    /// Multi-scenario YAML with phase_offset and clock_group deserializes correctly.
    #[test]
    fn multi_scenario_config_with_phase_offset_and_clock_group_deserializes() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 1
    duration: 10s
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: constant
      value: 95.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: memory_usage
    rate: 1
    duration: 10s
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: constant
      value: 88.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 2);

        assert_eq!(config.scenarios[0].phase_offset(), Some("0s"));
        assert_eq!(config.scenarios[0].clock_group(), Some("alert-test"));
        assert_eq!(config.scenarios[1].phase_offset(), Some("3s"));
        assert_eq!(config.scenarios[1].clock_group(), Some("alert-test"));
    }

    /// Existing multi-scenario YAML without phase_offset/clock_group still works.
    #[test]
    fn multi_scenario_config_without_phase_offset_backward_compatible() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 100
    duration: 10s
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert_eq!(config.scenarios[0].phase_offset(), None);
        assert_eq!(config.scenarios[0].clock_group(), None);
    }

    /// The example multi-metric-correlation.yaml file deserializes correctly.
    #[test]
    fn multi_metric_correlation_example_deserializes() {
        let yaml = include_str!("../../../examples/multi-metric-correlation.yaml");
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 2, "example must have 2 scenarios");

        // First scenario: cpu_usage with phase_offset "0s"
        assert_eq!(config.scenarios[0].phase_offset(), Some("0s"));
        assert_eq!(config.scenarios[0].clock_group(), Some("alert-test"));

        // Second scenario: memory_usage_percent with phase_offset "3s"
        assert_eq!(config.scenarios[1].phase_offset(), Some("3s"));
        assert_eq!(config.scenarios[1].clock_group(), Some("alert-test"));

        // Both should be metrics entries
        assert!(matches!(config.scenarios[0], ScenarioEntry::Metrics(_)));
        assert!(matches!(config.scenarios[1], ScenarioEntry::Metrics(_)));
    }

    /// Multi-scenario YAML with a Logs entry including phase_offset deserializes.
    #[test]
    fn multi_scenario_config_logs_entry_with_phase_offset() {
        let yaml = r#"
scenarios:
  - signal_type: logs
    name: delayed_logs
    rate: 10
    duration: 10s
    phase_offset: "2s"
    clock_group: log-group
    generator:
      type: template
      templates:
        - message: "log event"
          field_pools: {}
    encoder:
      type: json_lines
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert_eq!(config.scenarios[0].phase_offset(), Some("2s"));
        assert_eq!(config.scenarios[0].clock_group(), Some("log-group"));
    }

    // -----------------------------------------------------------------------
    // phase_offset parseable by parse_duration
    // -----------------------------------------------------------------------

    /// phase_offset values are parseable by parse_duration.
    #[test]
    fn phase_offset_values_are_parseable_as_durations() {
        use crate::config::validate::parse_duration;

        let yaml = r#"
name: parse_test
rate: 10
generator:
  type: constant
  value: 1.0
phase_offset: "3s"
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dur = parse_duration(config.phase_offset.as_deref().unwrap()).unwrap();
        assert_eq!(dur, std::time::Duration::from_secs(3));
    }

    // -----------------------------------------------------------------------
    // cardinality_spikes deserialization
    // -----------------------------------------------------------------------

    /// YAML with cardinality_spikes deserializes into Some(Vec).
    #[test]
    fn scenario_config_cardinality_spikes_deserializes_from_yaml() {
        let yaml = r#"
name: spike_test
rate: 10
generator:
  type: constant
  value: 1.0
cardinality_spikes:
  - label: pod_name
    every: 2m
    for: 30s
    cardinality: 500
    strategy: counter
    prefix: "pod-"
  - label: error_msg
    every: 5m
    for: 1m
    cardinality: 1000
    strategy: random
    seed: 42
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let spikes = config
            .cardinality_spikes
            .as_ref()
            .expect("cardinality_spikes must be Some");
        assert_eq!(spikes.len(), 2);
        assert_eq!(spikes[0].label, "pod_name");
        assert_eq!(spikes[0].cardinality, 500);
        assert_eq!(spikes[0].strategy, SpikeStrategy::Counter);
        assert_eq!(spikes[0].prefix.as_deref(), Some("pod-"));
        assert_eq!(spikes[1].label, "error_msg");
        assert_eq!(spikes[1].strategy, SpikeStrategy::Random);
        assert_eq!(spikes[1].seed, Some(42));
    }

    /// YAML without cardinality_spikes defaults to None.
    #[test]
    fn scenario_config_cardinality_spikes_defaults_to_none() {
        let yaml = r#"
name: no_spike
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            config.cardinality_spikes.is_none(),
            "cardinality_spikes must be None when absent from YAML"
        );
    }

    /// SpikeStrategy defaults to Counter when not specified in YAML.
    #[test]
    fn spike_strategy_defaults_to_counter() {
        let yaml = r#"
name: default_strategy
rate: 10
generator:
  type: constant
  value: 1.0
cardinality_spikes:
  - label: pod_name
    every: 1m
    for: 10s
    cardinality: 10
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let spikes = config.base.cardinality_spikes.unwrap();
        assert_eq!(spikes[0].strategy, SpikeStrategy::Counter);
    }

    /// LogScenarioConfig also supports cardinality_spikes.
    #[test]
    fn log_scenario_config_cardinality_spikes_deserializes() {
        let yaml = r#"
name: log_spike
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
cardinality_spikes:
  - label: pod_name
    every: 1m
    for: 10s
    cardinality: 100
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let spikes = config.base.cardinality_spikes.unwrap();
        assert_eq!(spikes.len(), 1);
        assert_eq!(spikes[0].label, "pod_name");
    }

    /// Backward compatibility: existing YAML without cardinality_spikes still works.
    #[test]
    fn backward_compatible_yaml_without_spikes() {
        let yaml = r#"
name: compat_test
rate: 100
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
labels:
  hostname: t0-a1
gaps:
  every: 2m
  for: 20s
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.cardinality_spikes.is_none());
        assert!(config.gaps.is_some());
        assert_eq!(config.name, "compat_test");
    }

    // -----------------------------------------------------------------------
    // BaseScheduleConfig: Clone + Debug contract
    // -----------------------------------------------------------------------

    /// BaseScheduleConfig is Clone and Debug.
    #[test]
    fn base_schedule_config_is_clone_and_debug() {
        let base = BaseScheduleConfig {
            name: "test".to_string(),
            rate: 42.0,
            duration: Some("10s".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            jitter: None,
            jitter_seed: None,
        };
        let cloned = base.clone();
        assert_eq!(cloned.name, "test");
        assert_eq!(cloned.rate, 42.0);
        let dbg = format!("{base:?}");
        assert!(
            dbg.contains("BaseScheduleConfig"),
            "Debug output must contain type name"
        );
    }

    // -----------------------------------------------------------------------
    // Deref: ScenarioConfig fields accessible directly
    // -----------------------------------------------------------------------

    /// ScenarioConfig fields from BaseScheduleConfig are accessible via Deref.
    #[test]
    fn scenario_config_deref_accesses_base_fields() {
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "deref_test".to_string(),
                rate: 99.0,
                duration: Some("5s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("1s".to_string()),
                clock_group: Some("group-a".to_string()),
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        };
        // All these access via Deref — no explicit `.base.` needed.
        assert_eq!(config.name, "deref_test");
        assert_eq!(config.rate, 99.0);
        assert_eq!(config.duration.as_deref(), Some("5s"));
        assert!(config.gaps.is_none());
        assert_eq!(config.phase_offset.as_deref(), Some("1s"));
        assert_eq!(config.clock_group.as_deref(), Some("group-a"));
    }

    /// LogScenarioConfig fields from BaseScheduleConfig are accessible via Deref.
    #[test]
    fn log_scenario_config_deref_accesses_base_fields() {
        let config = LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "log_deref".to_string(),
                rate: 50.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        };
        assert_eq!(config.name, "log_deref");
        assert_eq!(config.rate, 50.0);
        assert!(config.duration.is_none());
    }

    // -----------------------------------------------------------------------
    // DerefMut: ScenarioConfig base fields mutable via DerefMut
    // -----------------------------------------------------------------------

    /// ScenarioConfig base fields can be mutated via DerefMut.
    #[test]
    fn scenario_config_deref_mut_allows_base_field_mutation() {
        let mut config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "original".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        };
        config.name = "mutated".to_string();
        config.rate = 999.0;
        config.duration = Some("30s".to_string());
        assert_eq!(config.name, "mutated");
        assert_eq!(config.rate, 999.0);
        assert_eq!(config.duration.as_deref(), Some("30s"));
    }

    // -----------------------------------------------------------------------
    // Flatten: YAML with base fields and generator deserializes correctly
    // -----------------------------------------------------------------------

    /// ScenarioConfig deserializes with all fields at the top level (serde flatten).
    #[test]
    fn scenario_config_flatten_deserializes_all_fields() {
        let yaml = r#"
name: flatten_test
rate: 100
duration: 30s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
gaps:
  every: 2m
  for: 20s
bursts:
  every: 10s
  for: 2s
  multiplier: 5.0
labels:
  hostname: t0-a1
  zone: eu1
encoder:
  type: prometheus_text
sink:
  type: stdout
phase_offset: "5s"
clock_group: correlation
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "flatten_test");
        assert_eq!(config.rate, 100.0);
        assert_eq!(config.duration.as_deref(), Some("30s"));
        assert!(config.gaps.is_some());
        assert!(config.bursts.is_some());
        let labels = config.labels.as_ref().unwrap();
        assert_eq!(labels.get("hostname").map(String::as_str), Some("t0-a1"));
        assert!(matches!(
            config.encoder,
            EncoderConfig::PrometheusText { .. }
        ));
        assert!(matches!(config.base.sink, SinkConfig::Stdout));
        assert_eq!(config.phase_offset.as_deref(), Some("5s"));
        assert_eq!(config.clock_group.as_deref(), Some("correlation"));
    }

    /// LogScenarioConfig deserializes with all fields at the top level (serde flatten).
    #[test]
    fn log_scenario_config_flatten_deserializes_all_fields() {
        let yaml = r#"
name: log_flatten
rate: 20
duration: 60s
generator:
  type: template
  templates:
    - message: "hello"
      field_pools: {}
labels:
  env: prod
encoder:
  type: syslog
  hostname: myhost
  app_name: myapp
sink:
  type: stdout
phase_offset: "2s"
clock_group: log-group
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "log_flatten");
        assert_eq!(config.rate, 20.0);
        assert_eq!(config.duration.as_deref(), Some("60s"));
        let labels = config.labels.as_ref().unwrap();
        assert_eq!(labels.get("env").map(String::as_str), Some("prod"));
        assert_eq!(config.phase_offset.as_deref(), Some("2s"));
        assert_eq!(config.clock_group.as_deref(), Some("log-group"));
    }

    // -----------------------------------------------------------------------
    // Encoder defaults remain correct per signal type
    // -----------------------------------------------------------------------

    /// ScenarioConfig defaults encoder to prometheus_text.
    #[test]
    fn scenario_config_encoder_defaults_to_prometheus_text() {
        let yaml = r#"
name: enc_default
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config.encoder, EncoderConfig::PrometheusText { .. }),
            "ScenarioConfig encoder default must be prometheus_text, got {:?}",
            config.encoder
        );
    }

    /// LogScenarioConfig defaults encoder to json_lines.
    #[test]
    fn log_scenario_config_encoder_defaults_to_json_lines() {
        let yaml = r#"
name: log_enc_default
rate: 10
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            matches!(config.encoder, EncoderConfig::JsonLines { .. }),
            "LogScenarioConfig encoder default must be json_lines, got {:?}",
            config.encoder
        );
    }

    // -----------------------------------------------------------------------
    // dynamic_labels deserialization
    // -----------------------------------------------------------------------

    /// dynamic_labels with counter strategy deserializes from YAML.
    #[test]
    fn dynamic_labels_counter_deserializes_from_yaml() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
dynamic_labels:
  - key: hostname
    prefix: "host-"
    cardinality: 10
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dls = config
            .dynamic_labels
            .as_ref()
            .expect("dynamic_labels must be present");
        assert_eq!(dls.len(), 1);
        assert_eq!(dls[0].key, "hostname");
        match &dls[0].strategy {
            DynamicLabelStrategy::Counter {
                prefix,
                cardinality,
            } => {
                assert_eq!(prefix.as_deref(), Some("host-"));
                assert_eq!(*cardinality, 10);
            }
            other => panic!("expected Counter strategy, got {other:?}"),
        }
    }

    /// dynamic_labels with values list strategy deserializes from YAML.
    #[test]
    fn dynamic_labels_values_list_deserializes_from_yaml() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
dynamic_labels:
  - key: region
    values: [us-east-1, us-west-2, eu-west-1]
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dls = config
            .dynamic_labels
            .as_ref()
            .expect("dynamic_labels must be present");
        assert_eq!(dls.len(), 1);
        assert_eq!(dls[0].key, "region");
        match &dls[0].strategy {
            DynamicLabelStrategy::ValuesList { values } => {
                assert_eq!(values, &["us-east-1", "us-west-2", "eu-west-1"]);
            }
            other => panic!("expected ValuesList strategy, got {other:?}"),
        }
    }

    /// dynamic_labels defaults to None when not specified.
    #[test]
    fn dynamic_labels_defaults_to_none() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.dynamic_labels.is_none());
    }

    /// Multiple dynamic_labels entries deserialize correctly.
    #[test]
    fn dynamic_labels_multiple_entries_deserialize() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
dynamic_labels:
  - key: hostname
    prefix: "host-"
    cardinality: 10
  - key: region
    values: [us-east, eu-west]
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dls = config
            .dynamic_labels
            .as_ref()
            .expect("dynamic_labels must be present");
        assert_eq!(dls.len(), 2);
        assert_eq!(dls[0].key, "hostname");
        assert_eq!(dls[1].key, "region");
    }

    /// dynamic_labels on LogScenarioConfig deserializes from YAML.
    #[test]
    fn dynamic_labels_on_log_config_deserializes() {
        let yaml = r#"
name: test_logs
rate: 10
generator:
  type: template
  templates:
    - message: "test event"
      field_pools: {}
dynamic_labels:
  - key: pod_name
    prefix: "pod-"
    cardinality: 5
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dls = config
            .dynamic_labels
            .as_ref()
            .expect("dynamic_labels must be present");
        assert_eq!(dls.len(), 1);
        assert_eq!(dls[0].key, "pod_name");
    }

    /// Counter strategy with no prefix defaults prefix to None in config.
    #[test]
    fn dynamic_labels_counter_no_prefix_deserializes() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
dynamic_labels:
  - key: zone
    cardinality: 3
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let dls = config
            .dynamic_labels
            .as_ref()
            .expect("dynamic_labels must be present");
        match &dls[0].strategy {
            DynamicLabelStrategy::Counter {
                prefix,
                cardinality,
            } => {
                assert!(prefix.is_none(), "prefix should be None when not specified");
                assert_eq!(*cardinality, 3);
            }
            other => panic!("expected Counter strategy, got {other:?}"),
        }
    }

    /// static labels and dynamic_labels coexist in the same config.
    #[test]
    fn dynamic_labels_and_static_labels_coexist() {
        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
labels:
  env: prod
dynamic_labels:
  - key: hostname
    prefix: "host-"
    cardinality: 5
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.labels.is_some(), "static labels must be present");
        assert!(
            config.dynamic_labels.is_some(),
            "dynamic labels must be present"
        );
        let static_labels = config.labels.as_ref().unwrap();
        assert_eq!(static_labels.get("env"), Some(&"prod".to_string()));
    }

    // -----------------------------------------------------------------------
    // csv_replay multi-column: YAML deserialization
    // -----------------------------------------------------------------------

    /// csv_replay with `columns` deserializes correctly from YAML.
    #[test]
    fn csv_replay_columns_deserializes_from_yaml() {
        let yaml = r#"
name: multi_col
rate: 1
generator:
  type: csv_replay
  file: data.csv
  has_header: true
  columns:
    - index: 1
      name: cpu_percent
    - index: 2
      name: mem_percent
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match &config.generator {
            GeneratorConfig::CsvReplay {
                columns, column, ..
            } => {
                assert!(
                    column.is_none(),
                    "column should be None when columns is set"
                );
                let cols = columns.as_ref().expect("columns should be Some");
                assert_eq!(cols.len(), 2);
                assert_eq!(cols[0].index, 1);
                assert_eq!(cols[0].name, "cpu_percent");
                assert_eq!(cols[1].index, 2);
                assert_eq!(cols[1].name, "mem_percent");
            }
            other => panic!("expected CsvReplay variant, got {other:?}"),
        }
    }

    /// csv_replay without `columns` deserializes with columns as None.
    #[test]
    fn csv_replay_without_columns_field_has_none() {
        let yaml = r#"
name: single_col
rate: 1
generator:
  type: csv_replay
  file: data.csv
  column: 1
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match &config.generator {
            GeneratorConfig::CsvReplay {
                columns, column, ..
            } => {
                assert_eq!(*column, Some(1));
                assert!(
                    columns.is_none(),
                    "columns should be None when not specified"
                );
            }
            other => panic!("expected CsvReplay variant, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod expand_tests {
    use super::*;
    use crate::encoder::EncoderConfig;
    use crate::generator::{CsvColumnSpec, GeneratorConfig};
    use crate::sink::SinkConfig;

    /// Build a base `ScenarioConfig` with a csv_replay generator for testing.
    fn csv_replay_config(
        name: &str,
        column: Option<usize>,
        columns: Option<Vec<CsvColumnSpec>>,
    ) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                name: name.to_string(),
                rate: 10.0,
                duration: Some("30s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: Some([("host".to_string(), "srv1".to_string())].into()),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: Some(0.5),
                jitter_seed: Some(42),
                dynamic_labels: None,
            },
            generator: GeneratorConfig::CsvReplay {
                file: "data.csv".to_string(),
                column,
                has_header: Some(true),
                repeat: Some(true),
                columns,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
        }
    }

    // -----------------------------------------------------------------------
    // expand_scenario: pass-through (no columns)
    // -----------------------------------------------------------------------

    /// When columns is None, expand_scenario returns the config unchanged.
    #[test]
    fn pass_through_when_no_columns() {
        let config = csv_replay_config("single_metric", Some(1), None);
        let result = expand_scenario(config.clone()).expect("must succeed");
        assert_eq!(result.len(), 1, "should return exactly one config");
        assert_eq!(result[0].name, "single_metric");
    }

    /// A non-csv_replay generator passes through unchanged.
    #[test]
    fn non_csv_replay_passes_through() {
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "const_metric".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
            },
            generator: GeneratorConfig::Constant { value: 42.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        };
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "const_metric");
    }

    // -----------------------------------------------------------------------
    // expand_scenario: two-column expansion
    // -----------------------------------------------------------------------

    /// Two columns expand into two configs with correct names and column indices.
    #[test]
    fn two_column_expansion() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu_percent".to_string(),
            },
            CsvColumnSpec {
                index: 2,
                name: "mem_percent".to_string(),
            },
        ];
        let config = csv_replay_config("parent", None, Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 2, "should produce two expanded configs");

        // First expanded config
        assert_eq!(result[0].name, "cpu_percent");
        match &result[0].generator {
            GeneratorConfig::CsvReplay {
                column,
                columns,
                file,
                has_header,
                repeat,
                ..
            } => {
                assert_eq!(*column, Some(1));
                assert!(columns.is_none(), "columns must be None after expansion");
                assert_eq!(file, "data.csv", "file must be inherited");
                assert_eq!(*has_header, Some(true), "has_header must be inherited");
                assert_eq!(*repeat, Some(true), "repeat must be inherited");
            }
            other => panic!("expected CsvReplay, got {other:?}"),
        }

        // Second expanded config
        assert_eq!(result[1].name, "mem_percent");
        match &result[1].generator {
            GeneratorConfig::CsvReplay {
                column, columns, ..
            } => {
                assert_eq!(*column, Some(2));
                assert!(columns.is_none());
            }
            other => panic!("expected CsvReplay, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // expand_scenario: three-column expansion
    // -----------------------------------------------------------------------

    /// Three columns expand into three configs.
    #[test]
    fn three_column_expansion() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
            },
            CsvColumnSpec {
                index: 3,
                name: "disk_io".to_string(),
            },
        ];
        let config = csv_replay_config("parent", None, Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "cpu");
        assert_eq!(result[1].name, "mem");
        assert_eq!(result[2].name, "disk_io");

        // Verify each has the correct column index
        for (i, expected_col) in [(0, 1), (1, 2), (2, 3)] {
            match &result[i].generator {
                GeneratorConfig::CsvReplay { column, .. } => {
                    assert_eq!(*column, Some(expected_col), "config[{i}] column");
                }
                other => panic!("expected CsvReplay, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // expand_scenario: inherited fields
    // -----------------------------------------------------------------------

    /// Expanded configs inherit all schedule/delivery fields from the parent.
    #[test]
    fn expanded_configs_inherit_parent_fields() {
        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "metric_a".to_string(),
        }];
        let config = csv_replay_config("parent", None, Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 1);
        let child = &result[0];

        // Schedule fields
        assert_eq!(child.rate, 10.0, "rate must be inherited");
        assert_eq!(
            child.duration.as_deref(),
            Some("30s"),
            "duration must be inherited"
        );

        // Labels
        let labels = child.labels.as_ref().expect("labels must be inherited");
        assert_eq!(labels.get("host").map(|s| s.as_str()), Some("srv1"));

        // Jitter
        assert_eq!(child.jitter, Some(0.5));
        assert_eq!(child.jitter_seed, Some(42));

        // Encoder and sink
        assert!(matches!(
            child.encoder,
            EncoderConfig::PrometheusText { .. }
        ));
        assert!(matches!(child.sink, SinkConfig::Stdout));
    }

    // -----------------------------------------------------------------------
    // expand_scenario: error — column and columns both set
    // -----------------------------------------------------------------------

    /// Setting both column and columns returns an error.
    #[test]
    fn column_and_columns_both_set_returns_error() {
        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "cpu".to_string(),
        }];
        let config = csv_replay_config("conflict", Some(1), Some(cols));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "error must mention mutual exclusivity, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // expand_scenario: error — empty columns list
    // -----------------------------------------------------------------------

    /// An empty columns list returns an error.
    #[test]
    fn empty_columns_list_returns_error() {
        let config = csv_replay_config("empty", None, Some(vec![]));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("must not be empty"),
            "error must mention empty list, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // expand_entry: metrics wrapping
    // -----------------------------------------------------------------------

    /// expand_entry wraps expanded metrics configs back in ScenarioEntry::Metrics.
    #[test]
    fn expand_entry_metrics_two_columns() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
            },
        ];
        let config = csv_replay_config("parent", None, Some(cols));
        let entry = ScenarioEntry::Metrics(config);
        let result = expand_entry(entry).expect("must succeed");

        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], ScenarioEntry::Metrics(_)));
        assert!(matches!(result[1], ScenarioEntry::Metrics(_)));
    }

    /// expand_entry passes log entries through unchanged.
    #[test]
    fn expand_entry_logs_passes_through() {
        use crate::generator::{LogGeneratorConfig, TemplateConfig};
        use std::collections::BTreeMap;

        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "app_logs".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        let result = expand_entry(entry).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ScenarioEntry::Logs(_)));
    }
}
