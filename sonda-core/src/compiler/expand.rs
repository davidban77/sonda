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
//! scope here. Phase 2 deliberately left pack entry labels *unmerged* with
//! `defaults.labels` so this pass can interleave levels 4 and 5 between
//! them.
//!
//! An extension's own `shared_labels` sit between its base's and level 5:
//! base, then each extension outward, then per-metric. That ordering is not
//! a case in the composer — [`crate::packs::extend::materialize`] merges the
//! maps as it folds the chain, so level 4 is one map by the time this pass
//! sees it, and level 5 is read post-deviation for the same reason. The
//! chain is resolved before composition begins.
//!
//! Inline entries do **not** re-apply `defaults_labels`: Phase 2 already
//! merged them eagerly and we must not double-apply. Inline entries are
//! copied through with their label map intact.
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
//! After synthesis, a post-expansion uniqueness check runs over every
//! effective pack-entry id *and* every emitted [`ExpandedEntry::id`]:
//! collisions between user-authored ids and auto-generated ids (or between
//! two pack sub-signals) are rejected via
//! [`ExpandError::DuplicateEntryId`]. The parser's id uniqueness pass only
//! sees user-provided ids, so this pass closes the gap.
//!
//! ## Sub-signal IDs and duplicate metric names
//!
//! When a pack's metrics are unique by name (the common case), the per-metric
//! sub-signal id takes the form `"{effective_entry_id}.{metric_name}"` —
//! e.g. the `telegraf_snmp_interface` pack produces
//! `net.ifOperStatus`, `net.ifHCInOctets`, etc.
//!
//! When two or more [`MetricSpec`][crate::packs::MetricSpec] entries in a
//! single pack share a `name` (the `node_exporter_cpu` pack ships eight
//! `node_cpu_seconds_total` specs differentiated only by `labels.mode`), the
//! bare `{effective_entry_id}.{metric_name}` id would collide. Such a pack
//! must give every one of those specs a unique `id:` —
//! [`crate::packs::validate_pack`] refuses it otherwise — and the sub-signal
//! id is then `{effective_entry_id}.{name}.{id}`, e.g.
//! `cpu.node_cpu_seconds_total.user`. Unique metric names keep their clean
//! form, so a dotted `after.ref` into a pack sub-signal (matrix row 11.7)
//! stays ergonomic for the majority of packs.
//!
//! The id is the spec's own handle rather than its position: an earlier
//! scheme suffixed `"#{spec_index}"`, which renumbered every later spec when
//! one was inserted ahead of it.
//!
//! ## Worked example
//!
//! Given a pack entry written as:
//!
//! ```yaml
//! scenarios:
//!   - signal_type: metrics      # no `id:`, anonymous entry at index 0
//!     pack: telegraf_snmp_interface
//! ```
//!
//! and assuming `telegraf_snmp_interface` contains four metrics
//! (`ifOperStatus`, `ifHCInOctets`, `ifHCOutOctets`, `ifInErrors`), this pass
//! emits four [`ExpandedEntry`]s with the following ids:
//!
//! | `id` | derivation |
//! |------|------------|
//! | `telegraf_snmp_interface_0.ifOperStatus` | auto pack-entry id + metric name |
//! | `telegraf_snmp_interface_0.ifHCInOctets` | auto pack-entry id + metric name |
//! | `telegraf_snmp_interface_0.ifHCOutOctets` | auto pack-entry id + metric name |
//! | `telegraf_snmp_interface_0.ifInErrors` | auto pack-entry id + metric name |
//!
//! If the same pack entry had a user-provided `id: primary`, the ids above
//! would instead read `primary.ifOperStatus`, `primary.ifHCInOctets`, and so
//! on.
//!
//! # Field propagation (parent pack entry → expanded metric)
//!
//! Spec §4.3 step 7 lists the fields that propagate from a pack entry to
//! each expanded signal. The full set is wider than the spec's illustrative
//! list; this pass copies the following fields from the parent
//! [`NormalizedEntry`] onto every emitted [`ExpandedEntry`]:
//!
//! | Field | Propagation rule |
//! |-------|------------------|
//! | `rate` | copied verbatim (inherited from defaults in Phase 2) |
//! | `duration` | copied verbatim |
//! | `encoder` | copied verbatim |
//! | `sink` | copied verbatim |
//! | `jitter`, `jitter_seed` | copied verbatim |
//! | `gaps` | cloned verbatim |
//! | `bursts` | cloned verbatim |
//! | `cardinality_spikes` | cloned verbatim |
//! | `dynamic_labels` | cloned verbatim |
//! | `phase_offset` | cloned verbatim |
//! | `clock_group` | cloned verbatim |
//! | `after` | per-metric override `after` wins, else parent entry `after` (see below) |
//!
//! Per-metric override fields in [`MetricOverride`] (`generator`, `labels`,
//! `after`) replace or layer on top of the parent's values as documented
//! above and in the label precedence chain. No other fields on a
//! [`MetricOverride`] exist today; adding one requires both extending
//! [`MetricOverride`] and teaching this pass to propagate it.
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
//! - pack definitions with no metrics,
//! - duplicate entry ids after synthesis (user-authored id colliding with
//!   an auto-generated pack-entry id, or sub-signal ids colliding with one
//!   another).

use std::collections::BTreeMap;

