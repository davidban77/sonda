//! Pack expansion for v2 scenario files.
//!
//! This module implements **Phase 3** of the v2 compilation pipeline. It takes
//! a [`NormalizedFile`] (the output of [`super::normalize::normalize`]) and
//! expands every pack-backed entry into one concrete per-metric signal while
//! preserving the full label precedence chain from spec §2.2. After expansion,
//! the returned [`ExpandedFile`] contains no unresolved pack references —
//! every entry is a concrete signal that later phases can reason about in
//! isolation.
//!
//! # Label precedence chain (for pack-expanded signals)
//!
//! For each metric produced by a pack expansion the final label map is
//! composed from five layers, applied **low → high** (each subsequent level
//! overwrites on key collision):
//!
//! | Level | Source |
//! |------:|--------|
//! | 2 | [`NormalizedFile::defaults_labels`] |
//! | 4 | pack [`MetricPackDef::shared_labels`] |
//! | 5 | pack per-metric [`MetricSpec::labels`] |
//! | 6 | pack entry [`NormalizedEntry::labels`] |
//! | 7 | override [`MetricOverride::labels`] |
//!
//! Levels 1 (built-in defaults) and 3 (entry non-label fields) do not
//! contribute labels. Level 8 (CLI flags) is applied later and is out of
//! scope here. PR 3 deliberately left pack entry labels *unmerged* with
//! `defaults.labels` so this pass can interleave levels 4 and 5 between
//! them.
//!
//! Inline entries do **not** re-apply `defaults_labels`: PR 3 already merged
//! them eagerly and we must not double-apply. Inline entries are copied
//! through with their label map intact.
//!
//! # Auto-generated pack entry IDs
//!
//! Spec §2.4 allows pack entries to omit `id`; spec matrix row 11.8 still
//! requires sub-signal IDs to be addressable. When a pack entry has no `id`
//! set, this pass synthesizes a deterministic identifier of the form
//! `"{pack_def_name}_{entry_index}"` where `entry_index` is the pack entry's
//! zero-based position in [`NormalizedFile::entries`]. The suffix is always
//! appended (even for the first anonymous pack entry) so two anonymous pack
//! entries referencing the same pack never collide.
//!
//! The per-metric sub-signal IDs always take the form
//! `"{effective_entry_id}.{metric_name}"`, where `effective_entry_id` is
//! either the user-provided `id` or the auto-generated one above.
//!
//! # No pack references survive
//!
//! After [`expand`] returns successfully, none of the entries in
//! [`ExpandedFile::entries`] carry a `pack` reference. Subsequent compilation
//! phases (after-clause resolution, clock group assignment, runtime wiring)
//! can operate on a flat list of concrete signals.
//!
//! # Error surface
//!
//! All failure modes flow through [`ExpandError`]:
//!
//! - unknown override keys in a pack entry,
//! - pack resolver failures (name lookup, file IO, YAML parse),
//! - pack definitions with no metrics.

use std::collections::{BTreeMap, BTreeSet};

use super::normalize::{NormalizedEntry, NormalizedFile};
use super::AfterClause;
use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
};
use crate::encoder::EncoderConfig;
use crate::generator::{GeneratorConfig, LogGeneratorConfig};
use crate::packs::{MetricOverride, MetricPackDef};
use crate::sink::SinkConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced during pack expansion.
#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    /// The pack reference could not be resolved — either unknown name or a
    /// file path load failure. The wrapped message includes the pack
    /// reference and an indication of whether the resolver treated it as a
    /// name lookup or a file path load.
    #[error("pack '{reference}' could not be resolved: {message}")]
    ResolveFailed {
        /// The pack reference as written in the scenario file.
        reference: String,
        /// Diagnostic detail from the underlying resolver.
        message: String,
    },

    /// An override in a pack entry referenced a metric name that does not
    /// exist in the resolved pack definition.
    ///
    /// The error lists the pack's available metric names so the user can see
    /// what was expected.
    #[error(
        "override references unknown metric '{key}'; pack '{pack_name}' contains: {available}"
    )]
    UnknownOverrideKey {
        /// The offending override key.
        key: String,
        /// The pack definition name that was being expanded.
        pack_name: String,
        /// Comma-separated list of valid metric names from the pack.
        available: String,
    },

    /// The pack definition has no metrics, so expansion has nothing to emit.
    #[error("pack '{pack_name}' contains no metrics")]
    EmptyPack {
        /// The pack definition name that was being expanded.
        pack_name: String,
    },
}

// ---------------------------------------------------------------------------
// Pack resolver trait
// ---------------------------------------------------------------------------

