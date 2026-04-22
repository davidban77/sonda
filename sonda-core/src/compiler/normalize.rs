//! Defaults resolution and entry normalization for v2 scenario files.
//!
//! This module implements **Phase 2** of the v2 compilation pipeline: it takes
//! a [`ScenarioFile`] produced by [`super::parse::parse`] and flattens the
//! shared `defaults:` block into each entry, applying the documented
//! precedence rules.
//!
//! # Precedence (entry-level fields)
//!
//! For every entry, the resolver picks the first non-`None` value in this
//! order:
//!
//! 1. the value set on the entry,
//! 2. the value set under the file-level `defaults:` block,
//! 3. a built-in fallback for `encoder` and `sink` (see below).
//!
//! The higher-precedence levels (pack `shared_labels`, pack per-metric,
//! override labels, CLI flags) are not applied here; they belong to later
//! compilation phases.
//!
//! # Built-in fallbacks
//!
//! When neither the entry nor `defaults:` sets an encoder, the normalizer
//! picks one based on the entry's `signal_type`:
//!
//! | Signal type | Default encoder  |
//! |-------------|------------------|
//! | `metrics`   | `prometheus_text`|
//! | `histogram` | `prometheus_text`|
//! | `summary`   | `prometheus_text`|
//! | `logs`      | `json_lines`     |
//!
//! The built-in fallback for `sink` is always `stdout`.
//!
//! # Labels merge (inline vs. pack entries)
//!
//! Inline entries (those with their own `generator` / `log_generator`) have
//! no downstream label-composition steps, so their labels are merged eagerly
//! here: `defaults.labels ∪ entry.labels`, entry keys winning on collision.
//! If either side is `None` the merged map equals the other side; if both
//! are `None` the entry keeps `None`.
//!
//! Pack entries (`pack: some_name`) behave differently. Per spec §2.2 the
//! final label map for a pack metric is composed at eight distinct
//! precedence levels, ordered **low → high** (lowest number is applied
//! first, each subsequent level overwrites on key collision):
//!
//! 1. Sonda built-in defaults (no label default today — listed for parity
//!    with the non-label precedence chain)
//! 2. `defaults.labels`
//! 3. pack definition's top-level fields (shared rate/job, etc.)
//! 4. pack `shared_labels`
//! 5. pack per-metric labels
//! 6. pack entry-level labels (the entry under `scenarios:`)
//! 7. override-level labels (`entry.overrides[metric].labels`)
//! 8. CLI flags (applied at runtime, PR 7 scope)
//!
//! Eagerly merging levels 2 and 6 into a single map (as we do for inline
//! entries) would collapse those two layers, making it impossible for pack
//! expansion to interleave levels 3–5 at their correct precedence. A pack
//! `shared_labels: { job: snmp }` (level 4) must be able to override a
//! `defaults.labels: { job: web }` (level 2) while still being overridden
//! by an entry `labels: { job: api }` (level 6) — which requires preserving
//! the boundary.
//!
//! Therefore, for pack entries, [`NormalizedEntry::labels`] carries **only
//! the entry's own labels** (unchanged, including `None`) at level 6. The
//! file-level `defaults.labels` map is surfaced separately on
//! [`NormalizedFile::defaults_labels`] so pack expansion (Phase 3) can
//! apply it at precedence level 2.
//!
//! # Pack entries (other fields)
//!
//! Pack entries still inherit `rate`, `duration`, `encoder`, and `sink`
//! eagerly — those fields do not participate in pack-level composition, so
//! there is no benefit to deferring them.
//!
//! # Validation
//!
//! After merging, every normalized entry must have a concrete `rate`
//! value; missing `rate` is a compile-time error identifying the entry by
//! index plus its `name`, falling back to `id`, then to `pack`, then to
//! `<unnamed>` when none of those are set. Range checks on `rate` (must be
//! `> 0`) are deferred to the existing validator invoked during
//! `prepare_entries` in Phase 5.

use std::collections::BTreeMap;

use super::{AfterClause, Defaults, Entry, ScenarioFile};
use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
};
use crate::encoder::EncoderConfig;
use crate::generator::{GeneratorConfig, LogGeneratorConfig};
use crate::packs::MetricOverride;
use crate::sink::SinkConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced during defaults resolution.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NormalizeError {
    /// An entry has no `rate` either inline or via the `defaults:` block.
    ///
    /// The offending entry is identified by its zero-based index and, when
    /// available, its `name` or `id` for human-readable diagnostics.
    #[error("entry {index} ({label}): missing required field 'rate' (set it on the entry or in defaults:)")]
    MissingRate {
        /// Zero-based index of the entry in the parsed `scenarios` list.
        index: usize,
        /// Human-readable label: the entry's `name`, falling back to `id`,
        /// falling back to `pack`, falling back to `<unnamed>`.
        label: String,
    },
}