use super::normalize::{NormalizedEntry, NormalizedFile};
use super::{AfterClause, DelayClause, WhileClause};
use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
    GapWindowConfig, OnSinkError,
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
#[non_exhaustive]
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

    /// The pack itself is not addressable: a repeated metric name without
    /// unique ids, or a metric name containing the selector separator.
    ///
    /// Raised before any selector is resolved, so no reference can encounter
    /// an ambiguous pack.
    #[error("pack '{pack_name}' is not addressable: {message}")]
    InvalidPack {
        /// The pack definition name that failed validation.
        pack_name: String,
        /// The [`crate::packs::PackValidationError`] rendered.
        message: String,
    },

    /// An override key did not address exactly one metric spec.
    ///
    /// Either it matched nothing, or it was a bare name against a metric
    /// name the pack repeats — which previously applied the override to
    /// every spec sharing that name.
    #[error("override {message}")]
    UnresolvableOverrideKey {
        /// The pack definition name that was being expanded.
        pack_name: String,
        /// The [`crate::packs::SelectorError`] rendered.
        message: String,
    },

    /// A pack's `extends:` named a base the resolver could not produce.
    ///
    /// Separate from [`ExpandError::ResolveFailed`] because the reference
    /// that failed is not the one the scenario names: the reader has to go
    /// to the extending pack to fix it.
    #[error("pack '{by}' extends '{base}', which could not be resolved: {message}")]
    ExtendsUnresolved {
        /// The base reference as `extends:` wrote it.
        base: String,
        /// The pack whose `extends:` named it.
        by: String,
        /// The resolver's own diagnostic.
        message: String,
    },

    /// The `extends:` chain returns to a pack it already passed through.
    #[error("extension cycle: {}", .cycle.join(" -> "))]
    ExtensionCycle {
        /// The loop as references, starting and ending at the pack that
        /// closes it.
        cycle: Vec<String>,
    },

    /// An extension could not be materialized against its base.
    #[error("{message}")]
    ExtendFailed {
        /// The extension being materialized.
        pack_name: String,
        /// The [`crate::packs::extend::ExtendError`] rendered.
        message: String,
    },

    /// A pack declared `deviations:` with no `extends:` to deviate from.
    ///
    /// Only packs above the root are folded, so such a block would never be
    /// applied and never mentioned — the silent no-op this feature refuses
    /// everywhere else.
    #[error(
        "pack '{pack_name}' declares {count} deviation(s) but no `extends:`; \
         there is no base to deviate from"
    )]
    DeviationsWithoutExtends {
        /// The pack that declared them.
        pack_name: String,
        /// How many, so the message does not send the reader hunting for one.
        count: usize,
    },

    /// Two override keys address the same metric spec.
    ///

    /// A spec whose name is unique *and* which declares an `id:` answers to
    /// both `name` and `name.id`. Writing both would apply one and discard
    /// the other without a diagnostic.
    #[error("override {message}")]
    ConflictingOverrideKeys {
        /// The pack definition name that was being expanded.
        pack_name: String,
        /// The [`crate::packs::OverrideKeyError`] rendered.
        message: String,
    },

    /// Two entries ended up with the same identifier after pack expansion.
    ///
    /// The parser already rejects duplicate **user-provided** ids, but this
    /// pass synthesizes ids for anonymous pack entries (see the module docs'
    /// "Auto-generated pack entry IDs" section) and composes sub-signal ids
    /// of the form `"{effective_entry_id}.{metric_name}"`. Those synthesized
    /// ids can clash with user-authored ids or with one another; such
    /// collisions are detected here so later phases (e.g. the Phase 4
    /// reference index) see a unique id space.
    ///
    /// The `first_source` / `second_source` fields describe where each
    /// collider originated so the diagnostic points at both contributors.
    #[error(
        "duplicate entry id '{id}' after pack expansion: \
         {first_source} conflicts with {second_source}"
    )]
    DuplicateEntryId {
        /// The colliding identifier.
        id: String,
        /// Description of the first source that produced the id.
        first_source: String,
        /// Description of the second source that produced the same id.
        second_source: String,
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
    /// File-level `scenario_name` carried verbatim. Pure metadata —
    /// ignored by every compiler phase, surfaced for runtime conflict checks.
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub scenario_name: Option<String>,
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
    /// Emission-time anchor (absolute RFC 3339, signed offset, or `now`).
    pub start_time: Option<String>,
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
    /// One-shot silent windows at fixed offsets from scenario start.
    pub gap_windows: Option<Vec<GapWindowConfig>>,
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
    /// propagated verbatim. Resolution into timing offsets is Phase 4's job.
    pub after: Option<AfterClause>,
    /// Continuous lifecycle gate on another signal's value.
    ///
    /// Override-level `while:` replaces entry-level `while:` for that
    /// metric; otherwise the parent's `while:` is propagated verbatim.
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub while_clause: Option<WhileClause>,
    /// Open / close debounce windows for `while_clause` transitions.
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub delay_clause: Option<DelayClause>,

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
    /// Resolved sink-error policy.
    pub on_sink_error: OnSinkError,

    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub metric_type: Option<crate::config::PromMetricType>,
    #[cfg_attr(feature = "config", serde(skip_serializing_if = "Option::is_none"))]
    pub help: Option<String>,
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
/// Id uniqueness — including collisions between user-provided ids and
/// auto-synthesized pack-entry ids — is enforced after expansion; the parser
/// only validates user-provided ids.
///
/// # Errors
///
/// - [`ExpandError::ResolveFailed`] when the resolver cannot produce a
///   [`MetricPackDef`] for a pack reference.
/// - [`ExpandError::UnknownOverrideKey`] when an override targets a metric
///   that is not present in the resolved pack definition.
/// - [`ExpandError::EmptyPack`] when the resolved pack has no metrics.
/// - [`ExpandError::DuplicateEntryId`] when two entries end up with the
///   same identifier after synthesis (e.g. a user-authored inline id
///   colliding with an auto-generated pack-entry id, or two sub-signal
///   ids composing to the same string).
pub fn expand<R: PackResolver>(
    file: NormalizedFile,
    resolver: &R,
) -> Result<ExpandedFile, ExpandError> {
    let defaults_labels = file.defaults_labels;
    let mut entries: Vec<ExpandedEntry> = Vec::with_capacity(file.entries.len());
    // Collects every id that occupies the signal-id namespace so we can
    // catch collisions between user-authored ids, synthesized pack-entry
    // ids, and pack sub-signal ids in a single pass.
    let mut id_registry: BTreeMap<String, String> = BTreeMap::new();

    for (index, entry) in file.entries.into_iter().enumerate() {
        if entry.pack.is_some() {
            expand_pack_entry(
                entry,
                index,
                defaults_labels.as_ref(),
                resolver,
                &mut entries,
                &mut id_registry,
            )?;
        } else {
            let expanded = expand_inline_entry(entry);
            if let Some(id) = expanded.id.as_ref() {
                record_id(&mut id_registry, id, format!("inline entry '{id}'"))?;
            }
            entries.push(expanded);
        }
    }

    Ok(ExpandedFile {
        version: file.version,
        scenario_name: file.scenario_name,
        entries,
    })
}

/// Insert an identifier into the post-expansion uniqueness registry.
///
/// Returns [`ExpandError::DuplicateEntryId`] if `id` was already registered,
/// tagging both the previous and current source so the diagnostic points at
/// both contributors.
fn record_id(
    registry: &mut BTreeMap<String, String>,
    id: &str,
    source: String,
) -> Result<(), ExpandError> {
    if let Some(prior) = registry.get(id) {
        return Err(ExpandError::DuplicateEntryId {
            id: id.to_string(),
            first_source: prior.clone(),
            second_source: source,
        });
    }
    registry.insert(id.to_string(), source);
    Ok(())
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
        start_time: entry.start_time,
        generator: entry.generator,
        log_generator: entry.log_generator,
        labels: entry.labels,
        dynamic_labels: entry.dynamic_labels,
        encoder: entry.encoder,
        sink: entry.sink,
        jitter: entry.jitter,
        jitter_seed: entry.jitter_seed,
        gaps: entry.gaps,
        gap_windows: entry.gap_windows,
        bursts: entry.bursts,
        cardinality_spikes: entry.cardinality_spikes,
        phase_offset: entry.phase_offset,
        clock_group: entry.clock_group,
        after: entry.after,
        while_clause: entry.while_clause,
        delay_clause: entry.delay_clause,
        distribution: entry.distribution,
        buckets: entry.buckets,
        quantiles: entry.quantiles,
        observations_per_tick: entry.observations_per_tick,
        mean_shift_per_sec: entry.mean_shift_per_sec,
        on_sink_error: entry.on_sink_error,
        seed: entry.seed,
        metric_type: entry.metric_type,
        help: entry.help,
    }
}

