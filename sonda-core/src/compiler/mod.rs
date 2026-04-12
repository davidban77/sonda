//! Version 2 scenario format: AST types and parser.
//!
//! This module defines the parsed representation of a v2 scenario file before
//! any compilation (defaults resolution, pack expansion, or after-clause
//! evaluation). The [`ScenarioFile`] is a direct, faithful representation of
//! the YAML on disk.
//!
//! All types use `deny_unknown_fields` to reject YAML typos at parse time.
//! This is a deliberate strictness choice — adding new schema fields requires
//! updating these types.
//!
//! # Submodules
//!
//! - [`parse`] — YAML deserialization, schema validation, and version detection.
//! - [`normalize`] — `defaults:` resolution and entry-level normalization.
//! - [`expand`] — pack expansion inside `scenarios:` (Phase 3).

#[cfg(feature = "config")]
pub mod parse;

#[cfg(feature = "config")]
pub mod normalize;

#[cfg(feature = "config")]
pub mod expand;

use std::collections::BTreeMap;

use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
};
use crate::encoder::EncoderConfig;
use crate::generator::{GeneratorConfig, LogGeneratorConfig};
use crate::packs::MetricOverride;
use crate::sink::SinkConfig;

// ---------------------------------------------------------------------------
// Compiler AST types
// ---------------------------------------------------------------------------

/// A parsed v2 scenario file.
///
/// This is the top-level AST node produced by [`parse::parse`]. It captures
/// the exact structure of the YAML input without resolving defaults, expanding
/// packs, or compiling after-clauses.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct ScenarioFile {
    /// Schema version. Must be `2`.
    pub version: u32,
    /// Optional shared defaults inherited by all entries.
    #[cfg_attr(feature = "config", serde(default))]
    pub defaults: Option<Defaults>,
    /// One or more scenario entries (inline signals or pack references).
    pub scenarios: Vec<Entry>,
}

/// Shared defaults inherited by all entries in a v2 scenario file.
///
/// Fields set here act as fallbacks for entries that omit the corresponding
/// field. Defaults resolution is performed in a later compilation phase (PR 3),
/// not during parsing.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct Defaults {
    /// Default event rate in events per second.
    #[cfg_attr(feature = "config", serde(default))]
    pub rate: Option<f64>,
    /// Default total run duration (e.g. `"30s"`, `"5m"`).
    #[cfg_attr(feature = "config", serde(default))]
    pub duration: Option<String>,
    /// Default encoder configuration.
    #[cfg_attr(feature = "config", serde(default))]
    pub encoder: Option<EncoderConfig>,
    /// Default sink configuration.
    #[cfg_attr(feature = "config", serde(default))]
    pub sink: Option<SinkConfig>,
    /// Default static labels merged into every entry.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<BTreeMap<String, String>>,
}

