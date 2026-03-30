//! Scenario configuration: YAML deserialization and validation.

pub mod validate;

use std::collections::HashMap;

use serde::Deserialize;

use crate::encoder::EncoderConfig;
use crate::generator::{GeneratorConfig, LogGeneratorConfig};
use crate::sink::SinkConfig;

/// Gap window configuration — a recurring silent period within a scenario.
///
/// During a gap the scheduler emits no events. The gap repeats on a fixed
/// cycle defined by `every`, and each instance lasts for `for`.
#[derive(Debug, Clone, Deserialize)]
pub struct GapConfig {
    /// How often the gap recurs (e.g. `"2m"`).
    pub every: String,
    /// How long each gap lasts (e.g. `"20s"`). Must be less than `every`.
    pub r#for: String,
}

/// Burst window configuration — a recurring high-rate period within a scenario.
///
/// During a burst the event rate is multiplied by `multiplier`. The burst
/// repeats on a fixed cycle defined by `every`, and each instance lasts for `for`.
///
/// If a gap and burst overlap in time, the gap takes priority and no events
/// are emitted.
#[derive(Debug, Clone, Deserialize)]
pub struct BurstConfig {
    /// How often the burst recurs (e.g. `"10s"`).
    pub every: String,
    /// How long each burst lasts (e.g. `"2s"`). Must be less than `every`.
    pub r#for: String,
    /// Rate multiplier during the burst (must be strictly positive).
    pub multiplier: f64,
}

fn default_encoder() -> EncoderConfig {
    EncoderConfig::PrometheusText
}

fn default_log_encoder() -> EncoderConfig {
    EncoderConfig::JsonLines
}

fn default_sink() -> SinkConfig {
    SinkConfig::Stdout
}

/// Full configuration for a single scenario run.
///
/// Deserialized from a YAML scenario file. CLI flags can override any field.
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
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioConfig {
    /// Metric name emitted by this scenario (must be a valid Prometheus metric name).
    pub name: String,
    /// Target event rate in events per second. Must be strictly positive.
    pub rate: f64,
    /// Optional total run duration (e.g. `"30s"`, `"5m"`). `None` means run indefinitely.
    #[serde(default)]
    pub duration: Option<String>,
    /// Value generator configuration.
    pub generator: GeneratorConfig,
    /// Optional gap window: recurring silent periods in the event stream.
    #[serde(default)]
    pub gaps: Option<GapConfig>,
    /// Optional burst window: recurring high-rate periods in the event stream.
    ///
    /// When both a gap and a burst overlap in time, the gap takes priority.
    #[serde(default)]
    pub bursts: Option<BurstConfig>,
    /// Static labels attached to every emitted event.
    #[serde(default)]
    pub labels: Option<HashMap<String, String>>,
    /// Output encoder. Defaults to `prometheus_text`.
    #[serde(default = "default_encoder")]
    pub encoder: EncoderConfig,
    /// Output sink. Defaults to `stdout`.
    #[serde(default = "default_sink")]
    pub sink: SinkConfig,
    /// Delay before starting this scenario, relative to the group start time.
    ///
    /// Only meaningful in multi-scenario mode. Enables temporal correlation
    /// between scenarios: "metric A starts immediately, metric B starts 30s
    /// later". Accepts a duration string (e.g. `"30s"`, `"1m"`, `"500ms"`).
    #[serde(default)]
    pub phase_offset: Option<String>,
    /// Clock group identifier for multi-scenario correlation.
    ///
    /// Scenarios with the same `clock_group` value share a common start time
    /// reference. For MVP this provides a shared start reference only; advanced
    /// cross-scenario signaling is deferred to a future phase.
    #[serde(default)]
    pub clock_group: Option<String>,
}

/// A single entry in a multi-scenario configuration.
///
/// The `signal_type` tag selects whether this entry is a metrics or logs scenario.
/// Deserialized from a YAML multi-scenario file where each element of the
/// `scenarios` list carries a `signal_type: metrics` or `signal_type: logs` key.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "signal_type")]
pub enum ScenarioEntry {
    /// A metrics scenario entry.
    #[serde(rename = "metrics")]
    Metrics(ScenarioConfig),
    /// A logs scenario entry.
    #[serde(rename = "logs")]
    Logs(LogScenarioConfig),
}

impl ScenarioEntry {
    /// Return the `phase_offset` duration string, if set on the inner config.
    pub fn phase_offset(&self) -> Option<&str> {
        match self {
            ScenarioEntry::Metrics(c) => c.phase_offset.as_deref(),
            ScenarioEntry::Logs(c) => c.phase_offset.as_deref(),
        }
    }

