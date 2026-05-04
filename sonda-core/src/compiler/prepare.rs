//! Translation boundary from the v2 compiler output to the existing runtime's
//! input shape.
//!
//! This module is **Phase 6** of the v2 compilation pipeline (the execution
//! plan's "Prepare Entries" step). It consumes a [`CompiledFile`] produced by
//! [`compile_after`][crate::compiler::compile_after::compile_after] and
//! produces a `Vec<ScenarioEntry>` — the exact input shape that the existing
//! [`prepare_entries`][crate::schedule::launch::prepare_entries] function
//! already understands.
//!
//! # Why a dedicated module
//!
//! The runtime's [`ScenarioEntry`] was the v1 launch input and cannot be
//! changed without breaking the scheduler contract. The compiler's
//! [`CompiledEntry`] is a forward-compatible shape that can carry fields the
//! runtime does not yet consume. Keeping the translation in its own module
//! gives us:
//!
//! - A single obvious place to update when either shape evolves.
//! - A thin, allocation-conservative one-shot conversion (runs once at launch
//!   time, not per tick).
//! - Typed errors for the narrow set of "shape invariant broken" cases that
//!   only become visible at the dispatch boundary (e.g. unknown `signal_type`
//!   strings, missing per-variant required fields).
//!
//! The translator does **not** parse durations — `phase_offset` is passed
//! through verbatim so
//! [`prepare_entries`][crate::schedule::launch::prepare_entries] remains the
//! sole `parse_phase_offset` caller.

use std::collections::{BTreeMap, HashMap};