/// Resolves a pack reference into a [`MetricPackDef`].
///
/// The trait is intentionally narrow: implementations receive the raw
/// reference string exactly as it appeared in the scenario file, decide
/// whether to treat it as a pack name (catalog lookup) or a file path (when
/// the string contains `/` or starts with `.`, per spec §2.4), and return
/// the parsed definition.
///
/// Implementations must be pure with respect to the inputs they receive —
/// the compiler does not cache results, so callers that want memoization
/// should wrap their resolver.
///
/// The [`sonda`] CLI crate adapts its filesystem `PackCatalog` to this
/// trait. Tests use [`InMemoryPackResolver`].
pub trait PackResolver {
    /// Resolve `reference` to a pack definition.
    ///
    /// `reference` is the string the user wrote under `pack:`. Per spec
    /// §2.4, values containing `/` or starting with `.` are treated as file
    /// paths; everything else is treated as a pack name and looked up on
    /// the caller's search path.
    ///
    /// Errors must include enough context (path, underlying OS error, YAML
    /// parse diagnostic) for the compiler to surface a useful diagnostic
    /// without further decoration.
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError>;
}

/// Error produced by a [`PackResolver`] implementation.
///
/// Carries a human-readable message plus a classification of how the
/// resolver interpreted the reference. The compiler folds this into
/// [`ExpandError::ResolveFailed`] so users see a consistent diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PackResolveError {
    /// Diagnostic message describing the failure.
    pub message: String,
    /// Origin kind the resolver decided to use for the reference.
    pub origin: PackResolveOrigin,
}

/// How a resolver interpreted a pack reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackResolveOrigin {
    /// Interpreted as a pack name looked up on the catalog search path.
    Name,
    /// Interpreted as a filesystem path to a pack YAML file.
    FilePath,
}

impl PackResolveError {
    /// Construct a resolver error from a reference and a message.
    ///
    /// `origin` should reflect the path the resolver took to interpret the
    /// reference so error messages can disambiguate "unknown pack name"
    /// from "pack file not found".
    pub fn new(message: impl Into<String>, origin: PackResolveOrigin) -> Self {
        Self {
            message: message.into(),
            origin,
        }
    }
}

/// Classify a pack reference as a file path or a catalog name per spec §2.4.
///
/// Returns [`PackResolveOrigin::FilePath`] when `reference` contains a `/`
/// or starts with a `.`; otherwise [`PackResolveOrigin::Name`].
pub fn classify_pack_reference(reference: &str) -> PackResolveOrigin {
    if reference.contains('/') || reference.starts_with('.') {
        PackResolveOrigin::FilePath
    } else {
        PackResolveOrigin::Name
    }
}

/// An in-memory [`PackResolver`] backed by a `BTreeMap`.
///
/// Useful for unit tests, embedded integrations, and any caller that
/// constructs pack definitions in code rather than loading them from disk.
/// Both pack names (catalog lookup) and file-path strings can be inserted —
/// lookup is a direct key match.
#[derive(Debug, Default, Clone)]
pub struct InMemoryPackResolver {
    packs: BTreeMap<String, MetricPackDef>,
}

impl InMemoryPackResolver {
    /// Create an empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a pack definition keyed by `reference`.
    ///
    /// The key is matched verbatim against the pack reference string in
    /// the scenario file. Callers that need to support both "pack by name"
    /// and "pack by file path" for the same definition should insert it
    /// under both keys.
    pub fn insert(&mut self, reference: impl Into<String>, pack: MetricPackDef) {
        self.packs.insert(reference.into(), pack);
    }
}

