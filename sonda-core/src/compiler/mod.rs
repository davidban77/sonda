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
//! - [`env_interpolate`] — Phase 0: pre-parse `${VAR}` / `${VAR:-default}`
//!   substitution against the process environment.
//! - [`parse`] — YAML deserialization, schema validation, and version detection.
//! - [`normalize`] — `defaults:` resolution and entry-level normalization.
//! - [`expand`] — pack expansion inside `scenarios:` (Phase 3).
//! - [`timing`] — pure threshold-crossing math for every supported generator.
//! - [`compile_after`] — `after` clause resolution, dependency graph, and
//!   clock-group assignment (Phases 4 and 5).
//! - [`prepare`] — translation from [`compile_after::CompiledFile`] into the
//!   runtime's `Vec<ScenarioEntry>` input shape (Phase 6).

#[cfg(feature = "config")]
pub mod env_interpolate;

#[cfg(feature = "config")]
pub mod parse;

#[cfg(feature = "config")]
pub mod normalize;

#[cfg(feature = "config")]
pub mod expand;

pub mod timing;

#[cfg(feature = "config")]
pub mod compile_after;

#[cfg(feature = "config")]
pub mod prepare;

use std::collections::BTreeMap;

use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
    OnSinkError,
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
///
/// # Catalog metadata
///
/// The three optional fields [`scenario_name`](Self::scenario_name),
/// [`category`](Self::category), and [`description`](Self::description)
/// mirror the v1 top-level metadata shape so the CLI catalog probe
/// (`sonda::scenarios::read_scenario_metadata`) reads v1 and v2 files
/// through the same `Deserialize` struct. The compiler pipeline itself
/// (normalize → expand → compile_after → prepare) does **not** consume
/// these fields — they are pure metadata, not compile input.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct ScenarioFile {
    /// Schema version. Must be `2`.
    pub version: u32,
    /// Discriminator declaring whether the file is a runnable scenario
    /// or a composable pack definition.
    pub kind: Kind,
    /// Optional file-level metadata tags surfaced by `sonda list --tag`.
    /// Carried through normalization unchanged; ignored at runtime.
    #[cfg_attr(feature = "config", serde(default))]
    pub tags: Vec<String>,
    /// Catalog display name (kebab-case). When present it overrides the
    /// filename-derived name in the CLI catalog probe. Pure metadata —
    /// ignored by every compiler phase.
    #[cfg_attr(feature = "config", serde(default))]
    pub scenario_name: Option<String>,
    /// Catalog category used by `scenarios list --category <name>` and
    /// `catalog list --category <name>`. Allowed values are enforced by
    /// the CLI CI validation (`infrastructure`, `network`, `application`,
    /// `observability`); the AST itself does not constrain the string.
    /// Pure metadata — ignored by every compiler phase.
    #[cfg_attr(feature = "config", serde(default))]
    pub category: Option<String>,
    /// One-line human-readable description surfaced by
    /// `scenarios list` / `catalog list` and `scenarios show`. Pure
    /// metadata — ignored by every compiler phase.
    #[cfg_attr(feature = "config", serde(default))]
    pub description: Option<String>,
    /// Optional shared defaults inherited by all entries.
    #[cfg_attr(feature = "config", serde(default))]
    pub defaults: Option<Defaults>,
    /// One or more scenario entries (inline signals or pack references).
    /// Empty when `kind: composable` — composable files carry no entries.
    #[cfg_attr(feature = "config", serde(default))]
    pub scenarios: Vec<Entry>,
}

/// Discriminator declaring the role of a v2 YAML file.
///
/// Required at the top level of every v2 scenario file. `Runnable` files
/// carry one or more scenario entries (inline or via `pack:` references)
/// and are executable. `Composable` files are pack definitions; their
/// body matches [`MetricPackDef`](crate::packs::MetricPackDef) and they
/// are referenced from runnable files via `pack:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
pub enum Kind {
    Runnable,
    Composable,
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
    /// Default total run duration (e.g. `"30s"`, `"5m"`). Applied per entry —
    /// each entry runs for this long from its own resolved start, so a cascade's
    /// total wall-clock is `max(phase_offset + duration)`, not `duration`.
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
    /// Default sink-error policy inherited by every entry.
    #[cfg_attr(feature = "config", serde(default))]
    pub on_sink_error: Option<OnSinkError>,
    /// Default `while:` clause inherited by every entry.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "while", skip_serializing_if = "Option::is_none")
    )]
    pub while_clause: Option<WhileClause>,
    /// Default `delay:` clause inherited by every entry.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "delay", skip_serializing_if = "Option::is_none")
    )]
    pub delay_clause: Option<DelayClause>,
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
    /// Continuous lifecycle gate on another signal's value.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "while", skip_serializing_if = "Option::is_none")
    )]
    pub while_clause: Option<WhileClause>,
    /// Open / close debounce windows applied to `while_clause` transitions.
    #[cfg_attr(
        feature = "config",
        serde(default, rename = "delay", skip_serializing_if = "Option::is_none")
    )]
    pub delay_clause: Option<DelayClause>,

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
    /// Per-entry sink-error policy (overrides defaults).
    #[cfg_attr(feature = "config", serde(default))]
    pub on_sink_error: Option<OnSinkError>,
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