    /// Return the `clock_group` identifier, if set on the inner config.
    pub fn clock_group(&self) -> Option<&str> {
        match self {
            ScenarioEntry::Metrics(c) => c.clock_group.as_deref(),
            ScenarioEntry::Logs(c) => c.clock_group.as_deref(),
        }
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
#[derive(Debug, Clone, Deserialize)]
pub struct MultiScenarioConfig {
    /// The list of scenarios to run concurrently.
    pub scenarios: Vec<ScenarioEntry>,
}

/// Full configuration for a single log scenario run.
///
/// Deserialized from a YAML scenario file. CLI flags can override any field.
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
#[derive(Debug, Clone, Deserialize)]
pub struct LogScenarioConfig {
    /// Scenario name (used for identification and logging).
    pub name: String,
    /// Target event rate in events per second. Must be strictly positive.
    pub rate: f64,
    /// Optional total run duration (e.g. `"30s"`, `"5m"`). `None` means run indefinitely.
    #[serde(default)]
    pub duration: Option<String>,
    /// Log generator configuration.
    pub generator: LogGeneratorConfig,
    /// Optional gap window: recurring silent periods in the event stream.
    #[serde(default)]
    pub gaps: Option<GapConfig>,
    /// Optional burst window: recurring high-rate periods.
    #[serde(default)]
    pub bursts: Option<BurstConfig>,
    /// Static labels attached to every emitted log event.
    #[serde(default)]
    pub labels: Option<HashMap<String, String>>,
    /// Output encoder. Defaults to `json_lines`.
    #[serde(default = "default_log_encoder")]
    pub encoder: EncoderConfig,
    /// Output sink. Defaults to `stdout`.
    #[serde(default = "default_sink")]
    pub sink: SinkConfig,
    /// Delay before starting this scenario, relative to the group start time.
    ///
    /// Only meaningful in multi-scenario mode. Enables temporal correlation
    /// between scenarios: "metric A starts immediately, metric B starts 30s
    /// later". Accepts a duration string (e.g. `"30s"`, `"1m"`, `"500ms"`).
    #[serde(default)]
    pub phase_offset: Option<String>,
    /// Clock group identifier for multi-scenario correlation.
    ///
    /// Scenarios with the same `clock_group` value share a common start time
    /// reference. For MVP this provides a shared start reference only; advanced
    /// cross-scenario signaling is deferred to a future phase.
    #[serde(default)]
    pub clock_group: Option<String>,
}

#[cfg(test)]
mod tests {
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: LogScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: LogScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: LogScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: LogScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.clock_group.is_none());
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
            name: "accessor_test".to_string(),
            rate: 10.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout,
            phase_offset: Some("5s".to_string()),
            clock_group: None,
        });
        assert_eq!(entry.phase_offset(), Some("5s"));
    }

    /// ScenarioEntry::phase_offset() returns None when not set on Metrics.
    #[test]
    fn scenario_entry_phase_offset_returns_none_for_metrics_without_offset() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "no_offset".to_string(),
            rate: 10.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        assert_eq!(entry.phase_offset(), None);
    }

    /// ScenarioEntry::phase_offset() returns the phase_offset for a Logs entry.
    #[test]
    fn scenario_entry_phase_offset_returns_value_for_logs() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            name: "log_accessor".to_string(),
            rate: 10.0,
            duration: None,
            generator: LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::JsonLines,
            sink: SinkConfig::Stdout,
            phase_offset: Some("10s".to_string()),
            clock_group: None,
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
            name: "group_accessor".to_string(),
            rate: 10.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: Some("my-group".to_string()),
        });
        assert_eq!(entry.clock_group(), Some("my-group"));
    }

    /// ScenarioEntry::clock_group() returns None when not set.
    #[test]
    fn scenario_entry_clock_group_returns_none_when_absent() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "no_group_acc".to_string(),
            rate: 10.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        assert_eq!(entry.clock_group(), None);
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
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert_eq!(config.scenarios[0].phase_offset(), None);
        assert_eq!(config.scenarios[0].clock_group(), None);
    }

    /// The example multi-metric-correlation.yaml file deserializes correctly.
    #[test]
    fn multi_metric_correlation_example_deserializes() {
        let yaml = include_str!("../../../examples/multi-metric-correlation.yaml");
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
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
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        let dur = parse_duration(config.phase_offset.as_deref().unwrap()).unwrap();
        assert_eq!(dur, std::time::Duration::from_secs(3));
    }
}
