//! Scenario configuration: YAML deserialization and validation.

pub mod validate;

use std::collections::HashMap;

use serde::Deserialize;

use crate::encoder::EncoderConfig;
use crate::generator::GeneratorConfig;
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

fn default_encoder() -> EncoderConfig {
    EncoderConfig::PrometheusText
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
/// encoder: prometheus_text
/// sink: stdout
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
    /// Static labels attached to every emitted event.
    #[serde(default)]
    pub labels: Option<HashMap<String, String>>,
    /// Output encoder. Defaults to `prometheus_text`.
    #[serde(default = "default_encoder")]
    pub encoder: EncoderConfig,
    /// Output sink. Defaults to `stdout`.
    #[serde(default = "default_sink")]
    pub sink: SinkConfig,
}
