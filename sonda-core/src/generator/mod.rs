//! Value generators produce f64 values for each tick.
//!
//! All generators implement the `ValueGenerator` trait and are constructed
//! via `create_generator()` which returns `Box<dyn ValueGenerator>`.
//!
//! Log generators implement the `LogGenerator` trait and produce `LogEvent`
//! values. They are constructed via `create_log_generator()`.
//!
//! Histogram and summary generators produce multi-valued samples per tick
//! (bucket counts + count + sum, or quantile values + count + sum). They
//! hold cumulative state and do not implement `ValueGenerator`. See
//! [`histogram::HistogramGenerator`] and [`summary::SummaryGenerator`].

pub mod constant;
pub mod csv_header;
pub mod csv_replay;
pub mod histogram;
pub mod jitter;
pub mod log_replay;
pub mod log_template;
pub mod sawtooth;
pub mod sequence;
pub mod sine;
pub mod spike;
pub mod step;
pub mod summary;
pub mod uniform;

pub use self::jitter::JitterWrapper;

use std::collections::{BTreeMap, HashMap};

use self::constant::Constant;
use self::csv_replay::CsvReplayGenerator;
use self::log_replay::LogReplayGenerator;
use self::log_template::{LogTemplateGenerator, TemplateEntry};
use self::sawtooth::Sawtooth;
use self::sequence::SequenceGenerator;
use self::sine::Sine;
use self::spike::SpikeGenerator;
use self::step::StepGenerator;
use self::uniform::UniformRandom;
use crate::model::log::{LogEvent, Severity};
use crate::{ConfigError, SondaError};

/// A generator produces a single f64 value for a given tick index.
///
/// Implementations must be deterministic for a given configuration and tick.
/// Side effects are not allowed in `value()`.
pub trait ValueGenerator: Send + Sync {
    /// Produce a value for the given tick index (0-based, monotonically increasing).
    fn value(&self, tick: u64) -> f64;
}

/// Specification for a single CSV column in a multi-column `csv_replay`
/// configuration.
///
/// When the `columns` field is set on a `CsvReplay` generator config, each
/// `CsvColumnSpec` specifies a column index and the metric name to use when
/// that column is expanded into its own independent scenario.
///
/// # Example YAML
///
/// ```yaml
/// columns:
///   - index: 1
///     name: cpu_percent
///   - index: 2
///     name: mem_percent
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub struct CsvColumnSpec {
    /// Zero-based column index in the CSV file.
    pub index: usize,
    /// Metric name for the expanded scenario.
    pub name: String,
    /// Optional per-column labels merged with scenario-level labels during
    /// expansion. Column labels override scenario-level labels on key conflict.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<HashMap<String, String>>,
}

/// Domain-specific value mapping for the [`GeneratorConfig::Flap`] alias.
///
/// The variant selects an `(up_value, down_value)` pair aligned with
/// gNMI / openconfig conventions:
///
/// | Variant | `up_value` | `down_value` | Convention |
/// |---|---|---|---|
/// | `Boolean` | 1.0 | 0.0 | Generic boolean |
/// | `LinkState` | 1.0 | 0.0 | Synonym of `Boolean` |
/// | `OperState` | 1.0 | 2.0 | gNMI oper-state (UP=1, DOWN=2) |
/// | `AdminState` | 1.0 | 2.0 | gNMI admin-state (UP=1, DOWN=2) |
/// | `NeighborState` | 6.0 | 1.0 | BGP neighbor-state (ESTABLISHED=6, IDLE=1) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FlapEnum {
    Boolean,
    LinkState,
    OperState,
    AdminState,
    NeighborState,
}

impl FlapEnum {
    /// Return the `(up_value, down_value)` pair this variant selects.
    pub fn defaults(self) -> (f64, f64) {
        match self {
            FlapEnum::Boolean | FlapEnum::LinkState => (1.0, 0.0),
            FlapEnum::OperState | FlapEnum::AdminState => (1.0, 2.0),
            FlapEnum::NeighborState => (6.0, 1.0),
        }
    }
}