/// Strict comparison operator for a [`WhileClause`].
///
/// Only `<` and `>` are accepted. Non-strict operators (`<=`, `>=`, `==`,
/// `!=`) are rejected at deserialize time with a hint pointing to the
/// strict alternatives — equality on `f64` over a continuous gate is
/// numerically unsafe and forbidden by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub enum WhileOp {
    #[cfg_attr(feature = "config", serde(rename = "<"))]
    LessThan,
    #[cfg_attr(feature = "config", serde(rename = ">"))]
    GreaterThan,
}

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for WhileOp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "<" => Ok(WhileOp::LessThan),
            ">" => Ok(WhileOp::GreaterThan),
            other => Err(serde::de::Error::custom(format!(
                "unsupported operator '{other}' on while: — only strict \
                 comparisons '<' and '>' are accepted"
            ))),
        }
    }
}

/// Continuous lifecycle gate on another signal's value.
///
/// ```yaml
/// while:
///   ref: link_state
///   op: ">"
///   value: 0
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "config",
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct WhileClause {
    #[cfg_attr(feature = "config", serde(rename = "ref"))]
    pub ref_id: String,
    pub op: WhileOp,
    pub value: f64,
}

/// Open / close debounce windows applied to a [`WhileClause`] transition.
///
/// `open` debounces a `false → true` transition; `close` debounces
/// `true → false`. Either may be omitted (treated as `0s`). Validation
/// requires `delay:` to be paired with `while:`; standalone `delay:`
/// rejects at normalize time.
///
/// Durations are parsed from human-readable strings (`"250ms"`, `"5s"`)
/// at YAML deserialization time, so the runtime never re-parses.
///
/// `close` accepts two shapes for backward compatibility:
/// - `close: 5s` — legacy duration shorthand (carries no extra fields).
/// - `close: { duration: 5s, snap_to: 1, stale_marker: false }` — extended
///   form for [`PROMETHEUS_STALE_NAN`](crate::encoder::remote_write::PROMETHEUS_STALE_NAN)
///   recovery control on `running → paused`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct DelayClause {
    #[cfg_attr(
        feature = "config",
        serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "delay_duration_opt"
        )
    )]
    pub open: Option<std::time::Duration>,
    #[cfg_attr(
        feature = "config",
        serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "delay_duration_opt"
        )
    )]
    pub close: Option<std::time::Duration>,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub close_stale_marker: Option<bool>,
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub close_snap_to: Option<f64>,
}

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for DelayClause {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CloseStruct {
            #[serde(default)]
            duration: Option<String>,
            #[serde(default)]
            snap_to: Option<f64>,
            #[serde(default)]
            stale_marker: Option<bool>,
        }

        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum CloseShape {
            Duration(String),
            Extended(CloseStruct),
        }

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(default)]
            open: Option<String>,
            #[serde(default)]
            close: Option<CloseShape>,
        }

        let raw = Raw::deserialize(deserializer)?;

        let open = match raw.open {
            Some(s) => Some(
                crate::config::validate::parse_delay_duration(&s)
                    .map_err(serde::de::Error::custom)?,
            ),
            None => None,
        };

        let (close, close_snap_to, close_stale_marker) = match raw.close {
            None => (None, None, None),
            Some(CloseShape::Duration(s)) => {
                let dur = crate::config::validate::parse_delay_duration(&s)
                    .map_err(serde::de::Error::custom)?;
                (Some(dur), None, None)
            }
            Some(CloseShape::Extended(ext)) => {
                let dur = match ext.duration {
                    Some(s) => Some(
                        crate::config::validate::parse_delay_duration(&s)
                            .map_err(serde::de::Error::custom)?,
                    ),
                    None => None,
                };
                (dur, ext.snap_to, ext.stale_marker)
            }
        };

        Ok(DelayClause {
            open,
            close,
            close_stale_marker,
            close_snap_to,
        })
    }
}

#[cfg(feature = "config")]
mod delay_duration_opt {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => serializer.serialize_str(&format_duration(*d)),
            None => serializer.serialize_none(),
        }
    }

    fn format_duration(d: Duration) -> String {
        let total_ms = d.as_millis();
        if total_ms == 0 {
            return "0ms".to_string();
        }
        if total_ms.is_multiple_of(3_600_000) {
            return format!("{}h", total_ms / 3_600_000);
        }
        if total_ms.is_multiple_of(60_000) {
            return format!("{}m", total_ms / 60_000);
        }
        if total_ms.is_multiple_of(1_000) {
            return format!("{}s", total_ms / 1_000);
        }
        format!("{total_ms}ms")
    }
}

/// Discriminator labeling an edge or diagnostic as `after:` vs `while:`.
///
/// Used as the edge label in the dependency graph and as a field on
/// [`compile_after::CompileAfterError`] variants that span both clause
/// families. `#[non_exhaustive]` so future clause types extend without a
/// breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
#[non_exhaustive]
pub enum ClauseKind {
    After,
    While,
}

impl std::fmt::Display for ClauseKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ClauseKind::After => "after",
            ClauseKind::While => "while",
        })
    }
}