// ---------------------------------------------------------------------------
// Pack expansion
// ---------------------------------------------------------------------------

/// Expand a single pack-backed [`NormalizedEntry`] into one [`ExpandedEntry`]
/// per metric in the resolved pack, appending to `out` and tracking every
/// produced id in `id_registry` for the post-expansion uniqueness check.
fn expand_pack_entry<R: PackResolver>(
    entry: NormalizedEntry,
    entry_index: usize,
    defaults_labels: Option<&BTreeMap<String, String>>,
    resolver: &R,
    out: &mut Vec<ExpandedEntry>,
    id_registry: &mut BTreeMap<String, String>,
) -> Result<(), ExpandError> {
    // `entry.pack` is Some() by the caller's check; unwrap defensively.
    let reference = entry
        .pack
        .as_deref()
        .expect("expand_pack_entry called with non-pack entry; caller must check");

    let pack = resolve_pack_chain(reference, resolver)?;

    if pack.metrics.is_empty() {
        return Err(ExpandError::EmptyPack {
            pack_name: pack.name,
        });
    }

    let override_indices = resolve_override_selectors(&pack, entry.overrides.as_ref())?;

    let (effective_entry_id, effective_id_source) = match entry.id.clone() {
        Some(id) => (id.clone(), format!("pack entry '{id}' (user-provided id)")),
        None => {
            let synthesized = format!("{}_{}", pack.name, entry_index);
            (
                synthesized.clone(),
                format!(
                    "pack entry at index {entry_index} (auto-generated id '{synthesized}' \
                     from pack '{}')",
                    pack.name
                ),
            )
        }
    };

    // The effective pack-entry id occupies the signal-id namespace even
    // though no single `ExpandedEntry` carries it verbatim: its sub-signal
    // ids live underneath (e.g. `{effective_entry_id}.{metric_name}`) and a
    // future `after.ref` targeting `effective_entry_id` would resolve into
    // this namespace. Register it so user-authored ids cannot silently
    // shadow an auto-generated pack-entry id and vice versa.
    record_id(id_registry, &effective_entry_id, effective_id_source)?;

    for (spec_index, metric) in pack.metrics.iter().enumerate() {
        let override_for_metric = override_indices
            .get(&spec_index)
            .and_then(|key| entry.overrides.as_ref().and_then(|map| map.get(*key)));

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
        // Phase 4's job.
        let after = override_for_metric
            .and_then(|o| o.after.clone())
            .or_else(|| entry.after.clone());
        let while_clause = override_for_metric
            .and_then(|o| o.while_clause.clone())
            .or_else(|| entry.while_clause.clone());
        let delay_clause = override_for_metric
            .and_then(|o| o.delay_clause.clone())
            .or_else(|| entry.delay_clause.clone());

        // `{entry}.{selector}` — the selector being `name` or `name.id`. The
        // spec's own selector is the handle, so the id a user writes in an
        // override is the id they see in the compiled signal. This replaces
        // the positional `#{spec_index}` suffix, which renumbered whenever a
        // spec was inserted into the pack ahead of it.
        let sub_signal_id = format!("{}.{}", effective_entry_id, metric.selector());
        record_id(
            id_registry,
            &sub_signal_id,
            format!(
                "pack sub-signal '{sub_signal_id}' (pack '{}', metric '{}' at index {spec_index})",
                pack.name, metric.name
            ),
        )?;

        out.push(ExpandedEntry {
            id: Some(sub_signal_id),
            signal_type: "metrics".to_string(),
            name: metric.name.clone(),
            rate: entry.rate,
            duration: entry.duration.clone(),
            start_time: entry.start_time.clone(),
            generator: Some(generator),
            log_generator: None,
            labels,
            dynamic_labels: entry.dynamic_labels.clone(),
            encoder: entry.encoder.clone(),
            sink: entry.sink.clone(),
            jitter: entry.jitter,
            jitter_seed: entry.jitter_seed,
            gaps: entry.gaps.clone(),
            gap_windows: entry.gap_windows.clone(),
            bursts: entry.bursts.clone(),
            cardinality_spikes: entry.cardinality_spikes.clone(),
            phase_offset: entry.phase_offset.clone(),
            clock_group: entry.clock_group.clone(),
            after,
            while_clause,
            delay_clause,
            distribution: None,
            buckets: None,
            quantiles: None,
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
            on_sink_error: entry.on_sink_error,
            metric_type: entry.metric_type,
            help: entry.help.clone(),
        });
    }

    Ok(())
}

