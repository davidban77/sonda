//! Scenario configuration types and validation.
//!
//! The `Serialize` and `Deserialize` impls on all config types are available
//! only when the `config` Cargo feature is enabled (active by default). Without
//! the feature, configs can still be constructed in code — only YAML/JSON
//! serialization and parsing are gated.

pub mod aliases;
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GapConfig {
    /// How often the gap recurs (e.g. `"2m"`).
    pub every: String,
    /// How long each gap lasts (e.g. `"20s"`). Must be less than `every`.
    pub r#for: String,
}

/// One non-recurring silent window at a known offset from scenario start.
///
/// The sibling of [`GapConfig`] for silence that *happened* rather than
/// silence that *recurs*. A periodic gap answers "simulate a scrape gap every
/// 60s"; this answers "the exporter was down from 04:12 to 04:19", which has
/// no period to speak of.
///
/// It is a separate field rather than a new shape on `gaps:` deliberately: an
/// untagged enum on an existing public config key risks silently
/// reinterpreting YAML that already parses, and a sibling field is additive
/// and boring.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GapWindowConfig {
    /// Offset from scenario start at which the silence begins (e.g. `"60s"`).
    ///
    /// An offset, not a length: `"0s"` is a scenario that begins already
    /// inside the silence, which is what a capture taken during an outage
    /// looks like.
    pub at: String,
    /// How long the silence lasts (e.g. `"90s"`).
    pub r#for: String,
}

impl GapWindowConfig {
    /// Resolve this window into `(offset from start, length)`.
    ///
    /// One definition because two callers ask the same question and must get
    /// the same answer: the scheduler, which suppresses emission inside the
    /// window, and the csv_replay cross-check, which asks whether that window
    /// covers a blank cell. If the two parsed these fields differently, the
    /// cross-check would bless a file the scheduler then replays wrongly —
    /// exactly the divergence that makes a check worse than no check.
    ///
    /// `at` accepts zero; `for` does not, because a window that silences
    /// nothing is a mistake worth surfacing rather than a no-op to absorb.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] if either field is not a valid duration.
    pub fn resolve(&self) -> Result<(std::time::Duration, std::time::Duration), SondaError> {
        Ok((
            validate::parse_offset_duration(&self.at)?,
            validate::parse_duration(&self.r#for)?,
        ))
    }
}

/// Strategy for generating unique label values during a cardinality spike.
///
/// Determines how the spike window produces distinct values for the injected
/// label key on each tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BurstConfig {
    /// How often the burst recurs (e.g. `"10s"`).
    pub every: String,
    /// How long each burst lasts (e.g. `"2s"`). Must be less than `every`.
    pub r#for: String,
    /// Rate multiplier during the burst (must be strictly positive).
    pub multiplier: f64,
}

/// Prometheus exposition metric kind for `# TYPE` annotations.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
pub enum PromMetricType {
    Gauge,
    Counter,
    Histogram,
    Summary,
    Untyped,
}

impl PromMetricType {
    /// Canonical lowercase string used on the Prometheus `# TYPE` line.
    pub fn as_str(&self) -> &'static str {
        match self {
            PromMetricType::Gauge => "gauge",
            PromMetricType::Counter => "counter",
            PromMetricType::Histogram => "histogram",
            PromMetricType::Summary => "summary",
            PromMetricType::Untyped => "untyped",
        }
    }
}

/// Prometheus metadata attached to a launched scenario handle.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromMeta {
    pub metric_type: PromMetricType,
    pub help: Option<String>,
}

impl PromMeta {
    pub fn new(metric_type: PromMetricType, help: Option<String>) -> Self {
        Self { metric_type, help }
    }
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

/// Policy for handling sink I/O errors during a running scenario.
///
/// `Warn` (the default) logs a rate-limited message, increments error stats,
/// drops the failing batch, and continues ticking. `Fail` propagates the
/// error and terminates the scenario thread — the historical behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
pub enum OnSinkError {
    #[default]
    Warn,
    Fail,
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BaseScheduleConfig {
    /// Scenario name (metric name for metrics, identifier for logs).
    pub name: String,
    /// Target event rate in events per second. Must be strictly positive.
    pub rate: f64,
    /// Optional total run duration (e.g. `"30s"`, `"5m"`). `None` means run indefinitely.
    #[cfg_attr(feature = "config", serde(default))]
    pub duration: Option<String>,
    /// Optional emission-time anchor. Absolute RFC 3339 (`2026-05-08T14:00:00Z`),
    /// signed relative offset (`+24h`, `-7d`), or `now`. `None` means `now`.
    #[cfg_attr(feature = "config", serde(default))]
    pub start_time: Option<String>,
    /// Optional gap window: recurring silent periods in the event stream.
    #[cfg_attr(feature = "config", serde(default))]
    pub gaps: Option<GapConfig>,
    /// Optional one-shot silent windows at fixed offsets from scenario start.
    ///
    /// Coexists with [`Self::gaps`]: both suppress emission and either may be
    /// absent. Where `gaps` describes silence that recurs, this describes
    /// silence that happened — the shape a captured incident has.
    #[cfg_attr(feature = "config", serde(default))]
    pub gap_windows: Option<Vec<GapWindowConfig>>,
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
    /// Provenance of [`Self::clock_group`] from the v2 compiler.
    ///
    /// Populated by [`crate::compiler::prepare`] when an entry traverses
    /// the v2 compile pipeline. Carries:
    ///
    /// - `Some(true)` — the compiler synthesized
    ///   `chain_{lowest_lex_id}` because the `after:` component had no
    ///   user-supplied `clock_group`.
    /// - `Some(false)` — the value was adopted from an explicit user
    ///   assignment (including explicit values that happen to start with
    ///   `chain_`).
    /// - `None` — the entry did not flow through the v2 compiler (v1
    ///   loaders, hand-built configs); display code must not show an
    ///   `(auto)` marker.
    ///
    /// Hidden from YAML serialization because it is a compiler-derived
    /// field, not user-supplied input. Skipped from deserialization for
    /// the same reason — round-tripping a config never resurrects this
    /// flag.
    #[cfg_attr(feature = "config", serde(skip))]
    pub clock_group_is_auto: Option<bool>,
    /// Optional jitter amplitude. When set, adds uniform noise in
    /// `[-jitter, +jitter]` to every generated value. Defaults to `None` (no jitter).
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter: Option<f64>,
    /// Optional seed for jitter noise. Defaults to `0` when absent.
    /// Different seeds produce different noise sequences.
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter_seed: Option<u64>,
    /// Behavior when a sink write returns an I/O error mid-run.
    #[cfg_attr(feature = "config", serde(default))]
    pub on_sink_error: OnSinkError,
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScenarioConfig {
    /// Shared schedule and delivery fields.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub base: BaseScheduleConfig,
    /// Value generator configuration.
    pub generator: GeneratorConfig,
    /// Output encoder. Defaults to `prometheus_text`.
    #[cfg_attr(feature = "config", serde(default = "default_encoder"))]
    pub encoder: EncoderConfig,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub metric_type: Option<PromMetricType>,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub help: Option<String>,
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

/// Distribution model configuration for histogram and summary generators.
///
/// Determines how sample values are distributed when the generator produces
/// observations on each tick. Deserialized from YAML via the `type` tag.
///
/// # Example YAML
///
/// ```yaml
/// distribution:
///   type: exponential
///   rate: 10.0
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "config", serde(tag = "type"))]
#[non_exhaustive]
pub enum DistributionConfig {
    /// Exponential distribution with rate parameter lambda.
    ///
    /// Mean = 1/lambda. Models latency distributions.
    #[cfg_attr(feature = "config", serde(rename = "exponential"))]
    Exponential {
        /// Rate parameter (lambda). Must be strictly positive.
        rate: f64,
    },
    /// Normal (Gaussian) distribution.
    #[cfg_attr(feature = "config", serde(rename = "normal"))]
    Normal {
        /// Center of the distribution.
        mean: f64,
        /// Spread of the distribution. Must be strictly positive.
        stddev: f64,
    },
    /// Uniform distribution over `[min, max]`.
    #[cfg_attr(feature = "config", serde(rename = "uniform"))]
    Uniform {
        /// Lower bound (inclusive).
        min: f64,
        /// Upper bound (inclusive).
        max: f64,
    },
}

/// Full configuration for a single histogram scenario run.
///
/// Embeds [`BaseScheduleConfig`] for the shared schedule and delivery fields,
/// adding histogram-specific parameters: bucket boundaries, distribution model,
/// observations per tick, mean shift, and seed.
///
/// # Example YAML
///
/// ```yaml
/// signal_type: histogram
/// name: http_request_duration_seconds
/// rate: 1
/// duration: 5m
/// buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
/// distribution:
///   type: exponential
///   rate: 10.0
/// observations_per_tick: 100
/// seed: 42
/// labels:
///   method: GET
/// encoder:
///   type: prometheus_text
/// sink:
///   type: stdout
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistogramScenarioConfig {
    /// Shared schedule and delivery fields.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub base: BaseScheduleConfig,
    /// Histogram bucket upper bounds. When `None`, uses the default Prometheus
    /// bucket boundaries: `[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]`.
    #[cfg_attr(feature = "config", serde(default))]
    pub buckets: Option<Vec<f64>>,
    /// Distribution model for generating observations.
    pub distribution: DistributionConfig,
    /// Number of observations to sample per tick. Defaults to 100.
    #[cfg_attr(feature = "config", serde(default))]
    pub observations_per_tick: Option<u64>,
    /// Linear drift applied to the distribution center per second. Defaults to 0.0.
    #[cfg_attr(feature = "config", serde(default))]
    pub mean_shift_per_sec: Option<f64>,
    /// Determinism seed for the RNG. Defaults to 0.
    #[cfg_attr(feature = "config", serde(default))]
    pub seed: Option<u64>,
    /// Output encoder. Defaults to `prometheus_text`.
    #[cfg_attr(feature = "config", serde(default = "default_encoder"))]
    pub encoder: EncoderConfig,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub metric_type: Option<PromMetricType>,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub help: Option<String>,
}

impl std::ops::Deref for HistogramScenarioConfig {
    type Target = BaseScheduleConfig;

    fn deref(&self) -> &BaseScheduleConfig {
        &self.base
    }
}

impl std::ops::DerefMut for HistogramScenarioConfig {
    fn deref_mut(&mut self) -> &mut BaseScheduleConfig {
        &mut self.base
    }
}

/// Full configuration for a single summary scenario run.
///
/// Embeds [`BaseScheduleConfig`] for the shared schedule and delivery fields,
/// adding summary-specific parameters: quantile targets, distribution model,
/// observations per tick, mean shift, and seed.
///
/// # Example YAML
///
/// ```yaml
/// signal_type: summary
/// name: rpc_duration_seconds
/// rate: 1
/// duration: 5m
/// quantiles: [0.5, 0.9, 0.95, 0.99]
/// distribution:
///   type: normal
///   mean: 0.1
///   stddev: 0.02
/// observations_per_tick: 100
/// seed: 42
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SummaryScenarioConfig {
    /// Shared schedule and delivery fields.
    #[cfg_attr(feature = "config", serde(flatten))]
    pub base: BaseScheduleConfig,
    /// Quantile targets to compute. When `None`, uses default quantiles:
    /// `[0.5, 0.9, 0.95, 0.99]`.
    #[cfg_attr(feature = "config", serde(default))]
    pub quantiles: Option<Vec<f64>>,
    /// Distribution model for generating observations.
    pub distribution: DistributionConfig,
    /// Number of observations to sample per tick. Defaults to 100.
    #[cfg_attr(feature = "config", serde(default))]
    pub observations_per_tick: Option<u64>,
    /// Linear drift applied to the distribution center per second. Defaults to 0.0.
    #[cfg_attr(feature = "config", serde(default))]
    pub mean_shift_per_sec: Option<f64>,
    /// Determinism seed for the RNG. Defaults to 0.
    #[cfg_attr(feature = "config", serde(default))]
    pub seed: Option<u64>,
    /// Output encoder. Defaults to `prometheus_text`.
    #[cfg_attr(feature = "config", serde(default = "default_encoder"))]
    pub encoder: EncoderConfig,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub metric_type: Option<PromMetricType>,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub help: Option<String>,
}

impl std::ops::Deref for SummaryScenarioConfig {
    type Target = BaseScheduleConfig;