// ---------------------------------------------------------------------------
// Normalized representation
// ---------------------------------------------------------------------------

/// A v2 scenario file with all defaults applied.
///
/// This is the output of [`normalize`]. The `defaults:` block has been
/// flattened into each [`NormalizedEntry`] for fields that do not participate
/// in pack-level composition (`rate`, `duration`, `encoder`, `sink`). The
/// `defaults.labels` map is handled specially: see the module docs for the
/// full precedence chain.
///
/// # Invariants
///
/// - Every entry has a concrete `rate`, `encoder`, and `sink`.
/// - For **inline** entries, [`NormalizedEntry::labels`] contains the merged
///   result of `defaults.labels` and the entry's own labels (entry wins on
///   conflict).
/// - For **pack** entries, [`NormalizedEntry::labels`] contains only the
///   entry's own labels (unchanged, possibly `None`). The file-level
///   `defaults.labels` is carried on [`Self::defaults_labels`] for Phase 3
///   pack expansion to apply at the correct precedence slot.
/// - Pack entries retain their `pack` and `overrides` fields untouched —
///   pack expansion is performed in a later phase.
/// - `after` clauses, `phase_offset`, and `clock_group` are carried through
///   unchanged.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct NormalizedFile {
    /// Schema version. Always `2` after normalization.
    pub version: u32,
    /// The file-level `defaults.labels` map, carried forward verbatim for
    /// later compilation phases to apply at the correct precedence slot.
    ///
    /// For pack entries this is the level-2 label layer (per spec §2.2) that
    /// pack expansion must interleave with pack `shared_labels` (level 4),
    /// pack per-metric labels (level 5), and entry-level labels (level 6).
    /// For inline entries the merge is already baked into
    /// [`NormalizedEntry::labels`] so this map is redundant — but carrying
    /// it here for both cases keeps the type uniform.
    ///
    /// `None` when the source file had no `defaults:` block or when
    /// `defaults.labels` was omitted or empty.
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub defaults_labels: Option<BTreeMap<String, String>>,
    /// All entries with defaults applied, in the order they appeared in
    /// the source file.
    pub entries: Vec<NormalizedEntry>,
}

/// A single scenario entry with all defaults resolved.
///
/// Fields that could inherit from `defaults:` are now guaranteed to hold a
/// concrete value (`rate`, `encoder`, `sink`). Fields that do not inherit
/// (pack references, histogram/summary configuration, `after` clauses)
/// are carried through unchanged.
///
/// This type is deliberately close in shape to [`Entry`] so that later
/// compilation phases can walk the same field set without a translation
/// step. The invariants above make the "missing rate/encoder/sink" states
/// unrepresentable after normalization.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct NormalizedEntry {
    /// Unique identifier for causal dependency references (`after.ref`).
    pub id: Option<String>,
    /// Signal type: `"metrics"`, `"logs"`, `"histogram"`, or `"summary"`.
    pub signal_type: String,
    /// Metric or scenario name. `None` for pack-backed entries.
    pub name: Option<String>,
    /// Event rate in events per second. Always set after normalization.
    pub rate: f64,
    /// Total run duration (e.g. `"30s"`, `"5m"`). `None` means "run until
    /// stopped" and is preserved through normalization.
    pub duration: Option<String>,
    /// Value generator configuration (metrics signals only).
    pub generator: Option<GeneratorConfig>,
    /// Log generator configuration (logs signals only).
    pub log_generator: Option<LogGeneratorConfig>,
    /// Static labels attached to every emitted event.
    ///
    /// For **inline** entries this is the merged map of `defaults.labels`
    /// and the entry's own labels, with entry keys winning on conflict.
    ///
    /// For **pack** entries this is the entry's own labels **unchanged**
    /// (possibly `None`). The file-level `defaults.labels` is NOT merged in
    /// — it is carried separately on [`NormalizedFile::defaults_labels`] so
    /// pack expansion can apply it at the correct precedence level. See the
    /// module docs for the full rationale.
    pub labels: Option<BTreeMap<String, String>>,
    /// Dynamic (rotating) label configurations.
    pub dynamic_labels: Option<Vec<DynamicLabelConfig>>,
    /// Encoder configuration. Always set after normalization.
    pub encoder: EncoderConfig,
    /// Sink configuration. Always set after normalization.
    pub sink: SinkConfig,
    /// Jitter amplitude applied to generated values.
    pub jitter: Option<f64>,
    /// Deterministic seed for jitter RNG.
    pub jitter_seed: Option<u64>,
    /// Recurring silent-period configuration.
    pub gaps: Option<GapConfig>,
    /// Recurring high-rate burst configuration.
    pub bursts: Option<BurstConfig>,
    /// Cardinality spike configurations.
    pub cardinality_spikes: Option<Vec<CardinalitySpikeConfig>>,
    /// Phase offset for staggered start within a clock group.
    pub phase_offset: Option<String>,
    /// Clock group for coordinated timing across entries.
    pub clock_group: Option<String>,
    /// Causal dependency on another signal's value.
    pub after: Option<AfterClause>,

    // -- Pack-backed entry fields (carried through untouched) --
    /// Pack name or file path. Mutually exclusive with `generator`.
    pub pack: Option<String>,
    /// Per-metric overrides within the referenced pack.
    pub overrides: Option<BTreeMap<String, MetricOverride>>,

    // -- Histogram / summary fields (carried through untouched) --
    /// Distribution model for histogram or summary observations.
    pub distribution: Option<DistributionConfig>,
    /// Histogram bucket boundaries (histogram only).
    pub buckets: Option<Vec<f64>>,
    /// Summary quantile boundaries (summary only).
    pub quantiles: Option<Vec<f64>>,
    /// Number of observations sampled per tick.
    pub observations_per_tick: Option<u32>,
    /// Linear drift applied to the distribution mean each second.
    pub mean_shift_per_sec: Option<f64>,
    /// Deterministic seed for histogram/summary sampling.
    pub seed: Option<u64>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolve the `defaults:` block into every entry of a parsed v2 scenario