/// Resolve `reference` and its whole `extends:` chain into one effective pack.
///
/// Walks base-ward collecting the chain, then folds it leaf-ward with
/// [`crate::packs::extend::materialize`], validating each link's
/// addressability before the next one resolves selectors against it. The
/// returned pack is indistinguishable from a hand-written one, which is why
/// nothing downstream of here knows that packs can extend.
///
/// # On the cycle check
///
/// `extends:` takes one value, so the chain is a path and not a graph: a
/// cycle is a reference appearing twice on the path being walked, which is
/// what the visited set below detects. The design called for reusing
/// `compile_after`'s Kahn + DFS machinery; that machinery exists because
/// `after:` chains branch, and with out-degree one its gray-node bookkeeping
/// degenerates to exactly this set. Generalizing it would have added a type
/// parameter to a working file to express the same thing.
///
/// # Errors
///
/// - [`ExpandError::ResolveFailed`] for the leaf reference, and
///   [`ExpandError::ExtendsUnresolved`] for a base named by `extends:` —
///   distinct because "your `pack:` is wrong" and "a pack you referenced
///   names a base that is missing" send the reader to different files.
/// - [`ExpandError::ExtensionCycle`] naming the loop.
/// - [`ExpandError::InvalidPack`] when any link, or the materialized result,
///   is not addressable.
/// - [`ExpandError::ExtendFailed`] for the [`crate::packs::extend`] errors.
fn resolve_pack_chain<R: PackResolver>(
    reference: &str,
    resolver: &R,
) -> Result<MetricPackDef, ExpandError> {
    let leaf = resolver
        .resolve(reference)
        .map_err(|e| ExpandError::ResolveFailed {
            reference: reference.to_string(),
            message: e.message,
        })?;

    // Leaf first; `chain` ends at the root.
    let mut chain = vec![leaf];
    let mut visited: Vec<String> = vec![reference.to_string()];

    while let Some(base_ref) = chain.last().expect("chain is never empty").extends.clone() {
        if let Some(at) = visited.iter().position(|seen| *seen == base_ref) {
            // Render the loop from where it closes, in the direction the
            // user reads `extends:`, and repeat the entry point so the
            // cycle is visibly closed.
            let mut cycle: Vec<String> = visited[at..].to_vec();
            cycle.push(base_ref);
            return Err(ExpandError::ExtensionCycle { cycle });
        }
        let base = resolver
            .resolve(&base_ref)
            .map_err(|e| ExpandError::ExtendsUnresolved {
                base: base_ref.clone(),
                by: chain.last().expect("chain is never empty").name.clone(),
                message: e.message,
            })?;
        visited.push(base_ref);
        chain.push(base);
    }

    // Fold root-ward-to-leaf: each extension is materialized against the
    // pack accumulated so far.
    let mut resolved = chain.pop().expect("chain is never empty");
    // The root has nothing to deviate from, and only packs *above* the root
    // are folded — so a root's own `deviations:` would never be looked at.
    // A deviation that does nothing is the no-op this feature refuses
    // everywhere else, so it is refused here too rather than dropped.
    if !resolved.deviations.is_empty() {
        return Err(ExpandError::DeviationsWithoutExtends {
            pack_name: resolved.name.clone(),
            count: resolved.deviations.len(),
        });
    }
    validate_addressable(&resolved)?;
    while let Some(extension) = chain.pop() {
        let extension_name = extension.name.clone();
        resolved = crate::packs::extend::materialize(&extension, &resolved).map_err(|e| {
            ExpandError::ExtendFailed {
                pack_name: extension_name,
                message: e.to_string(),
            }
        })?;
        validate_addressable(&resolved)?;
    }
    Ok(resolved)
}

/// Every spec must be addressable by exactly one selector before anything
/// tries to address one. This is what lets `resolve_selector` treat
/// ambiguity as impossible rather than re-deriving the rule — and it runs on
/// each materialized step too, since two added metrics can collide with each
/// other in a way no single link exhibits.
fn validate_addressable(pack: &MetricPackDef) -> Result<(), ExpandError> {
    crate::packs::validate_pack(pack).map_err(|e| ExpandError::InvalidPack {
        pack_name: pack.name.clone(),
        message: e.to_string(),
    })
}

/// Resolve each override key to the one spec index it addresses, mapping
/// [`crate::packs::resolve_override_keys`]'s outcome onto this pass's errors.
///
/// The keying rule itself lives in `packs` so this path and the v1
/// [`crate::packs::expand_pack`] path cannot drift apart.
fn resolve_override_selectors<'a>(
    pack: &MetricPackDef,
    overrides: Option<&'a BTreeMap<String, MetricOverride>>,
) -> Result<BTreeMap<usize, &'a str>, ExpandError> {
    let Some(overrides) = overrides else {
        return Ok(BTreeMap::new());
    };

    crate::packs::resolve_override_keys(pack, overrides.keys().map(String::as_str)).map_err(|e| {
        match e {
            // A key naming a metric the pack does not have keeps the older,
            // more specific diagnostic — it is a different mistake from
            // naming one ambiguously.
            crate::packs::OverrideKeyError::Unresolvable(
                crate::packs::SelectorError::NoMatch {
                    selector,
                    available,
                    ..
                },
            ) => ExpandError::UnknownOverrideKey {
                key: selector,
                pack_name: pack.name.clone(),
                available,
            },
            crate::packs::OverrideKeyError::Unresolvable(ambiguous) => {
                ExpandError::UnresolvableOverrideKey {
                    pack_name: pack.name.clone(),
                    message: ambiguous.to_string(),
                }
            }
            conflict @ crate::packs::OverrideKeyError::Conflict { .. } => {
                ExpandError::ConflictingOverrideKeys {
                    pack_name: pack.name.clone(),
                    message: conflict.to_string(),
                }
            }
        }
    })
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
            extends: None,
            deviations: Vec::new(),
            name: "telegraf_snmp_interface".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: Some(shared),
            metrics: vec![
                MetricSpec {
                    name: "ifOperStatus".to_string(),
                    id: None,
                    labels: None,
                    generator: Some(GeneratorConfig::Constant { value: 1.0 }),
                },
                MetricSpec {
                    name: "ifHCInOctets".to_string(),
                    id: None,
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
            extends: None,
            deviations: Vec::new(),
            name: "node_exporter_cpu".to_string(),
            description: "test".to_string(),
            category: "infrastructure".to_string(),
            shared_labels: Some(shared),
            metrics: vec![
                MetricSpec {
                    name: "node_cpu_seconds_total".to_string(),
                    id: Some("user".to_string()),
                    labels: Some(user_labels),
                    generator: Some(GeneratorConfig::Step {
                        start: Some(0.0),
                        step_size: 0.25,
                        max: None,
                    }),
                },
                MetricSpec {
                    name: "node_cpu_seconds_total".to_string(),
                    id: Some("system".to_string()),
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

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::plain_name("telegraf_snmp_interface",  PackResolveOrigin::Name)]
    #[case::dot_relative("./packs/custom.yaml",    PackResolveOrigin::FilePath)]
    #[case::absolute_path("/abs/path/pack.yaml",   PackResolveOrigin::FilePath)]
    #[case::plain_relative("rel/pack.yaml",        PackResolveOrigin::FilePath)]
    fn classify_pack_reference_distinguishes_name_and_file_path(
        #[case] reference: &str,
        #[case] expected: PackResolveOrigin,
    ) {
        assert_eq!(classify_pack_reference(reference), expected);
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
kind: runnable
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
kind: runnable
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

    #[rustfmt::skip]
    #[rstest::rstest]
    // User-supplied id becomes the effective entry id; sub-signal ids use
    // the clean `{entry_id}.{metric}` shape.
    #[case::user_supplied_entry_id(r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: primary
    signal_type: metrics
    pack: telegraf_snmp_interface
"#, "primary.ifOperStatus", "primary.ifHCInOctets")]
    // Anonymous pack entries use the auto-id scheme
    // `{pack_def_name}_{entry_index}`, so at index 0 the effective id is
    // `telegraf_snmp_interface_0`.
    #[case::auto_generated_entry_id(r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#, "telegraf_snmp_interface_0.ifOperStatus", "telegraf_snmp_interface_0.ifHCInOctets")]
    fn sub_signal_ids_follow_effective_entry_id(
        #[case] yaml: &str,
        #[case] expected_first: &str,
        #[case] expected_second: &str,
    ) {
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        assert_eq!(expanded.entries[0].id.as_deref(), Some(expected_first));
        assert_eq!(expanded.entries[1].id.as_deref(), Some(expected_second));
    }

    #[test]
    fn two_anonymous_pack_entries_disambiguate_by_index() {
        let yaml = r#"
version: 2
kind: runnable
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
            extends: None,
            deviations: Vec::new(),
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                id: None,
                labels: Some(metric_labels),
                generator: Some(GeneratorConfig::Constant { value: 0.0 }),
            }],
        };

        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
kind: runnable
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
kind: runnable
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
            extends: None,
            deviations: Vec::new(),
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: Some(shared),
            metrics: vec![MetricSpec {
                name: "m".to_string(),
                id: None,
                labels: None,
                generator: Some(GeneratorConfig::Constant { value: 0.0 }),
            }],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
kind: runnable
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
        // Inline entries must NOT re-apply defaults_labels; Phase 2 already
        // merged them. Verify that exactly the merged set from normalize
        // shows up here — not doubled, not missing a defaults key.
        let yaml = r#"
version: 2
kind: runnable
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
kind: runnable
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
            extends: None,
            deviations: Vec::new(),
            name: "p".to_string(),
            description: "t".to_string(),
            category: "c".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "x".to_string(),
                id: None,
                labels: None,
                generator: None,
            }],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("p", pack);

        let yaml = r#"