    fn deref(&self) -> &BaseScheduleConfig {
        &self.base
    }
}

impl std::ops::DerefMut for SummaryScenarioConfig {
    fn deref_mut(&mut self) -> &mut BaseScheduleConfig {
        &mut self.base
    }
}

/// A single entry in a multi-scenario configuration.
///
/// The `signal_type` tag selects whether this entry is a metrics, logs,
/// histogram, or summary scenario.
/// Deserialized from a YAML multi-scenario file where each element of the
/// `scenarios` list carries a `signal_type` key.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "config", serde(tag = "signal_type"))]
#[non_exhaustive]
pub enum ScenarioEntry {
    /// A metrics scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "metrics"))]
    Metrics(ScenarioConfig),
    /// A logs scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "logs"))]
    Logs(LogScenarioConfig),
    /// A histogram scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "histogram"))]
    Histogram(HistogramScenarioConfig),
    /// A summary scenario entry.
    #[cfg_attr(feature = "config", serde(rename = "summary"))]
    Summary(SummaryScenarioConfig),
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
            ScenarioEntry::Histogram(c) => &c.base,
            ScenarioEntry::Summary(c) => &c.base,
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

    /// Return the v2-compiler-derived provenance for [`Self::clock_group`].
    ///
    /// `Some(true)` when the v2 compiler synthesized the `chain_<id>`
    /// name; `Some(false)` for explicit user assignments via the v2
    /// pipeline; `None` for entries that bypassed the v2 compiler (v1
    /// loaders, hand-built configs).
    pub fn clock_group_is_auto(&self) -> Option<bool> {
        self.base().clock_group_is_auto
    }

    /// Return the human-readable signal type name for this entry.
    ///
    /// Matches the `signal_type:` discriminant used in v2 scenario YAML
    /// (`"metrics"`, `"logs"`, `"histogram"`, `"summary"`). Intended for
    /// diagnostics and user-facing mismatch messages.
    ///
    /// [`ScenarioEntry`] is `#[non_exhaustive]`; variants added in the
    /// future surface here as `"unknown"` until wired in. The catch-all
    /// arm is intra-crate-unreachable today (Rust only enforces
    /// `#[non_exhaustive]` across crate boundaries) but is retained so
    /// the forward-compat contract is visible at the method site.
    #[allow(unreachable_patterns)]
    pub fn signal_type_name(&self) -> &'static str {
        match self {
            ScenarioEntry::Metrics(_) => "metrics",
            ScenarioEntry::Logs(_) => "logs",
            ScenarioEntry::Histogram(_) => "histogram",
            ScenarioEntry::Summary(_) => "summary",
            // `ScenarioEntry` is `#[non_exhaustive]` across the crate boundary;
            // future signal kinds will surface here as "unknown" until wired in.
            _ => "unknown",
        }
    }
}

/// Validate the `columns` field of a `CsvReplay` generator config.
///
/// Returns an error when:
/// - `columns` is `Some` but the list is empty.
/// - `columns` contains duplicate indices.
/// - `columns` contains duplicate metric names.
///
/// This validation is called before expansion so that invalid configs are
/// rejected early with a clear error message.
///
/// # Errors
///
/// Returns [`SondaError::Config`] with a descriptive message.
fn validate_csv_columns(columns: &Option<Vec<CsvColumnSpec>>) -> Result<(), SondaError> {
    if let Some(ref cols) = columns {
        if cols.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "csv_replay: 'columns' must not be empty; provide at least one column spec or omit the field",
            )));
        }

        // Reject duplicate column indices.
        let mut seen_indices = std::collections::HashSet::with_capacity(cols.len());
        for spec in cols {
            if !seen_indices.insert(spec.index) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: duplicate column index {}; each column index must be unique",
                    spec.index
                ))));
            }
        }

        // Reject duplicate metric names.
        let mut seen_names = std::collections::HashSet::with_capacity(cols.len());
        for spec in cols {
            if !seen_names.insert(&spec.name) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: duplicate column name '{}'; each column name must be unique",
                    spec.name
                ))));
            }
        }
    }
    Ok(())
}

/// Read the first non-comment, non-empty line from a CSV file.
///
/// Uses a [`BufReader`](std::io::BufReader) to read only as many lines as
/// needed, avoiding loading the entire file into memory.
///
/// # Errors
///
/// Returns [`SondaError::Generator`] with [`GeneratorError::FileRead`] if the
/// file cannot be opened or read. Returns [`SondaError::Config`] if the file
/// has no non-comment, non-empty lines.
fn read_csv_header(path: &str) -> Result<String, SondaError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|e| {
        SondaError::Generator(crate::GeneratorError::FileRead {
            path: path.to_string(),
            source: e,
        })
    })?;
    let reader = std::io::BufReader::new(file);

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| {
            SondaError::Generator(crate::GeneratorError::FileRead {
                path: path.to_string(),
                source: e,
            })
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return Ok(line);
    }

    Err(SondaError::Config(ConfigError::invalid(format!(
        "csv_replay: file {:?} has no non-comment, non-empty lines",
        path
    ))))
}

/// Re-export of the shared header detection logic from [`crate::generator::csv_header`].
fn is_csv_header_line(line: &str) -> bool {
    crate::generator::csv_header::is_header_line(line)
}

fn read_csv_first_lines(path: &str, max_lines: usize) -> Result<Vec<String>, SondaError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|e| {
        SondaError::Generator(crate::GeneratorError::FileRead {
            path: path.to_string(),
            source: e,
        })
    })?;
    let reader = std::io::BufReader::new(file);
    let mut out = Vec::with_capacity(max_lines);

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| {
            SondaError::Generator(crate::GeneratorError::FileRead {
                path: path.to_string(),
                source: e,
            })
        })?;
        if is_csv_skippable_line(&line) {
            continue;
        }
        out.push(line);
        if out.len() >= max_lines {
            break;
        }
    }
    Ok(out)
}

/// A line carrying no data: blank, or a `#` comment.
///
/// One definition because two readers walk the same file and must agree on
/// which lines are rows — the sampled rate derivation and the full-file
/// monotonicity check. If they disagreed, the row numbers in an error message
/// would not point at the row the reader has to fix.
fn is_csv_skippable_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// Refuse a timestamp column that does not strictly increase.
///
/// [`compute_csv_delta_seconds`] reads only [`CSV_DELTA_SAMPLE_ROWS`] rows, so
/// the monotonicity it enforces covers the head of the file. This walks every
/// row: a repeated or out-of-order stamp anywhere is wrong data, not a pacing
/// choice, and replaying it would silently reorder the capture.
///
/// Streams rather than collecting — a capture is not bounded in length.
///
/// # Errors
///
/// Names the first offending row and both stamps. Row numbers count data rows
/// from 0, matching [`compute_csv_delta_seconds`].
fn validate_csv_timestamps_monotonic(path: &str, ts_col_idx: usize) -> Result<(), SondaError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(|e| {
        SondaError::Generator(crate::GeneratorError::FileRead {
            path: path.to_string(),
            source: e,
        })
    })?;

    let mut prev: Option<f64> = None;
    let mut row_idx = 0usize;
    let mut header_checked = false;

    for line_result in std::io::BufReader::new(file).lines() {
        let line = line_result.map_err(|e| {
            SondaError::Generator(crate::GeneratorError::FileRead {
                path: path.to_string(),
                source: e,
            })
        })?;
        if is_csv_skippable_line(&line) {
            continue;
        }
        if !header_checked {
            header_checked = true;
            if is_csv_header_line(&line) {
                continue;
            }
        }

        let cell = line.split(',').nth(ts_col_idx).unwrap_or("");
        let ts = parse_csv_timestamp(cell).ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "csv_replay: failed to parse timestamp at data row {row_idx} column {ts_col_idx}: {cell:?}"
            )))
        })?;

        if let Some(previous) = prev {
            if ts <= previous {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: file {path:?} has non-monotonic timestamps at data row \
                     {row_idx}: {ts} is not greater than the previous row's {previous}.\n\n  \
                     hint: the timestamp column must strictly increase; sonda replays rows in \
                     file order and derives the rate from it"
                ))));
            }
        }
        prev = Some(ts);
        row_idx += 1;
    }

    Ok(())
}

fn parse_csv_timestamp(cell: &str) -> Option<f64> {
    let v: f64 = cell.trim().parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    if v > 1e12 {
        Some(v / 1000.0)
    } else {
        Some(v)
    }
}

fn compute_csv_delta_seconds(path: &str, ts_col_idx: usize) -> Result<f64, SondaError> {
    let lines = read_csv_first_lines(path, CSV_DELTA_SAMPLE_ROWS + 1)?;
    if lines.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: file {:?} has no non-comment, non-empty lines",
            path
        ))));
    }

    let data_lines: &[String] = if is_csv_header_line(&lines[0]) {
        &lines[1..]
    } else {
        &lines[..]
    };

    let mut timestamps = Vec::with_capacity(data_lines.len());
    for (row_idx, line) in data_lines.iter().enumerate() {
        let cell = line.split(',').nth(ts_col_idx).unwrap_or("");
        let ts = parse_csv_timestamp(cell).ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "csv_replay: failed to parse timestamp at data row {} column {}: {:?}",
                row_idx, ts_col_idx, cell
            )))
        })?;
        timestamps.push(ts);
    }

    if timestamps.len() < 2 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: file {:?} has fewer than 2 data rows; cannot derive replay rate",
            path
        ))));
    }

    median_delta_seconds(&timestamps).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: cannot derive a replay interval from {path:?}: {e}"
        )))
    })
}

/// The replay interval a run of timestamps implies: the median pairwise delta.
///
/// **The one definition of that reduction.** `compute_csv_delta_seconds` calls
/// it over the timestamps it parsed from a file, and
/// [`crate::acquire::yaml_out`] calls it over the instants it is about to
/// write, so the interval a capture is emitted against and the interval the
/// engine derives from that capture are the same number by construction.
///
/// They were not, and the gap was not obvious: a second implementation over
/// there modelled this as "the delta between the first two rows". That agrees
/// with the median for most steps and disagrees when the step's millisecond
/// value is a clean half, because the two round to different sides — and the
/// emitted `gap_windows:` then drift against the replay until the capture no
/// longer loads. Reimplementing it a third time to fix that reproduces the
/// same class of bug, so there is one of it and both sides call it.
///
/// Median rather than mean because a capture can contain one irregular delta —
/// a scrape that landed late — and a mean would smear that across the whole
/// replay.
///
/// # Errors
///
/// Returns a bare message naming the problem; callers attach the file. Fails on
/// fewer than two timestamps, on a non-monotonic pair, and on a reduction that
/// does not come out positive and finite.
pub(crate) fn median_delta_seconds(timestamps: &[f64]) -> Result<f64, String> {
    if timestamps.len() < 2 {
        return Err(format!(
            "fewer than 2 data rows ({}); there is no interval between them",
            timestamps.len()
        ));
    }

    let mut deltas = Vec::with_capacity(timestamps.len() - 1);
    for pair in timestamps.windows(2) {
        let d = pair[1] - pair[0];
        if d <= 0.0 {
            return Err(format!(
                "non-monotonic timestamps (row {} value {} <= previous {})",
                deltas.len() + 1,
                pair[1],
                pair[0]
            ));
        }
        deltas.push(d);
    }

    deltas.sort_by(|a, b| {
        a.partial_cmp(b)
            .expect("CSV deltas are finite and positive")
    });
    let mid = deltas.len() / 2;
    let median = if deltas.len() % 2 == 0 {
        (deltas[mid - 1] + deltas[mid]) / 2.0
    } else {
        deltas[mid]
    };

    if median <= 0.0 || !median.is_finite() {
        return Err(format!("derived median Δt {median} is not positive"));
    }

    Ok(median)
}