/// Configuration for a value generator, used for YAML deserialization.
///
/// The `type` field selects which generator to instantiate. Additional fields
/// are specific to each variant.
///
/// # Core generators
///
/// ```yaml
/// generator:
///   type: sine
///   amplitude: 5.0
///   period_secs: 30
///   offset: 10.0
/// ```
///
/// # Operational aliases
///
/// Aliases desugar into core generators at config expansion time. They use
/// domain-relevant parameter names and have sensible defaults.
///
/// ```yaml
/// # Normal healthy oscillation (desugars to sine + jitter)
/// generator:
///   type: steady
///   center: 75.0
///   amplitude: 10.0
///   period: "60s"
///   noise: 2.0
///
/// # Resource leak (desugars to sawtooth)
/// generator:
///   type: leak
///   baseline: 40.0
///   ceiling: 95.0
///   time_to_ceiling: "120s"
///
/// # Interface flap (desugars to sequence)
/// generator:
///   type: flap
///   up_duration: "10s"
///   down_duration: "5s"
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(tag = "type"))]
#[non_exhaustive]
pub enum GeneratorConfig {
    /// A generator that always returns the same value.
    #[cfg_attr(feature = "config", serde(rename = "constant"))]
    Constant {
        /// The fixed value returned on every tick.
        value: f64,
    },
    /// A generator that returns deterministically random values in `[min, max]`.
    #[cfg_attr(feature = "config", serde(rename = "uniform"))]
    Uniform {
        /// Lower bound of the output range (inclusive).
        min: f64,
        /// Upper bound of the output range (inclusive).
        max: f64,
        /// Optional seed for deterministic replay. Defaults to 0 when absent.
        seed: Option<u64>,
    },
    /// A generator that follows a sine curve.
    #[cfg_attr(feature = "config", serde(rename = "sine"))]
    Sine {
        /// Half the peak-to-peak swing of the wave.
        amplitude: f64,
        /// Duration of one full cycle in seconds.
        period_secs: f64,
        /// Vertical offset applied to every sample (the wave's midpoint).
        offset: f64,
    },
    /// A generator that linearly ramps from `min` to `max` then resets.
    #[cfg_attr(feature = "config", serde(rename = "sawtooth"))]
    Sawtooth {
        /// Value at the start of each period.
        min: f64,
        /// Value approached at the end of each period (never reached).
        max: f64,
        /// Duration of one full ramp in seconds.
        period_secs: f64,
    },
    /// A generator that steps through an explicit sequence of values.
    #[cfg_attr(feature = "config", serde(rename = "sequence"))]
    Sequence {
        /// The ordered list of values to step through. Must not be empty.
        values: Vec<f64>,
        /// When true (default), the sequence cycles. When false, the last value
        /// is returned for all ticks beyond the sequence length.
        repeat: Option<bool>,
    },
    /// A generator that outputs a baseline value with periodic spikes.
    #[cfg_attr(feature = "config", serde(rename = "spike"))]
    Spike {
        /// The normal output value between spikes.
        baseline: f64,
        /// The amount added to baseline during a spike.
        magnitude: f64,
        /// How long each spike lasts in seconds.
        duration_secs: f64,
        /// Time between spike starts in seconds.
        interval_secs: f64,
    },
    /// A generator that replays numeric values from a CSV file.
    #[cfg_attr(feature = "config", serde(rename = "csv_replay"))]
    CsvReplay {
        /// Path to the CSV file containing numeric values.
        file: String,
        /// Internal: zero-based column index, set by `expand_scenario`.
        ///
        /// Not user-facing in YAML — set during config expansion. When
        /// `None`, defaults to `0` at generator creation time.
        #[cfg_attr(feature = "config", serde(skip))]
        column: Option<usize>,
        /// Explicit column specifications. When present, the config layer
        /// expands this single scenario into N independent single-column
        /// scenarios before launch.
        ///
        /// When absent, columns are auto-discovered from the CSV header row.
        /// An empty list is an error.
        #[cfg_attr(feature = "config", serde(default))]
        columns: Option<Vec<CsvColumnSpec>>,
        /// Whether to loop back to the first value after exhausting the CSV.
        /// Defaults to `true`.
        #[cfg_attr(feature = "config", serde(default))]
        repeat: Option<bool>,
        /// Replay speed multiplier (default `1.0`). `2.0` plays 2× faster,
        /// `0.5` plays 2× slower. Must be strictly positive.
        #[cfg_attr(feature = "config", serde(default))]
        timescale: Option<f64>,
        /// Fallback metric name for auto-discovered columns whose header has
        /// no `__name__`. Suffixed with `_<index>` when multiple such columns
        /// share the fallback.
        #[cfg_attr(feature = "config", serde(default))]
        default_metric_name: Option<String>,
    },
    /// A monotonic step counter: `start + tick * step_size`, with optional wrap-around.
    ///
    /// Useful for testing `rate()` and `increase()` PromQL functions.
    #[cfg_attr(feature = "config", serde(rename = "step"))]
    Step {
        /// Initial value at tick 0. Defaults to 0.0 when absent.
        #[cfg_attr(feature = "config", serde(default))]
        start: Option<f64>,
        /// Increment applied per tick.
        step_size: f64,
        /// Optional wrap-around threshold. When set and greater than `start`,
        /// the value wraps via modular arithmetic.
        max: Option<f64>,
    },