version: 2
kind: runnable
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
kind: runnable
defaults: { rate: 1, duration: 5m }
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
    fn entry_level_while_propagates_to_every_metric() {
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1, duration: 5m }
scenarios:
  - id: head
    signal_type: metrics
    name: head
    generator: { type: constant, value: 1 }
  - id: tail
    signal_type: metrics
    pack: telegraf_snmp_interface
    while:
      ref: head
      op: ">"
      value: 5
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        let pack_subs: Vec<_> = expanded
            .entries
            .iter()
            .filter(|e| {
                e.id.as_deref()
                    .map(|s| s.starts_with("tail."))
                    .unwrap_or(false)
            })
            .collect();
        assert!(!pack_subs.is_empty());
        for e in pack_subs {
            let w = e.while_clause.as_ref().expect("while must be propagated");
            assert_eq!(w.ref_id, "head");
        }
    }

    #[test]
    fn override_while_replaces_entry_while_for_that_metric() {
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1, duration: 5m }
scenarios:
  - id: head
    signal_type: metrics
    name: head
    generator: { type: constant, value: 1 }
  - id: other
    signal_type: metrics
    name: other
    generator: { type: constant, value: 1 }
  - id: tail
    signal_type: metrics
    pack: telegraf_snmp_interface
    while:
      ref: head
      op: ">"
      value: 5
    overrides:
      ifOperStatus:
        while:
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
        assert_eq!(oper.while_clause.as_ref().unwrap().ref_id, "other");
        let in_octets = expanded
            .entries
            .iter()
            .find(|e| e.name == "ifHCInOctets")
            .unwrap();
        assert_eq!(in_octets.while_clause.as_ref().unwrap().ref_id, "head");
    }

    #[test]
    fn override_after_replaces_entry_after_for_that_metric() {
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1, duration: 5m }
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
kind: runnable
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
kind: runnable
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
kind: runnable
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
kind: runnable
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
            extends: None,
            deviations: Vec::new(),
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
kind: runnable
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
kind: runnable
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
kind: runnable
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
kind: runnable
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

    #[test]
    fn repeated_metric_names_produce_unique_sub_signal_ids() {
        // Regression anchor: every ExpandedEntry.id must be unique even
        // when a pack ships multiple MetricSpec entries under one name
        // (e.g. node_exporter_cpu). Such a pack must give every one of those
        // specs a unique `id:`, and that id is the sub-signal handle.
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: cpu
    signal_type: metrics
    pack: node_exporter_cpu
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("node_exporter_cpu", node_cpu_pack());
        let expanded = expand_yaml(yaml, &resolver);

        let ids: Vec<&str> = expanded
            .entries
            .iter()
            .map(|e| {
                e.id.as_deref()
                    .expect("pack-expanded entries always carry an id")
            })
            .collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            ids.len(),
            "sub-signal ids must be unique; saw {ids:?}"
        );

        // Exact id shape: the spec's own `id:` is the handle, so inserting a
        // spec ahead of these two would not renumber them.
        assert_eq!(ids[0], "cpu.node_cpu_seconds_total.user");
        assert_eq!(ids[1], "cpu.node_cpu_seconds_total.system");
    }

    #[test]
    fn unique_metric_names_keep_clean_sub_signal_ids() {
        // The `.{id}` suffix appears only for a spec that declares one —
        // which a repeated name requires. Packs whose
        // metrics are unique by name (like telegraf_snmp_interface) keep
        // the clean `{effective_entry_id}.{metric_name}` form so dotted
        // `after.ref` into a pack sub-signal stays ergonomic.
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: net
    signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);

        let ids: Vec<&str> = expanded
            .entries
            .iter()
            .filter_map(|e| e.id.as_deref())
            .collect();
        assert_eq!(ids, vec!["net.ifOperStatus", "net.ifHCInOctets"]);
    }

    // -----------------------------------------------------------------------
    // Post-expansion id uniqueness (user-provided vs. auto-synthesized)
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    // Reviewer-described case: the user writes an inline id that equals
    // what the anonymous pack entry at the next position would synthesize.
    // The parser's id uniqueness pass never sees the synthesized id, so
    // this pass must catch the collision.
    #[case::inline_first_then_auto(r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: telegraf_snmp_interface_1
    signal_type: metrics
    name: cpu
    generator: { type: constant, value: 1 }
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#, "telegraf_snmp_interface_1", "inline entry", "auto-generated")]
    // Reverse ordering: anonymous pack entry comes first, user-written id
    // collides with the synthesized name afterward. The registry must flag
    // the collision regardless of source order.
    #[case::auto_first_then_inline(r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
  - id: telegraf_snmp_interface_0
    signal_type: metrics
    name: cpu
    generator: { type: constant, value: 1 }