/// How many data rows [`compute_csv_delta_seconds`] samples when deriving the
/// interval.
///
/// Exported so [`crate::acquire::yaml_out`] samples the same window. Sampling a
/// different number of rows would put a different set of deltas into the
/// median, which is the same divergence the shared reduction exists to prevent.
pub(crate) const CSV_DELTA_SAMPLE_ROWS: usize = 100;

/// Expand a `csv_replay` scenario into one config per data column, deriving
/// `rate` from the CSV's column-0 timestamps (`rate = timescale / median Δt`).
/// Non-`csv_replay` configs pass through unchanged.
pub fn expand_scenario(config: ScenarioConfig) -> Result<Vec<ScenarioConfig>, SondaError> {
    let (file, columns_field, timescale_opt, default_name_opt, repeat_field) =
        match &config.generator {
            GeneratorConfig::CsvReplay {
                file,
                columns,
                timescale,
                default_metric_name,
                repeat,
                ..
            } => (
                file.clone(),
                columns.clone(),
                *timescale,
                default_metric_name.clone(),
                *repeat,
            ),
            _ => return Ok(vec![config]),
        };

    validate_csv_columns(&columns_field)?;
    let timescale = validate_csv_timescale(timescale_opt)?;

    let header_line = read_csv_header(&file)?;
    let has_header = is_csv_header_line(&header_line);
    let parsed_header = if has_header {
        Some(crate::generator::csv_header::parse_header_row(
            &header_line,
        )?)
    } else {
        None
    };

    let specs = if let Some(cols) = columns_field {
        merge_header_labels_into_specs(cols, parsed_header.as_deref())
    } else {
        let parsed = parsed_header.ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(
                "csv_replay: CSV file has no header row (first data line is all numeric); \
                 provide explicit 'columns' in the config",
            ))
        })?;
        auto_discover_specs(parsed, default_name_opt.as_deref())?
    };

    validate_csv_timestamps_monotonic(&file, 0)?;
    let delta = compute_csv_delta_seconds(&file, 0)?;
    let derived_rate = timescale / delta;

    // Blank cells and `gap_windows:` must describe the same silence. Checked
    // here rather than in the generator because this is the one place that
    // knows both halves: the file, and the schedule config that declares the
    // windows. Every column is checked — a blank in the third series is as
    // wrong as a blank in the first.
    //
    // The replay clock runs at `derived_rate`, so data row n stands for the
    // instant n / derived_rate. That is the same number the scheduler will
    // compute from elapsed time, which is why the two agree about where a
    // window falls.
    // Run unconditionally, NOT only when `gap_windows:` is present. Guarding
    // this on the windows existing would skip the check in exactly the case it
    // most needs to fire — a CSV with blank cells and no windows declared at
    // all, which is the shape a hand-edited capture takes. An absent list is
    // an empty list here, so "blank with no window" is caught rather than
    // waved through.
    let windows: Vec<(f64, f64)> = config
        .base
        .gap_windows
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|w| -> Result<(f64, f64), SondaError> {
            let (at, dur) = w.resolve()?;
            let at = at.as_secs_f64();
            Ok((at, at + dur.as_secs_f64()))
        })
        .collect::<Result<_, _>>()?;
    {
        let content = std::fs::read_to_string(&file).map_err(|e| {
            SondaError::Generator(crate::GeneratorError::FileRead {
                path: file.clone(),
                source: e,
            })
        })?;
        let step_secs = 1.0 / derived_rate;

        // The rows alone do not say which instants get played — `repeat` loops
        // the column and `repeat: false` clamps past its end — so the check is
        // given the playback, resolved here where both the generator's `repeat`
        // and the schedule's `duration:` are in hand.
        //
        // `repeat` is resolved through the SAME `unwrap_or(true)` the generator
        // factory applies. Reading the Option here and defaulting differently
        // is how a check ends up disagreeing with the thing it checks.
        let repeat = repeat_field.unwrap_or(true);
        let last_tick = config
            .base
            .duration
            .as_deref()
            .map(crate::config::validate::parse_duration)
            .transpose()?
            .map(|d| {
                // Ticks land at 0, step, 2*step, … while t < duration, so the
                // count is ceil(duration / step) and the last index is one
                // below it. `parse_duration` refuses zero, so a positive
                // duration always yields at least one tick — `map`, not
                // `and_then`, because `None` here means "unbounded" and must
                // not double as "no ticks played".
                let ticks = (d.as_secs_f64() / step_secs).ceil() as u64;
                ticks.saturating_sub(1)
            });

        for spec in &specs {
            let (values, blanks) =
                crate::generator::csv_replay::column_values_and_gaps(&content, spec.index)?;
            crate::generator::csv_replay::cross_check_gap_windows(
                &blanks,
                &crate::generator::csv_replay::Playback {
                    row_count: values.len(),
                    repeat,
                    last_tick,
                    bursts: config.base.bursts.is_some(),
                },
                &windows,
                step_secs,
            )?;
        }
    }

    let user_rate = config.base.rate;
    if (user_rate - derived_rate).abs() > 1e-9 {
        tracing::warn!(
            scenario = %config.base.name,
            user_rate,
            derived_rate,
            csv_delta_secs = delta,
            timescale,
            "csv_replay '{}': overriding rate={} with derived rate={} samples/s (CSV Δt={}s, timescale={})",
            config.base.name,
            user_rate,
            derived_rate,
            delta,
            timescale,
        );
    }

    let expanded = specs
        .into_iter()
        .map(|spec| {
            let mut child = config.clone();
            child.base.name = spec.name;
            child.base.rate = derived_rate;

            if let Some(ref col_labels) = spec.labels {
                let merged = child.base.labels.get_or_insert_with(HashMap::new);
                for (k, v) in col_labels {
                    merged.insert(k.clone(), v.clone());
                }
            }

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

fn validate_csv_timescale(timescale: Option<f64>) -> Result<f64, SondaError> {
    let ts = timescale.unwrap_or(1.0);
    if !(ts.is_finite() && ts > 0.0) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: 'timescale' must be a positive finite number, got {}",
            ts
        ))));
    }
    Ok(ts)
}

fn merge_header_labels_into_specs(
    user_specs: Vec<CsvColumnSpec>,
    parsed_header: Option<&[crate::generator::csv_header::ParsedColumnHeader]>,
) -> Vec<CsvColumnSpec> {
    user_specs
        .into_iter()
        .map(|spec| {
            let header_labels = parsed_header
                .and_then(|hdr| hdr.get(spec.index))
                .map(|h| &h.labels);
            let merged_labels = match (header_labels, spec.labels.as_ref()) {
                (None, None) => None,
                (None, Some(user)) => Some(user.clone()),
                (Some(hdr), None) if hdr.is_empty() => None,
                (Some(hdr), None) => Some(hdr.clone()),
                (Some(hdr), Some(user)) => {
                    let mut out = hdr.clone();
                    for (k, v) in user {
                        out.insert(k.clone(), v.clone());
                    }
                    Some(out)
                }
            };
            CsvColumnSpec {
                labels: merged_labels,
                ..spec
            }
        })
        .collect()
}

fn auto_discover_specs(
    parsed: Vec<crate::generator::csv_header::ParsedColumnHeader>,
    default_metric_name: Option<&str>,
) -> Result<Vec<CsvColumnSpec>, SondaError> {
    let nameless: Vec<usize> = parsed
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, ph)| ph.metric_name.is_none())
        .map(|(i, _)| i)
        .collect();
    let needs_suffix = nameless.len() > 1;

    let mut specs = Vec::with_capacity(parsed.len().saturating_sub(1));
    let mut default_derived_indices = Vec::new();
    let mut explicit_names = std::collections::HashSet::new();
    for (i, ph) in parsed.into_iter().enumerate().skip(1) {
        let (name, derived_from_default) = match ph.metric_name {
            Some(n) => {
                explicit_names.insert(n.clone());
                (n, false)
            }
            None => {
                let base = default_metric_name.ok_or_else(|| {
                    SondaError::Config(ConfigError::invalid(format!(
                        "csv_replay: column {} has no metric name \
                         (header has labels only with no __name__); \
                         set 'default_metric_name' on the generator config",
                        i
                    )))
                })?;
                let n = if needs_suffix {
                    format!("{}_{}", base, i)
                } else {
                    base.to_string()
                };
                (n, true)
            }
        };
        if derived_from_default {
            default_derived_indices.push(specs.len());
        }
        let labels = if ph.labels.is_empty() {
            None
        } else {
            Some(ph.labels)
        };
        specs.push(CsvColumnSpec {
            index: i,
            name,
            labels,
        });
    }

    if specs.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "csv_replay: no data columns found after skipping column 0",
        )));
    }

    for idx in default_derived_indices {
        let name = &specs[idx].name;
        if explicit_names.contains(name) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "csv_replay: default_metric_name produced '{name}' for column {col} \
                 which collides with an explicitly named column. Rename the \
                 conflicting __name__ in the CSV header or set a different \
                 'default_metric_name'.",
                col = specs[idx].index
            ))));
        }
    }

    Ok(specs)
}