    // -----------------------------------------------------------------
    // Operational aliases — syntactic sugar that desugars into the above
    // generators at config expansion time. The runtime never sees these.
    // -----------------------------------------------------------------
    /// Binary up/down toggle modeling an interface flap.
    ///
    /// Desugars into a [`Sequence`](GeneratorConfig::Sequence) generator that
    /// alternates between `up_value` (default 1.0) and `down_value` (default 0.0).
    /// The number of consecutive up/down ticks is derived from `up_duration` and
    /// `down_duration` relative to the scenario `rate`.
    ///
    /// The optional `enum:` shorthand selects up/down values aligned with
    /// common gNMI / openconfig conventions (oper-state, admin-state,
    /// BGP neighbor-state). Mutually exclusive with explicit `up_value` /
    /// `down_value`.
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: flap
    ///   up_duration: "10s"
    ///   down_duration: "5s"
    ///   enum: oper_state    # up_value=1.0, down_value=2.0
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "flap"))]
    Flap {
        /// How long the signal stays in the "up" state per cycle.
        /// Defaults to `"10s"`.
        #[cfg_attr(feature = "config", serde(default))]
        up_duration: Option<String>,
        /// How long the signal stays in the "down" state per cycle.
        /// Defaults to `"5s"`.
        #[cfg_attr(feature = "config", serde(default))]
        down_duration: Option<String>,
        /// Value emitted during the "up" state. Defaults to `1.0`.
        #[cfg_attr(feature = "config", serde(default))]
        up_value: Option<f64>,
        /// Value emitted during the "down" state. Defaults to `0.0`.
        #[cfg_attr(feature = "config", serde(default))]
        down_value: Option<f64>,
        /// Domain-specific shorthand selecting `(up_value, down_value)` per
        /// the [`FlapEnum`] mapping. Mutually exclusive with `up_value` /
        /// `down_value`.
        #[cfg_attr(
            feature = "config",
            serde(default, rename = "enum", skip_serializing_if = "Option::is_none")
        )]
        enum_kind: Option<FlapEnum>,
    },

    /// Resource filling up and resetting on a repeating cycle (e.g. disk usage
    /// sawtoothing after log rotation).
    ///
    /// Desugars into a [`Sawtooth`](GeneratorConfig::Sawtooth) generator with
    /// `min = baseline`, `max = ceiling`, `period_secs` derived from
    /// `time_to_saturate`. The sawtooth resets to `baseline` after each
    /// `time_to_saturate` period, modeling a resource that fills and is
    /// periodically reclaimed.
    ///
    /// # Distinction from `Leak`
    ///
    /// - **Saturation**: repeating fill-and-reset cycle. Default period is
    ///   `"5m"`.
    /// - **Leak**: one-way ramp, no reset expected within the scenario
    ///   duration. Default period is `"10m"`.
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: saturation
    ///   baseline: 20.0
    ///   ceiling: 95.0
    ///   time_to_saturate: "5m"
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "saturation"))]
    Saturation {
        /// Resource level at the start of each cycle. Defaults to `0.0`.
        #[cfg_attr(feature = "config", serde(default))]
        baseline: Option<f64>,
        /// Maximum resource level before reset. Defaults to `100.0`.
        #[cfg_attr(feature = "config", serde(default))]
        ceiling: Option<f64>,
        /// Duration of one fill cycle. Defaults to `"5m"`.
        #[cfg_attr(feature = "config", serde(default))]
        time_to_saturate: Option<String>,
    },

    /// Resource growing toward a ceiling without resetting — a one-way ramp
    /// modeling a memory leak or similar resource exhaustion.
    ///
    /// Desugars into a [`Sawtooth`](GeneratorConfig::Sawtooth) generator.
    /// The intent is that `time_to_ceiling` equals or exceeds the scenario
    /// `duration` so values only ramp upward and never reset within the run.
    /// If the scenario has a `duration` set and `time_to_ceiling` is shorter
    /// than that duration, desugaring returns a config error because the
    /// sawtooth would reset mid-run, which is the
    /// [`Saturation`](GeneratorConfig::Saturation) pattern instead.
    ///
    /// # Distinction from `Saturation`
    ///
    /// - **Leak**: one-way ramp, no reset expected. `time_to_ceiling` should
    ///   be >= scenario `duration`. Default period is `"10m"`.
    /// - **Saturation**: repeating fill-and-reset cycle. Default period is
    ///   `"5m"`.
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: leak
    ///   baseline: 40.0
    ///   ceiling: 95.0
    ///   time_to_ceiling: "120s"
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "leak"))]
    Leak {
        /// Initial resource level. Defaults to `0.0`.
        #[cfg_attr(feature = "config", serde(default))]
        baseline: Option<f64>,
        /// Target ceiling value. Defaults to `100.0`.
        #[cfg_attr(feature = "config", serde(default))]
        ceiling: Option<f64>,
        /// Time to grow from baseline to ceiling. Defaults to `"10m"`.
        /// The sawtooth period is set to this value so values only ramp
        /// upward within the scenario duration.
        #[cfg_attr(feature = "config", serde(default))]
        time_to_ceiling: Option<String>,
    },

    /// Gradual performance loss with noise — models degradation over time
    /// (e.g. growing latency, increasing error rate).
    ///
    /// Desugars into a [`Sawtooth`](GeneratorConfig::Sawtooth) generator with
    /// jitter automatically applied on [`BaseScheduleConfig`].
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: degradation
    ///   baseline: 0.05
    ///   ceiling: 0.5
    ///   time_to_degrade: "60s"
    ///   noise: 0.02
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "degradation"))]
    Degradation {
        /// Starting performance level. Defaults to `0.0`.
        #[cfg_attr(feature = "config", serde(default))]
        baseline: Option<f64>,
        /// Worst-case performance level. Defaults to `100.0`.
        #[cfg_attr(feature = "config", serde(default))]
        ceiling: Option<f64>,
        /// Duration of the degradation ramp. Defaults to `"5m"`.
        #[cfg_attr(feature = "config", serde(default))]
        time_to_degrade: Option<String>,
        /// Jitter amplitude added as noise. Defaults to `1.0`.
        #[cfg_attr(feature = "config", serde(default))]
        noise: Option<f64>,
        /// Seed for the noise generator. Defaults to `0`.
        #[cfg_attr(feature = "config", serde(default))]
        noise_seed: Option<u64>,
    },

    /// Normal healthy oscillation around a center value — the "everything is
    /// fine" baseline signal.
    ///
    /// Desugars into a [`Sine`](GeneratorConfig::Sine) generator with jitter
    /// automatically applied on [`BaseScheduleConfig`].
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: steady
    ///   center: 75.0
    ///   amplitude: 10.0
    ///   period: "60s"
    ///   noise: 2.0
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "steady"))]
    Steady {
        /// Center of the oscillation (the sine wave's offset). Defaults to `50.0`.
        #[cfg_attr(feature = "config", serde(default))]
        center: Option<f64>,
        /// Half the peak-to-peak swing. Defaults to `10.0`.
        #[cfg_attr(feature = "config", serde(default))]
        amplitude: Option<f64>,
        /// Duration of one full oscillation cycle. Defaults to `"60s"`.
        #[cfg_attr(feature = "config", serde(default))]
        period: Option<String>,
        /// Jitter amplitude added as noise. Defaults to `1.0`.
        #[cfg_attr(feature = "config", serde(default))]
        noise: Option<f64>,
        /// Seed for the noise generator. Defaults to `0`.
        #[cfg_attr(feature = "config", serde(default))]
        noise_seed: Option<u64>,
    },

    /// Periodic anomalous bursts above a baseline — models sudden spikes
    /// in CPU, memory, or request rate.
    ///
    /// Desugars into a [`Spike`](GeneratorConfig::Spike) generator.
    ///
    /// # Example YAML
    ///
    /// ```yaml
    /// generator:
    ///   type: spike_event
    ///   baseline: 35.0
    ///   spike_height: 60.0
    ///   spike_duration: "10s"
    ///   spike_interval: "30s"
    /// ```
    #[cfg_attr(feature = "config", serde(rename = "spike_event"))]
    SpikeEvent {
        /// Normal output value between spikes. Defaults to `0.0`.
        #[cfg_attr(feature = "config", serde(default))]
        baseline: Option<f64>,
        /// Amount added to baseline during a spike. Defaults to `100.0`.
        #[cfg_attr(feature = "config", serde(default))]
        spike_height: Option<f64>,
        /// How long each spike lasts. Defaults to `"10s"`.
        #[cfg_attr(feature = "config", serde(default))]
        spike_duration: Option<String>,
        /// Time between spike starts. Defaults to `"30s"`.
        #[cfg_attr(feature = "config", serde(default))]
        spike_interval: Option<String>,
    },
}