"#, "telegraf_snmp_interface_0", "auto-generated", "inline entry")]
    fn duplicate_entry_id_detected_regardless_of_source_order(
        #[case] yaml: &str,
        #[case] expected_id: &str,
        #[case] expected_first_substr: &str,
        #[case] expected_second_substr: &str,
    ) {
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let err = expand(normalized, &resolver).expect_err("must fail");
        match err {
            ExpandError::DuplicateEntryId {
                id,
                first_source,
                second_source,
            } => {
                assert_eq!(id, expected_id);
                assert!(
                    first_source.contains(expected_first_substr),
                    "unexpected first source: {first_source}"
                );
                assert!(
                    second_source.contains(expected_second_substr),
                    "unexpected second source: {second_source}"
                );
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    #[test]
    fn duplicate_entry_id_error_preserves_both_sources() {
        // The diagnostic must name both contributors so users can locate
        // each side of the collision. Parser-level id validation rejects
        // `.` and `#` in user ids, so the only reachable collisions travel
        // between inline ids and synthesized pack-entry ids; both sources
        // appear in the error regardless of document order.
        //
        // The pack entry here sits at index 1, so its auto-id is
        // `telegraf_snmp_interface_1`; the inline entry claims that id
        // first.
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: telegraf_snmp_interface_1
    signal_type: metrics
    name: cpu
    generator: { type: constant, value: 1 }
  - signal_type: metrics
    pack: telegraf_snmp_interface
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let err = expand(normalized, &resolver).expect_err("must fail");
        let rendered = err.to_string();
        assert!(
            rendered.contains("'telegraf_snmp_interface_1'"),
            "error must name the colliding id: {rendered}"
        );
        assert!(
            rendered.contains("inline entry"),
            "error must name the inline source: {rendered}"
        );
        assert!(
            rendered.contains("auto-generated"),
            "error must name the auto-generated source: {rendered}"
        );
    }

    // -----------------------------------------------------------------------
    // Pack by file path
    // -----------------------------------------------------------------------

    #[test]
    fn pack_by_file_path_is_resolved_through_trait() {
        let yaml = r#"
version: 2
kind: runnable
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

    // -----------------------------------------------------------------------
    // Selector-keyed overrides (W4 phase 1b)
    // -----------------------------------------------------------------------

    const CPU_SCENARIO: &str = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: cpu
    signal_type: metrics
    pack: node_exporter_cpu
    overrides:
      SELECTOR:
        generator: { type: constant, value: 12345.0 }
"#;

    fn expand_cpu_with_override(selector: &str) -> Result<ExpandedFile, ExpandError> {
        let yaml = CPU_SCENARIO.replace("SELECTOR", selector);
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("node_exporter_cpu", node_cpu_pack());
        let parsed = parse(&yaml).expect("parse must succeed");
        let normalized = normalize(parsed).expect("normalize must succeed");
        expand(normalized, &resolver)
    }

    fn generators(file: &ExpandedFile) -> Vec<Option<&GeneratorConfig>> {
        file.entries.iter().map(|e| e.generator.as_ref()).collect()
    }

    /// The measured defect: `node_cpu_seconds_total` names two specs, and a
    /// bare key used to replace the generator of BOTH. It is now refused.
    #[test]
    fn a_bare_override_key_against_a_repeated_name_is_refused() {
        let err = expand_cpu_with_override("node_cpu_seconds_total")
            .expect_err("a bare key naming two specs must not be guessed at");
        let msg = format!("{err}");
        assert!(msg.contains("ambiguous"), "got: {msg}");
        assert!(
            msg.contains("node_cpu_seconds_total.user")
                && msg.contains("node_cpu_seconds_total.system"),
            "the message must list the ids to pick from: {msg}"
        );
    }

    /// And the corrected twin passes, touching exactly one spec.
    #[test]
    fn a_name_dot_id_override_key_reaches_exactly_one_spec() {
        let expanded = expand_cpu_with_override("node_cpu_seconds_total.user")
            .expect("an unambiguous key must expand");
        let gens = generators(&expanded);
        assert_eq!(gens.len(), 2);
        assert!(
            matches!(gens[0], Some(GeneratorConfig::Constant { value }) if *value == 12345.0),
            "the addressed spec takes the override, got: {:?}",
            gens[0]
        );
        assert!(
            matches!(gens[1], Some(GeneratorConfig::Step { .. })),
            "its sibling keeps the pack's own generator, got: {:?}",
            gens[1]
        );
    }

    /// A unique name that also declares an id answers to both `name` and
    /// `name.id`. Writing both used to apply whichever sorted last and
    /// discard the other without a word.
    #[test]
    fn two_override_keys_addressing_one_spec_are_refused() {
        const YAML: &str = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: iface
    signal_type: metrics
    pack: single
    overrides:
      ifOperStatus:
        generator: { type: constant, value: 111.0 }
      ifOperStatus.up:
        generator: { type: constant, value: 222.0 }
"#;
        let pack = MetricPackDef {
            extends: None,
            deviations: Vec::new(),
            name: "single".to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: "ifOperStatus".to_string(),
                id: Some("up".to_string()),
                labels: None,
                generator: Some(GeneratorConfig::Constant { value: 1.0 }),
            }],
        };
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("single", pack);
        let parsed = parse(YAML).expect("parse must succeed");
        let normalized = normalize(parsed).expect("normalize must succeed");

        let err = expand(normalized, &resolver).expect_err("one spec, two keys, must not pick");
        let msg = format!("{err}");
        assert!(
            msg.contains("ifOperStatus") && msg.contains("ifOperStatus.up"),
            "the message must name both keys: {msg}"
        );
        assert!(
            matches!(err, ExpandError::ConflictingOverrideKeys { .. }),
            "got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Extension chains (W4 phase 1c)
    // -----------------------------------------------------------------------

    const EXTENDING_SCENARIO: &str = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: dev
    signal_type: metrics
    pack: LEAF
"#;

    /// A pack with `extends:` set and one added metric, named `<name>_m`.
    fn extending_pack(name: &str, base: Option<&str>) -> MetricPackDef {
        MetricPackDef {
            name: name.to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            extends: base.map(str::to_string),
            shared_labels: None,
            metrics: vec![MetricSpec {
                name: format!("{name}_m"),
                id: None,
                labels: None,
                generator: Some(GeneratorConfig::Constant { value: 1.0 }),
            }],
            deviations: Vec::new(),
        }
    }

    fn expand_chain(leaf: &str, packs: Vec<MetricPackDef>) -> Result<ExpandedFile, ExpandError> {
        let mut resolver = InMemoryPackResolver::new();
        for pack in packs {
            let key = pack.name.clone();
            resolver.insert(key, pack);
        }
        let yaml = EXTENDING_SCENARIO.replace("LEAF", leaf);
        let parsed = parse(&yaml).expect("parse must succeed");
        let normalized = normalize(parsed).expect("normalize must succeed");
        expand(normalized, &resolver)
    }

    /// The whole chain materializes, and the resolved order is root-first —
    /// a base's metrics precede the metrics of the pack extending it.
    #[test]
    fn a_three_deep_chain_resolves_root_first() {
        let expanded = expand_chain(
            "c",
            vec![
                extending_pack("a", None),
                extending_pack("b", Some("a")),
                extending_pack("c", Some("b")),
            ],
        )
        .expect("a chain must resolve");

        let names: Vec<&str> = expanded.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a_m", "b_m", "c_m"]);
    }

    /// Sub-signal ids are built from the *leaf* pack's name, because the
    /// leaf is the pack the scenario referenced.
    #[test]
    fn the_resolved_pack_keeps_the_leafs_identity() {
        let expanded = expand_chain(
            "c",
            vec![extending_pack("a", None), extending_pack("c", Some("a"))],
        )
        .expect("must resolve");
        let ids: Vec<&str> = expanded
            .entries
            .iter()
            .filter_map(|e| e.id.as_deref())
            .collect();
        assert_eq!(ids, vec!["dev.a_m", "dev.c_m"]);
    }

    #[test]
    fn an_extends_naming_a_missing_base_says_which_pack_named_it() {
        // `b` extends `a`, but only `b` is in the resolver.
        let err = expand_chain("b", vec![extending_pack("b", Some("a"))])
            .expect_err("a missing base must error");
        let msg = format!("{err}");
        assert!(
            matches!(err, ExpandError::ExtendsUnresolved { .. }),
            "got: {err:?}"
        );
        assert!(
            msg.contains("'b' extends 'a'"),
            "must name both the extender and the base: {msg}"
        );
    }

    /// A missing `pack:` and a missing `extends:` are different mistakes in
    /// different files, so they must not collapse into one diagnostic.
    #[test]
    fn a_missing_leaf_and_a_missing_base_are_different_errors() {
        let missing_leaf =
            expand_chain("nope", vec![extending_pack("a", None)]).expect_err("must error");
        assert!(
            matches!(missing_leaf, ExpandError::ResolveFailed { .. }),
            "got: {missing_leaf:?}"
        );
    }

    #[test]
    fn a_two_pack_cycle_is_refused_naming_the_loop() {
        let err = expand_chain(
            "a",
            vec![
                extending_pack("a", Some("b")),
                extending_pack("b", Some("a")),
            ],
        )
        .expect_err("a cycle must error");
        match &err {
            ExpandError::ExtensionCycle { cycle } => {
                assert_eq!(
                    cycle,
                    &vec!["a".to_string(), "b".to_string(), "a".to_string()]
                );
            }
            other => panic!("expected ExtensionCycle, got {other:?}"),
        }
        assert!(format!("{err}").contains("a -> b -> a"), "{err}");
    }

    /// A pack extending itself is the degenerate cycle, and the one a user
    /// writes by copy-pasting a pack and forgetting to edit `extends:`.
    #[test]
    fn a_pack_extending_itself_is_refused() {
        let err = expand_chain("a", vec![extending_pack("a", Some("a"))]).expect_err("must error");
        match &err {
            ExpandError::ExtensionCycle { cycle } => {
                assert_eq!(cycle, &vec!["a".to_string(), "a".to_string()])
            }
            other => panic!("expected ExtensionCycle, got {other:?}"),
        }
    }

    /// A cycle deeper than the entry point still terminates: the walk is
    /// keyed on every reference it has passed, not only on where it began.
    #[test]
    fn a_cycle_the_entry_point_is_not_part_of_is_still_refused() {
        let err = expand_chain(
            "c",
            vec![
                extending_pack("c", Some("b")),
                extending_pack("b", Some("a")),
                extending_pack("a", Some("b")),
            ],
        )
        .expect_err("must error");
        match &err {
            ExpandError::ExtensionCycle { cycle } => {
                assert_eq!(
                    cycle,
                    &vec!["b".to_string(), "a".to_string(), "b".to_string()]
                );
            }
            other => panic!("expected ExtensionCycle, got {other:?}"),
        }
    }

    /// Re-declaring a selector the base has is caught by the additive rule,
    /// which names the base so the reader knows where the other one is.
    #[test]
    fn an_addition_repeating_a_base_selector_names_the_base() {
        let mut base = extending_pack("a", None);
        base.metrics[0].name = "shared".to_string();
        let mut ext = extending_pack("c", Some("a"));
        ext.metrics[0].name = "shared".to_string();

        let err = expand_chain("c", vec![base, ext]).expect_err("must error");
        let msg = format!("{err}");
        assert!(
            matches!(err, ExpandError::ExtendFailed { .. }),
            "got: {err:?}"
        );
        assert!(msg.contains("already declared by base 'a'"), "{msg}");
    }

    /// The case no single link exhibits, and the reason addressability is
    /// re-checked after every fold: `cpu` and `cpu.x` are *different*
    /// selectors, so the additive rule is satisfied — but the materialized
    /// pack repeats the name `cpu` with only one of them carrying an id,
    /// which is unaddressable.
    #[test]
    fn a_fold_that_makes_a_name_repeat_without_ids_is_refused() {
        let mut base = extending_pack("a", None);
        base.metrics[0].name = "cpu".to_string();
        base.metrics[0].id = None;

        let mut ext = extending_pack("c", Some("a"));
        ext.metrics[0].name = "cpu".to_string();
        ext.metrics[0].id = Some("x".to_string());

        let err = expand_chain("c", vec![base, ext]).expect_err("must error");
        let msg = format!("{err}");
        assert!(
            matches!(err, ExpandError::InvalidPack { .. }),
            "the materialized pack is what fails, got: {err:?}"
        );
        assert!(
            msg.contains("appears 2 times") && msg.contains("declare none"),
            "{msg}"
        );
    }

    /// Only packs above the root are folded, so a root's own `deviations:`
    /// would never be looked at. Measured before the guard: a lone pack
    /// deviating on a selector that does not even exist compiled clean.
    #[test]
    fn deviations_on_a_pack_with_no_extends_are_refused_not_dropped() {
        let mut lone = extending_pack("a", None);
        lone.deviations = vec![crate::packs::Deviation {
            metric: "nope".to_string(),
            replace: None,
            not_supported: true,
        }];

        let err = expand_chain("a", vec![lone]).expect_err("must not be dropped");
        match &err {
            ExpandError::DeviationsWithoutExtends { pack_name, count } => {
                assert_eq!(pack_name, "a");
                assert_eq!(*count, 1);
            }
            other => panic!("expected DeviationsWithoutExtends, got {other:?}"),
        }
    }

    /// The same block one link up is applied rather than refused — the
    /// guard keys on having no base, not on declaring deviations.
    #[test]
    fn deviations_on_a_pack_that_does_extend_are_applied() {
        let base = extending_pack("a", None);
        let mut ext = extending_pack("c", Some("a"));
        ext.deviations = vec![crate::packs::Deviation {
            metric: "a_m".to_string(),
            replace: None,
            not_supported: true,
        }];

        let expanded = expand_chain("c", vec![base, ext]).expect("must resolve");
        let names: Vec<&str> = expanded.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["c_m"], "the base's metric was removed");
    }

    /// An extension whose base is empty and which adds nothing resolves to
    /// no metrics at all — reported as an empty pack, not as a success.
    #[test]
    fn a_chain_resolving_to_no_metrics_is_an_empty_pack() {
        let mut base = extending_pack("a", None);
        base.metrics.clear();
        let mut ext = extending_pack("c", Some("a"));
        ext.metrics.clear();

        let err = expand_chain("c", vec![base, ext]).expect_err("must error");
        assert!(matches!(err, ExpandError::EmptyPack { .. }), "got: {err:?}");
    }

    /// The extension's slot in the precedence chain, measured through the
    /// real pipeline rather than on the materializer alone: base shared <
    /// extension shared < per-metric < entry.
    #[test]
    fn the_extension_shared_labels_slot_sits_between_base_and_per_metric() {
        const YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  labels: { from_defaults: defaults, tier: defaults }
scenarios:
  - id: dev
    signal_type: metrics
    pack: c
    labels: { tier: entry }
"#;
        let mut base = extending_pack("a", None);
        base.shared_labels = Some(
            [
                ("from_base", "base"),
                ("overridden_by_ext", "base"),
                ("tier", "base"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );

        let mut ext = extending_pack("c", Some("a"));
        ext.shared_labels = Some(
            [
                ("from_ext", "ext"),
                ("overridden_by_ext", "ext"),
                ("tier", "ext"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        ext.metrics[0].labels = Some(
            [("from_metric", "metric"), ("tier", "metric")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );

        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("a", base);
        resolver.insert("c", ext);
        let parsed = parse(YAML).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let expanded = expand(normalized, &resolver).expect("must expand");

        // `a_m` comes from the base and has no per-metric labels; `c_m` is
        // the extension's own and carries them.
        let c_m = expanded
            .entries
            .iter()
            .find(|e| e.name == "c_m")
            .expect("the extension's metric must be present");
        let labels = c_m.labels.as_ref().expect("labels present");
        let at = |k: &str| labels.get(k).map(String::as_str);

        assert_eq!(at("from_defaults"), Some("defaults"));
        assert_eq!(at("from_base"), Some("base"), "the base still contributes");
        assert_eq!(at("from_ext"), Some("ext"));
        assert_eq!(at("from_metric"), Some("metric"));
        assert_eq!(
            at("overridden_by_ext"),
            Some("ext"),
            "the extension outranks its base"
        );
        assert_eq!(
            at("tier"),
            Some("entry"),
            "and the entry outranks all three pack layers"
        );

        // The base's own metric sees the extension's shared labels too —
        // they are the resolved pack's, not the extension's metrics' alone.
        let a_m = expanded
            .entries
            .iter()
            .find(|e| e.name == "a_m")
            .expect("the base's metric must be present");
        assert_eq!(
            a_m.labels
                .as_ref()
                .and_then(|l| l.get("from_ext"))
                .map(String::as_str),
            Some("ext")
        );
    }

    #[test]
    fn an_override_key_naming_no_spec_lists_the_real_selectors() {
        let err = expand_cpu_with_override("node_cpu_seconds_total.nope")
            .expect_err("an unknown id must error");
        let msg = format!("{err}");
        assert!(msg.contains("nope"), "got: {msg}");
        assert!(
            msg.contains("node_cpu_seconds_total.user"),
            "must name what the pack does have: {msg}"
        );
    }

    /// A pack whose repeated name carries no ids cannot be expanded at all —
    /// the refusal is the pack's, before any selector is resolved.
    #[test]
    fn a_pack_with_a_repeated_name_and_no_ids_is_refused_at_expansion() {
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: cpu
    signal_type: metrics
    pack: node_exporter_cpu
"#;
        let mut pack = node_cpu_pack();
        for spec in pack.metrics.iter_mut() {
            spec.id = None;
        }
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("node_exporter_cpu", pack);
        let parsed = parse(yaml).expect("parse must succeed");
        let normalized = normalize(parsed).expect("normalize must succeed");
        let err = expand(normalized, &resolver).expect_err("an unaddressable pack must not load");
        assert!(
            matches!(err, ExpandError::InvalidPack { .. }),
            "got: {err:?}"
        );
        assert!(format!("{err}").contains("unique `id:`"), "got: {err}");
    }

    /// A unique name still takes a bare key — the common case is unchanged.
    #[test]
    fn a_bare_override_key_against_a_unique_name_still_works() {
        let yaml = r#"
version: 2
kind: runnable
defaults: { rate: 1 }
scenarios:
  - id: net
    signal_type: metrics
    pack: telegraf_snmp_interface
    overrides:
      ifOperStatus:
        generator: { type: constant, value: 7.0 }
"#;
        let mut resolver = InMemoryPackResolver::new();
        resolver.insert("telegraf_snmp_interface", telegraf_pack());
        let expanded = expand_yaml(yaml, &resolver);
        let target = expanded
            .entries
            .iter()
            .find(|e| e.name == "ifOperStatus")
            .expect("ifOperStatus present");
        assert!(
            matches!(&target.generator, Some(GeneratorConfig::Constant { value }) if *value == 7.0),
            "got: {:?}",
            target.generator
        );
    }
}