/// Expand a [`ScenarioEntry`] that uses multi-column `csv_replay`.
///
/// For `ScenarioEntry::Metrics`, delegates to [`expand_scenario`] and wraps
/// the results back in `ScenarioEntry::Metrics`. For `ScenarioEntry::Logs`
/// with a `csv_replay` generator, delegates to [`expand_log_scenario`].
/// All other entries pass through unchanged.
pub fn expand_entry(entry: ScenarioEntry) -> Result<Vec<ScenarioEntry>, SondaError> {
    match entry {
        ScenarioEntry::Metrics(config) => {
            let expanded = expand_scenario(config)?;
            Ok(expanded.into_iter().map(ScenarioEntry::Metrics).collect())
        }
        ScenarioEntry::Logs(config) => {
            let expanded = expand_log_scenario(config)?;
            Ok(expanded.into_iter().map(ScenarioEntry::Logs).collect())
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
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// Expand a log scenario using `log_csv_replay`, deriving the scenario rate
/// from the CSV's resolved timestamp column (`rate = timescale / median Δt`).
///
/// Non-`csv_replay` log generators pass through unchanged. Always returns a
/// single-element vector.
pub fn expand_log_scenario(
    config: LogScenarioConfig,
) -> Result<Vec<LogScenarioConfig>, SondaError> {
    let (file, timescale_opt, columns) = match &config.generator {
        LogGeneratorConfig::CsvReplay {
            file,
            timescale,
            columns,
            ..
        } => (file.clone(), *timescale, columns.clone()),
        _ => return Ok(vec![config]),
    };

    let timescale = validate_csv_timescale(timescale_opt)?;
    let ts_col_idx =
        crate::generator::log_csv_replay::resolve_timestamp_column_index(&file, columns.as_ref())?;
    validate_csv_timestamps_monotonic(&file, ts_col_idx)?;
    let delta = compute_csv_delta_seconds(&file, ts_col_idx)?;
    let derived_rate = timescale / delta;
    let user_rate = config.base.rate;

    if (user_rate - derived_rate).abs() > 1e-9 {
        tracing::warn!(
            scenario = %config.base.name,
            user_rate,
            derived_rate,
            csv_delta_secs = delta,
            timescale,
            "log_csv_replay '{}': overriding rate={} with derived rate={} samples/s (CSV Δt={}s, timescale={})",
            config.base.name,
            user_rate,
            derived_rate,
            delta,
            timescale,
        );
    }

    let fallback_count = count_log_csv_severity_fallbacks(&config.generator, &file)?;
    if fallback_count > 0 {
        tracing::warn!(
            scenario = %config.base.name,
            fallback_count,
            "log_csv_replay '{}': {} row(s) used default_severity due to missing or unparseable severity values",
            config.base.name,
            fallback_count,
        );
    }

    let mut child = config;
    child.base.rate = derived_rate;
    Ok(vec![child])
}

fn count_log_csv_severity_fallbacks(
    generator: &LogGeneratorConfig,
    file: &str,
) -> Result<usize, SondaError> {
    let LogGeneratorConfig::CsvReplay {
        columns,
        default_severity,
        repeat,
        ..
    } = generator
    else {
        return Ok(0);
    };

    let content = std::fs::read_to_string(file).map_err(|e| {
        SondaError::Generator(crate::GeneratorError::FileRead {
            path: file.to_string(),
            source: e,
        })
    })?;

    let (_gen, fallback) =
        crate::generator::log_csv_replay::LogCsvReplayGenerator::from_str_with_fallback_count(
            &content,
            columns.as_ref(),
            default_severity.unwrap_or(crate::model::log::Severity::Info),
            repeat.unwrap_or(true),
        )?;
    Ok(fallback)
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        assert_eq!(entry.phase_offset(), Some("5s"));
    }

    /// ScenarioEntry::phase_offset() returns None when not set on Metrics.
    #[test]
    fn scenario_entry_phase_offset_returns_none_for_metrics_without_offset() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        assert_eq!(entry.phase_offset(), None);
    }

    /// ScenarioEntry::phase_offset() returns the phase_offset for a Logs entry.
    #[test]
    fn scenario_entry_phase_offset_returns_value_for_logs() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        assert_eq!(entry.clock_group(), Some("my-group"));
    }

    /// ScenarioEntry::clock_group() returns None when not set.
    #[test]
    fn scenario_entry_clock_group_returns_none_when_absent() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        assert_eq!(entry.base().name, "base_test");
        assert_eq!(entry.base().rate, 42.0);
    }

    /// ScenarioEntry::base() returns the shared config for a Logs entry.
    #[test]
    fn scenario_entry_base_returns_shared_config_for_logs() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
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
            gap_windows: None,
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
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: crate::OnSinkError::Warn,
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                assert!(column.is_none(), "column is serde(skip), should be None");
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
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match &config.generator {
            GeneratorConfig::CsvReplay {
                columns, column, ..
            } => {
                assert_eq!(*column, None, "column is serde(skip)");
                assert!(
                    columns.is_none(),
                    "columns should be None when not specified"
                );
            }
            other => panic!("expected CsvReplay variant, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // ScenarioEntry::signal_type_name()
    // -----------------------------------------------------------------------

    /// `signal_type_name()` returns the v2 YAML discriminant string for
    /// every currently wired [`ScenarioEntry`] variant.
    #[test]
    fn scenario_entry_signal_type_name_covers_all_variants() {
        // Metrics entry.
        let metrics_yaml = r#"
signal_type: metrics
name: cpu
rate: 1
generator:
  type: constant
  value: 1.0
"#;
        let metrics: ScenarioEntry = serde_yaml_ng::from_str(metrics_yaml).unwrap();
        assert_eq!(metrics.signal_type_name(), "metrics");

        // Logs entry.
        let logs_yaml = r#"
signal_type: logs
name: app_logs
rate: 1
generator:
  type: csv_replay
  file: /tmp/does-not-need-to-exist.csv
"#;
        let logs: ScenarioEntry = serde_yaml_ng::from_str(logs_yaml).unwrap();
        assert_eq!(logs.signal_type_name(), "logs");

        // Histogram entry.
        let histogram_yaml = r#"
signal_type: histogram
name: req_latency
rate: 1
observations_per_tick: 100
buckets: [0.1, 0.5, 1.0]
distribution:
  type: uniform
  min: 0.0
  max: 1.0
"#;
        let histogram: ScenarioEntry = serde_yaml_ng::from_str(histogram_yaml).unwrap();
        assert_eq!(histogram.signal_type_name(), "histogram");

        // Summary entry.
        let summary_yaml = r#"
signal_type: summary
name: req_latency_summary
rate: 1
observations_per_tick: 100
quantiles: [0.5, 0.9, 0.99]
distribution:
  type: uniform
  min: 0.0
  max: 1.0
"#;
        let summary: ScenarioEntry = serde_yaml_ng::from_str(summary_yaml).unwrap();
        assert_eq!(summary.signal_type_name(), "summary");
    }
}

#[cfg(test)]
mod expand_tests {
    use super::*;
    use crate::encoder::EncoderConfig;
    use crate::generator::{CsvColumnSpec, GeneratorConfig};
    use crate::sink::SinkConfig;

    /// Build a base `ScenarioConfig` with a csv_replay generator for testing.
    fn csv_replay_config(name: &str, columns: Option<Vec<CsvColumnSpec>>) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: Some(0.5),
                jitter_seed: Some(42),
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: "data.csv".to_string(),
                column: None,
                repeat: Some(true),
                columns,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        }
    }

    fn write_temp_timing_csv(header: &str, data_rows: usize) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        if !header.is_empty() {
            writeln!(tmp, "{}", header).expect("write header");
        }
        for i in 0..data_rows {
            let ts = 1_700_000_000 + (i as u64) * 10;
            writeln!(tmp, "{ts},42.5,60.0,80.0,99.0").expect("write row");
        }
        tmp.flush().expect("flush");
        tmp
    }

    fn set_csv_file(config: &mut ScenarioConfig, path: String) {
        if let GeneratorConfig::CsvReplay { ref mut file, .. } = config.generator {
            *file = path;
        }
    }

    fn config_with_csv(
        name: &str,
        columns: Option<Vec<CsvColumnSpec>>,
    ) -> (ScenarioConfig, tempfile::NamedTempFile) {
        let tmp = write_temp_timing_csv("Time,cpu,mem,disk,net", 3);
        let mut config = csv_replay_config(name, columns);
        set_csv_file(&mut config, tmp.path().to_string_lossy().into_owned());
        (config, tmp)
    }

    // -----------------------------------------------------------------------
    // expand_scenario: pass-through (no columns)
    // -----------------------------------------------------------------------

    /// When columns is None and the CSV has a header, expand_scenario
    /// auto-discovers columns from the header.
    #[test]
    fn auto_discover_from_header_when_no_columns() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "Time,cpu_usage\n1700000000,42.5\n1700000010,43.0\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let mut config = csv_replay_config("single_metric", None);
        set_csv_file(&mut config, path);
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1, "should auto-discover 1 data column");
        assert_eq!(result[0].name, "cpu_usage");

        drop(tmp);
    }

    /// When columns is None and the CSV has no header (all numeric),
    /// expand_scenario returns an error asking for explicit columns.
    #[test]
    fn no_columns_no_header_returns_error() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "1700000000,42.5\n1700000010,55.3\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let mut config = csv_replay_config("all_numeric", None);
        set_csv_file(&mut config, path);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no header row"),
            "error must mention no header row, got: {msg}"
        );

        drop(tmp);
    }

    /// A non-csv_replay generator passes through unchanged.
    #[test]
    fn non_csv_replay_passes_through() {
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 42.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "mem_percent".to_string(),
                labels: None,
            },
        ];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
        let expected_file = match &config.generator {
            GeneratorConfig::CsvReplay { file, .. } => file.clone(),
            _ => unreachable!(),
        };
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 2, "should produce two expanded configs");

        // First expanded config
        assert_eq!(result[0].name, "cpu_percent");
        match &result[0].generator {
            GeneratorConfig::CsvReplay {
                column,
                columns,
                file,
                repeat,
                ..
            } => {
                assert_eq!(*column, Some(1));
                assert!(columns.is_none(), "columns must be None after expansion");
                assert_eq!(file, &expected_file, "file must be inherited");
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
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 3,
                name: "disk_io".to_string(),
                labels: None,
            },
        ];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
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
            labels: None,
        }];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 1);
        let child = &result[0];

        // Schedule fields — rate is overridden by CSV-derived rate.
        assert!(
            (child.rate - 0.1).abs() < 1e-9,
            "rate must be derived from CSV Δt=10s (got {})",
            child.rate
        );
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

    /// Expanded configs inherit non-None schedule fields (gaps, bursts) from the parent.
    #[test]
    fn expanded_configs_inherit_non_none_gaps_and_bursts() {
        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "metric_a".to_string(),
            labels: None,
        }];
        let (mut config, _tmp) = config_with_csv("parent", Some(cols));
        config.base.gaps = Some(GapConfig {
            every: "2m".to_string(),
            r#for: "20s".to_string(),
        });
        config.base.bursts = Some(BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 3.0,
        });
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1);
        let child = &result[0];

        let gaps = child.gaps.as_ref().expect("gaps must be inherited");
        assert_eq!(gaps.every, "2m");
        assert_eq!(gaps.r#for, "20s");

        let bursts = child.bursts.as_ref().expect("bursts must be inherited");
        assert_eq!(bursts.every, "10s");
        assert_eq!(bursts.r#for, "2s");
        assert_eq!(bursts.multiplier, 3.0);
    }

    // -----------------------------------------------------------------------
    // expand_scenario: error — empty columns list
    // -----------------------------------------------------------------------

    /// An empty columns list returns an error.
    #[test]
    fn empty_columns_list_returns_error() {
        let config = csv_replay_config("empty", Some(vec![]));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("must not be empty"),
            "error must mention empty list, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // expand_scenario: error — duplicate column indices
    // -----------------------------------------------------------------------

    /// Two columns with the same index returns an error.
    #[test]
    fn duplicate_column_index_returns_error() {
        let cols = vec![
            CsvColumnSpec {
                index: 2,
                name: "cpu".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
                labels: None,
            },
        ];
        let config = csv_replay_config("dupe_idx", Some(cols));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate column index 2"),
            "error must mention duplicate index, got: {msg}"
        );
    }

    /// Three columns where only the last two share an index.
    #[test]
    fn duplicate_column_index_not_first_returns_error() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 3,
                name: "mem".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 3,
                name: "disk".to_string(),
                labels: None,
            },
        ];
        let config = csv_replay_config("dupe_idx_late", Some(cols));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate column index 3"),
            "error must mention duplicate index, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // expand_scenario: error — duplicate column names
    // -----------------------------------------------------------------------

    /// Two columns with the same name returns an error.
    #[test]
    fn duplicate_column_name_returns_error() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "cpu".to_string(),
                labels: None,
            },
        ];
        let config = csv_replay_config("dupe_name", Some(cols));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate column name 'cpu'"),
            "error must mention duplicate name, got: {msg}"
        );
    }

    /// Three columns where only the last two share a name.
    #[test]
    fn duplicate_column_name_not_first_returns_error() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
                labels: None,
            },
            CsvColumnSpec {
                index: 3,
                name: "mem".to_string(),
                labels: None,
            },
        ];
        let config = csv_replay_config("dupe_name_late", Some(cols));
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate column name 'mem'"),
            "error must mention duplicate name, got: {msg}"
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
                labels: None,
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
                labels: None,
            },
        ];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
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
                gap_windows: None,
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
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
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

    // -----------------------------------------------------------------------
    // expand_scenario: per-column labels
    // -----------------------------------------------------------------------

    /// Per-column labels are merged into the child scenario's base labels.
    #[test]
    fn per_column_labels_merge_into_child() {
        let cols = vec![
            CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
                labels: Some(
                    [("instance".to_string(), "host1".to_string())]
                        .into_iter()
                        .collect(),
                ),
            },
            CsvColumnSpec {
                index: 2,
                name: "mem".to_string(),
                labels: Some(
                    [("instance".to_string(), "host2".to_string())]
                        .into_iter()
                        .collect(),
                ),
            },
        ];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 2);

        // First child should have instance=host1, plus inherited host=srv1.
        let labels0 = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels0.get("instance").map(|s| s.as_str()), Some("host1"));
        assert_eq!(labels0.get("host").map(|s| s.as_str()), Some("srv1"));

        // Second child should have instance=host2, plus inherited host=srv1.
        let labels1 = result[1].labels.as_ref().expect("labels must exist");
        assert_eq!(labels1.get("instance").map(|s| s.as_str()), Some("host2"));
        assert_eq!(labels1.get("host").map(|s| s.as_str()), Some("srv1"));
    }

    /// Per-column labels override scenario-level labels on key conflict.
    #[test]
    fn per_column_labels_override_scenario_level_on_conflict() {
        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "cpu".to_string(),
            labels: Some(
                [("host".to_string(), "override-host".to_string())]
                    .into_iter()
                    .collect(),
            ),
        }];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 1);
        let labels = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("host").map(|s| s.as_str()),
            Some("override-host"),
            "column labels must override scenario-level labels"
        );
    }

    /// Columns without labels do not disturb scenario-level labels.
    #[test]
    fn columns_without_labels_preserve_scenario_labels() {
        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "cpu".to_string(),
            labels: None,
        }];
        let (config, _tmp) = config_with_csv("parent", Some(cols));
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 1);
        let labels = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("host").map(|s| s.as_str()),
            Some("srv1"),
            "scenario-level labels must be preserved"
        );
    }

    // -----------------------------------------------------------------------
    // expand_scenario: auto-discovery (columns: None)
    // -----------------------------------------------------------------------

    /// Auto-discovery reads header from temp file and expands.
    #[test]
    fn auto_discovery_expands_from_csv_header() {
        use std::io::Write;

        // Use a simpler header format — format 4 plain names.
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            tmp,
            "Time,cpu_usage,mem_usage\n1700000000,42.5,60.0\n1700000010,43.0,61.0\n"
        )
        .expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "auto_test".to_string(),
                rate: 1.0,
                duration: Some("60s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: Some(
                    [("env".to_string(), "test".to_string())]
                        .into_iter()
                        .collect(),
                ),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: path,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 2, "should expand to 2 columns (skip Time)");
        assert_eq!(result[0].name, "cpu_usage");
        assert_eq!(result[1].name, "mem_usage");

        // Both should inherit env=test
        for child in &result {
            let labels = child.labels.as_ref().expect("labels must be inherited");
            assert_eq!(labels.get("env").map(|s| s.as_str()), Some("test"));
        }

        // Verify expanded generators have correct column indices.
        match &result[0].generator {
            GeneratorConfig::CsvReplay {
                column, columns, ..
            } => {
                assert_eq!(*column, Some(1));
                assert!(columns.is_none());
            }
            other => panic!("expected CsvReplay, got {other:?}"),
        }
        match &result[1].generator {
            GeneratorConfig::CsvReplay { column, .. } => {
                assert_eq!(*column, Some(2));
            }
            other => panic!("expected CsvReplay, got {other:?}"),
        }

        // Keep temp file alive until assertions complete.
        drop(tmp);
    }

    /// Auto-discovery with Grafana-style headers extracts labels.
    #[test]
    fn auto_discovery_grafana_style_extracts_labels() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        // Use RFC 4180 quoting: "" inside quoted fields becomes "
        let header = r#""Time","{__name__=""up"", instance=""host1"", job=""prom""}","{__name__=""up"", instance=""host2"", job=""node""}""#;
        write!(tmp, "{header}\n1704067200000,1,1\n1704067210000,1,1\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "grafana_auto".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: Some(
                    [("env".to_string(), "production".to_string())]
                        .into_iter()
                        .collect(),
                ),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: path,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let result = expand_scenario(config).expect("must succeed");

        assert_eq!(result.len(), 2);

        // Both should have metric name "up".
        assert_eq!(result[0].name, "up");
        assert_eq!(result[1].name, "up");

        // First column: instance=host1, job=prom, env=production
        let labels0 = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels0.get("instance").map(|s| s.as_str()), Some("host1"));
        assert_eq!(labels0.get("job").map(|s| s.as_str()), Some("prom"));
        assert_eq!(labels0.get("env").map(|s| s.as_str()), Some("production"));

        // Second column: instance=host2, job=node, env=production
        let labels1 = result[1].labels.as_ref().expect("labels must exist");
        assert_eq!(labels1.get("instance").map(|s| s.as_str()), Some("host2"));
        assert_eq!(labels1.get("job").map(|s| s.as_str()), Some("node"));
        assert_eq!(labels1.get("env").map(|s| s.as_str()), Some("production"));

        drop(tmp);
    }

    // -----------------------------------------------------------------------
    // Auto-discovery: edge cases
    // -----------------------------------------------------------------------

    /// Auto-discovery on a file with no data columns (only timestamp) errors.
    #[test]
    fn auto_discovery_single_column_file_returns_error() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "Time\n1700000000\n1700000010\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "no_data_cols".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: path,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no data columns"),
            "error must mention no data columns, got: {msg}"
        );

        drop(tmp);
    }

    /// A CSV with a single data column (header + values, no time column)
    /// auto-discovers one column, but column 0 is skipped as time, yielding
    /// no data columns and producing an error.
    #[test]
    fn auto_discovery_single_data_column_no_time_yields_no_data_columns() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "metric_name\n42.5\n55.0\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "single_data_col".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: path,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no data columns"),
            "error must mention no data columns, got: {msg}"
        );

        drop(tmp);
    }

    /// Auto-discovery on a missing file returns a generator error.
    #[test]
    fn auto_discovery_missing_file_returns_generator_error() {
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "missing_file".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: "/nonexistent/path.csv".to_string(),
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let err = expand_scenario(config).expect_err("must fail");
        assert!(
            matches!(err, SondaError::Generator(_)),
            "missing file should be a Generator error, got: {err:?}"
        );
    }

    /// Auto-discovery on a file with all-numeric first row returns an error.
    #[test]
    fn auto_discovery_all_numeric_returns_error() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "1000,42.5,60.0\n2000,55.3,70.1\n").expect("write csv");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "no_header".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file: path,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale: None,
                default_metric_name: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        };
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no header row"),
            "error must mention no header row, got: {msg}"
        );

        drop(tmp);
    }

    // -----------------------------------------------------------------------
    // Deserialization: per-column labels
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_per_column_labels_from_yaml() {
        let yaml = r#"
name: labeled_cols
rate: 1
generator:
  type: csv_replay
  file: data.csv
  columns:
    - index: 1
      name: cpu_percent
      labels:
        instance: host1
        job: node
    - index: 2
      name: mem_percent
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match &config.generator {
            GeneratorConfig::CsvReplay { columns, .. } => {
                let cols = columns.as_ref().expect("columns should be Some");
                assert_eq!(cols.len(), 2);

                // First column has labels.
                let labels0 = cols[0].labels.as_ref().expect("col 0 labels must be Some");
                assert_eq!(labels0.get("instance").map(|s| s.as_str()), Some("host1"));
                assert_eq!(labels0.get("job").map(|s| s.as_str()), Some("node"));

                // Second column has no labels.
                assert!(cols[1].labels.is_none());
            }
            other => panic!("expected CsvReplay variant, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // HistogramScenarioConfig deserialization
    // -----------------------------------------------------------------------

    /// Histogram config deserializes from YAML with all fields.
    #[test]
    #[cfg(feature = "config")]
    fn histogram_config_deserializes_from_yaml() {
        let yaml = r#"
name: http_request_duration_seconds
rate: 1
duration: 5m
buckets: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
distribution:
  type: exponential
  rate: 10.0
observations_per_tick: 100
mean_shift_per_sec: 0.001
seed: 42
labels:
  method: GET
"#;
        let config: HistogramScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "http_request_duration_seconds");
        assert_eq!(config.rate, 1.0);
        assert_eq!(config.buckets.as_ref().unwrap().len(), 11);
        assert_eq!(config.observations_per_tick, Some(100));
        assert_eq!(config.mean_shift_per_sec, Some(0.001));
        assert_eq!(config.seed, Some(42));
    }

    /// Histogram config with omitted optional fields uses defaults.
    #[test]
    #[cfg(feature = "config")]
    fn histogram_config_defaults_when_omitted() {
        let yaml = r#"
name: latency
rate: 1
distribution:
  type: exponential
  rate: 5.0
"#;
        let config: HistogramScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.buckets.is_none());
        assert!(config.observations_per_tick.is_none());
        assert!(config.mean_shift_per_sec.is_none());
        assert!(config.seed.is_none());
    }

    /// Histogram config with normal distribution.
    #[test]
    #[cfg(feature = "config")]
    fn histogram_config_normal_distribution() {
        let yaml = r#"
name: latency
rate: 1
distribution:
  type: normal
  mean: 0.1
  stddev: 0.02
"#;
        let config: HistogramScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.distribution {
            DistributionConfig::Normal { mean, stddev } => {
                assert_eq!(mean, 0.1);
                assert_eq!(stddev, 0.02);
            }
            _ => panic!("expected Normal distribution"),
        }
    }

    /// Histogram config with uniform distribution.
    #[test]
    #[cfg(feature = "config")]
    fn histogram_config_uniform_distribution() {
        let yaml = r#"
name: latency
rate: 1
distribution:
  type: uniform
  min: 0.0
  max: 1.0
"#;
        let config: HistogramScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.distribution {
            DistributionConfig::Uniform { min, max } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 1.0);
            }
            _ => panic!("expected Uniform distribution"),
        }
    }

    // -----------------------------------------------------------------------
    // SummaryScenarioConfig deserialization
    // -----------------------------------------------------------------------

    /// Summary config deserializes from YAML with all fields.
    #[test]
    #[cfg(feature = "config")]
    fn summary_config_deserializes_from_yaml() {
        let yaml = r#"
name: rpc_duration_seconds
rate: 1
duration: 5m
quantiles: [0.5, 0.9, 0.95, 0.99]
distribution:
  type: normal
  mean: 0.1
  stddev: 0.02
observations_per_tick: 100
seed: 42
"#;
        let config: SummaryScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.name, "rpc_duration_seconds");
        assert_eq!(config.rate, 1.0);
        assert_eq!(config.quantiles.as_ref().unwrap().len(), 4);
        assert_eq!(config.observations_per_tick, Some(100));
        assert_eq!(config.seed, Some(42));
    }

    /// Summary config with omitted optional fields uses defaults.
    #[test]
    #[cfg(feature = "config")]
    fn summary_config_defaults_when_omitted() {
        let yaml = r#"
name: rpc_latency
rate: 1
distribution:
  type: exponential
  rate: 5.0
"#;
        let config: SummaryScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.quantiles.is_none());
        assert!(config.observations_per_tick.is_none());
        assert!(config.seed.is_none());
    }

    // -----------------------------------------------------------------------
    // ScenarioEntry: Histogram and Summary variants
    // -----------------------------------------------------------------------

    /// ScenarioEntry::base() works for histogram entries.
    #[test]
    #[cfg(feature = "config")]
    fn scenario_entry_base_works_for_histogram() {
        let yaml = r#"
signal_type: histogram
name: test_hist
rate: 5
distribution:
  type: exponential
  rate: 10.0
"#;
        let entry: ScenarioEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(entry.base().name, "test_hist");
        assert_eq!(entry.base().rate, 5.0);
    }

    /// ScenarioEntry::base() works for summary entries.
    #[test]
    #[cfg(feature = "config")]
    fn scenario_entry_base_works_for_summary() {
        let yaml = r#"
signal_type: summary
name: test_sum
rate: 5
distribution:
  type: normal
  mean: 0.1
  stddev: 0.02
"#;
        let entry: ScenarioEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(entry.base().name, "test_sum");
        assert_eq!(entry.base().rate, 5.0);
    }

    /// expand_entry passes through Histogram and Summary unchanged.
    #[test]
    fn expand_entry_passes_through_histogram() {
        let entry = ScenarioEntry::Histogram(HistogramScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "test_hist".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: crate::sink::SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            buckets: None,
            distribution: DistributionConfig::Exponential { rate: 10.0 },
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        let result = expand_entry(entry).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ScenarioEntry::Histogram(_)));
    }

    /// expand_entry passes through Summary unchanged.
    #[test]
    fn expand_entry_passes_through_summary() {
        let entry = ScenarioEntry::Summary(SummaryScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "test_sum".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: crate::sink::SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            quantiles: None,
            distribution: DistributionConfig::Normal {
                mean: 0.1,
                stddev: 0.02,
            },
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        });
        let result = expand_entry(entry).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ScenarioEntry::Summary(_)));
    }

    // -----------------------------------------------------------------------
    // rate-derivation tests
    // -----------------------------------------------------------------------

    fn write_two_col_csv(rows: &[(u64, f64)]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "Time,cpu_usage").expect("write header");
        for (ts, v) in rows {
            writeln!(tmp, "{ts},{v}").expect("write row");
        }
        tmp.flush().expect("flush");
        tmp
    }

    fn build_csv_replay_scenario(
        file: String,
        rate: f64,
        timescale: Option<f64>,
        default_metric_name: Option<String>,
    ) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "ts_test".to_string(),
                rate,
                duration: Some("60s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                dynamic_labels: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::CsvReplay {
                file,
                column: None,
                repeat: Some(true),
                columns: None,
                timescale,
                default_metric_name,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        }
    }

    #[test]
    fn rate_derived_from_csv_ten_second_steps_yields_point_one() {
        let tmp = write_two_col_csv(&[
            (1_700_000_000, 1.0),
            (1_700_000_010, 2.0),
            (1_700_000_020, 3.0),
        ]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, None, None);
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert!(
            (result[0].rate - 0.1).abs() < 1e-9,
            "expected rate 0.1, got {}",
            result[0].rate
        );
    }

    #[test]
    fn rate_derived_with_timescale_two_yields_point_two() {
        let tmp = write_two_col_csv(&[
            (1_700_000_000, 1.0),
            (1_700_000_010, 2.0),
            (1_700_000_020, 3.0),
        ]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, Some(2.0), None);
        let result = expand_scenario(config).expect("must succeed");
        assert!(
            (result[0].rate - 0.2).abs() < 1e-9,
            "expected rate 0.2 (2x speed), got {}",
            result[0].rate
        );
    }

    #[test]
    fn rate_derived_with_timescale_half_yields_point_zero_five() {
        let tmp = write_two_col_csv(&[
            (1_700_000_000, 1.0),
            (1_700_000_010, 2.0),
            (1_700_000_020, 3.0),
        ]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, Some(0.5), None);
        let result = expand_scenario(config).expect("must succeed");
        assert!(
            (result[0].rate - 0.05).abs() < 1e-9,
            "expected rate 0.05 (half speed), got {}",
            result[0].rate
        );
    }

    #[test]
    fn epoch_milliseconds_heuristic_treats_values_above_threshold_as_ms() {
        let tmp = write_two_col_csv(&[
            (1_700_000_000_000, 1.0),
            (1_700_000_010_000, 2.0),
            (1_700_000_020_000, 3.0),
        ]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, None, None);
        let result = expand_scenario(config).expect("must succeed");
        assert!(
            (result[0].rate - 0.1).abs() < 1e-9,
            "ms epoch should yield 0.1 (10s Δt), got {}",
            result[0].rate
        );
    }

    #[test]
    fn non_monotonic_timestamps_return_clear_error() {
        let tmp = write_two_col_csv(&[(1_700_000_010, 1.0), (1_700_000_000, 2.0)]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, None, None);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("non-monotonic"),
            "error should mention non-monotonic, got: {msg}"
        );
    }

    #[test]
    fn single_data_row_returns_clear_error() {
        let tmp = write_two_col_csv(&[(1_700_000_000, 1.0)]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, None, None);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("fewer than 2 data rows"),
            "error should mention min rows, got: {msg}"
        );
    }

    #[test]
    fn timescale_zero_returns_validation_error() {
        let tmp = write_two_col_csv(&[(1_700_000_000, 1.0), (1_700_000_010, 2.0)]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, Some(0.0), None);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("timescale"),
            "error should mention timescale, got: {msg}"
        );
    }

    #[test]
    fn timescale_negative_returns_validation_error() {
        let tmp = write_two_col_csv(&[(1_700_000_000, 1.0), (1_700_000_010, 2.0)]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 1.0, Some(-1.0), None);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("timescale"),
            "error should mention timescale, got: {msg}"
        );
    }

    #[tracing_test::traced_test]
    #[test]
    fn user_rate_override_emits_warning() {
        let tmp = write_two_col_csv(&[(1_700_000_000, 1.0), (1_700_000_010, 2.0)]);
        let path = tmp.path().to_string_lossy().into_owned();
        let config = build_csv_replay_scenario(path, 5.0, None, None);
        let result = expand_scenario(config).expect("must succeed");
        assert!(
            (result[0].rate - 0.1).abs() < 1e-9,
            "rate should be derived (0.1), got {}",
            result[0].rate
        );
        assert!(
            logs_contain("overriding rate"),
            "tracing warn should contain 'overriding rate'"
        );
    }

    // -----------------------------------------------------------------------
    // blank-cell / gap_windows cross-check
    //
    // Driven through `expand_scenario` rather than by calling
    // `cross_check_gap_windows` directly, because the defect this guards
    // against is the two halves disagreeing: the window offsets the scheduler
    // resolves, and the row-to-instant mapping the check computes from the
    // derived rate. Calling the checker with hand-computed windows would test
    // the half that was never in doubt.
    // -----------------------------------------------------------------------

    /// A CSV whose rows are 10s apart, with `None` writing a blank cell.
    fn write_csv_with_blanks(values: &[Option<f64>]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "Time,cpu_usage").expect("write header");
        for (i, v) in values.iter().enumerate() {
            let ts = 1_700_000_000 + (i as u64) * 10;
            match v {
                Some(v) => writeln!(tmp, "{ts},{v}"),
                None => writeln!(tmp, "{ts},"),
            }
            .expect("write row");
        }
        tmp.flush().expect("flush");
        tmp
    }

    /// Build the scenario and attach the windows under test.
    ///
    /// Rows are 10s apart, so the derived rate is 0.1/s and data row *n*
    /// stands for the instant 10n seconds. `duration: 60s` covers ticks 0..=5.
    ///
    /// `repeat: false` throughout, because a capture containing silence cannot
    /// loop and is refused before the coverage question is reached — that rule
    /// has its own cases below.
    fn expand_with_windows(
        values: &[Option<f64>],
        windows: Option<Vec<GapWindowConfig>>,
    ) -> Result<Vec<ScenarioConfig>, SondaError> {
        expand_with_playback(values, windows, Some(false), Some("60s"))
    }

    /// The same, with `repeat` and `duration` under the caller's control.
    fn expand_with_playback(
        values: &[Option<f64>],
        windows: Option<Vec<GapWindowConfig>>,
        repeat: Option<bool>,
        duration: Option<&str>,
    ) -> Result<Vec<ScenarioConfig>, SondaError> {
        expand_full(values, windows, repeat, duration, None, None)
    }

    /// Every input the check consults, under the caller's control.
    fn expand_full(
        values: &[Option<f64>],
        windows: Option<Vec<GapWindowConfig>>,
        repeat: Option<bool>,
        duration: Option<&str>,
        bursts: Option<BurstConfig>,
        phase_offset: Option<&str>,
    ) -> Result<Vec<ScenarioConfig>, SondaError> {
        let tmp = write_csv_with_blanks(values);
        let path = tmp.path().to_string_lossy().into_owned();
        let mut config = build_csv_replay_scenario(path, 1.0, None, None);
        config.base.gap_windows = windows;
        config.base.duration = duration.map(str::to_string);
        config.base.bursts = bursts;
        config.base.phase_offset = phase_offset.map(str::to_string);
        if let GeneratorConfig::CsvReplay { repeat: r, .. } = &mut config.generator {
            *r = repeat;
        }
        expand_scenario(config)
    }

    fn window(at: &str, r#for: &str) -> GapWindowConfig {
        GapWindowConfig {
            at: at.to_string(),
            r#for: r#for.to_string(),
        }
    }

    /// Blanks covered by a declared window are the case the feature exists for.
    #[test]
    fn blanks_inside_a_declared_window_are_accepted() {
        // Rows 2 and 3 -> instants 20s and 30s; the window covers [20s, 40s).
        let result = expand_with_windows(
            &[Some(1.0), Some(2.0), None, None, Some(5.0)],
            Some(vec![window("20s", "20s")]),
        )
        .expect("blanks covered by a window must be accepted");
        assert_eq!(result.len(), 1);
    }

    /// A blank with no window declared at all is the shape a hand-edited
    /// capture takes, and the case a check guarded on `gap_windows.is_some()`
    /// would have skipped.
    #[test]
    fn a_blank_with_no_windows_declared_is_refused() {
        let err = expand_with_windows(&[Some(1.0), None, Some(3.0)], None)
            .expect_err("a blank with no declared window must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("not covered by any `gap_windows:` entry"),
            "error should name the uncovered blank, got: {msg}"
        );
        assert!(
            msg.contains("data row(s) 2"),
            "error should name data row 2, got: {msg}"
        );
    }

    /// A blank outside every declared window is refused even though windows
    /// exist — presence of a window is not coverage of this blank.
    #[test]
    fn a_blank_outside_the_declared_window_is_refused() {
        // Blank at row 1 -> 10s; the window covers [30s, 40s).
        let err = expand_with_windows(
            &[Some(1.0), None, Some(3.0), Some(4.0), Some(5.0)],
            Some(vec![window("30s", "10s")]),
        )
        .expect_err("a blank outside the window must be refused");
        assert!(
            err.to_string().contains("data row(s) 2"),
            "error should name data row 2, got: {err}"
        );
    }

    /// The other direction: a window over rows that *have* values would invent
    /// silence the capture does not contain.
    #[test]
    fn a_window_over_recorded_samples_is_refused() {
        let err = expand_with_windows(
            &[Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
            Some(vec![window("10s", "20s")]),
        )
        .expect_err("a window over present samples must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("fall inside a `gap_windows:` entry"),
            "error should describe silencing recorded data, got: {msg}"
        );
        assert!(
            msg.contains("data row(s) 2, 3"),
            "error should name rows 2 and 3, got: {msg}"
        );
    }

    /// A capture that begins during the outage: row 0 is blank, and `at: 0s`
    /// is the only window that can cover it.
    #[test]
    fn a_window_at_zero_covers_a_blank_first_row() {
        let result = expand_with_windows(
            &[None, None, Some(3.0), Some(4.0)],
            Some(vec![window("0s", "20s")]),
        )
        .expect("`at: 0s` must cover the first rows");
        assert_eq!(result.len(), 1);
    }

    /// Windows are half-open, so the row at the closing instant is outside.
    ///
    /// This is the boundary both halves have to agree on: the scheduler stops
    /// suppressing at `at + for`, so a blank there would replay as a NaN.
    #[test]
    fn the_row_at_the_window_end_is_not_covered() {
        // Blank at row 2 -> 20s; the window is [0s, 20s), which ends there.
        let err = expand_with_windows(
            &[None, None, None, Some(4.0)],
            Some(vec![window("0s", "20s")]),
        )
        .expect_err("the row at the window's closing instant must not be covered");
        assert!(
            err.to_string().contains("data row(s) 3"),
            "error should name data row 3, got: {err}"
        );
    }

    /// A file whose blanks all sit in separate declared windows is accepted —
    /// coverage is per-row, not "some window exists somewhere".
    #[test]
    fn several_windows_each_cover_their_own_blanks() {
        let result = expand_with_windows(
            &[Some(1.0), None, Some(3.0), None, Some(5.0)],
            Some(vec![window("10s", "10s"), window("30s", "10s")]),
        )
        .expect("each blank covered by its own window must be accepted");
        assert_eq!(result.len(), 1);
    }

    /// A wholly mismatched file summarises rather than printing every row.
    #[test]
    fn a_long_run_of_uncovered_blanks_is_summarised() {
        let values: Vec<Option<f64>> = std::iter::once(Some(1.0))
            .chain(std::iter::repeat_n(None, 12))
            .collect();
        let err = expand_with_windows(&values, None)
            .expect_err("uncovered blanks must be refused whatever the count");
        let msg = err.to_string();
        assert!(
            msg.contains("and 4 more"),
            "12 offenders should name 8 and summarise 4, got: {msg}"
        );
    }

    // ---- the playback, not just the file ----------------------------------
    //
    // The row list says nothing about the instants a run actually reaches:
    // `repeat` loops it and `repeat: false` clamps past its end. Both leaked a
    // NaN past a green cross-check, and both are reachable from a default.

    /// A capture containing silence cannot loop, and `repeat` defaults to true.
    ///
    /// This is the reachable-by-default case: nothing in the YAML says
    /// `repeat`, the file has a blank, the window covers it on the first pass,
    /// and on the second cycle the same row replays where no window is.
    #[test]
    fn blanks_are_refused_when_repeat_is_left_to_its_default() {
        let err = expand_with_playback(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            None,
            Some("60s"),
        )
        .expect_err("a looping capture with silence must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot loop") && msg.contains("repeat"),
            "error should name the looping rule, got: {msg}"
        );
    }

    /// The same file with `repeat: true` written out, so the rule is not an
    /// artefact of the default resolving.
    #[test]
    fn blanks_are_refused_when_repeat_is_explicitly_true() {
        let err = expand_with_playback(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            Some(true),
            Some("60s"),
        )
        .expect_err("a looping capture with silence must be refused");
        assert!(err.to_string().contains("cannot loop"), "got: {err}");
    }

    /// The other direction: the identical file loops fine once the blank is
    /// gone, so the rule is about silence and not about `repeat` itself.
    #[test]
    fn a_capture_without_silence_may_still_loop() {
        let result = expand_with_playback(
            &[Some(1.0), Some(2.0), Some(3.0)],
            None,
            Some(true),
            Some("60s"),
        )
        .expect("a capture with no blanks must still be allowed to loop");
        assert_eq!(result.len(), 1);
    }

    /// And with `repeat: false` the same blank-carrying file is accepted.
    #[test]
    fn blanks_are_accepted_once_the_capture_stops_looping() {
        let result = expand_with_playback(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            Some(false),
            Some("60s"),
        )
        .expect("`repeat: false` must accept the same file");
        assert_eq!(result.len(), 1);
    }

    /// A capture that ends during the outage, replayed past its own length.
    ///
    /// `repeat: false` holds the final slot for every remaining tick, so a
    /// blank last row keeps emitting — as `NaN`, outside every window, with the
    /// per-row check green because the file has no row for those instants.
    #[test]
    fn a_blank_last_row_held_past_the_data_is_refused() {
        // Rows at 0s, 10s, 20s; the run goes to 60s, so ticks 3..=5 hold row 2.
        let err = expand_with_playback(
            &[Some(1.0), Some(2.0), None],
            Some(vec![window("20s", "10s")]),
            Some(false),
            Some("60s"),
        )
        .expect_err("held silence past the data must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("outlives its data"),
            "error should name the clamp, got: {msg}"
        );
        // Tick 3 is the first instant past the data that no window covers.
        // Named as a starting point rather than a list: the held tail can be
        // tens of millions of ticks, and enumerating them is what the interval
        // walk exists to avoid.
        assert!(
            msg.contains("the first such instant is tick 3"),
            "error should name the first uncovered tick, got: {msg}"
        );
    }

    /// The fix the error suggests works: a window reaching the end of the run.
    ///
    /// This is a real capture shape — the exporter went down and had not come
    /// back when the capture ended — so it must stay expressible.
    #[test]
    fn a_blank_last_row_is_accepted_when_the_window_reaches_the_end() {
        let result = expand_with_playback(
            &[Some(1.0), Some(2.0), None],
            Some(vec![window("20s", "45s")]),
            Some(false),
            Some("60s"),
        )
        .expect("a window covering the held tail must be accepted");
        assert_eq!(result.len(), 1);
    }

    /// The other suggested fix: stop the run at the data.
    #[test]
    fn a_blank_last_row_is_accepted_when_the_run_ends_with_the_data() {
        let result = expand_with_playback(
            &[Some(1.0), Some(2.0), None],
            Some(vec![window("20s", "10s")]),
            Some(false),
            Some("30s"),
        )
        .expect("a run that ends with its data must be accepted");
        assert_eq!(result.len(), 1);
    }

    /// With no `duration:` the held silence never ends, and no finite window
    /// reaches it — so the error says that rather than naming a window to add.
    #[test]
    fn a_blank_last_row_on_an_unbounded_run_is_refused() {
        let err = expand_with_playback(
            &[Some(1.0), Some(2.0), None],
            Some(vec![window("20s", "10s")]),
            Some(false),
            None,
        )
        .expect_err("unbounded held silence must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("no `duration:`") && msg.contains("forever"),
            "error should explain why no window helps, got: {msg}"
        );
    }

    /// A present last row is not subject to the clamp rule: what the run holds
    /// past the data is a value, not silence.
    #[test]
    fn a_present_last_row_held_past_the_data_is_fine() {
        let result = expand_with_playback(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            Some(false),
            Some("60s"),
        )
        .expect("holding a present value past the data is not silence");
        assert_eq!(result.len(), 1);
    }

    /// A burst compresses the tick grid, so no row lands on `n x step` any
    /// more and the windows would fall on the wrong rows.
    ///
    /// Measured before this rule existed: `every: 4s, for: 2s, multiplier: 4`
    /// on a 1/s eight-row capture played every row inside the first two
    /// seconds — the burst occupies the head of the cycle, so the compression
    /// is not confined to the rows "inside" it.
    #[test]
    fn blanks_are_refused_when_the_scenario_bursts() {
        let err = expand_full(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            Some(false),
            Some("60s"),
            Some(BurstConfig {
                every: "40s".to_string(),
                r#for: "20s".to_string(),
                multiplier: 4.0,
            }),
            None,
        )
        .expect_err("blanks under a compressed grid must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("bursts:") && msg.contains("compresses the tick grid"),
            "error should name the burst and why it matters, got: {msg}"
        );
    }

    /// The other direction: bursts stay legal on a capture with no silence.
    ///
    /// The grid still slides — nothing depends on where a particular row lands
    /// when no row has to line up with a declared window.
    #[test]
    fn a_capture_without_silence_may_still_burst() {
        let result = expand_full(
            &[Some(1.0), Some(2.0), Some(3.0)],
            None,
            Some(false),
            Some("60s"),
            Some(BurstConfig {
                every: "40s".to_string(),
                r#for: "20s".to_string(),
                multiplier: 4.0,
            }),
            None,
        )
        .expect("a blank-free capture must still be allowed to burst");
        assert_eq!(result.len(), 1);
    }

    /// `phase_offset:` is deliberately *not* refused alongside `bursts:`.
    ///
    /// It delays the whole scenario before the loop's clock starts, so the tick
    /// grid and the windows shift together and nothing moves relative to
    /// anything else. That was measured through the CLI rather than derived,
    /// because reading the call chain is how the clamp case was got wrong.
    ///
    /// The measurement has to answer two questions, and the first one is the
    /// one a single run cannot: **is `phase_offset` applied on this path at
    /// all?** If it were silently ignored, the suppressed row would be right
    /// for the wrong reason. Wall time across a sweep says it is —
    /// `none/1500ms/2s/5s/7s` on a four-row capture ran in
    /// `3.21/4.62/5.22/8.22/10.02` seconds, tracking the offset. The rows
    /// emitted stayed `[0, 2, 3]` throughout, with row 1 — the blank —
    /// suppressed every time.
    ///
    /// Those offsets are chosen to exclude the failure they would otherwise
    /// hide. Had the loop's clock started *before* the delay, an offset of 5s
    /// or 7s would have consumed the whole `[1s, 2s)` window during the wait
    /// and suppressed nothing (four rows), and 7s exceeds the 4s duration, so
    /// the run would have emitted nothing at all. Both counterfactuals are
    /// excluded by the same table. `1500ms` is there so the result cannot be an
    /// artefact of the offset being a whole number of steps.
    ///
    /// What this test pins is the narrower claim it can actually make: the
    /// check does not reject the combination. It would not notice a future
    /// change that started the loop's clock before the delay; that is what the
    /// measurement above covers, and it is worth re-running if the launch path
    /// moves.
    #[test]
    fn phase_offset_is_not_refused_alongside_blanks() {
        let result = expand_full(
            &[Some(1.0), None, Some(3.0)],
            Some(vec![window("10s", "10s")]),
            Some(false),
            Some("60s"),
            None,
            Some("20s"),
        )
        .expect("phase_offset shifts grid and windows together");
        assert_eq!(result.len(), 1);
    }

    // -----------------------------------------------------------------------
    // default-metric-name fallback tests
    // -----------------------------------------------------------------------

    #[test]
    fn default_metric_name_used_when_header_lacks_name_single_column() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "Time,{{instance=\"prod-01\"}}").expect("write header");
        writeln!(tmp, "1700000000,42.5").expect("write row");
        writeln!(tmp, "1700000010,43.0").expect("write row");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = build_csv_replay_scenario(path, 1.0, None, Some("node_cpu".to_string()));
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "node_cpu");
        let labels = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels.get("instance").map(String::as_str), Some("prod-01"));
    }

    #[test]
    fn default_metric_name_suffixes_index_when_multiple_columns_lack_name() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(
            tmp,
            "Time,{{instance=\"prod-01\"}},{{instance=\"prod-02\"}}"
        )
        .expect("write header");
        writeln!(tmp, "1700000000,42.5,55.0").expect("write row");
        writeln!(tmp, "1700000010,43.0,56.0").expect("write row");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = build_csv_replay_scenario(path, 1.0, None, Some("node_cpu".to_string()));
        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "node_cpu_1");
        assert_eq!(result[1].name, "node_cpu_2");

        let labels0 = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels0.get("instance").map(String::as_str), Some("prod-01"));
        let labels1 = result[1].labels.as_ref().expect("labels must exist");
        assert_eq!(labels1.get("instance").map(String::as_str), Some("prod-02"));
    }

    #[test]
    fn missing_default_metric_name_still_errors_when_header_lacks_name() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "Time,{{instance=\"prod-01\"}}").expect("write header");
        writeln!(tmp, "1700000000,42.5").expect("write row");
        writeln!(tmp, "1700000010,43.0").expect("write row");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let config = build_csv_replay_scenario(path, 1.0, None, None);
        let err = expand_scenario(config).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("default_metric_name"),
            "error should hint at default_metric_name, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // header-label merge tests
    // -----------------------------------------------------------------------

    #[test]
    fn explicit_columns_merge_header_labels_with_user_spec_labels() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, r#""Time","{{instance=""prod-01"", job=""node""}}""#).expect("write header");
        writeln!(tmp, "1700000000,42.5").expect("write row");
        writeln!(tmp, "1700000010,43.0").expect("write row");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "cpu".to_string(),
            labels: Some(
                [("site".to_string(), "us".to_string())]
                    .into_iter()
                    .collect(),
            ),
        }];
        let mut config = build_csv_replay_scenario(path, 1.0, None, None);
        if let GeneratorConfig::CsvReplay {
            ref mut columns, ..
        } = config.generator
        {
            *columns = Some(cols);
        }

        let result = expand_scenario(config).expect("must succeed");
        assert_eq!(result.len(), 1);
        let labels = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels.get("instance").map(String::as_str), Some("prod-01"));
        assert_eq!(labels.get("job").map(String::as_str), Some("node"));
        assert_eq!(labels.get("site").map(String::as_str), Some("us"));
    }

    #[test]
    fn explicit_user_spec_labels_override_header_labels_on_key_conflict() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "Time,{{instance=\"prod-01\"}}").expect("write header");
        writeln!(tmp, "1700000000,42.5").expect("write row");
        writeln!(tmp, "1700000010,43.0").expect("write row");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();

        let cols = vec![CsvColumnSpec {
            index: 1,
            name: "cpu".to_string(),
            labels: Some(
                [("instance".to_string(), "user-override".to_string())]
                    .into_iter()
                    .collect(),
            ),
        }];
        let mut config = build_csv_replay_scenario(path, 1.0, None, None);
        if let GeneratorConfig::CsvReplay {
            ref mut columns, ..
        } = config.generator
        {
            *columns = Some(cols);
        }

        let result = expand_scenario(config).expect("must succeed");
        let labels = result[0].labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("instance").map(String::as_str),
            Some("user-override")
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn timescale_field_deserializes_from_yaml() {
        let yaml = r#"
name: ts
rate: 1
generator:
  type: csv_replay
  file: data.csv
  timescale: 2.5
  default_metric_name: my_metric
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match &config.generator {
            GeneratorConfig::CsvReplay {
                timescale,
                default_metric_name,
                ..
            } => {
                assert_eq!(*timescale, Some(2.5));
                assert_eq!(default_metric_name.as_deref(), Some("my_metric"));
            }
            other => panic!("expected CsvReplay variant, got {other:?}"),
        }
    }

    fn write_temp_log_csv(content: &str) -> (tempfile::NamedTempFile, String) {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "{}", content).expect("write content");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();
        (tmp, path)
    }

    fn build_log_scenario(file: String, timescale: Option<f64>) -> LogScenarioConfig {
        LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "log_replay".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: OnSinkError::Warn,
            },
            generator: LogGeneratorConfig::CsvReplay {
                file,
                columns: None,
                repeat: Some(true),
                timescale,
                default_severity: None,
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        }
    }

    #[test]
    fn expand_log_scenario_derives_rate_from_ten_second_step_csv() {
        let csv = "timestamp,severity,message\n\
                   1700000000,info,a\n\
                   1700000010,info,b\n\
                   1700000020,info,c\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let config = build_log_scenario(path, None);
        let expanded = expand_log_scenario(config).expect("expand must succeed");
        assert_eq!(expanded.len(), 1);
        assert!(
            (expanded[0].base.rate - 0.1).abs() < 1e-9,
            "derived rate must be 1/10s = 0.1, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_with_timescale_two_doubles_rate() {
        let csv = "timestamp,severity,message\n\
                   1700000000,info,a\n\
                   1700000010,info,b\n\
                   1700000020,info,c\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let config = build_log_scenario(path, Some(2.0));
        let expanded = expand_log_scenario(config).expect("expand must succeed");
        assert!(
            (expanded[0].base.rate - 0.2).abs() < 1e-9,
            "timescale=2.0 must double rate to 0.2, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_overrides_user_rate_when_differs() {
        let csv = "timestamp,severity,message\n\
                   1700000000,info,a\n\
                   1700000010,info,b\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let mut config = build_log_scenario(path, None);
        config.base.rate = 999.0;
        let expanded = expand_log_scenario(config).expect("expand must succeed");
        assert!(
            (expanded[0].base.rate - 0.1).abs() < 1e-9,
            "user rate=999 must be overridden by derived rate=0.1, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_auto_discovers_timestamp_column() {
        let csv = "TIME,Severity,Message\n\
                   1700000000,info,a\n\
                   1700000005,info,b\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let config = build_log_scenario(path, None);
        let expanded = expand_log_scenario(config).expect("auto-discovery must work");
        assert!(
            (expanded[0].base.rate - 0.2).abs() < 1e-9,
            "5s step → rate 0.2, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_respects_explicit_columns_mapping() {
        let csv = "ts,sev,text\n\
                   1700000000,info,a\n\
                   1700000003,info,b\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let mut config = build_log_scenario(path, None);
        if let LogGeneratorConfig::CsvReplay {
            ref mut columns, ..
        } = config.generator
        {
            *columns = Some(crate::generator::log_csv_replay::LogCsvColumns {
                timestamp: Some("ts".to_string()),
                severity: Some("sev".to_string()),
                message: Some("text".to_string()),
            });
        }
        let expanded = expand_log_scenario(config).expect("explicit columns must work");
        assert!(
            (expanded[0].base.rate - (1.0 / 3.0)).abs() < 1e-9,
            "3s step → rate 1/3, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_resolves_timestamp_in_non_zero_column() {
        let csv = "severity,timestamp,message\n\
                   info,1700000000,a\n\
                   warn,1700000004,b\n\
                   info,1700000008,c\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let config = build_log_scenario(path, None);
        let expanded = expand_log_scenario(config)
            .expect("timestamp in column 1 must be used for rate derivation");
        assert!(
            (expanded[0].base.rate - 0.25).abs() < 1e-9,
            "4s step in column 1 → rate 0.25, got {}",
            expanded[0].base.rate
        );
    }

    #[test]
    fn expand_log_scenario_rejects_non_monotonic_timestamps() {
        let csv = "timestamp,severity,message\n\
                   1700000000,info,a\n\
                   1700000010,info,b\n\
                   1700000005,info,c\n";
        let (_tmp, path) = write_temp_log_csv(csv);
        let config = build_log_scenario(path, None);
        let err = expand_log_scenario(config).expect_err("non-monotonic timestamps must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("non-monotonic"),
            "error message must mention non-monotonic, got: {msg}"
        );
    }

    // ======================================================================
    // Timestamp column monotonicity (full file, not just the sampled head)
    // ======================================================================

    /// Write a CSV whose timestamps are a clean 10s grid, then overwrite one
    /// row's stamp with `bad_ts` — returning the path and the row's index.
    fn csv_with_bad_row(rows: usize, bad_row: usize, bad_ts: f64) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "timestamp,cpu").expect("write header");
        for i in 0..rows {
            let ts = if i == bad_row {
                bad_ts
            } else {
                1_700_000_000.0 + (i as f64) * 10.0
            };
            writeln!(tmp, "{ts},42.5").expect("write row");
        }
        tmp.flush().expect("flush");
        tmp
    }

    #[test]
    fn monotonic_timestamps_are_accepted() {
        let tmp = csv_with_bad_row(150, usize::MAX, 0.0);
        validate_csv_timestamps_monotonic(&tmp.path().to_string_lossy(), 0)
            .expect("a strictly increasing column must be accepted");
    }

    #[test]
    fn repeated_timestamp_is_refused_naming_the_row() {
        // Row 4 repeats row 3's stamp.
        let tmp = csv_with_bad_row(20, 4, 1_700_000_030.0);
        let err = validate_csv_timestamps_monotonic(&tmp.path().to_string_lossy(), 0)
            .expect_err("a repeated timestamp must be refused");
        let msg = err.to_string();
        assert!(msg.contains("non-monotonic"), "got: {msg}");
        assert!(msg.contains("data row 4"), "must name the row, got: {msg}");
    }

    #[test]
    fn out_of_order_timestamp_is_refused_naming_the_row() {
        // Row 7 jumps backwards behind row 6.
        let tmp = csv_with_bad_row(20, 7, 1_700_000_000.0);
        let err = validate_csv_timestamps_monotonic(&tmp.path().to_string_lossy(), 0)
            .expect_err("an out-of-order timestamp must be refused");
        let msg = err.to_string();
        assert!(msg.contains("non-monotonic"), "got: {msg}");
        assert!(msg.contains("data row 7"), "must name the row, got: {msg}");
    }

    /// The gap this check exists to close.
    ///
    /// `compute_csv_delta_seconds` reads only `CSV_DELTA_SAMPLE_ROWS` rows, so
    /// its own monotonicity guard cannot see a defect past that window. Row 120
    /// is beyond it by construction: the assertion below pins that, so shrinking
    /// the sample constant cannot quietly turn this case into a duplicate of the
    /// head-of-file ones above.
    #[test]
    fn non_monotonic_row_beyond_the_sample_window_is_refused() {
        let bad_row = CSV_DELTA_SAMPLE_ROWS + 20;
        assert!(
            bad_row > CSV_DELTA_SAMPLE_ROWS,
            "the defect must sit outside the sampled window for this to test anything"
        );

        let tmp = csv_with_bad_row(bad_row + 30, bad_row, 1_700_000_000.0);
        let path = tmp.path().to_string_lossy().into_owned();

        // The sampled derivation is blind to it — that is the defect.
        compute_csv_delta_seconds(&path, 0)
            .expect("the sampled head is clean, so rate derivation still succeeds");

        let err = validate_csv_timestamps_monotonic(&path, 0)
            .expect_err("a defect past the sample window must still be refused");
        assert!(
            err.to_string().contains(&format!("data row {bad_row}")),
            "must name the row, got: {err}"
        );
    }

    #[test]
    fn importer_output_passes_the_monotonicity_check() {
        // The emitter writes a uniform grid; its own output must load.
        let tmp = write_temp_timing_csv("Time,cpu,mem,disk,net", 200);
        validate_csv_timestamps_monotonic(&tmp.path().to_string_lossy(), 0)
            .expect("importer-emitted CSV must pass");
    }
}