impl GeneratorConfig {
    /// Returns `true` if this variant is an operational alias that must be
    /// desugared before the generator factory can process it.
    pub fn is_alias(&self) -> bool {
        matches!(
            self,
            GeneratorConfig::Flap { .. }
                | GeneratorConfig::Saturation { .. }
                | GeneratorConfig::Leak { .. }
                | GeneratorConfig::Degradation { .. }
                | GeneratorConfig::Steady { .. }
                | GeneratorConfig::SpikeEvent { .. }
        )
    }
}

/// Construct a `Box<dyn ValueGenerator>` from the given configuration.
///
/// The `rate` parameter (events per second) is required by time-based generators
/// (`Sine`, `Sawtooth`) to convert `period_secs` into period ticks.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if the generator configuration is invalid
/// (e.g., an empty values list for the sequence generator).
///
/// **Note:** [`GeneratorConfig::CsvReplay`] configs with `columns` set must be expanded
/// via [`crate::config::expand_scenario`] before calling this function. Passing an
/// unexpanded multi-column config returns a [`ConfigError`].
pub fn create_generator(
    config: &GeneratorConfig,
    rate: f64,
) -> Result<Box<dyn ValueGenerator>, SondaError> {
    match config {
        GeneratorConfig::Constant { value } => Ok(Box::new(Constant::new(*value))),
        GeneratorConfig::Uniform { min, max, seed } => {
            Ok(Box::new(UniformRandom::new(*min, *max, seed.unwrap_or(0))))
        }
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => Ok(Box::new(Sine::new(*amplitude, *period_secs, *offset, rate))),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => Ok(Box::new(Sawtooth::new(*min, *max, *period_secs, rate))),
        GeneratorConfig::Spike {
            baseline,
            magnitude,
            duration_secs,
            interval_secs,
        } => {
            if *interval_secs <= 0.0 {
                return Err(SondaError::Config(ConfigError::invalid(
                    "spike generator requires interval_secs > 0",
                )));
            }
            if *duration_secs < 0.0 {
                return Err(SondaError::Config(ConfigError::invalid(
                    "spike generator requires duration_secs >= 0",
                )));
            }
            Ok(Box::new(SpikeGenerator::new(
                *baseline,
                *magnitude,
                *duration_secs,
                *interval_secs,
                rate,
            )))
        }
        GeneratorConfig::Sequence { values, repeat } => Ok(Box::new(SequenceGenerator::new(
            values.clone(),
            repeat.unwrap_or(true),
        )?)),
        GeneratorConfig::CsvReplay {
            file,
            column,
            repeat,
            columns,
            ..
        } => {
            if columns.is_some() {
                return Err(SondaError::Config(ConfigError::invalid(
                    "csv_replay: call expand_scenario before create_generator when 'columns' is set",
                )));
            }
            Ok(Box::new(CsvReplayGenerator::new(
                file,
                column.unwrap_or(0),
                repeat.unwrap_or(true),
            )?))
        }
        GeneratorConfig::Step {
            start,
            step_size,
            max,
        } => Ok(Box::new(StepGenerator::new(
            start.unwrap_or(0.0),
            *step_size,
            *max,
        ))),
        // Operational aliases must be desugared before reaching this factory.
        // If one arrives here it means the config expansion pipeline was bypassed.
        GeneratorConfig::Flap { .. }
        | GeneratorConfig::Saturation { .. }
        | GeneratorConfig::Leak { .. }
        | GeneratorConfig::Degradation { .. }
        | GeneratorConfig::Steady { .. }
        | GeneratorConfig::SpikeEvent { .. } => Err(SondaError::Config(ConfigError::invalid(
            "operational alias generator must be desugared via \
             desugar_entry() or desugar_scenario_config() before calling create_generator()",
        ))),
    }
}

/// Optionally wrap a generator with jitter noise.
///
/// Returns the generator unchanged if `jitter` is `None` or `Some(0.0)`.
/// When jitter is positive, wraps the generator in a [`JitterWrapper`] that
/// adds deterministic uniform noise in `[-jitter, +jitter]` to every value.
///
/// # Parameters
///
/// - `generator` — the inner generator to wrap.
/// - `jitter` — the jitter amplitude. `None` or `Some(0.0)` means no jitter.
/// - `jitter_seed` — optional seed for the noise sequence. Defaults to `0`
///   when `None`.
pub fn wrap_with_jitter(
    generator: Box<dyn ValueGenerator>,
    jitter: Option<f64>,
    jitter_seed: Option<u64>,
) -> Box<dyn ValueGenerator> {
    match jitter {
        Some(j) if j != 0.0 => Box::new(JitterWrapper::new(generator, j, jitter_seed.unwrap_or(0))),
        _ => generator,
    }
}

// ---------------------------------------------------------------------------
// Log generators
// ---------------------------------------------------------------------------

/// A log generator produces a `LogEvent` for a given tick index.
///
/// Implementations must be deterministic for a given configuration and tick.
/// Side effects are not allowed in `generate()`.
pub trait LogGenerator: Send + Sync {
    /// Produce a `LogEvent` for the given tick index (0-based, monotonically increasing).
    fn generate(&self, tick: u64) -> LogEvent;
}

/// Configuration for one message template used by [`LogGeneratorConfig::Template`].
///
/// The `message` field may contain `{placeholder}` tokens that are resolved
/// using the corresponding value pool from `field_pools`.
///
/// # Example YAML
///
/// ```yaml
/// message: "Request from {ip} to {endpoint}"
/// field_pools:
///   ip:
///     - "10.0.0.1"
///     - "10.0.0.2"
///   endpoint:
///     - "/api"
///     - "/health"
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub struct TemplateConfig {
    /// The message template. Use `{field_name}` for dynamic placeholders.
    pub message: String,
    /// Maps placeholder names to their value pools.
    ///
    /// Uses `BTreeMap` for deterministic iteration order, matching the
    /// codebase convention for ordered maps.
    #[cfg_attr(feature = "config", serde(default))]
    pub field_pools: BTreeMap<String, Vec<String>>,
}