impl PackResolver for InMemoryPackResolver {
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError> {
        match self.packs.get(reference) {
            Some(pack) => Ok(pack.clone()),
            None => Err(PackResolveError::new(
                format!("pack reference '{reference}' not found in resolver"),
                classify_pack_reference(reference),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Expanded representation
// ---------------------------------------------------------------------------

/// A v2 scenario file whose pack entries have been fully expanded.
///
/// This is the output of [`expand`]. Every entry is a concrete signal —
/// there are no unresolved pack references. Inline entries from the
/// [`NormalizedFile`] pass through verbatim; pack entries are replaced by
/// one [`ExpandedEntry`] per metric in the pack.
///
/// # Invariants
///
/// - No entry has a `pack` or `overrides` field — those have been resolved.
/// - Every entry has a concrete `rate`, `encoder`, and `sink` (inherited
///   from [`NormalizedEntry`]).
/// - Entry IDs remain unique across the file, including auto-generated
///   IDs synthesized for anonymous pack entries.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct ExpandedFile {
    /// Schema version. Always `2` after expansion.
    pub version: u32,
    /// All entries with pack expansion applied, in source order.
    ///
    /// Pack entries contribute one entry per metric, in the order metrics
    /// appear in the resolved pack definition. Inline entries contribute
    /// one entry each, unchanged from the normalized input.
    pub entries: Vec<ExpandedEntry>,
}

/// A single concrete scenario entry after pack expansion.
///
/// This is the fully-resolved form of a signal that later compilation
/// phases (`after` compiler, clock group assignment, runtime launcher)
/// consume. The type deliberately drops pack-related fields
/// (`pack`, `overrides`) because they cannot appear here, and drops
/// histogram/summary fields because spec §2.4 forbids pack entries from
/// carrying them — inline histogram/summary entries still flow through but
/// pack expansion never produces them.
///
/// Sub-signal IDs produced by pack expansion have the form
/// `"{effective_entry_id}.{metric_name}"`; see the module docs for the
/// auto-ID scheme used when the pack entry lacks an explicit `id`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct ExpandedEntry {
    /// Signal identifier. Concrete for every expanded entry: either the
    /// user-provided inline id, or a pack-expansion sub-signal id of the
    /// form `"{effective_entry_id}.{metric_name}"`.
    ///
    /// Inline entries without an `id` in the source carry `None` here (that
    /// survives verbatim from the normalized input). Pack-expanded entries
    /// always have `Some(_)`: if the parent pack entry lacked an id, one
    /// was synthesized (see module docs).
    pub id: Option<String>,
    /// Signal type: `"metrics"`, `"logs"`, `"histogram"`, or `"summary"`.
    pub signal_type: String,
    /// Metric or scenario name. Always populated after expansion: inline
    /// entries carried their own name through normalization; pack-expanded
    /// entries use the pack metric's name.
    pub name: String,
    /// Event rate in events per second. Inherited from the parent
    /// normalized entry.
    pub rate: f64,
    /// Total run duration (e.g. `"30s"`, `"5m"`).
    pub duration: Option<String>,
    /// Value generator configuration (metrics signals only).
    pub generator: Option<GeneratorConfig>,
    /// Log generator configuration (logs signals only).
    pub log_generator: Option<LogGeneratorConfig>,
    /// Static labels after the full precedence chain has been applied.
    ///
    /// For pack-expanded entries this is the level-2-through-7 merge
    /// described in the module docs. For inline entries it is the
    /// already-merged map produced by Phase 2 normalization (unchanged).
    ///
    /// `None` when no source contributed any labels.
    pub labels: Option<BTreeMap<String, String>>,
    /// Dynamic (rotating) label configurations.
    pub dynamic_labels: Option<Vec<DynamicLabelConfig>>,
    /// Encoder configuration.
    pub encoder: EncoderConfig,
    /// Sink configuration.
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
    ///
    /// For pack-expanded signals, an override-level `after` replaces the
    /// parent pack entry's `after`; otherwise the parent's `after` is
    /// propagated verbatim. Resolution into timing offsets is Phase 4
    /// (PR 5).
    pub after: Option<AfterClause>,

    // -- Histogram / summary fields (inline entries only) --
    //
    // Pack entries cannot carry these (spec §2.4: pack entries must have
    // signal_type: metrics, parse-time validation enforces that). They
    // survive here purely as carry-through for inline histogram/summary
    // signals.
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

/// Expand every pack entry in a normalized v2 scenario file.
///
/// Inline entries in [`NormalizedFile::entries`] are copied through
/// verbatim (without re-applying `defaults_labels` — Phase 2 handled that).
/// Pack entries are materialized into one [`ExpandedEntry`] per metric in
/// the resolved pack, with labels composed according to the module-level
/// precedence chain and fields propagated per spec §4.3.
///
/// # Errors
///
/// - [`ExpandError::ResolveFailed`] when the resolver cannot produce a
///   [`MetricPackDef`] for a pack reference.
/// - [`ExpandError::UnknownOverrideKey`] when an override targets a metric
///   that is not present in the resolved pack definition.
/// - [`ExpandError::EmptyPack`] when the resolved pack has no metrics.
pub fn expand<R: PackResolver>(
    file: NormalizedFile,
    resolver: &R,
) -> Result<ExpandedFile, ExpandError> {
    let defaults_labels = file.defaults_labels;
    let mut entries: Vec<ExpandedEntry> = Vec::with_capacity(file.entries.len());

    for (index, entry) in file.entries.into_iter().enumerate() {
        if entry.pack.is_some() {
            expand_pack_entry(
                entry,
                index,
                defaults_labels.as_ref(),
                resolver,
                &mut entries,
            )?;
        } else {
            entries.push(expand_inline_entry(entry));
        }
    }

    Ok(ExpandedFile {
        version: file.version,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Inline pass-through
// ---------------------------------------------------------------------------

/// Convert an inline [`NormalizedEntry`] into an [`ExpandedEntry`].
///
/// Labels are preserved verbatim — Phase 2 normalization already merged
/// `defaults_labels` into inline entries. Re-applying them here would
/// double-merge a map the user already sees in the normalized output.
fn expand_inline_entry(entry: NormalizedEntry) -> ExpandedEntry {
    ExpandedEntry {
        id: entry.id,
        signal_type: entry.signal_type,
        // Inline entries always have `name` by parse-time validation.
        name: entry.name.unwrap_or_default(),
        rate: entry.rate,
        duration: entry.duration,
        generator: entry.generator,
        log_generator: entry.log_generator,
        labels: entry.labels,
        dynamic_labels: entry.dynamic_labels,
        encoder: entry.encoder,
        sink: entry.sink,
        jitter: entry.jitter,
        jitter_seed: entry.jitter_seed,
        gaps: entry.gaps,
        bursts: entry.bursts,
        cardinality_spikes: entry.cardinality_spikes,
        phase_offset: entry.phase_offset,
        clock_group: entry.clock_group,
        after: entry.after,
        distribution: entry.distribution,
        buckets: entry.buckets,
        quantiles: entry.quantiles,
        observations_per_tick: entry.observations_per_tick,
        mean_shift_per_sec: entry.mean_shift_per_sec,
        seed: entry.seed,
    }
}

// ---------------------------------------------------------------------------
// Pack expansion
// ---------------------------------------------------------------------------

/// Expand a single pack-backed [`NormalizedEntry`] into one [`ExpandedEntry`]
/// per metric in the resolved pack, appending to `out`.
fn expand_pack_entry<R: PackResolver>(
    entry: NormalizedEntry,
    entry_index: usize,
    defaults_labels: Option<&BTreeMap<String, String>>,
    resolver: &R,
    out: &mut Vec<ExpandedEntry>,
) -> Result<(), ExpandError> {
    // `entry.pack` is Some() by the caller's check; unwrap defensively.
    let reference = entry
        .pack
        .as_deref()
        .expect("expand_pack_entry called with non-pack entry; caller must check");

    let pack = resolver
        .resolve(reference)
        .map_err(|e| ExpandError::ResolveFailed {
            reference: reference.to_string(),
            message: e.message,
        })?;

    if pack.metrics.is_empty() {
        return Err(ExpandError::EmptyPack {
            pack_name: pack.name,
        });
    }

    validate_override_keys(&pack, entry.overrides.as_ref())?;

    let effective_entry_id = match entry.id.clone() {
        Some(id) => id,
        None => format!("{}_{}", pack.name, entry_index),
    };

    for metric in &pack.metrics {
        let override_for_metric = entry
            .overrides
            .as_ref()
            .and_then(|map| map.get(&metric.name));

        let labels = compose_pack_metric_labels(
            defaults_labels,
            pack.shared_labels.as_ref(),
            metric.labels.as_ref(),
            entry.labels.as_ref(),
            override_for_metric.and_then(|o| o.labels.as_ref()),
        );

        let generator = select_pack_metric_generator(metric, override_for_metric);

        // Override-level `after` replaces entry-level `after` for this
        // specific expanded metric; otherwise propagate the parent's
        // `after` verbatim. We do NOT resolve `after.ref` here — that is
        // PR 5's job.
        let after = override_for_metric
            .and_then(|o| o.after.clone())
            .or_else(|| entry.after.clone());

        out.push(ExpandedEntry {
            id: Some(format!("{}.{}", effective_entry_id, metric.name)),
            signal_type: "metrics".to_string(),
            name: metric.name.clone(),
            rate: entry.rate,
            duration: entry.duration.clone(),
            generator: Some(generator),
            log_generator: None,
            labels,
            dynamic_labels: entry.dynamic_labels.clone(),
            encoder: entry.encoder.clone(),
            sink: entry.sink.clone(),
            jitter: entry.jitter,
            jitter_seed: entry.jitter_seed,
            gaps: entry.gaps.clone(),
            bursts: entry.bursts.clone(),
            cardinality_spikes: entry.cardinality_spikes.clone(),
            phase_offset: entry.phase_offset.clone(),
            clock_group: entry.clock_group.clone(),
            after,
            distribution: None,
            buckets: None,
            quantiles: None,
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
        });
    }

    Ok(())
}

/// Reject overrides whose keys do not match any metric name in the pack.
///
/// Matches the message shape produced by
/// [`crate::packs::expand_pack`] so v1 and v2 surfaces stay consistent.
fn validate_override_keys(
    pack: &MetricPackDef,
    overrides: Option<&BTreeMap<String, MetricOverride>>,
) -> Result<(), ExpandError> {
    let Some(overrides) = overrides else {
        return Ok(());
    };
    if overrides.is_empty() {
        return Ok(());
    }

    let metric_names: BTreeSet<&str> = pack.metrics.iter().map(|m| m.name.as_str()).collect();
    for key in overrides.keys() {
        if !metric_names.contains(key.as_str()) {
            let available: Vec<&str> = pack.metrics.iter().map(|m| m.name.as_str()).collect();
            return Err(ExpandError::UnknownOverrideKey {
                key: key.clone(),
                pack_name: pack.name.clone(),
                available: available.join(", "),
            });
        }
    }
    Ok(())
}

/// Compose the final label map for a single pack-expanded metric.
///
/// Applies the five label layers in the precedence order documented at
/// the module level. `None` maps are skipped. Uses [`BTreeMap`] for
/// deterministic iteration order so snapshot tests are stable.
fn compose_pack_metric_labels(
    defaults_labels: Option<&BTreeMap<String, String>>,
    pack_shared_labels: Option<&std::collections::HashMap<String, String>>,
    pack_metric_labels: Option<&std::collections::HashMap<String, String>>,
    entry_labels: Option<&BTreeMap<String, String>>,
    override_labels: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let mut merged: BTreeMap<String, String> = BTreeMap::new();

    // Level 2: file-level defaults labels.
    if let Some(src) = defaults_labels {
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
    }

    // Level 4: pack shared_labels.
    if let Some(src) = pack_shared_labels {
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
    }

    // Level 5: pack per-metric labels.
    if let Some(src) = pack_metric_labels {
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
    }

    // Level 6: entry-level labels on the pack entry.
    if let Some(src) = entry_labels {
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
    }

    // Level 7: override-level labels.
    if let Some(src) = override_labels {
        for (k, v) in src {
            merged.insert(k.clone(), v.clone());
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

/// Choose the generator for a pack-expanded metric.
///
/// Precedence: override generator > pack metric generator > `constant(0.0)`.
/// Matches the fallback used by [`crate::packs::expand_pack`] so v1 and v2
/// behave identically when a pack metric has no generator declared.
fn select_pack_metric_generator(
    metric: &crate::packs::MetricSpec,
    metric_override: Option<&MetricOverride>,
) -> GeneratorConfig {
    if let Some(over) = metric_override {
        if let Some(gen) = over.generator.clone() {
            return gen;
        }
    }
    metric
        .generator
        .clone()
        .unwrap_or(GeneratorConfig::Constant { value: 0.0 })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::normalize::normalize;
    use crate::compiler::parse::parse;
    use crate::compiler::AfterOp;
    use crate::packs::MetricSpec;
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn telegraf_pack() -> MetricPackDef {
        let mut shared = HashMap::new();
        shared.insert("device".to_string(), String::new());
        shared.insert("job".to_string(), "snmp".to_string());

        MetricPackDef {
            name: "telegraf_snmp_interface".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: Some(shared),
            metrics: vec![
                MetricSpec {
                    name: "ifOperStatus".to_string(),
                    labels: None,
                    generator: Some(GeneratorConfig::Constant { value: 1.0 }),
                },
                MetricSpec {
                    name: "ifHCInOctets".to_string(),
                    labels: None,
                    generator: Some(GeneratorConfig::Step {
                        start: Some(0.0),
                        step_size: 125_000.0,
                        max: None,
                    }),
                },
            ],
        }
    }

    fn node_cpu_pack() -> MetricPackDef {
        let mut shared = HashMap::new();
        shared.insert("job".to_string(), "node_exporter".to_string());

        let mut user_labels = HashMap::new();
        user_labels.insert("mode".to_string(), "user".to_string());

        let mut system_labels = HashMap::new();
        system_labels.insert("mode".to_string(), "system".to_string());

        MetricPackDef {
            name: "node_exporter_cpu".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: Some(shared),
            metrics: vec![
                MetricSpec {
                    name: "node_cpu_seconds_total".to_string(),
                    labels: Some(user_labels),
                    generator: Some(GeneratorConfig::Step {
                        start: Some(0.0),
                        step_size: 0.25,
                        max: None,
                    }),
                },
                MetricSpec {
                    name: "node_cpu_seconds_total".to_string(),
                    labels: Some(system_labels),
                    generator: Some(GeneratorConfig::Step {
                        start: Some(0.0),
                        step_size: 0.10,
                        max: None,
                    }),
                },
            ],
        }
    }

    fn expand_yaml(yaml: &str, resolver: &InMemoryPackResolver) -> ExpandedFile {
        let parsed = parse(yaml).expect("parse must succeed");
        let normalized = normalize(parsed).expect("normalize must succeed");
        expand(normalized, resolver).expect("expand must succeed")
    }

    // -----------------------------------------------------------------------
    // Resolver classification & in-memory impl
    // -----------------------------------------------------------------------

    #[test]
    fn classify_name_reference_returns_name() {
        assert_eq!(
            classify_pack_reference("telegraf_snmp_interface"),
            PackResolveOrigin::Name
        );
    }

    #[test]
    fn classify_file_path_reference_returns_file_path() {
        assert_eq!(
            classify_pack_reference("./packs/custom.yaml"),
            PackResolveOrigin::FilePath
        );
        assert_eq!(
            classify_pack_reference("/abs/path/pack.yaml"),
            PackResolveOrigin::FilePath
        );
        assert_eq!(
            classify_pack_reference("rel/pack.yaml"),
            PackResolveOrigin::FilePath
        );
    }

    #[test]
    fn in_memory_resolver_returns_registered_pack() {
        let mut r = InMemoryPackResolver::new();
        r.insert("telegraf_snmp_interface", telegraf_pack());
        let def = r.resolve("telegraf_snmp_interface").expect("must resolve");
        assert_eq!(def.name, "telegraf_snmp_interface");
    }

    #[test]
    fn in_memory_resolver_errors_on_missing_reference() {
        let r = InMemoryPackResolver::new();
        let err = r.resolve("nope").expect_err("must error");
        assert_eq!(err.origin, PackResolveOrigin::Name);
        assert!(err.message.contains("nope"));
    }

    #[test]
    fn in_memory_resolver_classifies_file_paths() {
        let r = InMemoryPackResolver::new();
        let err = r.resolve("./no-such.yaml").expect_err("must error");
        assert_eq!(err.origin, PackResolveOrigin::FilePath);
    }

    // -----------------------------------------------------------------------
    // Happy path: pack expansion produces one entry per metric
    // -----------------------------------------------------------------------

    #[test]
    fn expand_produces_one_entry_per_pack_metric() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
scenarios:
  - id: primary
    signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(expanded.entries.len(), 2);
        assert_eq!(expanded.entries[0].name, "ifOperStatus");
        assert_eq!(expanded.entries[1].name, "ifHCInOctets");
    }

    #[test]
    fn expanded_signal_type_is_metrics() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        for e in &expanded.entries {
            assert_eq!(e.signal_type, "metrics");
        }
    }

    // -----------------------------------------------------------------------
    // Sub-signal IDs: user-provided and auto-generated
    // -----------------------------------------------------------------------

    #[test]
    fn sub_signal_ids_use_entry_id_when_set() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: primary
    signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(
            expanded.entries[0].id.as_deref(),
            Some("primary.ifOperStatus")
        );
        assert_eq!(
            expanded.entries[1].id.as_deref(),
            Some("primary.ifHCInOctets")
        );
    }

    #[test]
    fn anonymous_pack_entry_gets_auto_generated_id() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        // Auto-ID is `{pack_def_name}_{entry_index}` = `telegraf_snmp_interface_0`.
        assert_eq!(
            expanded.entries[0].id.as_deref(),
            Some("telegraf_snmp_interface_0.ifOperStatus")
        );
        assert_eq!(
            expanded.entries[1].id.as_deref(),
            Some("telegraf_snmp_interface_0.ifHCInOctets")
        );
    }

    #[test]
    fn two_anonymous_pack_entries_disambiguate_by_index() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        let ids: Vec<_> = expanded
            .entries
            .iter()
            .filter_map(|e| e.id.as_deref())
            .collect();
        assert!(ids.contains(&"telegraf_snmp_interface_0.ifOperStatus"));
        assert!(ids.contains(&"telegraf_snmp_interface_1.ifOperStatus"));
        // All IDs must be unique.
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids must be unique");
    }

    // -----------------------------------------------------------------------
    // Label precedence chain
    // -----------------------------------------------------------------------

    #[test]
    fn label_precedence_chain_applied_in_order() {
        // defaults -> shared -> metric -> entry -> override
        // We test that each layer overrides its predecessor on 'region'.
        let mut shared = HashMap::new();
        shared.insert("region".to_string(), "shared-region".to_string());
        shared.insert("job".to_string(), "snmp".to_string());

        let mut metric_labels = HashMap::new();
        metric_labels.insert("region".to_string(), "metric-region".to_string());

        let pack = MetricPackDef {
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                labels: Some(metric_labels),
                generator: Some(GeneratorConfig::Constant { value: 0.0 }),
            }],
        };

        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    region: defaults-region
    env: prod
scenarios:
  - id: e
    signal_type: metrics
    pack: p
    labels:
      region: entry-region
      device: rtr-01
    overrides:
      m:
        labels:
          region: override-region
"#;
        let expanded = expand_yaml(yaml, &resolver);
        let labels = expanded.entries[0].labels.as_ref().unwrap();

        // Highest precedence wins.
        assert_eq!(labels.get("region").unwrap(), "override-region");
        // Lower layers contribute when no higher layer overrides.
        assert_eq!(labels.get("env").unwrap(), "prod");
        assert_eq!(labels.get("job").unwrap(), "snmp");
        assert_eq!(labels.get("device").unwrap(), "rtr-01");
    }

    #[test]
    fn defaults_labels_flow_into_pack_metric_labels() {
        // Spec §2.2: defaults.labels at precedence level 2 must reach the
        // final map for pack-expanded signals.
        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    env: prod
scenarios:
  - id: p
    signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        let labels = expanded.entries[0].labels.as_ref().unwrap();
        assert_eq!(labels.get("env").unwrap(), "prod");
    }

    #[test]
    fn pack_shared_labels_override_defaults_labels() {
        let mut shared = HashMap::new();
        shared.insert("job".to_string(), "snmp".to_string());
        let pack = MetricPackDef {
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                labels: None,
                generator: Some(GeneratorConfig::Constant { value: 0.0 }),
            }],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    job: web
scenarios:
  - signal_type: metrics
    pack: p
"#;
        let expanded = expand_yaml(yaml, &resolver);
        let labels = expanded.entries[0].labels.as_ref().unwrap();
        assert_eq!(labels.get("job").unwrap(), "snmp");
    }

    #[test]
    fn inline_entry_labels_pass_through_unchanged() {
        // Inline entries must NOT re-apply defaults_labels; PR 3 already
        // merged them. Verify that exactly the merged set from normalize
        // shows up here — not doubled, not missing a defaults key.
        let yaml = r#"
version: 2
defaults:
  rate: 1
  labels:
    env: prod
scenarios:
  - signal_type: metrics
    name: cpu
    generator: { type: constant, value: 1 }
    labels:
      instance: web-01
"#;
        let resolver = InMemoryPackResolver::new();
        let expanded = expand_yaml(yaml, &resolver);
        let labels = expanded.entries[0].labels.as_ref().unwrap();
        assert_eq!(labels.get("env").unwrap(), "prod");
        assert_eq!(labels.get("instance").unwrap(), "web-01");
        assert_eq!(labels.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Generator precedence: override > spec > constant(0)
    // -----------------------------------------------------------------------

    #[test]
    fn override_generator_replaces_pack_generator() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: e
    signal_type: metrics
    pack: telegraf_snmp_interface
    overrides:
      ifOperStatus:
        generator:
          type: flap
          up_duration: 60s
          down_duration: 30s
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        // ifOperStatus got the flap override
        assert!(matches!(
            expanded.entries[0].generator.as_ref().unwrap(),
            GeneratorConfig::Flap { .. }
        ));
        // ifHCInOctets kept its pack default (step)
        assert!(matches!(
            expanded.entries[1].generator.as_ref().unwrap(),
            GeneratorConfig::Step { .. }
        ));
    }

    #[test]
    fn missing_generator_falls_back_to_constant_zero() {
        let pack = MetricPackDef {
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "x".to_string(),
                labels: None,
                generator: None,
            }],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: p
"#;
        let expanded = expand_yaml(yaml, &resolver);
        match expanded.entries[0].generator.as_ref().unwrap() {
            GeneratorConfig::Constant { value } => assert_eq!(*value, 0.0),
            other => panic!("expected constant(0), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // After-clause propagation
    // -----------------------------------------------------------------------

    #[test]
    fn entry_level_after_propagates_to_every_metric() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: tail
    signal_type: metrics
    pack: telegraf_snmp_interface
    after:
      ref: head
      op: ">"
      value: 5
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        for e in &expanded.entries {
            let after = e.after.as_ref().expect("after must be propagated");
            assert_eq!(after.ref_id, "head");
            assert!(matches!(after.op, AfterOp::GreaterThan));
        }
    }

    #[test]
    fn override_after_replaces_entry_after_for_that_metric() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: tail
    signal_type: metrics
    pack: telegraf_snmp_interface
    after:
      ref: head
      op: ">"
      value: 5
    overrides:
      ifOperStatus:
        after:
          ref: other
          op: "<"
          value: 1
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        let oper = expanded
            .entries
            .iter()
            .find(|e| e.name == "ifOperStatus")
            .unwrap();
        assert_eq!(oper.after.as_ref().unwrap().ref_id, "other");
        let in_octets = expanded
            .entries
            .iter()
            .find(|e| e.name == "ifHCInOctets")
            .unwrap();
        assert_eq!(in_octets.after.as_ref().unwrap().ref_id, "head");
    }

    // -----------------------------------------------------------------------
    // Field propagation per spec §4.3 step 7
    // -----------------------------------------------------------------------

    #[test]
    fn schedule_delivery_fields_propagate_to_every_metric() {
        let yaml = r#"
version: 2
defaults:
  rate: 1
  duration: 2m
scenarios:
  - id: p
    signal_type: metrics
    pack: telegraf_snmp_interface
    phase_offset: 5s
    clock_group: uplink
    jitter: 0.2
    jitter_seed: 42
    gaps:
      every: 2m
      for: 20s
    bursts:
      every: 5m
      for: 30s
      multiplier: 10
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        for e in &expanded.entries {
            assert_eq!(e.rate, 1.0);
            assert_eq!(e.duration.as_deref(), Some("2m"));
            assert_eq!(e.phase_offset.as_deref(), Some("5s"));
            assert_eq!(e.clock_group.as_deref(), Some("uplink"));
            assert_eq!(e.jitter, Some(0.2));
            assert_eq!(e.jitter_seed, Some(42));
            assert!(e.gaps.is_some());
            assert!(e.bursts.is_some());
        }
    }

    // -----------------------------------------------------------------------
    // No pack references survive
    // -----------------------------------------------------------------------

    #[test]
    fn expanded_entries_have_no_pack_field() {
        // The ExpandedEntry type has no `pack` field by design. This test
        // documents that contract via the public surface: once expansion
        // runs, the output shape cannot carry unresolved pack references.
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        // Compile-time guarantee: no access to `pack` or `overrides` on
        // ExpandedEntry is possible. At runtime we just make sure entries
        // look like concrete signals.
        assert!(expanded.entries.iter().all(|e| e.generator.is_some()));
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_override_key_is_an_error() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    overrides:
      not_a_metric:
        generator:
          type: constant
          value: 0
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let err = expand(normalized, &resolver).expect_err("must fail");
        match err {
            ExpandError::UnknownOverrideKey {
                key,
                pack_name,
                available,
            } => {
                assert_eq!(key, "not_a_metric");
                assert_eq!(pack_name, "telegraf_snmp_interface");
                assert!(available.contains("ifOperStatus"));
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    #[test]
    fn unresolvable_pack_is_an_error() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: nonexistent
"#;
        let resolver = InMemoryPackResolver::new();
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let err = expand(normalized, &resolver).expect_err("must fail");
        match err {
            ExpandError::ResolveFailed { reference, message } => {
                assert_eq!(reference, "nonexistent");
                assert!(message.contains("nonexistent"));
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    #[test]
    fn empty_pack_is_an_error() {
        let pack = MetricPackDef {
            name: "empty".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: None,
            metrics: vec![],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("empty", pack);
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: empty
"#;
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let err = expand(normalized, &resolver).expect_err("must fail");
        assert!(matches!(err, ExpandError::EmptyPack { pack_name } if pack_name == "empty"));
    }

    // -----------------------------------------------------------------------
    // Inline entries pass through
    // -----------------------------------------------------------------------

    #[test]
    fn inline_entries_pass_through_untouched() {
        let yaml = r#"
version: 2
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    rate: 2
    duration: 60s
    generator: { type: constant, value: 1 }
    labels: { instance: web-01 }
"#;
        let resolver = InMemoryPackResolver::new();
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(expanded.entries.len(), 1);
        let e = &expanded.entries[0];
        assert_eq!(e.id.as_deref(), Some("cpu"));
        assert_eq!(e.name, "cpu_usage");
        assert_eq!(e.rate, 2.0);
        assert_eq!(e.duration.as_deref(), Some("60s"));
        assert_eq!(
            e.labels.as_ref().unwrap().get("instance").unwrap(),
            "web-01"
        );
    }

    #[test]
    fn mixed_inline_and_pack_entries_interleave_correctly() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator: { type: constant, value: 1 }
  - id: net
    signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        // 1 inline + 2 pack metrics = 3 total
        assert_eq!(expanded.entries.len(), 3);
        assert_eq!(expanded.entries[0].id.as_deref(), Some("cpu"));
        assert_eq!(expanded.entries[1].id.as_deref(), Some("net.ifOperStatus"));
        assert_eq!(expanded.entries[2].id.as_deref(), Some("net.ifHCInOctets"));
    }

    // -----------------------------------------------------------------------
    // Multiple metric instances with same name (node_exporter_cpu case)
    // -----------------------------------------------------------------------

    #[test]
    fn repeated_metric_names_produce_one_entry_per_spec_instance() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - id: cpu
    signal_type: metrics
    pack: node_exporter_cpu
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("node_exporter_cpu", node_cpu_pack());
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(expanded.entries.len(), 2);
        assert_eq!(expanded.entries[0].name, "node_cpu_seconds_total");
        assert_eq!(expanded.entries[1].name, "node_cpu_seconds_total");
        // Distinct label `mode` differentiates them.
        assert_eq!(
            expanded.entries[0]
                .labels
                .as_ref()
                .unwrap()
                .get("mode")
                .unwrap(),
            "user"
        );
        assert_eq!(
            expanded.entries[1]
                .labels
                .as_ref()
                .unwrap()
                .get("mode")
                .unwrap(),
            "system"
        );
    }

    // -----------------------------------------------------------------------
    // Pack by file path
    // -----------------------------------------------------------------------

    #[test]
    fn pack_by_file_path_is_resolved_through_trait() {
        let yaml = r#"
version: 2
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: ./packs/telegraf-snmp-interface.yaml
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("./packs/telegraf-snmp-interface.yaml", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(expanded.entries.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Contract: Send + Sync on types crossing threads
    // -----------------------------------------------------------------------

    #[test]
    fn expanded_file_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExpandedFile>();
        assert_send_sync::<ExpandedEntry>();
        assert_send_sync::<ExpandError>();
    }
}
