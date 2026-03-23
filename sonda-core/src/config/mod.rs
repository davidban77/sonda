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