/// Configuration for a log generator, used for YAML deserialization.
///
/// The `type` field selects which generator to instantiate.
///
/// # Example YAML — template generator
///
/// ```yaml
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
///   seed: 42
/// ```
///
/// # Example YAML — replay generator
///
/// ```yaml
/// generator:
///   type: replay
///   file: /var/log/app.log
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(tag = "type"))]
pub enum LogGeneratorConfig {
    /// Generates events from message templates with randomized field pool values.
    #[cfg_attr(feature = "config", serde(rename = "template"))]
    Template {
        /// One or more template entries. Templates are selected round-robin by tick.
        templates: Vec<TemplateConfig>,
        /// Optional severity weight map. Keys are severity names (`info`, `warn`, etc.),
        /// values are relative weights. Defaults to `info: 1.0` when absent.
        #[cfg_attr(feature = "config", serde(default))]
        severity_weights: Option<HashMap<String, f64>>,
        /// Seed for deterministic replay. Defaults to `0` when absent.
        seed: Option<u64>,
    },
    /// Replays lines from a file, cycling back to the start when exhausted.
    #[cfg_attr(feature = "config", serde(rename = "replay"))]
    Replay {
        /// Path to the file containing log lines to replay.
        file: String,
    },
}

/// Parse a severity name string into a [`Severity`] variant.
fn parse_severity(s: &str) -> Result<Severity, SondaError> {
    match s.to_lowercase().as_str() {
        "trace" => Ok(Severity::Trace),
        "debug" => Ok(Severity::Debug),
        "info" => Ok(Severity::Info),
        "warn" | "warning" => Ok(Severity::Warn),
        "error" => Ok(Severity::Error),
        "fatal" => Ok(Severity::Fatal),
        other => Err(SondaError::Config(ConfigError::invalid(format!(
            "unknown severity {:?}: must be one of trace, debug, info, warn, error, fatal",
            other
        )))),
    }
}