use crate::compiler::compile_after::{CompiledEntry, CompiledFile};
use crate::config::{
    BaseScheduleConfig, HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by [`prepare`].
///
/// Every variant carries enough context to identify the offending
/// [`CompiledEntry`] — either its user-provided `id` or, when the id is
/// absent, its `name`. Preferring `id` matches the convention used by
/// [`CompileAfterError`][crate::compiler::compile_after::CompileAfterError],
/// so diagnostics chain cleanly when both phases need to report on the same
/// entry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrepareError {
    /// The entry's `signal_type` was not one of the four recognized variants
    /// (`"metrics"`, `"logs"`, `"histogram"`, `"summary"`).
    ///
    /// The parser already rejects unknown signal types, but this variant
    /// keeps the translator self-contained: callers that construct a
    /// [`CompiledFile`] in code without going through
    /// [`parse`][crate::compiler::parse::parse] still get a proper error
    /// instead of a `match` panic.
    #[error("entry '{entry_label}': unknown signal_type '{signal_type}'")]
    UnknownSignalType {
        /// The entry's id (or name when id is absent).
        entry_label: String,
        /// The unrecognized signal type string as it appeared in the compiled entry.
        signal_type: String,
    },

    /// A metrics entry had no `generator` field set.
    ///
    /// `signal_type: metrics` is the only variant that reads `generator`
    /// from the compiled entry. When the YAML path is used the parser
    /// rejects a metrics entry missing `generator` at parse-time; reaching
    /// this variant therefore implies the [`CompiledFile`] was built in
    /// code rather than through [`parse`][crate::compiler::parse::parse].
    #[error("entry '{entry_label}' (signal_type: metrics): missing required field 'generator'")]
    MissingGenerator {
        /// The entry's id (or name when id is absent).
        entry_label: String,
    },

    /// A logs entry had no `log_generator` field set.
    ///
    /// `signal_type: logs` is the only variant that reads `log_generator`
    /// from the compiled entry. When the YAML path is used the parser
    /// rejects a logs entry missing `log_generator` at parse-time;
    /// reaching this variant therefore implies the [`CompiledFile`] was
    /// built in code rather than through
    /// [`parse`][crate::compiler::parse::parse].
    #[error("entry '{entry_label}' (signal_type: logs): missing required field 'log_generator'")]
    MissingLogGenerator {
        /// The entry's id (or name when id is absent).
        entry_label: String,
    },

    /// A histogram or summary entry had no `distribution` field set.
    ///
    /// `signal_type: histogram` and `signal_type: summary` both read
    /// `distribution` from the compiled entry. When the YAML path is used
    /// the parser rejects either shape missing `distribution` at
    /// parse-time; reaching this variant therefore implies the
    /// [`CompiledFile`] was built in code rather than through
    /// [`parse`][crate::compiler::parse::parse].
    #[error(
        "entry '{entry_label}' (signal_type: {signal_type}): missing required field 'distribution'"
    )]
    MissingDistribution {
        /// The entry's id (or name when id is absent).
        entry_label: String,
        /// The signal type that requires a distribution (`"histogram"` or `"summary"`).
        signal_type: String,
    },

    /// The compiled file's `version` was not `2`.
    ///
    /// Defense-in-depth against programmatic callers that construct a
    /// [`CompiledFile`] in code with a non-v2 version. Going through
    /// [`parse`][crate::compiler::parse::parse] already pins the version
    /// at parse-time, so this variant is unreachable via the YAML path.
    #[error("unsupported compiled file version: expected 2, got {version}")]
    UnsupportedVersion {
        /// The rejected version value as carried by the compiled file.
        version: u32,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Translate a [`CompiledFile`] into the runtime's
/// `Vec<ScenarioEntry>` input shape.
///
/// This is a **one-shot** conversion intended to run once at launch time.
/// Per-tick allocations are not affected — every [`ScenarioEntry`] produced
/// by this function takes the same hot path as if it had been constructed
/// directly by the v1 loader.
///
/// Field-by-field mapping lives in per-variant helpers; see the module
/// source for the exact wiring. Each helper consumes its [`CompiledEntry`]
/// by value so no deep clone is performed on the generator or label maps.
///
/// `CompiledEntry::id` is intentionally dropped during translation: its
/// job ended in Phase 4+5's dependency resolution, and [`ScenarioEntry`]
/// has no `id` field. Future observability wiring that wants to correlate
/// runtime back to v2 ids will need another channel (e.g. a side map keyed
/// on `name` + `clock_group`).
///
/// # Errors
///
/// Returns [`PrepareError`] on the first entry that fails translation:
///
/// - [`PrepareError::UnsupportedVersion`] if `file.version` is not `2`.
/// - [`PrepareError::UnknownSignalType`] when `signal_type` is not one of
///   `"metrics"`, `"logs"`, `"histogram"`, or `"summary"`.
/// - [`PrepareError::MissingGenerator`] for a metrics entry missing `generator`.
/// - [`PrepareError::MissingLogGenerator`] for a logs entry missing `log_generator`.
/// - [`PrepareError::MissingDistribution`] for a histogram/summary entry
///   missing `distribution`.
///
/// The short-circuiting semantics match the v2 compiler's other passes —
/// no partial output is returned on failure.
pub fn prepare(file: CompiledFile) -> Result<Vec<ScenarioEntry>, PrepareError> {
    let CompiledFile { version, entries } = file;
    if version != 2 {
        return Err(PrepareError::UnsupportedVersion { version });
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        out.push(translate_entry(entry)?);
    }
    Ok(out)
}

/// Translate a single [`CompiledEntry`] into the matching [`ScenarioEntry`]
/// variant.
///
/// Exposed for callers that want to fan out translation themselves (e.g.
/// partial runtime wiring in tests). Most callers should use [`prepare`].
///
/// # Errors
///
/// See [`prepare`] for the full error semantics.
pub fn translate_entry(entry: CompiledEntry) -> Result<ScenarioEntry, PrepareError> {
    match entry.signal_type.as_str() {
        "metrics" => metrics_entry(entry).map(ScenarioEntry::Metrics),
        "logs" => logs_entry(entry).map(ScenarioEntry::Logs),
        "histogram" => histogram_entry(entry).map(ScenarioEntry::Histogram),
        "summary" => summary_entry(entry).map(ScenarioEntry::Summary),
        _ => Err(PrepareError::UnknownSignalType {
            entry_label: describe(&entry),
            signal_type: entry.signal_type,
        }),
    }
}

// ---------------------------------------------------------------------------
// Per-variant helpers
// ---------------------------------------------------------------------------

/// Produce a human-readable label for a [`CompiledEntry`] — prefers the
/// explicit `id`, falls back to `name`.
fn describe(entry: &CompiledEntry) -> String {
    entry.id.clone().unwrap_or_else(|| entry.name.clone())
}

/// Build a [`BaseScheduleConfig`] from the shared fields of a
/// [`CompiledEntry`].
///
/// The only non-trivial conversion is `labels: Option<BTreeMap<...>>` →
/// `Option<HashMap<...>>`. The runtime uses `HashMap` internally but the
/// compiler operates on `BTreeMap` for deterministic iteration — this
/// one-shot conversion is the only place the shape changes.
///
/// The `clock_group_is_auto` provenance is mapped to
/// `Some(bool)` so v1-loaded entries (which never traverse this
/// translator) read `None` and render without the `(auto)` suffix.
fn build_base(entry: &mut CompiledEntry) -> BaseScheduleConfig {
    let labels = entry.labels.take().map(btree_to_hash);

    let clock_group = entry.clock_group.take();
    let clock_group_is_auto = clock_group.as_ref().map(|_| entry.clock_group_is_auto);

    BaseScheduleConfig {
        name: std::mem::take(&mut entry.name),
        rate: entry.rate,
        duration: entry.duration.take(),
        gaps: entry.gaps.take(),
        bursts: entry.bursts.take(),
        cardinality_spikes: entry.cardinality_spikes.take(),
        dynamic_labels: entry.dynamic_labels.take(),
        labels,
        sink: std::mem::replace(&mut entry.sink, crate::sink::SinkConfig::Stdout),
        phase_offset: entry.phase_offset.take(),
        clock_group,
        clock_group_is_auto,
        jitter: entry.jitter,
        jitter_seed: entry.jitter_seed,
        on_sink_error: entry.on_sink_error,
    }
}

/// Convert an ordered label map into an unordered one without reallocating
/// any keys or values.
fn btree_to_hash(m: BTreeMap<String, String>) -> HashMap<String, String> {
    let mut hm = HashMap::with_capacity(m.len());
    hm.extend(m);
    hm
}

/// Materialize a metrics [`ScenarioConfig`] from a compiled entry.
fn metrics_entry(mut entry: CompiledEntry) -> Result<ScenarioConfig, PrepareError> {
    let generator = entry
        .generator
        .take()
        .ok_or_else(|| PrepareError::MissingGenerator {
            entry_label: describe(&entry),
        })?;
    // `encoder` is non-`Option` on CompiledEntry (Phase 2 normalize filled it
    // in), so a mem::replace with a cheap placeholder is sufficient.
    let encoder = std::mem::replace(
        &mut entry.encoder,
        crate::encoder::EncoderConfig::PrometheusText { precision: None },
    );
    let base = build_base(&mut entry);
    Ok(ScenarioConfig {
        base,
        generator,
        encoder,
    })
}

/// Materialize a logs [`LogScenarioConfig`] from a compiled entry.
fn logs_entry(mut entry: CompiledEntry) -> Result<LogScenarioConfig, PrepareError> {
    let generator =
        entry
            .log_generator
            .take()
            .ok_or_else(|| PrepareError::MissingLogGenerator {
                entry_label: describe(&entry),
            })?;
    let encoder = std::mem::replace(
        &mut entry.encoder,
        crate::encoder::EncoderConfig::JsonLines { precision: None },
    );
    let base = build_base(&mut entry);
    Ok(LogScenarioConfig {
        base,
        generator,
        encoder,
    })
}

/// Materialize a [`HistogramScenarioConfig`] from a compiled entry.
fn histogram_entry(mut entry: CompiledEntry) -> Result<HistogramScenarioConfig, PrepareError> {
    let distribution =
        entry
            .distribution
            .take()
            .ok_or_else(|| PrepareError::MissingDistribution {
                entry_label: describe(&entry),
                signal_type: "histogram".to_string(),
            })?;
    let buckets = entry.buckets.take();
    let observations_per_tick = entry.observations_per_tick.map(u64::from);
    let mean_shift_per_sec = entry.mean_shift_per_sec;
    let seed = entry.seed;
    let encoder = std::mem::replace(
        &mut entry.encoder,
        crate::encoder::EncoderConfig::PrometheusText { precision: None },
    );
    let base = build_base(&mut entry);
    Ok(HistogramScenarioConfig {
        base,
        buckets,
        distribution,
        observations_per_tick,
        mean_shift_per_sec,
        seed,
        encoder,
    })
}

/// Materialize a [`SummaryScenarioConfig`] from a compiled entry.
fn summary_entry(mut entry: CompiledEntry) -> Result<SummaryScenarioConfig, PrepareError> {
    let distribution =
        entry
            .distribution
            .take()
            .ok_or_else(|| PrepareError::MissingDistribution {
                entry_label: describe(&entry),
                signal_type: "summary".to_string(),
            })?;
    let quantiles = entry.quantiles.take();
    let observations_per_tick = entry.observations_per_tick.map(u64::from);
    let mean_shift_per_sec = entry.mean_shift_per_sec;
    let seed = entry.seed;
    let encoder = std::mem::replace(
        &mut entry.encoder,
        crate::encoder::EncoderConfig::PrometheusText { precision: None },
    );
    let base = build_base(&mut entry);
    Ok(SummaryScenarioConfig {
        base,
        quantiles,
        distribution,
        observations_per_tick,
        mean_shift_per_sec,
        seed,
        encoder,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "config"))]
mod tests {
    use std::collections::BTreeMap;

    use rstest::rstest;

    use super::*;
    use crate::config::DistributionConfig;
    use crate::encoder::EncoderConfig;
    use crate::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use crate::sink::SinkConfig;

    // -- Builders -----------------------------------------------------------

    /// Build a minimal [`CompiledEntry`] — every variant-specific field is
    /// absent so tests can pick which ones to set.
    fn bare(signal_type: &str, name: &str) -> CompiledEntry {
        CompiledEntry {
            id: None,
            signal_type: signal_type.to_string(),
            name: name.to_string(),
            rate: 10.0,
            duration: Some("1s".to_string()),
            generator: None,
            log_generator: None,
            labels: None,
            dynamic_labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            jitter: None,
            jitter_seed: None,
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: false,
            distribution: None,
            buckets: None,
            quantiles: None,
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
            on_sink_error: crate::OnSinkError::Warn,
            while_clause: None,
            delay_clause: None,
            after_ref: None,
        }
    }

    fn metrics_compiled(name: &str) -> CompiledEntry {
        let mut e = bare("metrics", name);
        e.generator = Some(GeneratorConfig::Constant { value: 1.0 });
        e
    }

    fn logs_compiled(name: &str) -> CompiledEntry {
        let mut e = bare("logs", name);
        e.log_generator = Some(LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "hi".to_string(),
                field_pools: BTreeMap::new(),
            }],
            severity_weights: None,
            seed: Some(0),
        });
        e.encoder = EncoderConfig::JsonLines { precision: None };
        e
    }

    fn histogram_compiled(name: &str) -> CompiledEntry {
        let mut e = bare("histogram", name);
        e.distribution = Some(DistributionConfig::Exponential { rate: 10.0 });
        e.buckets = Some(vec![0.1, 1.0, 10.0]);
        e
    }

    fn summary_compiled(name: &str) -> CompiledEntry {
        let mut e = bare("summary", name);
        e.distribution = Some(DistributionConfig::Normal {
            mean: 0.1,
            stddev: 0.02,
        });
        e.quantiles = Some(vec![0.5, 0.9, 0.99]);
        e
    }

    fn file_with(entry: CompiledEntry) -> CompiledFile {
        CompiledFile {
            version: 2,
            entries: vec![entry],
        }
    }

    // -- Happy paths --------------------------------------------------------

    /// A metrics entry with a constant generator round-trips into
    /// `ScenarioEntry::Metrics` with name and rate preserved.
    #[test]
    fn metrics_entry_translates_to_scenario_entry_metrics() {
        let file = file_with(metrics_compiled("cpu_usage"));
        let out = prepare(file).expect("translate must succeed");
        assert_eq!(out.len(), 1);
        match &out[0] {
            ScenarioEntry::Metrics(c) => {
                assert_eq!(c.base.name, "cpu_usage");
                assert_eq!(c.base.rate, 10.0);
                assert!(matches!(c.generator, GeneratorConfig::Constant { .. }));
            }
            other => panic!("expected Metrics, got {other:?}"),
        }
    }

    /// A logs entry with a template generator round-trips into
    /// `ScenarioEntry::Logs` with the log_generator preserved.
    #[test]
    fn logs_entry_translates_to_scenario_entry_logs() {
        let file = file_with(logs_compiled("app_logs"));
        let out = prepare(file).expect("translate must succeed");
        match &out[0] {
            ScenarioEntry::Logs(c) => {
                assert_eq!(c.base.name, "app_logs");
                assert!(matches!(c.generator, LogGeneratorConfig::Template { .. }));
            }
            other => panic!("expected Logs, got {other:?}"),
        }
    }

    /// A histogram entry translates with distribution, buckets, and encoder
    /// preserved.
    #[test]
    fn histogram_entry_translates_with_distribution_and_buckets() {
        let file = file_with(histogram_compiled("http_request_duration"));
        let out = prepare(file).expect("translate must succeed");
        match &out[0] {
            ScenarioEntry::Histogram(c) => {
                assert_eq!(c.base.name, "http_request_duration");
                assert_eq!(c.buckets.as_deref(), Some(&[0.1, 1.0, 10.0][..]));
                assert!(matches!(
                    c.distribution,
                    DistributionConfig::Exponential { .. }
                ));
            }
            other => panic!("expected Histogram, got {other:?}"),
        }
    }

    /// A summary entry translates with distribution, quantiles, and encoder
    /// preserved.
    #[test]
    fn summary_entry_translates_with_distribution_and_quantiles() {
        let file = file_with(summary_compiled("rpc_duration"));
        let out = prepare(file).expect("translate must succeed");
        match &out[0] {
            ScenarioEntry::Summary(c) => {
                assert_eq!(c.base.name, "rpc_duration");
                assert_eq!(c.quantiles.as_deref(), Some(&[0.5, 0.9, 0.99][..]));
                assert!(matches!(c.distribution, DistributionConfig::Normal { .. }));
            }
            other => panic!("expected Summary, got {other:?}"),
        }
    }

    /// The order of entries in the compiled file is preserved verbatim.
    #[test]
    fn prepare_preserves_entry_order() {
        let file = CompiledFile {
            version: 2,
            entries: vec![
                metrics_compiled("first"),
                logs_compiled("second"),
                histogram_compiled("third"),
                summary_compiled("fourth"),
            ],
        };
        let out = prepare(file).expect("translate must succeed");
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].base().name, "first");
        assert_eq!(out[1].base().name, "second");
        assert_eq!(out[2].base().name, "third");
        assert_eq!(out[3].base().name, "fourth");
    }

    /// An empty compiled file translates to an empty vec without error.
    #[test]
    fn prepare_empty_file_returns_empty_vec() {
        let file = CompiledFile {
            version: 2,
            entries: vec![],
        };
        let out = prepare(file).expect("empty file must translate cleanly");
        assert!(out.is_empty());
    }

    // -- phase_offset / clock_group carry-through ---------------------------

    /// phase_offset is passed through as a string — the translator never
    /// parses it (that is `prepare_entries`'s job).
    #[test]
    fn phase_offset_string_is_passed_through_verbatim() {
        let mut entry = metrics_compiled("delayed");
        entry.phase_offset = Some("152.308s".to_string());
        let out = prepare(file_with(entry)).expect("translate");
        assert_eq!(out[0].phase_offset(), Some("152.308s"));
    }

    /// clock_group passes through unchanged on every variant.
    #[test]
    fn clock_group_is_passed_through_on_all_variants() {
        for factory in [
            metrics_compiled as fn(&str) -> CompiledEntry,
            logs_compiled,
            histogram_compiled,
            summary_compiled,
        ] {
            let mut entry = factory("any");
            entry.clock_group = Some("chain_alpha".to_string());
            let out = prepare(file_with(entry)).expect("translate");
            assert_eq!(out[0].clock_group(), Some("chain_alpha"));
        }
    }

    // -- Labels conversion --------------------------------------------------

    /// Every (key, value) pair from the BTreeMap appears in the resulting
    /// HashMap.
    #[test]
    fn labels_btree_to_hash_preserves_all_pairs() {
        let mut labels = BTreeMap::new();
        labels.insert("k1".to_string(), "v1".to_string());
        labels.insert("k2".to_string(), "v2".to_string());
        labels.insert("k3".to_string(), "v3".to_string());

        let mut entry = metrics_compiled("labeled");
        entry.labels = Some(labels.clone());

        let out = prepare(file_with(entry)).expect("translate");
        let hm = out[0]
            .base()
            .labels
            .as_ref()
            .expect("labels must carry through");
        assert_eq!(hm.len(), labels.len());
        for (k, v) in &labels {
            assert_eq!(hm.get(k).map(String::as_str), Some(v.as_str()));
        }
    }

    /// An empty `labels: Some(BTreeMap::new())` survives as
    /// `Some(HashMap::new())` (the shape semantically differs from `None`).
    #[test]
    fn labels_empty_btree_maps_to_empty_hash() {
        let mut entry = metrics_compiled("empty_labels");
        entry.labels = Some(BTreeMap::new());
        let out = prepare(file_with(entry)).expect("translate");
        let hm = out[0].base().labels.as_ref().expect("Some stays Some");
        assert!(hm.is_empty());
    }

    /// `labels: None` on the compiled entry survives as `None` on the
    /// scenario entry.
    #[test]
    fn labels_none_stays_none() {
        let entry = metrics_compiled("no_labels");
        let out = prepare(file_with(entry)).expect("translate");
        assert!(out[0].base().labels.is_none());
    }

    // -- observations_per_tick widening --------------------------------------

    /// `observations_per_tick: Some(0u32)` widens to `Some(0u64)` without
    /// clamping or overflow on histograms.
    #[test]
    fn histogram_observations_per_tick_widens_zero_correctly() {
        let mut entry = histogram_compiled("zero_obs");
        entry.observations_per_tick = Some(0);
        let out = prepare(file_with(entry)).expect("translate");
        match &out[0] {
            ScenarioEntry::Histogram(c) => {
                assert_eq!(c.observations_per_tick, Some(0u64));
            }
            _ => panic!("expected Histogram"),
        }
    }

    /// `observations_per_tick: Some(u32::MAX)` widens to `Some(4_294_967_295u64)`
    /// on histograms — no sign extension surprises.
    #[test]
    fn histogram_observations_per_tick_widens_u32_max_correctly() {
        let mut entry = histogram_compiled("max_obs");
        entry.observations_per_tick = Some(u32::MAX);
        let out = prepare(file_with(entry)).expect("translate");
        match &out[0] {
            ScenarioEntry::Histogram(c) => {
                assert_eq!(c.observations_per_tick, Some(u64::from(u32::MAX)));
                assert_eq!(c.observations_per_tick, Some(4_294_967_295_u64));
            }
            _ => panic!("expected Histogram"),
        }
    }

    /// The same widening holds for summary entries.
    #[test]
    fn summary_observations_per_tick_widens_u32_max_correctly() {
        let mut entry = summary_compiled("max_obs_summary");
        entry.observations_per_tick = Some(u32::MAX);
        let out = prepare(file_with(entry)).expect("translate");
        match &out[0] {
            ScenarioEntry::Summary(c) => {
                assert_eq!(c.observations_per_tick, Some(u64::from(u32::MAX)));
            }
            _ => panic!("expected Summary"),
        }
    }

    // -- Error cases --------------------------------------------------------

    /// An unknown `signal_type` string produces `UnknownSignalType` with the
    /// offending value surfaced.
    #[test]
    fn unknown_signal_type_produces_unknown_signal_type_error() {
        let mut entry = bare("traces", "bad");
        entry.id = Some("bad".to_string());
        let err = prepare(file_with(entry)).expect_err("unknown signal_type must fail");
        match err {
            PrepareError::UnknownSignalType {
                entry_label,
                signal_type,
            } => {
                assert_eq!(entry_label, "bad");
                assert_eq!(signal_type, "traces");
            }
            other => panic!("expected UnknownSignalType, got {other:?}"),
        }
    }

    /// The `entry_label` falls back to `name` when `id` is absent.
    #[test]
    fn unknown_signal_type_falls_back_to_name_when_id_absent() {
        let entry = bare("traces", "bad_by_name");
        let err = prepare(file_with(entry)).expect_err("unknown signal_type must fail");
        match err {
            PrepareError::UnknownSignalType { entry_label, .. } => {
                assert_eq!(entry_label, "bad_by_name");
            }
            other => panic!("expected UnknownSignalType, got {other:?}"),
        }
    }

    /// A metrics entry missing `generator` produces `MissingGenerator` with
    /// the entry label.
    #[test]
    fn metrics_without_generator_produces_missing_generator_error() {
        let mut entry = bare("metrics", "no_gen");
        entry.id = Some("no_gen".to_string());
        // generator deliberately left None
        let err = prepare(file_with(entry)).expect_err("missing generator must fail");
        match err {
            PrepareError::MissingGenerator { entry_label } => {
                assert_eq!(entry_label, "no_gen");
            }
            other => panic!("expected MissingGenerator, got {other:?}"),
        }
    }

    /// A logs entry missing `log_generator` produces `MissingLogGenerator`.
    #[test]
    fn logs_without_log_generator_produces_missing_log_generator_error() {
        let entry = bare("logs", "no_log_gen");
        let err = prepare(file_with(entry)).expect_err("missing log_generator must fail");
        match err {
            PrepareError::MissingLogGenerator { entry_label } => {
                assert_eq!(entry_label, "no_log_gen");
            }
            other => panic!("expected MissingLogGenerator, got {other:?}"),
        }
    }

    /// A histogram entry missing `distribution` produces `MissingDistribution`
    /// with `signal_type: "histogram"`.
    #[test]
    fn histogram_without_distribution_produces_missing_distribution_error() {
        let entry = bare("histogram", "no_dist_hist");
        let err = prepare(file_with(entry)).expect_err("missing distribution must fail");
        match err {
            PrepareError::MissingDistribution {
                entry_label,
                signal_type,
            } => {
                assert_eq!(entry_label, "no_dist_hist");
                assert_eq!(signal_type, "histogram");
            }
            other => panic!("expected MissingDistribution, got {other:?}"),
        }
    }

    /// A summary entry missing `distribution` produces `MissingDistribution`
    /// with `signal_type: "summary"`.
    #[test]
    fn summary_without_distribution_produces_missing_distribution_error() {
        let entry = bare("summary", "no_dist_summary");
        let err = prepare(file_with(entry)).expect_err("missing distribution must fail");
        match err {
            PrepareError::MissingDistribution {
                entry_label,
                signal_type,
            } => {
                assert_eq!(entry_label, "no_dist_summary");
                assert_eq!(signal_type, "summary");
            }
            other => panic!("expected MissingDistribution, got {other:?}"),
        }
    }

    /// Which `PrepareError` variant a missing-required-field shape must
    /// produce. Indexes the rstest matrix below: `metrics` -> generator,
    /// `logs` -> log_generator, `histogram`/`summary` -> distribution.
    #[derive(Debug, Clone, Copy)]
    enum ExpectedMissing {
        Generator,
        LogGenerator,
        Distribution,
    }

    /// Rstest matrix covering every signal_type whose shape-invariant can
    /// fail when the required generator/distribution field is absent.
    /// Each case asserts the exact `PrepareError` variant — strengthening
    /// the previous `is_err()` smoke check.
    #[rustfmt::skip]
    #[rstest]
    #[case::metrics("metrics", ExpectedMissing::Generator)]
    #[case::logs("logs", ExpectedMissing::LogGenerator)]
    #[case::histogram("histogram", ExpectedMissing::Distribution)]
    #[case::summary("summary", ExpectedMissing::Distribution)]
    fn missing_required_field_fails_per_signal_type(
        #[case] signal_type: &str,
        #[case] expected: ExpectedMissing,
    ) {
        let entry = bare(signal_type, "empty_shape");
        let err = prepare(file_with(entry)).err().unwrap_or_else(|| {
            panic!("signal_type '{signal_type}' missing required field must error")
        });
        let matched = match expected {
            ExpectedMissing::Generator => {
                matches!(err, PrepareError::MissingGenerator { ref entry_label } if entry_label == "empty_shape")
            }
            ExpectedMissing::LogGenerator => {
                matches!(err, PrepareError::MissingLogGenerator { ref entry_label } if entry_label == "empty_shape")
            }
            ExpectedMissing::Distribution => matches!(
                err,
                PrepareError::MissingDistribution { ref entry_label, signal_type: ref st }
                if entry_label == "empty_shape" && st == signal_type
            ),
        };
        assert!(
            matched,
            "signal_type '{signal_type}': expected {expected:?}, got {err:?}"
        );
    }

    // -- First-error propagation --------------------------------------------

    /// `prepare` surfaces the FIRST failing entry's error, leaving later
    /// entries unevaluated. Callers cannot observe partial output.
    #[test]
    fn prepare_fails_fast_on_first_bad_entry() {
        let file = CompiledFile {
            version: 2,
            entries: vec![
                metrics_compiled("ok_1"),
                bare("traces", "bad"),
                metrics_compiled("ok_2"),
            ],
        };
        let err = prepare(file).expect_err("bad entry in middle must fail");
        assert!(
            matches!(err, PrepareError::UnknownSignalType { .. }),
            "middle bad entry must produce UnknownSignalType, got {err:?}"
        );
    }

    // -- Contract: PrepareError is Send + Sync ------------------------------

    #[test]
    fn prepare_error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PrepareError>();
    }

    // -- Version gate -------------------------------------------------------

    /// A [`CompiledFile`] with a non-v2 version is rejected with
    /// [`PrepareError::UnsupportedVersion`] carrying the offending value.
    #[test]
    fn prepare_rejects_non_v2_version() {
        let file = CompiledFile {
            version: 3,
            entries: vec![metrics_compiled("never_translated")],
        };
        let err = prepare(file).expect_err("version != 2 must fail");
        match err {
            PrepareError::UnsupportedVersion { version } => assert_eq!(version, 3),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    /// The `UnsupportedVersion` check fires before any entry-level
    /// translation, so a bogus version with an otherwise-invalid entry
    /// surfaces the version error (not the entry error).
    #[test]
    fn prepare_version_check_precedes_entry_translation() {
        let file = CompiledFile {
            version: 0,
            entries: vec![bare("traces", "would_fail_if_translated")],
        };
        let err = prepare(file).expect_err("version 0 must fail");
        assert!(
            matches!(err, PrepareError::UnsupportedVersion { version: 0 }),
            "expected UnsupportedVersion {{ version: 0 }}, got {err:?}"
        );
    }
}