/// A single scenario entry in a v2 file.
///
/// An entry is either an **inline signal** (has `generator` and `name`) or a
/// **pack reference** (has `pack`). The two forms are mutually exclusive,
/// enforced at parse time.
///
/// All fields are optional in the struct to support flexible YAML authoring.
/// Semantic validation (required fields, mutual exclusion) is performed by
/// [`parse::parse`].
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct Entry {
    /// Unique identifier for causal dependency references (`after.ref`).
    #[cfg_attr(feature = "config", serde(default))]
    pub id: Option<String>,
    /// Signal type: `"metrics"`, `"logs"`, `"histogram"`, or `"summary"`.
    pub signal_type: String,
    /// Metric or scenario name. Required for inline entries.
    #[cfg_attr(feature = "config", serde(default))]
    pub name: Option<String>,
    /// Event rate in events per second.
    #[cfg_attr(feature = "config", serde(default))]
    pub rate: Option<f64>,
    /// Total run duration (e.g. `"30s"`, `"5m"`).
    #[cfg_attr(feature = "config", serde(default))]
    pub duration: Option<String>,
    /// Value generator configuration (for metrics).
    #[cfg_attr(feature = "config", serde(default))]
    pub generator: Option<GeneratorConfig>,
    /// Log generator configuration (for logs signal type).
    ///
    /// Mutually exclusive with `generator` — an entry uses one or the other
    /// depending on `signal_type`.
    #[cfg_attr(feature = "config", serde(default))]
    pub log_generator: Option<LogGeneratorConfig>,
    /// Static labels attached to every emitted event.
    #[cfg_attr(feature = "config", serde(default))]
    pub labels: Option<BTreeMap<String, String>>,
    /// Dynamic (rotating) label configurations.
    #[cfg_attr(feature = "config", serde(default))]
    pub dynamic_labels: Option<Vec<DynamicLabelConfig>>,
    /// Encoder configuration for this entry.
    #[cfg_attr(feature = "config", serde(default))]
    pub encoder: Option<EncoderConfig>,
    /// Sink configuration for this entry.
    #[cfg_attr(feature = "config", serde(default))]
    pub sink: Option<SinkConfig>,
    /// Jitter amplitude applied to generated values.
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter: Option<f64>,
    /// Deterministic seed for jitter RNG.
    #[cfg_attr(feature = "config", serde(default))]
    pub jitter_seed: Option<u64>,
    /// Recurring silent-period configuration.
    #[cfg_attr(feature = "config", serde(default))]
    pub gaps: Option<GapConfig>,
    /// Recurring high-rate burst configuration.
    #[cfg_attr(feature = "config", serde(default))]
    pub bursts: Option<BurstConfig>,
    /// Cardinality spike configurations.
    #[cfg_attr(feature = "config", serde(default))]
    pub cardinality_spikes: Option<Vec<CardinalitySpikeConfig>>,
    /// Phase offset for staggered start within a clock group.
    #[cfg_attr(feature = "config", serde(default))]
    pub phase_offset: Option<String>,
    /// Clock group for coordinated timing across entries.
    #[cfg_attr(feature = "config", serde(default))]
    pub clock_group: Option<String>,
    /// Causal dependency on another signal's value.
    #[cfg_attr(feature = "config", serde(default))]
    pub after: Option<AfterClause>,

    // -- Pack-backed entry fields --
    /// Pack name or file path. Mutually exclusive with `generator`.
    #[cfg_attr(feature = "config", serde(default))]
    pub pack: Option<String>,
    /// Per-metric overrides within the referenced pack.
    #[cfg_attr(feature = "config", serde(default))]
    pub overrides: Option<BTreeMap<String, MetricOverride>>,

    // -- Histogram / summary fields --
    /// Distribution model for histogram or summary observations.
    #[cfg_attr(feature = "config", serde(default))]
    pub distribution: Option<DistributionConfig>,
    /// Histogram bucket boundaries (histogram only).
    #[cfg_attr(feature = "config", serde(default))]
    pub buckets: Option<Vec<f64>>,
    /// Summary quantile boundaries (summary only).
    #[cfg_attr(feature = "config", serde(default))]
    pub quantiles: Option<Vec<f64>>,
    /// Number of observations sampled per tick.
    #[cfg_attr(feature = "config", serde(default))]
    pub observations_per_tick: Option<u32>,
    /// Linear drift applied to the distribution mean each second.
    #[cfg_attr(feature = "config", serde(default))]
    pub mean_shift_per_sec: Option<f64>,
    /// Deterministic seed for histogram/summary sampling.
    #[cfg_attr(feature = "config", serde(default))]
    pub seed: Option<u64>,
}

/// Comparison operator for an [`AfterClause`] threshold check.
///
/// Serde maps `"<"` to [`LessThan`](AfterOp::LessThan) and `">"` to
/// [`GreaterThan`](AfterOp::GreaterThan). Any other value is rejected at
/// deserialization time.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub enum AfterOp {
    /// The referenced signal's value must be less than the threshold.
    #[cfg_attr(feature = "config", serde(rename = "<"))]
    LessThan,
    /// The referenced signal's value must be greater than the threshold.
    #[cfg_attr(feature = "config", serde(rename = ">"))]
    GreaterThan,
}

/// Structured after-clause expressing a causal dependency on another signal.
///
/// When present on a [`Entry`], the entry will not start emitting until the
/// referenced signal's latest value satisfies the comparison. Compilation of
/// after-clauses into runtime timing is handled in a later phase (PR 5).
///
/// # YAML example
///
/// ```yaml
/// after:
///   ref: cpu_signal
///   op: ">"
///   value: 90.0
///   delay: "5s"
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct AfterClause {
    /// Target signal id to observe.
    ///
    /// Serialized as `"ref"` in YAML because `ref` is a Rust keyword.
    #[cfg_attr(feature = "config", serde(rename = "ref"))]
    pub ref_id: String,
    /// Comparison operator: `"<"` or `">"`.
    pub op: AfterOp,
    /// Threshold value for the comparison.
    pub value: f64,
    /// Optional additional delay after the condition is met.
    #[cfg_attr(feature = "config", serde(default))]
    pub delay: Option<String>,
}