/// Construct a `Box<dyn LogGenerator>` from the given configuration.
///
/// # Errors
/// - Returns [`SondaError::Config`] if severity weight keys are invalid.
/// - Returns [`SondaError::Config`] if the replay file is empty or cannot be parsed.
/// - Returns [`SondaError::Generator`] if the replay file cannot be read.
pub fn create_log_generator(
    config: &LogGeneratorConfig,
) -> Result<Box<dyn LogGenerator>, SondaError> {
    match config {
        LogGeneratorConfig::Template {
            templates,
            severity_weights,
            seed,
        } => {
            let seed = seed.unwrap_or(0);

            // Build severity weight vector from the optional map.
            let weights: Vec<(Severity, f64)> = if let Some(map) = severity_weights {
                let mut pairs = Vec::with_capacity(map.len());
                for (name, weight) in map {
                    let severity = parse_severity(name)?;
                    pairs.push((severity, *weight));
                }
                // Sort by severity ordinal for deterministic ordering.
                pairs.sort_by_key(|a| a.0);
                pairs
            } else {
                vec![]
            };

            // Convert TemplateConfig into TemplateEntry.
            let entries: Vec<TemplateEntry> = templates
                .iter()
                .map(|tc| TemplateEntry {
                    message: tc.message.clone(),
                    field_pools: tc.field_pools.clone(),
                })
                .collect();

            Ok(Box::new(LogTemplateGenerator::new(entries, weights, seed)))
        }
        LogGeneratorConfig::Replay { file } => {
            let path = std::path::Path::new(file);
            Ok(Box::new(LogReplayGenerator::from_file(path)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Factory tests -------------------------------------------------------

    #[test]
    fn factory_constant_returns_configured_value() {
        let config = GeneratorConfig::Constant { value: 1.0 };
        let gen = create_generator(&config, 100.0).expect("constant factory");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1_000_000), 1.0);
    }

    #[test]
    fn factory_uniform_returns_values_in_range() {
        let config = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: Some(7),
        };
        let gen = create_generator(&config, 100.0).expect("uniform factory");
        for tick in 0..1000 {
            let v = gen.value(tick);
            assert!(
                v >= 0.0 && v <= 1.0,
                "uniform value {v} out of [0,1] at tick {tick}"
            );
        }
    }

    #[test]
    fn factory_uniform_seed_none_defaults_to_zero_seed() {
        // When seed is None the factory must behave the same as seed Some(0).
        let config_none = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: None,
        };
        let config_zero = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: Some(0),
        };
        let gen_none = create_generator(&config_none, 1.0).expect("uniform none factory");
        let gen_zero = create_generator(&config_zero, 1.0).expect("uniform zero factory");
        for tick in 0..100 {
            assert_eq!(
                gen_none.value(tick),
                gen_zero.value(tick),
                "seed=None must equal seed=Some(0) at tick {tick}"
            );
        }
    }

    #[test]
    fn factory_sine_value_at_zero_equals_offset() {
        let config = GeneratorConfig::Sine {
            amplitude: 5.0,
            period_secs: 10.0,
            offset: 3.0,
        };
        let gen = create_generator(&config, 1.0).expect("sine factory");
        assert!(
            (gen.value(0) - 3.0).abs() < 1e-10,
            "sine factory: value(0) must equal offset"
        );
    }

    #[test]
    fn factory_sawtooth_value_at_zero_equals_min() {
        let config = GeneratorConfig::Sawtooth {
            min: 5.0,
            max: 15.0,
            period_secs: 10.0,
        };
        let gen = create_generator(&config, 1.0).expect("sawtooth factory");
        assert_eq!(
            gen.value(0),
            5.0,
            "sawtooth factory: value(0) must equal min"
        );
    }

    // ---- Sequence factory tests -----------------------------------------------

    #[test]
    fn factory_sequence_repeat_true_creates_working_generator() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0],
            repeat: Some(true),
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=true");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
        assert_eq!(gen.value(3), 1.0, "should wrap around");
    }

    #[test]
    fn factory_sequence_repeat_false_creates_working_generator() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0],
            repeat: Some(false),
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=false");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(4), 3.0, "should clamp to last value");
    }

    #[test]
    fn factory_sequence_repeat_none_defaults_to_true() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0],
            repeat: None,
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=None");
        // With repeat defaulting to true, tick=2 on a 2-element seq should wrap to index 0
        assert_eq!(
            gen.value(2),
            1.0,
            "repeat=None should default to true (cycling)"
        );
    }

    #[test]
    fn factory_sequence_empty_values_returns_error() {
        let config = GeneratorConfig::Sequence {
            values: vec![],
            repeat: Some(true),
        };
        let result = create_generator(&config, 1.0);
        assert!(result.is_err(), "empty sequence must return an error");
    }

    // ---- Step factory tests ---------------------------------------------------

    #[test]
    fn factory_step_linear_growth() {
        let config = GeneratorConfig::Step {
            start: None,
            step_size: 1.0,
            max: None,
        };
        let gen = create_generator(&config, 1.0).expect("step factory");
        assert_eq!(gen.value(0), 0.0);
        assert_eq!(gen.value(1), 1.0);
        assert_eq!(gen.value(100), 100.0);
    }

    #[test]
    fn factory_step_with_start() {
        let config = GeneratorConfig::Step {
            start: Some(10.0),
            step_size: 2.0,
            max: None,
        };
        let gen = create_generator(&config, 1.0).expect("step factory with start");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 12.0);
        assert_eq!(gen.value(5), 20.0);
    }

    #[test]
    fn factory_step_with_wrap() {
        let config = GeneratorConfig::Step {
            start: Some(0.0),
            step_size: 1.0,
            max: Some(3.0),
        };
        let gen = create_generator(&config, 1.0).expect("step factory with wrap");
        assert_eq!(gen.value(0), 0.0);
        assert_eq!(gen.value(3), 0.0, "should wrap at max");
        assert_eq!(gen.value(4), 1.0);
    }

    #[test]
    fn factory_step_start_none_defaults_to_zero() {
        let config_none = GeneratorConfig::Step {
            start: None,
            step_size: 1.0,
            max: None,
        };
        let config_zero = GeneratorConfig::Step {
            start: Some(0.0),
            step_size: 1.0,
            max: None,
        };
        let gen_none = create_generator(&config_none, 1.0).expect("step start=None");
        let gen_zero = create_generator(&config_zero, 1.0).expect("step start=0");
        for tick in 0..10 {
            assert_eq!(
                gen_none.value(tick),
                gen_zero.value(tick),
                "start=None must equal start=Some(0.0) at tick {tick}"
            );
        }
    }

    // ---- Spike factory tests --------------------------------------------------

    #[test]
    fn factory_spike_returns_baseline_outside_window() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 10.0,
            interval_secs: 60.0,
        };
        let gen = create_generator(&config, 1.0).expect("spike factory");
        // tick 15 is outside the 10-tick spike window
        assert_eq!(gen.value(15), 50.0);
    }

    #[test]
    fn factory_spike_returns_spike_inside_window() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 10.0,
            interval_secs: 60.0,
        };
        let gen = create_generator(&config, 1.0).expect("spike factory");
        // tick 5 is inside the 10-tick spike window
        assert_eq!(gen.value(5), 250.0);
    }

    #[test]
    fn factory_spike_zero_interval_returns_error() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 10.0,
            interval_secs: 0.0,
        };
        let result = create_generator(&config, 1.0);
        assert!(result.is_err(), "interval_secs=0 must return an error");
    }

    #[test]
    fn factory_spike_negative_interval_returns_error() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 10.0,
            interval_secs: -1.0,
        };
        let result = create_generator(&config, 1.0);
        assert!(
            result.is_err(),
            "negative interval_secs must return an error"
        );
    }

    #[test]
    fn factory_spike_negative_duration_returns_error() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: -5.0,
            interval_secs: 60.0,
        };
        let result = create_generator(&config, 1.0);
        assert!(
            result.is_err(),
            "negative duration_secs must return an error"
        );
    }

    #[test]
    fn factory_spike_zero_duration_succeeds() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 0.0,
            interval_secs: 60.0,
        };
        let gen = create_generator(&config, 1.0).expect("duration_secs=0 is valid");
        // With zero duration, all ticks should return baseline
        assert_eq!(gen.value(0), 50.0);
        assert_eq!(gen.value(30), 50.0);
    }

    #[test]
    fn factory_csv_replay_with_columns_returns_error() {
        let config = GeneratorConfig::CsvReplay {
            file: "data.csv".to_string(),
            column: None,
            repeat: None,
            columns: Some(vec![CsvColumnSpec {
                index: 1,
                name: "cpu".to_string(),
                labels: None,
            }]),
            timescale: None,
            default_metric_name: None,
        };
        let result = create_generator(&config, 1.0);
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("expand_scenario"),
                    "error must mention expand_scenario, got: {msg}"
                );
            }
            Ok(_) => panic!("csv_replay with columns set must return an error"),
        }
    }

    // ---- Config deserialization tests ----------------------------------------
    // These tests require the `config` feature (serde_yaml_ng).

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_constant_config() {
        let yaml = "type: constant\nvalue: 42.0\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize constant");
        match config {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 42.0);
            }
            _ => panic!("expected Constant variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_uniform_config_with_seed() {
        let yaml = "type: uniform\nmin: 1.0\nmax: 5.0\nseed: 99\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize uniform");
        match config {
            GeneratorConfig::Uniform { min, max, seed } => {
                assert_eq!(min, 1.0);
                assert_eq!(max, 5.0);
                assert_eq!(seed, Some(99));
            }
            _ => panic!("expected Uniform variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_uniform_config_without_seed() {
        let yaml = "type: uniform\nmin: 0.0\nmax: 10.0\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize uniform no seed");
        match config {
            GeneratorConfig::Uniform { min, max, seed } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 10.0);
                assert_eq!(seed, None);
            }
            _ => panic!("expected Uniform variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sine_config() {
        let yaml = "type: sine\namplitude: 5.0\nperiod_secs: 30\noffset: 10.0\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize sine");
        match config {
            GeneratorConfig::Sine {
                amplitude,
                period_secs,
                offset,
            } => {
                assert_eq!(amplitude, 5.0);
                assert_eq!(period_secs, 30.0);
                assert_eq!(offset, 10.0);
            }
            _ => panic!("expected Sine variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sawtooth_config() {
        let yaml = "type: sawtooth\nmin: 0.0\nmax: 100.0\nperiod_secs: 60.0\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize sawtooth");
        match config {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 100.0);
                assert_eq!(period_secs, 60.0);
            }
            _ => panic!("expected Sawtooth variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_step_config_full() {
        let yaml = "type: step\nstart: 10.0\nstep_size: 2.5\nmax: 100.0\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize step");
        match config {
            GeneratorConfig::Step {
                start,
                step_size,
                max,
            } => {
                assert_eq!(start, Some(10.0));
                assert_eq!(step_size, 2.5);
                assert_eq!(max, Some(100.0));
            }
            _ => panic!("expected Step variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_step_config_minimal() {
        let yaml = "type: step\nstep_size: 1.0\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize step minimal");
        match config {
            GeneratorConfig::Step {
                start,
                step_size,
                max,
            } => {
                assert_eq!(start, None, "start should default to None when omitted");
                assert_eq!(step_size, 1.0);
                assert_eq!(max, None, "max should be None when omitted");
            }
            _ => panic!("expected Step variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_step_config_integer_values() {
        // YAML integers should coerce to f64
        let yaml = "type: step\nstart: 0\nstep_size: 1\nmax: 1000\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize step with integers");
        match config {
            GeneratorConfig::Step {
                start,
                step_size,
                max,
            } => {
                assert_eq!(start, Some(0.0));
                assert_eq!(step_size, 1.0);
                assert_eq!(max, Some(1000.0));
            }
            _ => panic!("expected Step variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sequence_config_with_repeat() {
        let yaml = "type: sequence\nvalues: [1.0, 2.0, 3.0]\nrepeat: true\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize sequence with repeat");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![1.0, 2.0, 3.0]);
                assert_eq!(repeat, Some(true));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sequence_config_without_repeat() {
        let yaml = "type: sequence\nvalues: [10.0, 20.0]\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize sequence without repeat");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![10.0, 20.0]);
                assert_eq!(repeat, None, "repeat should be None when omitted");
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sequence_config_repeat_false() {
        let yaml = "type: sequence\nvalues: [5.0]\nrepeat: false\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize sequence repeat=false");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![5.0]);
                assert_eq!(repeat, Some(false));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_sequence_config_integer_values() {
        // YAML integers should coerce to f64
        let yaml = "type: sequence\nvalues: [10, 20, 30]\nrepeat: true\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize sequence with integer values");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![10.0, 20.0, 30.0]);
                assert_eq!(repeat, Some(true));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_spike_config() {
        let yaml =
            "type: spike\nbaseline: 50.0\nmagnitude: 200.0\nduration_secs: 10\ninterval_secs: 60\n";
        let config: GeneratorConfig = serde_yaml_ng::from_str(yaml).expect("deserialize spike");
        match config {
            GeneratorConfig::Spike {
                baseline,
                magnitude,
                duration_secs,
                interval_secs,
            } => {
                assert_eq!(baseline, 50.0);
                assert_eq!(magnitude, 200.0);
                assert_eq!(duration_secs, 10.0);
                assert_eq!(interval_secs, 60.0);
            }
            _ => panic!("expected Spike variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_spike_config_negative_magnitude() {
        let yaml =
            "type: spike\nbaseline: 100.0\nmagnitude: -50.0\nduration_secs: 5\ninterval_secs: 20\n";
        let config: GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize spike negative magnitude");
        match config {
            GeneratorConfig::Spike {
                baseline,
                magnitude,
                ..
            } => {
                assert_eq!(baseline, 100.0);
                assert_eq!(magnitude, -50.0);
            }
            _ => panic!("expected Spike variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_example_yaml_scenario_file() {
        // Validate the example file from examples/sequence-alert-test.yaml
        let yaml = "\
name: cpu_spike_test
rate: 1
duration: 80s

generator:
  type: sequence
  values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
  repeat: true

labels:
  instance: server-01
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
";
        let config: crate::config::ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("example YAML must deserialize");
        assert_eq!(config.name, "cpu_spike_test");
        assert_eq!(config.rate, 1.0);
        assert_eq!(config.duration, Some("80s".to_string()));
        match &config.generator {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values.len(), 16);
                assert_eq!(values[0], 10.0);
                assert_eq!(values[5], 95.0);
                assert_eq!(values[10], 10.0);
                assert_eq!(*repeat, Some(true));
            }
            _ => panic!("expected Sequence generator variant in example YAML"),
        }
    }

    // ---- Send + Sync contract tests ------------------------------------------

    // ---- wrap_with_jitter factory tests ----------------------------------------

    #[test]
    fn wrap_with_jitter_none_returns_unchanged() {
        let config = GeneratorConfig::Constant { value: 42.0 };
        let gen = create_generator(&config, 1.0).expect("constant factory");
        let wrapped = wrap_with_jitter(gen, None, None);
        for tick in 0..100 {
            assert_eq!(
                wrapped.value(tick),
                42.0,
                "jitter=None must return original values at tick {tick}"
            );
        }
    }

    #[test]
    fn wrap_with_jitter_zero_returns_unchanged() {
        let config = GeneratorConfig::Constant { value: 42.0 };
        let gen = create_generator(&config, 1.0).expect("constant factory");
        let wrapped = wrap_with_jitter(gen, Some(0.0), Some(99));
        for tick in 0..100 {
            assert_eq!(
                wrapped.value(tick),
                42.0,
                "jitter=0.0 must return original values at tick {tick}"
            );
        }
    }

    #[test]
    fn wrap_with_jitter_positive_produces_values_in_range() {
        let base = 100.0;
        let jitter_amp = 5.0;
        let config = GeneratorConfig::Constant { value: base };
        let gen = create_generator(&config, 1.0).expect("constant factory");
        let wrapped = wrap_with_jitter(gen, Some(jitter_amp), Some(42));
        for tick in 0..10_000 {
            let v = wrapped.value(tick);
            assert!(
                v >= base - jitter_amp && v <= base + jitter_amp,
                "value {v} at tick {tick} outside [{}, {}]",
                base - jitter_amp,
                base + jitter_amp
            );
        }
    }

    #[test]
    fn wrap_with_jitter_seed_none_defaults_to_zero() {
        let config = GeneratorConfig::Constant { value: 50.0 };
        let gen_none = create_generator(&config, 1.0).expect("factory");
        let gen_zero = create_generator(&config, 1.0).expect("factory");
        let wrapped_none = wrap_with_jitter(gen_none, Some(5.0), None);
        let wrapped_zero = wrap_with_jitter(gen_zero, Some(5.0), Some(0));
        for tick in 0..100 {
            assert_eq!(
                wrapped_none.value(tick),
                wrapped_zero.value(tick),
                "jitter_seed=None must equal jitter_seed=Some(0) at tick {tick}"
            );
        }
    }

    // ---- Send + Sync contract tests ------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn generators_are_send_and_sync() {
        // These are compile-time checks — if the types don't implement Send+Sync the
        // test binary will not compile.
        assert_send_sync::<crate::generator::uniform::UniformRandom>();
        assert_send_sync::<crate::generator::sine::Sine>();
        assert_send_sync::<crate::generator::sawtooth::Sawtooth>();
        assert_send_sync::<crate::generator::constant::Constant>();
        assert_send_sync::<crate::generator::sequence::SequenceGenerator>();
        assert_send_sync::<crate::generator::spike::SpikeGenerator>();
        assert_send_sync::<crate::generator::csv_replay::CsvReplayGenerator>();
        assert_send_sync::<crate::generator::step::StepGenerator>();
        assert_send_sync::<crate::generator::jitter::JitterWrapper>();
    }

    // ---- LogGeneratorConfig deserialization tests ----------------------------
    // These tests require the `config` feature (serde_yaml_ng).

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_log_template_config_minimal() {
        let yaml = "\
type: template
templates:
  - message: \"hello {name}\"
    field_pools:
      name:
        - alice
        - bob
";
        let config: LogGeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize template config");
        match config {
            LogGeneratorConfig::Template {
                templates,
                severity_weights,
                seed,
            } => {
                assert_eq!(templates.len(), 1);
                assert_eq!(templates[0].message, "hello {name}");
                assert!(templates[0].field_pools.contains_key("name"));
                assert_eq!(
                    templates[0].field_pools["name"],
                    vec!["alice".to_string(), "bob".to_string()]
                );
                assert!(
                    severity_weights.is_none(),
                    "severity_weights must default to None"
                );
                assert!(seed.is_none(), "seed must default to None");
            }
            _ => panic!("expected Template variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_log_template_config_with_weights_and_seed() {
        let yaml = "\
type: template
templates:
  - message: \"msg\"
    field_pools: {}
severity_weights:
  info: 0.7
  warn: 0.2
  error: 0.1
seed: 42
";
        let config: LogGeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize template config with weights");
        match config {
            LogGeneratorConfig::Template {
                severity_weights,
                seed,
                ..
            } => {
                let weights = severity_weights.expect("severity_weights should be present");
                assert!((weights["info"] - 0.7).abs() < 1e-10);
                assert!((weights["warn"] - 0.2).abs() < 1e-10);
                assert!((weights["error"] - 0.1).abs() < 1e-10);
                assert_eq!(seed, Some(42));
            }
            _ => panic!("expected Template variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_log_replay_config() {
        let yaml = "type: replay\nfile: /var/log/app.log\n";
        let config: LogGeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("deserialize replay config");
        match config {
            LogGeneratorConfig::Replay { file } => {
                assert_eq!(file, "/var/log/app.log");
            }
            _ => panic!("expected Replay variant"),
        }
    }

    // ---- create_log_generator factory tests ----------------------------------

    #[test]
    fn factory_template_config_creates_working_generator() {
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "event {id}".into(),
                field_pools: {
                    let mut m = BTreeMap::new();
                    m.insert("id".into(), vec!["1".into(), "2".into(), "3".into()]);
                    m
                },
            }],
            severity_weights: None,
            seed: Some(0),
        };
        let gen = create_log_generator(&config).expect("template factory must succeed");
        let event = gen.generate(0);
        // Must not contain unresolved placeholder.
        assert!(!event.message.contains('{'));
    }

    #[test]
    fn factory_template_config_seed_none_defaults_correctly() {
        // seed: None should not error and should produce a generator.
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "static message".into(),
                field_pools: BTreeMap::new(),
            }],
            severity_weights: None,
            seed: None,
        };
        let gen = create_log_generator(&config).expect("template with seed=None must succeed");
        assert_eq!(gen.generate(0).message, "static message");
    }

    #[test]
    fn factory_template_invalid_severity_key_returns_error() {
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "msg".into(),
                field_pools: BTreeMap::new(),
            }],
            severity_weights: {
                let mut m = HashMap::new();
                m.insert("bogus".into(), 1.0);
                Some(m)
            },
            seed: None,
        };
        let result = create_log_generator(&config);
        assert!(
            result.is_err(),
            "invalid severity key 'bogus' must produce Err"
        );
    }

    #[test]
    fn factory_replay_config_missing_file_returns_error() {
        let config = LogGeneratorConfig::Replay {
            file: "/this/path/does/not/exist.log".into(),
        };
        let result = create_log_generator(&config);
        assert!(result.is_err(), "missing replay file must produce Err");
    }

    #[test]
    fn factory_replay_config_creates_working_generator() {
        use std::io::Write;
        use tempfile::NamedTempFile;
        let mut tmp = NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "line one").expect("write");
        writeln!(tmp, "line two").expect("write");
        let config = LogGeneratorConfig::Replay {
            file: tmp.path().to_string_lossy().into_owned(),
        };
        let gen =
            create_log_generator(&config).expect("replay factory with real file must succeed");
        assert_eq!(gen.generate(0).message, "line one");
        assert_eq!(gen.generate(1).message, "line two");
        assert_eq!(gen.generate(2).message, "line one");
    }

    #[test]
    fn log_generators_are_send_and_sync() {
        assert_send_sync::<crate::generator::log_template::LogTemplateGenerator>();
        assert_send_sync::<crate::generator::log_replay::LogReplayGenerator>();
    }
}