/// file.
///
/// The returned [`NormalizedFile`] contains a [`NormalizedEntry`] per input
/// entry with the following fields materialized:
///
/// - `rate` inherits from `defaults.rate` when the entry omits it; missing
///   on both is an error (see [`NormalizeError::MissingRate`]).
/// - `duration` inherits from `defaults.duration`; absence is preserved as
///   "run until stopped".
/// - `encoder` inherits from `defaults.encoder`, otherwise defaults to
///   `prometheus_text` for metrics/histogram/summary and `json_lines` for
///   logs.
/// - `sink` inherits from `defaults.sink`, otherwise defaults to `stdout`.
/// - `labels` — **inline entries only**: the union of `defaults.labels` and
///   the entry's labels (entry wins on conflict). **Pack entries** keep
///   their own labels unchanged; `defaults.labels` is surfaced on
///   [`NormalizedFile::defaults_labels`] for Phase 3 pack expansion.
///
/// All other fields (pack info, histogram parameters, `after` clause,
/// `phase_offset`, `clock_group`, jitter, gaps, bursts, cardinality spikes,
/// dynamic labels, etc.) are carried through untouched.
///
/// # Errors
///
/// Returns [`NormalizeError::MissingRate`] when an entry has no `rate`
/// defined inline and the `defaults:` block does not supply one either.
/// The error message identifies the entry by index and, when available,
/// its `name`, `id`, or `pack` reference.
pub fn normalize(file: ScenarioFile) -> Result<NormalizedFile, NormalizeError> {
    let defaults = file.defaults;
    let defaults_labels = defaults
        .as_ref()
        .and_then(|d| d.labels.as_ref())
        .filter(|m| !m.is_empty())
        .cloned();
    let mut entries = Vec::with_capacity(file.scenarios.len());

    for (index, entry) in file.scenarios.into_iter().enumerate() {
        entries.push(normalize_entry(entry, index, defaults.as_ref())?);
    }

    Ok(NormalizedFile {
        version: file.version,
        defaults_labels,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Apply defaults to a single entry and validate required fields.
///
/// For inline entries, `defaults.labels` is merged into the entry's labels
/// eagerly. For pack entries, the merge is deferred to Phase 3 pack
/// expansion so the correct §2.2 precedence chain can be applied; see the
/// module docs for the full rationale.
fn normalize_entry(
    entry: Entry,
    index: usize,
    defaults: Option<&Defaults>,
) -> Result<NormalizedEntry, NormalizeError> {
    let rate = resolve_rate(&entry, defaults, index)?;
    let duration = entry
        .duration
        .or_else(|| defaults.and_then(|d| d.duration.clone()));
    let encoder = entry
        .encoder
        .or_else(|| defaults.and_then(|d| d.encoder.clone()))
        .unwrap_or_else(|| default_encoder_for(&entry.signal_type));
    let sink = entry
        .sink
        .or_else(|| defaults.and_then(|d| d.sink.clone()))
        .unwrap_or_else(default_sink);
    let labels = if entry.pack.is_some() {
        // Pack entries defer label composition to Phase 3 expansion; keep
        // only the entry's own labels here so pack shared/per-metric labels
        // can be inserted between defaults and entry levels (spec §2.2).
        entry.labels
    } else {
        merge_labels(defaults.and_then(|d| d.labels.as_ref()), entry.labels)
    };

    Ok(NormalizedEntry {
        id: entry.id,
        signal_type: entry.signal_type,
        name: entry.name,
        rate,
        duration,
        generator: entry.generator,
        log_generator: entry.log_generator,
        labels,
        dynamic_labels: entry.dynamic_labels,
        encoder,
        sink,
        jitter: entry.jitter,
        jitter_seed: entry.jitter_seed,
        gaps: entry.gaps,
        bursts: entry.bursts,
        cardinality_spikes: entry.cardinality_spikes,
        phase_offset: entry.phase_offset,
        clock_group: entry.clock_group,
        after: entry.after,
        pack: entry.pack,
        overrides: entry.overrides,
        distribution: entry.distribution,
        buckets: entry.buckets,
        quantiles: entry.quantiles,
        observations_per_tick: entry.observations_per_tick,
        mean_shift_per_sec: entry.mean_shift_per_sec,
        seed: entry.seed,
    })
}

/// Resolve `rate` from the entry or defaults, producing a diagnostic error
/// when neither is set.
fn resolve_rate(
    entry: &Entry,
    defaults: Option<&Defaults>,
    index: usize,
) -> Result<f64, NormalizeError> {
    if let Some(rate) = entry.rate {
        return Ok(rate);
    }
    if let Some(rate) = defaults.and_then(|d| d.rate) {
        return Ok(rate);
    }
    Err(NormalizeError::MissingRate {
        index,
        label: entry_label(entry),
    })
}

/// Pick a human-readable label for an entry for use in error messages.
///
/// Preference order: `name` → `id` → `pack` → `<unnamed>`.
fn entry_label(entry: &Entry) -> String {
    entry
        .name
        .clone()
        .or_else(|| entry.id.clone())
        .or_else(|| entry.pack.clone())
        .unwrap_or_else(|| "<unnamed>".to_string())
}

/// Return the built-in encoder default for a given signal type.
///
/// Unknown signal types fall through to `prometheus_text` as a neutral
/// default. Parse-time validation rejects unknown signal types, so this
/// branch is unreachable in practice; we still return a value rather than
/// panic to keep the function total.
fn default_encoder_for(signal_type: &str) -> EncoderConfig {
    match signal_type {
        "logs" => EncoderConfig::JsonLines { precision: None },
        _ => EncoderConfig::PrometheusText { precision: None },
    }
}

/// Return the built-in sink default (`stdout`).
fn default_sink() -> SinkConfig {
    SinkConfig::Stdout
}

/// Merge a file-level labels map with an entry-level labels map.
///
/// Entry-level keys win on conflict. If either side is `None`, the other
/// side is returned unchanged. If both sides are `None`, returns `None`.
fn merge_labels(
    defaults_labels: Option<&BTreeMap<String, String>>,
    entry_labels: Option<BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    match (defaults_labels, entry_labels) {
        (None, None) => None,
        (Some(d), None) => Some(d.clone()),
        (None, Some(e)) => Some(e),
        (Some(d), Some(e)) => {
            let mut merged = d.clone();
            for (k, v) in e {
                merged.insert(k, v);
            }
            Some(merged)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::parse::parse;
    use super::*;

    // ======================================================================
    // Helpers
    // ======================================================================

    fn normalize_yaml(yaml: &str) -> Result<NormalizedFile, NormalizeError> {
        let parsed = parse(yaml).expect("parse must succeed in normalization tests");
        normalize(parsed)
    }

    fn is_prometheus_text(encoder: &EncoderConfig) -> bool {
        matches!(encoder, EncoderConfig::PrometheusText { .. })
    }

    fn is_json_lines(encoder: &EncoderConfig) -> bool {
        matches!(encoder, EncoderConfig::JsonLines { .. })
    }

    fn is_stdout(sink: &SinkConfig) -> bool {
        matches!(sink, SinkConfig::Stdout)
    }

    // ======================================================================
    // Defaults inheritance
    // ======================================================================

    #[test]
    fn entry_inherits_rate_and_duration_from_defaults() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let entry = &file.entries[0];
        assert!((entry.rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(entry.duration.as_deref(), Some("5m"));
    }

    #[test]
    fn entry_rate_overrides_defaults_rate() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 10
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        assert!((file.entries[0].rate - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn entry_duration_overrides_defaults_duration() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - signal_type: metrics
    name: cpu
    duration: 30s
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        assert_eq!(file.entries[0].duration.as_deref(), Some("30s"));
    }

    #[test]
    fn entry_inherits_encoder_and_sink_from_defaults() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  encoder: { type: influx_lp }
  sink: { type: file, path: /tmp/out.txt }
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let entry = &file.entries[0];
        assert!(matches!(
            entry.encoder,
            EncoderConfig::InfluxLineProtocol { .. }
        ));
        assert!(matches!(entry.sink, SinkConfig::File { .. }));
    }

    #[test]
    fn entry_encoder_overrides_defaults_encoder() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  encoder: { type: influx_lp }
scenarios:
  - signal_type: metrics
    name: cpu
    encoder: { type: prometheus_text }
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        assert!(is_prometheus_text(&file.entries[0].encoder));
    }

    // ======================================================================
    // Built-in defaults
    // ======================================================================

    /// Expected built-in encoder for a signal type when defaults do not
    /// supply one. Lets the parametrized test below classify the encoder
    /// without introspecting its internal fields.
    #[derive(Copy, Clone)]
    enum ExpectedEncoder {
        PrometheusText,
        JsonLines,
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::metrics(r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator: { type: constant, value: 42 }
"#, ExpectedEncoder::PrometheusText)]
    #[case::histogram(r#"
version: 2
scenarios:
  - signal_type: histogram
    name: http_latency
    rate: 1
    distribution: { type: exponential, rate: 10.0 }
    buckets: [0.1, 0.5, 1.0]
    observations_per_tick: 50
    seed: 1
"#, ExpectedEncoder::PrometheusText)]
    #[case::summary(r#"
version: 2
scenarios:
  - signal_type: summary
    name: rpc_latency
    rate: 1
    distribution: { type: normal, mean: 0.1, stddev: 0.02 }
    quantiles: [0.5, 0.9, 0.99]
    observations_per_tick: 50
    seed: 1
"#, ExpectedEncoder::PrometheusText)]
    #[case::logs(r#"
version: 2
scenarios:
  - signal_type: logs
    name: app_logs
    rate: 1
    log_generator:
      type: template
      templates:
        - message: "hello"
"#, ExpectedEncoder::JsonLines)]
    fn signal_type_picks_built_in_encoder_and_stdout_sink(
        #[case] yaml: &str,
        #[case] expected: ExpectedEncoder,
    ) {
        let file = normalize_yaml(yaml).expect("must normalize");
        let entry = &file.entries[0];
        match expected {
            ExpectedEncoder::PrometheusText => assert!(is_prometheus_text(&entry.encoder)),
            ExpectedEncoder::JsonLines => assert!(is_json_lines(&entry.encoder)),
        }
        assert!(is_stdout(&entry.sink));
    }

    // ======================================================================
    // Labels merge
    // ======================================================================

    #[test]
    fn labels_merge_entry_wins_on_conflict() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    device: rtr-edge-01
    region: us-west-2
scenarios:
  - signal_type: metrics
    name: cpu
    labels:
      region: us-east-1
      interface: Gi0/0/0
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let labels = file.entries[0]
            .labels
            .as_ref()
            .expect("merged labels must exist");
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("rtr-edge-01")
        );
        assert_eq!(
            labels.get("region").map(String::as_str),
            Some("us-east-1"),
            "entry value must win on conflict"
        );
        assert_eq!(labels.get("interface").map(String::as_str), Some("Gi0/0/0"));
    }

    #[test]
    fn labels_from_defaults_alone_are_preserved() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    env: staging
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let labels = file.entries[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels.get("env").map(String::as_str), Some("staging"));
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn entry_labels_preserved_when_defaults_has_no_labels() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
scenarios:
  - signal_type: metrics
    name: cpu
    labels:
      job: api
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let labels = file.entries[0].labels.as_ref().expect("labels must exist");
        assert_eq!(labels.get("job").map(String::as_str), Some("api"));
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn no_labels_anywhere_produces_none() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        assert!(file.entries[0].labels.is_none());
    }

    // ======================================================================
    // Missing rate error
    //
    // The `label` field in MissingRate follows a priority chain:
    //   name > id > pack name.
    // This table exercises each arm — inline entry with only a name,
    // pack entry with id (id wins over pack), pack entry with neither
    // (falls back to pack name).
    // ======================================================================

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::inline_uses_name(r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 1.0 }
"#, "cpu")]
    #[case::pack_prefers_id(r#"
version: 2
scenarios:
  - id: snmp_iface
    signal_type: metrics
    pack: telegraf_snmp_interface
"#, "snmp_iface")]
    #[case::pack_falls_back_to_pack_name(r#"
version: 2
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#, "telegraf_snmp_interface")]
    fn missing_rate_error_label_follows_priority_chain(
        #[case] yaml: &str,
        #[case] expected_label: &str,
    ) {
        let err = normalize_yaml(yaml).expect_err("missing rate must fail");
        match err {
            NormalizeError::MissingRate { index, label } => {
                assert_eq!(index, 0);
                assert_eq!(label, expected_label);
            }
        }
    }

    #[test]
    fn missing_rate_message_mentions_entry_and_hint() {
        let yaml = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: bare
    generator: { type: constant, value: 1.0 }
"#;
        let err = normalize_yaml(yaml).expect_err("missing rate must fail");
        let msg = err.to_string();
        assert!(msg.contains("entry 0"), "error should mention entry index");
        assert!(msg.contains("bare"), "error should mention entry name");
        assert!(msg.contains("rate"), "error should mention rate");
        assert!(
            msg.contains("defaults"),
            "error should hint at defaults block"
        );
    }

    // ======================================================================
    // Shorthand normalization
    // ======================================================================

    #[test]
    fn shorthand_single_signal_normalizes_through_wrapped_form() {
        let yaml = r#"
version: 2
name: cpu_usage
signal_type: metrics
rate: 5
generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize shorthand");
        assert_eq!(file.entries.len(), 1);
        let entry = &file.entries[0];
        assert!((entry.rate - 5.0).abs() < f64::EPSILON);
        assert_eq!(entry.name.as_deref(), Some("cpu_usage"));
        assert!(is_prometheus_text(&entry.encoder));
        assert!(is_stdout(&entry.sink));
    }

    #[test]
    fn shorthand_logs_signal_picks_json_lines_default() {
        let yaml = r#"
version: 2
name: app_logs
signal_type: logs
rate: 2
log_generator:
  type: template
  templates:
    - message: "hello"
"#;
        let file = normalize_yaml(yaml).expect("must normalize logs shorthand");
        assert!(is_json_lines(&file.entries[0].encoder));
    }

    // ======================================================================
    // Pack entry normalization
    // ======================================================================

    #[test]
    fn pack_entry_inherits_defaults_but_defers_label_merge() {
        // Spec §2.2 reserves precedence levels 3–5 (pack shared fields,
        // pack shared_labels, pack per-metric labels) for Phase 3 expansion
        // between defaults (level 2) and entry labels (level 6). Eagerly
        // merging here would collapse that chain. This test documents the
        // asymmetry: pack entries keep their own labels verbatim, while
        // `defaults.labels` rides on NormalizedFile::defaults_labels.
        //
        // Example from the user: defaults.labels = {job: web}, entry =
        // {labels: {device: rtr-01}, pack: mypack}. Phase 3 will expand the
        // pack's shared_labels (e.g. {job: snmp}) on top of defaults,
        // then apply entry labels on top — yielding {job: snmp, device: rtr-01}.
        // If we merged here the pack's job override would be unreachable.
        let yaml = r#"
version: 2
defaults:
  rate: 1
  duration: 10m
  encoder: { type: prometheus_text }
  sink: { type: stdout }
  labels:
    job: web
scenarios:
  - id: primary_uplink
    signal_type: metrics
    pack: mypack
    labels:
      device: rtr-01
    overrides:
      ifOperStatus:
        generator: { type: constant, value: 0.0 }
"#;
        let file = normalize_yaml(yaml).expect("must normalize pack entry");
        let entry = &file.entries[0];
        assert_eq!(entry.pack.as_deref(), Some("mypack"));
        assert!(
            entry.overrides.is_some(),
            "overrides must be carried through untouched"
        );
        assert!((entry.rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(entry.duration.as_deref(), Some("10m"));
        assert!(is_prometheus_text(&entry.encoder));
        assert!(is_stdout(&entry.sink));

        // Pack entry labels are NOT merged with defaults.labels.
        let labels = entry.labels.as_ref().expect("entry labels must exist");
        assert_eq!(labels.len(), 1, "only entry labels — defaults not merged");
        assert_eq!(labels.get("device").map(String::as_str), Some("rtr-01"));
        assert!(
            !labels.contains_key("job"),
            "defaults.labels must not leak into pack entry's labels"
        );

        // defaults.labels is preserved verbatim at the file level for
        // Phase 3 pack expansion to apply.
        let d = file
            .defaults_labels
            .as_ref()
            .expect("defaults_labels must be surfaced");
        assert_eq!(d.get("job").map(String::as_str), Some("web"));
    }

    #[test]
    fn normalized_file_defaults_labels_matches_source() {
        // Present when defaults.labels is set and non-empty.
        let yaml_with = r#"
version: 2
defaults:
  rate: 1
  labels:
    env: prod
    region: us-east-1
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml_with).expect("must normalize");
        let d = file
            .defaults_labels
            .as_ref()
            .expect("defaults_labels must be Some when defaults.labels is set");
        assert_eq!(d.len(), 2);
        assert_eq!(d.get("env").map(String::as_str), Some("prod"));
        assert_eq!(d.get("region").map(String::as_str), Some("us-east-1"));

        // None when the file has no defaults block at all.
        let yaml_no_defaults = r#"
version: 2
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml_no_defaults).expect("must normalize");
        assert!(file.defaults_labels.is_none());

        // None when defaults exists but has no labels field.
        let yaml_no_labels = r#"
version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 42 }
"#;
        let file = normalize_yaml(yaml_no_labels).expect("must normalize");
        assert!(file.defaults_labels.is_none());
    }

    #[test]
    fn inline_and_pack_entries_compose_defaults_labels_asymmetrically() {
        // One file, two entries both "inheriting" defaults.labels. The
        // inline entry gets the eager merge; the pack entry does not.
        // defaults_labels must carry the source map verbatim for Phase 3.
        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    job: web
    region: us-east-1
scenarios:
  - signal_type: metrics
    name: cpu
    labels:
      host: node-01
    generator: { type: constant, value: 42 }

  - signal_type: metrics
    pack: mypack
    labels:
      device: rtr-01
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        assert_eq!(file.entries.len(), 2);

        // Inline entry: labels = defaults ∪ entry, entry wins on conflict.
        let inline = &file.entries[0];
        assert!(inline.pack.is_none());
        let inline_labels = inline.labels.as_ref().expect("inline labels must exist");
        assert_eq!(inline_labels.len(), 3, "defaults + entry merged");
        assert_eq!(inline_labels.get("job").map(String::as_str), Some("web"));
        assert_eq!(
            inline_labels.get("region").map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            inline_labels.get("host").map(String::as_str),
            Some("node-01")
        );

        // Pack entry: labels = entry's own labels only. No merge happened.
        let pack = &file.entries[1];
        assert_eq!(pack.pack.as_deref(), Some("mypack"));
        let pack_labels = pack.labels.as_ref().expect("pack entry labels must exist");
        assert_eq!(pack_labels.len(), 1, "only entry-level labels, no merge");
        assert_eq!(
            pack_labels.get("device").map(String::as_str),
            Some("rtr-01")
        );
        assert!(!pack_labels.contains_key("job"));
        assert!(!pack_labels.contains_key("region"));

        // File-level defaults_labels carries the source map verbatim.
        let d = file
            .defaults_labels
            .as_ref()
            .expect("defaults_labels must be Some");
        assert_eq!(d.len(), 2);
        assert_eq!(d.get("job").map(String::as_str), Some("web"));
        assert_eq!(d.get("region").map(String::as_str), Some("us-east-1"));
    }

    // ======================================================================
    // Multi-scenario mixed entries
    // ======================================================================

    #[test]
    fn multi_scenario_mixed_entries_all_normalize() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  duration: 5m
  encoder: { type: prometheus_text }
  sink: { type: stdout }
  labels:
    region: us-west-2
scenarios:
  - id: link_state
    signal_type: metrics
    name: interface_oper_state
    labels:
      interface: Gi0/0/0
      region: us-east-1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }

  - id: fast_metric
    signal_type: metrics
    name: cpu
    rate: 10
    generator: { type: constant, value: 42 }

  - signal_type: logs
    name: app_logs
    log_generator:
      type: template
      templates:
        - message: "hello"

  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-01
"#;
        let file = normalize_yaml(yaml).expect("must normalize multi-scenario");
        assert_eq!(file.entries.len(), 4);

        // Entry 0: inline metric, inherits rate/duration/encoder/sink,
        // labels merged with entry's region winning.
        let e0 = &file.entries[0];
        assert!((e0.rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(e0.duration.as_deref(), Some("5m"));
        assert!(is_prometheus_text(&e0.encoder));
        let labels0 = e0.labels.as_ref().expect("labels must exist");
        assert_eq!(labels0.get("region").map(String::as_str), Some("us-east-1"));
        assert_eq!(
            labels0.get("interface").map(String::as_str),
            Some("Gi0/0/0")
        );

        // Entry 1: rate override wins, inherits everything else.
        let e1 = &file.entries[1];
        assert!((e1.rate - 10.0).abs() < f64::EPSILON);
        assert_eq!(e1.duration.as_deref(), Some("5m"));
        let labels1 = e1.labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels1.get("region").map(String::as_str),
            Some("us-west-2"),
            "entry has no labels.region, defaults wins"
        );

        // Entry 2: logs, picks json_lines default (defaults.encoder is
        // prometheus_text but is overridden for logs? No — defaults.encoder
        // is explicitly set in this file, so every entry inherits it, even
        // logs. This is consistent with precedence: defaults wins over
        // built-in, even when the built-in would be signal-type-aware.
        let e2 = &file.entries[2];
        assert!(
            is_prometheus_text(&e2.encoder),
            "explicit defaults.encoder applies to all entries including logs"
        );

        // Entry 3: pack entry, carries through pack field but does NOT
        // merge defaults.labels (see module docs on the asymmetry).
        let e3 = &file.entries[3];
        assert_eq!(e3.pack.as_deref(), Some("telegraf_snmp_interface"));
        let labels3 = e3.labels.as_ref().expect("labels must exist");
        assert_eq!(labels3.len(), 1, "only entry-level labels on pack entry");
        assert_eq!(labels3.get("device").map(String::as_str), Some("rtr-01"));
        assert!(!labels3.contains_key("region"));

        // defaults.labels still travels with the file for Phase 3 to apply.
        let d = file
            .defaults_labels
            .as_ref()
            .expect("defaults_labels must be Some");
        assert_eq!(d.get("region").map(String::as_str), Some("us-west-2"));
    }

    // ======================================================================
    // Fields carried through untouched
    // ======================================================================

    #[test]
    fn after_clause_and_timing_fields_preserved() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
scenarios:
  - id: src
    signal_type: metrics
    name: source
    generator: { type: constant, value: 100.0 }

  - signal_type: metrics
    name: dependent
    phase_offset: 5s
    clock_group: group_a
    generator: { type: constant, value: 1.0 }
    after:
      ref: src
      op: ">"
      value: 50.0
      delay: 2s
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let dep = &file.entries[1];
        assert_eq!(dep.phase_offset.as_deref(), Some("5s"));
        assert_eq!(dep.clock_group.as_deref(), Some("group_a"));
        let after = dep.after.as_ref().expect("after must be preserved");
        assert_eq!(after.ref_id, "src");
        assert_eq!(after.delay.as_deref(), Some("2s"));
    }

    #[test]
    fn histogram_fields_preserved() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
scenarios:
  - signal_type: histogram
    name: latency
    distribution: { type: exponential, rate: 10.0 }
    buckets: [0.1, 0.5, 1.0]
    observations_per_tick: 100
    mean_shift_per_sec: 0.01
    seed: 42
"#;
        let file = normalize_yaml(yaml).expect("must normalize");
        let entry = &file.entries[0];
        assert!(entry.distribution.is_some());
        assert_eq!(entry.buckets.as_ref().map(Vec::len), Some(3));
        assert_eq!(entry.observations_per_tick, Some(100));
        assert_eq!(entry.mean_shift_per_sec, Some(0.01));
        assert_eq!(entry.seed, Some(42));
    }

    // ======================================================================
    // Contract tests
    // ======================================================================

    #[test]
    fn normalize_error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NormalizeError>();
    }

    #[test]
    fn normalized_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NormalizedFile>();
        assert_send_sync::<NormalizedEntry>();
    }

    // ======================================================================
    // Empty scenarios
    // ======================================================================

    #[test]
    fn empty_scenarios_list_normalizes_to_empty_entries() {
        let yaml = r#"
version: 2
scenarios: []
"#;
        let file = normalize_yaml(yaml).expect("must normalize empty list");
        assert_eq!(file.version, 2);
        assert!(file.entries.is_empty());
    }

    // ======================================================================
    // helper unit tests (internal)
    // ======================================================================

    #[test]
    fn merge_labels_both_none_returns_none() {
        assert!(merge_labels(None, None).is_none());
    }

    #[test]
    fn merge_labels_only_defaults_returns_defaults_clone() {
        let mut d = BTreeMap::new();
        d.insert("a".to_string(), "1".to_string());
        let merged = merge_labels(Some(&d), None).expect("must return map");
        assert_eq!(merged.get("a").map(String::as_str), Some("1"));
    }

    #[test]
    fn merge_labels_only_entry_returns_entry() {
        let mut e = BTreeMap::new();
        e.insert("b".to_string(), "2".to_string());
        let merged = merge_labels(None, Some(e)).expect("must return map");
        assert_eq!(merged.get("b").map(String::as_str), Some("2"));
    }

    #[test]
    fn merge_labels_entry_overrides_defaults_on_conflict() {
        let mut d = BTreeMap::new();
        d.insert("k".to_string(), "from_defaults".to_string());
        let mut e = BTreeMap::new();
        e.insert("k".to_string(), "from_entry".to_string());
        let merged = merge_labels(Some(&d), Some(e)).expect("must return map");
        assert_eq!(merged.get("k").map(String::as_str), Some("from_entry"));
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::metrics("metrics",     ExpectedEncoder::PrometheusText)]
    #[case::histogram("histogram", ExpectedEncoder::PrometheusText)]
    #[case::summary("summary",     ExpectedEncoder::PrometheusText)]
    #[case::logs("logs",           ExpectedEncoder::JsonLines)]
    fn default_encoder_per_signal_type(
        #[case] signal_type: &str,
        #[case] expected: ExpectedEncoder,
    ) {
        let encoder = default_encoder_for(signal_type);
        match expected {
            ExpectedEncoder::PrometheusText => {
                assert!(matches!(encoder, EncoderConfig::PrometheusText { .. }))
            }
            ExpectedEncoder::JsonLines => {
                assert!(matches!(encoder, EncoderConfig::JsonLines { .. }))
            }
        }
    }

    #[test]
    fn default_sink_is_stdout() {
        assert!(matches!(default_sink(), SinkConfig::Stdout));
    }
}
