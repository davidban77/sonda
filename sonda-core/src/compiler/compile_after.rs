//! `after` clause compilation and clock-group assignment (Phases 4 & 5).
//!
//! This module takes an [`ExpandedFile`] (the output of
//! [`super::expand::expand`]) and produces a [`CompiledFile`] where every
//! `after:` clause has been resolved into a concrete `phase_offset` and
//! every signal participating in a dependency chain has been assigned a
//! deterministic `clock_group`. The runtime never sees [`AfterClause`]
//! objects — by the time the runtime receives entries, causal ordering is
//! expressed purely through `phase_offset` + `clock_group`.
//!
//! # Pipeline position
//!
//! ```text
//! parse (§4.1) → normalize (§4.2) → expand (§4.3) → compile_after (§4.4 + §4.5) → runtime
//! ```
//!
//! The type itself witnesses the transition: [`CompiledEntry`] drops
//! [`AfterClause`] from its shape, exposing only `phase_offset: Option<String>`
//! and `clock_group: Option<String>`. If you have a `CompiledEntry`, all
//! `after` clauses in the file were resolvable.
//!
//! # Reference resolution (§3.2)
//!
//! The pass builds a flat `BTreeMap<String, &ExpandedEntry>` keyed on
//! [`ExpandedEntry::id`]. Inline entries with `id: None` are still valid
//! signals — they just cannot be referenced by any `after.ref`. Pack
//! sub-signals are addressable as `{entry}.{metric}` (for unique-by-name
//! packs) or `{entry}.{metric}#{spec_index}` for packs that ship multiple
//! [`MetricSpec`][crate::packs::MetricSpec]s under the same metric name.
//! When a user writes the bare `{entry}.{metric}` form against a
//! duplicate-name pack the compiler emits
//! [`CompileAfterError::AmbiguousSubSignalRef`] with the concrete
//! candidates so they know which `#N` to pick.
//!
//! # Timing computation (§3.3)
//!
//! For each signal with `after: Some(_)`:
//!
//! 1. Lower any operational alias on the **target's** generator into its
//!    core [`GeneratorConfig`] variant. The timing math in
//!    [`super::timing`] operates exclusively on the desugared form, which
//!    matches the runtime's view of the signal.
//! 2. Dispatch on the core variant to the matching `*_crossing_secs`
//!    function. Each generator has its own crossing formula; generators
//!    with ambiguous or non-deterministic output (`sine`, `uniform`,
//!    `csv_replay`) are rejected with [`CompileAfterError::UnsupportedGenerator`].
//! 3. Propagate the computed crossing time to the dependent signal,
//!    accumulating transitive offsets across the dependency chain.
//!
//! # Offset formula (§3.3, matrix 11.14)
//!
//! ```text
//! total_secs = user_phase_offset_secs + Σ crossing_time_secs + Σ delay_secs
//! ```
//!
//! The result is formatted back into a parseable duration string (e.g.
//! `"162.308s"`) for storage on [`CompiledEntry::phase_offset`]. The string
//! round-trips through
//! [`crate::config::validate::parse_duration`] so downstream passes can
//! treat it the same way they treat a user-supplied `phase_offset`.
//!
//! # Clock-group derivation (§4.5)
//!
//! The `after` dependency graph partitions signals into connected
//! components (treating edges as undirected for grouping purposes).
//! For every component with two or more members the pass assigns one
//! clock group:
//!
//! - if no entry in the component has an explicit `clock_group`, it is
//!   auto-assigned as `chain_{lowest_lex_entry_id}`;
//! - if exactly one distinct non-empty value is present, that value
//!   becomes the group for the whole component;
//! - if two distinct values are present, the pass emits
//!   [`CompileAfterError::ConflictingClockGroup`] naming both values and
//!   the offending entries (matrix row 11.16).
//!
//! Single-entry components (signals with no `after` and no dependents)
//! keep their explicit `clock_group` if set, otherwise stay `None`.
//!
//! # Cycle detection (§3.4, matrix row 10.6)
//!
//! A Kahn topological sort on the directed dependency graph yields the
//! resolution order. When the sort's output covers fewer entries than the
//! graph, a recursive DFS with white/gray/black coloring reconstructs the
//! cycle path (e.g. `["A", "B", "C", "A"]`) and surfaces it via
//! [`CompileAfterError::CircularDependency`]. Back-edge detection is
//! driven by `color[dep] == Gray`; the path-reconstruction vector records
//! the current ancestor chain so the cycle can be sliced out directly.
//!
//! # Cross-signal-type support (§3.5, matrix row 11.11)
//!
//! A dependent signal can be any `signal_type` — metrics, logs, histogram,
//! or summary — but the **target** must be a metrics signal with a
//! deterministic generator. Crossing math requires inverting a generator's
//! analytical form, which the non-metric signal types do not have; the
//! pass rejects such targets with
//! [`CompileAfterError::NonMetricsTarget`].
//!
//! # Pack references
//!
//! Pack entries are not themselves referenceable — the expand pass does
//! not emit an [`ExpandedEntry`] whose `id` matches the bare pack entry
//! id (e.g. `B`). Only the individual sub-signals materialize as
//! addressable entries, using the dotted form `{entry}.{metric}` (and
//! `{entry}.{metric}#{spec_index}` for duplicate-name packs). Writing
//! `after.ref: B` against a pack entry therefore fails with
//! [`CompileAfterError::UnknownRef`]; the `available` list in the
//! diagnostic shows the valid dotted ids. To attach `after:` to the whole
//! pack, set it on the pack entry itself — the expand pass propagates it
//! to every sub-signal — or use a specific dotted metric path.
//!
//! # Clock-group string equality
//!
//! Clock-group comparisons use exact string equality after filtering out
//! empty strings: `Some("")` is treated as "no explicit value" and
//! participates in auto-naming, while `Some("x")` and `Some("x ")` are
//! considered distinct (trailing whitespace is significant). Mixing a
//! blank and a concrete value inside one component resolves to the
//! concrete value without error; mixing two different non-empty values
//! (including whitespace variants) triggers
//! [`CompileAfterError::ConflictingClockGroup`].

use std::collections::{BTreeMap, VecDeque};

use super::expand::{ExpandedEntry, ExpandedFile};
use super::timing::{
    self, constant_crossing_secs, csv_replay_crossing_secs, sawtooth_crossing_secs,
    sequence_crossing_secs, sine_crossing_secs, spike_crossing_secs, step_crossing_secs,
    uniform_crossing_secs, Operator, TimingError,
};
use super::AfterOp;
use crate::config::validate::parse_duration;
use crate::config::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig, GapConfig,
};
use crate::encoder::EncoderConfig;
use crate::generator::{GeneratorConfig, LogGeneratorConfig};
use crate::sink::SinkConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the `after` clause compilation pass.
///
/// Every variant captures enough context to identify the offending entry
/// without re-reading the source YAML. Variants map one-to-one onto the
/// spec §3.4 validation table so diagnostics stay aligned with the
/// published error messages.
#[derive(Debug, thiserror::Error)]
pub enum CompileAfterError {
    /// An `after.ref` pointed to a signal id that does not exist in the
    /// expanded file.
    ///
    /// The `available` list contains every known signal id (sorted) so the
    /// user can spot the typo or missing entry quickly.
    #[error(
        "entry '{source_id}': after.ref '{ref_id}' does not match any signal id in this file. \
         Available ids: [{available}]"
    )]
    UnknownRef {
        /// The `id` (or descriptive label) of the entry whose `after` failed.
        source_id: String,
        /// The unresolved reference as written in the scenario file.
        ref_id: String,
        /// Comma-separated list of known ids in the file.
        available: String,
    },

    /// An `after.ref` used the bare `{entry}.{metric}` form against a
    /// duplicate-name pack metric. The user must pick one of the
    /// `#{spec_index}` variants.
    #[error(
        "after.ref '{ref_id}' is ambiguous: pack '{pack_entry_id}' ships multiple specs with \
         this metric name. Use one of: [{candidates}]"
    )]
    AmbiguousSubSignalRef {
        /// The ambiguous reference as written.
        ref_id: String,
        /// The pack entry id that produced the colliding sub-signals.
        pack_entry_id: String,
        /// Comma-separated list of disambiguated sub-signal ids.
        candidates: String,
    },

    /// An entry's `after.ref` pointed to its own id.
    #[error("entry '{source_id}': after.ref references itself")]
    SelfReference {
        /// The offending entry's id.
        source_id: String,
    },

    /// The dependency graph contains a cycle.
    ///
    /// `cycle` is a path of entry ids starting and ending at the same
    /// vertex (e.g. `["A", "B", "C", "A"]`).
    #[error("circular dependency detected: {}", .cycle.join(" -> "))]
    CircularDependency {
        /// Ordered list of entry ids forming the cycle, with the start
        /// vertex repeated at the end.
        cycle: Vec<String>,
    },

    /// The target of an `after.ref` uses a generator that does not support
    /// the requested operator.
    #[error(
        "entry '{source_id}': after.ref '{ref_id}' uses generator '{generator}' which does \
         not support {op} threshold crossings: {reason}"
    )]
    UnsupportedGenerator {
        /// The dependent entry.
        source_id: String,
        /// The referenced target.
        ref_id: String,
        /// The target's generator type (as the serde tag, e.g. `"sine"`).
        generator: String,
        /// The operator from the after clause.
        op: String,
        /// Diagnostic detail from the timing-math layer.
        reason: String,
    },

    /// The after clause threshold is outside the target signal's output
    /// range — the crossing will never happen.
    #[error("entry '{source_id}': after.ref '{ref_id}' op '{op}' value {value} -- {reason}")]
    OutOfRangeThreshold {
        /// The dependent entry.
        source_id: String,
        /// The referenced target.
        ref_id: String,
        /// The operator from the after clause.
        op: String,
        /// The threshold value from the after clause.
        value: f64,
        /// Diagnostic detail from the timing-math layer.
        reason: String,
    },

    /// The crossing condition is already satisfied at `t=0`; the crossing
    /// time is ambiguous.
    #[error(
        "entry '{source_id}': after.ref '{ref_id}' op '{op}' value {value} -- condition is \
         true at t=0, timing is ambiguous: {reason}"
    )]
    AmbiguousAtT0 {
        /// The dependent entry.
        source_id: String,
        /// The referenced target.
        ref_id: String,
        /// The operator from the after clause.
        op: String,
        /// The threshold value from the after clause.
        value: f64,
        /// Diagnostic detail from the timing-math layer.
        reason: String,
    },

    /// Two entries in the same dependency chain have different explicit
    /// `clock_group` values.
    #[error(
        "conflicting clock_group in dependency chain: entry '{first_entry}' has \
         clock_group '{first_group}', entry '{second_entry}' has clock_group '{second_group}'"
    )]
    ConflictingClockGroup {
        /// First entry whose clock_group participates in the conflict.
        first_entry: String,
        /// The first clock_group value.
        first_group: String,
        /// Second entry whose clock_group differs from the first.
        second_entry: String,
        /// The conflicting clock_group value.
        second_group: String,
    },

    /// The target of an `after.ref` is not a metrics signal.
    ///
    /// Cross-signal-type `after` (spec §3.5) allows the **dependent** to be
    /// any type, but the **target** must be metrics so the compiler can
    /// invert its analytical model for crossing math.
    #[error(
        "entry '{source_id}': after.ref '{ref_id}' resolves to a {signal_type} signal; \
         only metrics signals can be `after` targets"
    )]
    NonMetricsTarget {
        /// The dependent entry.
        source_id: String,
        /// The referenced target.
        ref_id: String,
        /// The target's actual signal type.
        signal_type: String,
    },

    /// A duration string on `after.delay`, the entry's `phase_offset`, or
    /// an alias parameter (e.g. `flap.up_duration`) was not parseable.
    #[error("entry '{source_id}': invalid duration '{input}' in {field}: {reason}")]
    InvalidDuration {
        /// The entry whose duration field failed to parse.
        source_id: String,
        /// Which field carried the bad value (`"after.delay"`, `"phase_offset"`, etc.).
        field: &'static str,
        /// The offending string as written.
        input: String,
        /// The underlying parse error message.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Compiled representation
// ---------------------------------------------------------------------------

/// A v2 scenario file with every `after:` clause resolved.
///
/// Mirrors [`ExpandedFile`] shape-for-shape, replacing [`ExpandedEntry`]
/// with [`CompiledEntry`]. The type witnesses that all reference
/// resolution, timing math, and clock-group assignment have completed
/// successfully.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct CompiledFile {
    /// Schema version. Always `2` after compilation.
    pub version: u32,
    /// Concrete scenario entries, in source order. Pack-expanded sub-signals
    /// appear consecutively as their parent entry was processed.
    pub entries: Vec<CompiledEntry>,
}

/// A single scenario entry with `after:` resolved and `clock_group`
/// finalized.
///
/// The `after: Option<AfterClause>` field from [`ExpandedEntry`] is gone
/// — the causal information it carried has been folded into
/// [`Self::phase_offset`] (computed crossing time plus any user-provided
/// offset plus the optional `delay`) and [`Self::clock_group`] (either the
/// user's explicit value or an auto-assigned `chain_{lowest_lex_id}`).
///
/// All other fields are copied verbatim from [`ExpandedEntry`]; this is a
/// pure enrichment pass, not a structural rewrite.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
pub struct CompiledEntry {
    /// Signal identifier, identical to [`ExpandedEntry::id`].
    pub id: Option<String>,
    /// Signal type: `"metrics"`, `"logs"`, `"histogram"`, or `"summary"`.
    pub signal_type: String,
    /// Metric or scenario name.
    pub name: String,
    /// Event rate in events per second.
    pub rate: f64,
    /// Total run duration (e.g. `"30s"`, `"5m"`).
    pub duration: Option<String>,
    /// Value generator configuration (metrics signals only).
    pub generator: Option<GeneratorConfig>,
    /// Log generator configuration (logs signals only).
    pub log_generator: Option<LogGeneratorConfig>,
    /// Static labels, already composed through the full precedence chain.
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
    /// Phase offset. Equals `user_phase_offset + Σ crossing_time + Σ delay`
    /// when the entry participated in an `after:` chain; otherwise the
    /// user's original value (or `None`).
    pub phase_offset: Option<String>,
    /// Clock group — either the user's explicit value, or an
    /// auto-assigned `chain_{lowest_lex_id}` for every member of a
    /// dependency chain with no explicit group.
    pub clock_group: Option<String>,

    // -- Histogram / summary fields (inline entries only) --
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

/// Compile every `after:` clause in an expanded v2 scenario file.
///
/// The returned [`CompiledFile`] contains one [`CompiledEntry`] per input
/// entry with `after:` resolved into `phase_offset`, and a deterministic
/// `clock_group` assigned to every signal participating in a dependency
/// chain. Entries without `after:` pass through unchanged.
///
/// # Behavior
///
/// - **Reference index.** A flat map keyed on [`ExpandedEntry::id`] covers
///   inline entries and pack sub-signals alike. Bare `{entry}.{metric}`
///   references against duplicate-name packs raise
///   [`CompileAfterError::AmbiguousSubSignalRef`].
/// - **Crossing math.** Aliases (`flap`, `saturation`, `leak`,
///   `degradation`, `spike_event`, `steady`) are desugared on the target
///   before dispatching to the matching `timing::*_crossing_secs` routine.
/// - **Transitive accumulation.** Kahn's topological sort orders entries
///   so that `phase_offset` for a dependent signal includes its target's
///   already-resolved offset.
/// - **Clock-group assignment.** Signals linked by `after:` are grouped
///   into connected components. If any component member carries an
///   explicit `clock_group`, that value is used; otherwise the group is
///   named `chain_{lowest_lex_id}`.
///
/// # Errors
///
/// Every variant of [`CompileAfterError`] is reachable; see the type-level
/// documentation for the one-to-one mapping onto spec §3.4 validation
/// conditions.
pub fn compile_after(file: ExpandedFile) -> Result<CompiledFile, CompileAfterError> {
    let ExpandedFile { version, entries } = file;

    // -----------------------------------------------------------------
    // Reference index
    // -----------------------------------------------------------------
    let id_to_idx = build_id_index(&entries);

    // -----------------------------------------------------------------
    // Validate each `after` clause against the index (before any math).
    // This catches unknown refs / ambiguous bare refs / self-references
    // early, producing clean diagnostics even when the graph is malformed
    // enough to thwart topological ordering.
    // -----------------------------------------------------------------
    for entry in &entries {
        let Some(clause) = &entry.after else { continue };
        let source_id = source_label(entry);

        resolve_reference(&clause.ref_id, &id_to_idx, &source_id)?;

        if let Some(own_id) = entry.id.as_deref() {
            if own_id == clause.ref_id {
                return Err(CompileAfterError::SelfReference {
                    source_id: source_id.into_owned(),
                });
            }
        }
    }

    // -----------------------------------------------------------------
    // Topological sort (Kahn's algorithm with in-degree tracking)
    // -----------------------------------------------------------------
    let n = entries.len();
    let mut in_degree = vec![0u32; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, entry) in entries.iter().enumerate() {
        if let Some(clause) = &entry.after {
            let dep_idx = id_to_idx[clause.ref_id.as_str()];
            in_degree[i] += 1;
            dependents[dep_idx].push(i);
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted: Vec<usize> = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        sorted.push(idx);
        for &dependent in &dependents[idx] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }
    if sorted.len() < n {
        let cycle = find_cycle(&entries, &id_to_idx);
        return Err(CompileAfterError::CircularDependency { cycle });
    }

    // -----------------------------------------------------------------
    // Offset accumulation
    // -----------------------------------------------------------------
    let mut total_offsets = vec![0.0_f64; n];
    let mut base_offsets = vec![0.0_f64; n]; // user-set phase_offset per entry

    for (i, entry) in entries.iter().enumerate() {
        if let Some(s) = entry.phase_offset.as_deref() {
            base_offsets[i] = parse_duration_secs(s, &source_label(entry), "phase_offset")?;
        }
    }

    for &idx in &sorted {
        let entry = &entries[idx];
        let Some(clause) = &entry.after else {
            total_offsets[idx] = base_offsets[idx];
            continue;
        };

        let source_id = source_label(entry).into_owned();
        let dep_idx = id_to_idx[clause.ref_id.as_str()];
        let target = &entries[dep_idx];

        // §3.5: only metrics signals can be `after` targets.
        if target.signal_type != "metrics" {
            return Err(CompileAfterError::NonMetricsTarget {
                source_id,
                ref_id: clause.ref_id.clone(),
                signal_type: target.signal_type.clone(),
            });
        }

        // Metrics non-pack inline entries are required to carry a `generator`
        // by the parser (`ParseError::MissingGeneratorOrPack`), and pack
        // expansion always materializes a generator on every sub-signal
        // (falling back to `constant(0)` when the pack spec has none).
        // Combined with the §3.5 metrics-target check above, a metrics
        // target with `generator: None` cannot occur at this point.
        let generator = target.generator.as_ref().unwrap_or_else(|| {
            unreachable!(
                "metrics target '{ref_id}' has no generator — parser and expand \
                 pass both guarantee metrics entries always carry one",
                ref_id = clause.ref_id
            )
        });

        let op = operator_from(&clause.op);
        let crossing = crossing_secs(generator, op, clause.value, target.rate).map_err(|err| {
            timing_to_error(err, &source_id, &clause.ref_id, generator, op, clause.value)
        })?;

        let delay = match clause.delay.as_deref() {
            Some(s) => parse_duration_secs(s, &source_id, "after.delay")?,
            None => 0.0,
        };

        total_offsets[idx] = base_offsets[idx] + total_offsets[dep_idx] + crossing + delay;
    }

    // -----------------------------------------------------------------
    // Clock-group assignment (spec §4.5)
    // -----------------------------------------------------------------
    let clock_groups = assign_clock_groups(&entries, &id_to_idx)?;

    // -----------------------------------------------------------------
    // Build CompiledEntry list
    // -----------------------------------------------------------------
    let mut out: Vec<CompiledEntry> = Vec::with_capacity(n);
    for (i, entry) in entries.into_iter().enumerate() {
        let phase_offset = if entry.after.is_some() || total_offsets[i] != 0.0 {
            Some(format_duration_secs(total_offsets[i]))
        } else {
            // No `after:` and no user-set offset → leave None.
            entry.phase_offset.clone()
        };

        let clock_group = clock_groups[i]
            .clone()
            .or_else(|| entry.clock_group.clone());

        out.push(CompiledEntry {
            id: entry.id,
            signal_type: entry.signal_type,
            name: entry.name,
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
            phase_offset,
            clock_group,
            distribution: entry.distribution,
            buckets: entry.buckets,
            quantiles: entry.quantiles,
            observations_per_tick: entry.observations_per_tick,
            mean_shift_per_sec: entry.mean_shift_per_sec,
            seed: entry.seed,
        });
    }

    Ok(CompiledFile {
        version,
        entries: out,
    })
}

// ---------------------------------------------------------------------------
// Reference index + resolution
// ---------------------------------------------------------------------------

/// Build a map from signal id to its index in the entries list.
///
/// Only entries with `Some(id)` are included. [`ExpandedEntry::id`]
/// uniqueness is enforced upstream by [`super::expand::expand`], so
/// duplicate inserts cannot occur here.
fn build_id_index(entries: &[ExpandedEntry]) -> BTreeMap<&str, usize> {
    let mut idx = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(id) = entry.id.as_deref() {
            idx.insert(id, i);
        }
    }
    idx
}

/// Resolve an `after.ref` against the reference index, producing a
/// precise diagnostic for unknown or ambiguous references.
///
/// Returns the resolved target index on success.
fn resolve_reference(
    ref_id: &str,
    id_to_idx: &BTreeMap<&str, usize>,
    source_id: &str,
) -> Result<usize, CompileAfterError> {
    if let Some(&idx) = id_to_idx.get(ref_id) {
        return Ok(idx);
    }

    // Ambiguous bare `{entry}.{metric}` against a duplicate-name pack?
    // Look for ids of the form `{ref_id}#{n}`.
    let prefix = format!("{ref_id}#");
    let candidates: Vec<&str> = id_to_idx
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .copied()
        .collect();
    if !candidates.is_empty() {
        // Strip everything after the final `.` to reconstruct the pack
        // entry id for the diagnostic.
        let pack_entry_id = ref_id
            .rsplit_once('.')
            .map(|(left, _)| left.to_string())
            .unwrap_or_default();
        return Err(CompileAfterError::AmbiguousSubSignalRef {
            ref_id: ref_id.to_string(),
            pack_entry_id,
            candidates: candidates.join(", "),
        });
    }

    let available: Vec<&str> = id_to_idx.keys().copied().collect();
    Err(CompileAfterError::UnknownRef {
        source_id: source_id.to_string(),
        ref_id: ref_id.to_string(),
        available: available.join(", "),
    })
}

/// Format an entry into a human-readable label for error messages.
///
/// Priority: `id` → `name` → `<anonymous entry>`. Returns `Cow<str>` so
/// the caller can avoid an allocation when the id is already available.
fn source_label(entry: &ExpandedEntry) -> std::borrow::Cow<'_, str> {
    if let Some(id) = entry.id.as_deref() {
        std::borrow::Cow::Borrowed(id)
    } else {
        std::borrow::Cow::Owned(format!("<anonymous:{}>", entry.name))
    }
}

// ---------------------------------------------------------------------------
// Crossing time dispatch
// ---------------------------------------------------------------------------

/// Dispatch a [`GeneratorConfig`] to the matching `timing::*_crossing_secs`
/// routine, desugaring operational aliases as needed.
///
/// The `rate` parameter is required by generators whose crossing time is
/// expressed in ticks (`step`, `sequence`, and anything derived from a
/// `Sequence` via the `flap` alias). Sawtooth / spike / sine variants
/// already encode their periods in seconds and ignore the rate.
fn crossing_secs(
    generator: &GeneratorConfig,
    op: Operator,
    threshold: f64,
    rate: f64,
) -> Result<f64, TimingError> {
    match generator {
        GeneratorConfig::Constant { value } => constant_crossing_secs(op, threshold, *value),
        GeneratorConfig::Uniform { .. } => uniform_crossing_secs(),
        GeneratorConfig::Sine { .. } => sine_crossing_secs(),
        GeneratorConfig::CsvReplay { .. } => csv_replay_crossing_secs(),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => sawtooth_crossing_secs(op, threshold, *min, *max, *period_secs),
        GeneratorConfig::Sequence { values, repeat } => {
            sequence_crossing_secs(op, threshold, values, *repeat, rate)
        }
        GeneratorConfig::Step {
            start,
            step_size,
            max,
        } => step_crossing_secs(op, threshold, start.unwrap_or(0.0), *step_size, *max, rate),
        GeneratorConfig::Spike {
            baseline,
            magnitude,
            duration_secs,
            ..
        } => spike_crossing_secs(op, threshold, *baseline, *magnitude, *duration_secs),

        // --- Operational aliases (desugar before dispatch) ---
        GeneratorConfig::Flap {
            up_duration,
            down_duration,
            up_value,
            down_value,
        } => {
            let up_secs = duration_or_default(up_duration.as_deref(), 10.0, "flap.up_duration")?;
            let down_secs =
                duration_or_default(down_duration.as_deref(), 5.0, "flap.down_duration")?;
            let up_val = up_value.unwrap_or(1.0);
            let down_val = down_value.unwrap_or(0.0);
            timing::flap_crossing_secs(op, threshold, up_secs, down_secs, up_val, down_val)
        }
        GeneratorConfig::Saturation {
            baseline,
            ceiling,
            time_to_saturate,
        } => {
            let bl = baseline.unwrap_or(0.0);
            let cl = ceiling.unwrap_or(100.0);
            let period = duration_or_default(
                time_to_saturate.as_deref(),
                5.0 * 60.0,
                "saturation.time_to_saturate",
            )?;
            sawtooth_crossing_secs(op, threshold, bl, cl, period)
        }
        GeneratorConfig::Leak {
            baseline,
            ceiling,
            time_to_ceiling,
        } => {
            let bl = baseline.unwrap_or(0.0);
            let cl = ceiling.unwrap_or(100.0);
            let period = duration_or_default(
                time_to_ceiling.as_deref(),
                10.0 * 60.0,
                "leak.time_to_ceiling",
            )?;
            sawtooth_crossing_secs(op, threshold, bl, cl, period)
        }
        GeneratorConfig::Degradation {
            baseline,
            ceiling,
            time_to_degrade,
            ..
        } => {
            let bl = baseline.unwrap_or(0.0);
            let cl = ceiling.unwrap_or(100.0);
            let period = duration_or_default(
                time_to_degrade.as_deref(),
                5.0 * 60.0,
                "degradation.time_to_degrade",
            )?;
            sawtooth_crossing_secs(op, threshold, bl, cl, period)
        }
        GeneratorConfig::Steady { .. } => timing::steady_crossing_secs(),
        GeneratorConfig::SpikeEvent {
            baseline,
            spike_height,
            spike_duration,
            ..
        } => {
            let bl = baseline.unwrap_or(0.0);
            let height = spike_height.unwrap_or(100.0);
            let dur = duration_or_default(
                spike_duration.as_deref(),
                10.0,
                "spike_event.spike_duration",
            )?;
            spike_crossing_secs(op, threshold, bl, height, dur)
        }
    }
}

/// Return the generator's serde tag as a `&'static str` for error messages.
fn generator_kind(generator: &GeneratorConfig) -> &'static str {
    match generator {
        GeneratorConfig::Constant { .. } => "constant",
        GeneratorConfig::Uniform { .. } => "uniform",
        GeneratorConfig::Sine { .. } => "sine",
        GeneratorConfig::Sawtooth { .. } => "sawtooth",
        GeneratorConfig::Sequence { .. } => "sequence",
        GeneratorConfig::Spike { .. } => "spike",
        GeneratorConfig::CsvReplay { .. } => "csv_replay",
        GeneratorConfig::Step { .. } => "step",
        GeneratorConfig::Flap { .. } => "flap",
        GeneratorConfig::Saturation { .. } => "saturation",
        GeneratorConfig::Leak { .. } => "leak",
        GeneratorConfig::Degradation { .. } => "degradation",
        GeneratorConfig::Steady { .. } => "steady",
        GeneratorConfig::SpikeEvent { .. } => "spike_event",
    }
}

/// Convert a [`TimingError`] into the appropriate [`CompileAfterError`]
/// variant, preserving the underlying reason string.
fn timing_to_error(
    err: TimingError,
    source_id: &str,
    ref_id: &str,
    generator: &GeneratorConfig,
    op: Operator,
    value: f64,
) -> CompileAfterError {
    let op = op.to_string();
    match err {
        TimingError::Unsupported { message } => CompileAfterError::UnsupportedGenerator {
            source_id: source_id.to_string(),
            ref_id: ref_id.to_string(),
            generator: generator_kind(generator).to_string(),
            op,
            reason: message,
        },
        TimingError::OutOfRange { message } => CompileAfterError::OutOfRangeThreshold {
            source_id: source_id.to_string(),
            ref_id: ref_id.to_string(),
            op,
            value,
            reason: message,
        },
        TimingError::Ambiguous { message } => CompileAfterError::AmbiguousAtT0 {
            source_id: source_id.to_string(),
            ref_id: ref_id.to_string(),
            op,
            value,
            reason: message,
        },
        TimingError::InvalidDuration {
            field,
            input,
            reason,
        } => CompileAfterError::InvalidDuration {
            source_id: source_id.to_string(),
            field,
            input,
            reason,
        },
    }
}

// ---------------------------------------------------------------------------
// Clock-group assignment
// ---------------------------------------------------------------------------

/// Assign a clock group to every entry based on the `after:` dependency
/// graph (treated as undirected for component detection).
///
/// The returned vector is indexed in parallel with `entries`. Each slot
/// holds either:
///
/// - `Some(group)` — the resolved group (explicit value, or
///   `chain_{lowest_lex_id}` auto-name) for every entry in a multi-node
///   connected component;
/// - `None` — for single-node components; callers should fall back to the
///   entry's explicit `clock_group` if set.
///
/// # Errors
///
/// Returns [`CompileAfterError::ConflictingClockGroup`] when a component
/// has two distinct non-empty explicit group values.
fn assign_clock_groups(
    entries: &[ExpandedEntry],
    id_to_idx: &BTreeMap<&str, usize>,
) -> Result<Vec<Option<String>>, CompileAfterError> {
    let n = entries.len();

    // Build an undirected adjacency list.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, entry) in entries.iter().enumerate() {
        if let Some(clause) = &entry.after {
            if let Some(&dep_idx) = id_to_idx.get(clause.ref_id.as_str()) {
                adj[i].push(dep_idx);
                adj[dep_idx].push(i);
            }
        }
    }

    let mut component_id = vec![usize::MAX; n];
    let mut components: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if component_id[start] != usize::MAX {
            continue;
        }
        let cid = components.len();
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(node) = stack.pop() {
            if component_id[node] != usize::MAX {
                continue;
            }
            component_id[node] = cid;
            members.push(node);
            for &next in &adj[node] {
                if component_id[next] == usize::MAX {
                    stack.push(next);
                }
            }
        }
        components.push(members);
    }

    let mut out = vec![None; n];
    for members in &components {
        if members.len() < 2 {
            continue;
        }

        // Collect all distinct non-empty explicit clock_group values.
        let mut distinct: BTreeMap<&str, usize> = BTreeMap::new();
        for &idx in members {
            if let Some(cg) = entries[idx].clock_group.as_deref() {
                if !cg.is_empty() {
                    distinct.entry(cg).or_insert(idx);
                }
            }
        }

        let resolved = match distinct.len() {
            0 => auto_chain_name(members, entries),
            1 => {
                let (&k, _) = distinct.iter().next().expect("len == 1");
                k.to_string()
            }
            _ => {
                let mut iter = distinct.iter();
                let (&first_group, &first_idx) = iter.next().expect("len >= 2");
                let (&second_group, &second_idx) = iter.next().expect("len >= 2");
                return Err(CompileAfterError::ConflictingClockGroup {
                    first_entry: source_label(&entries[first_idx]).into_owned(),
                    first_group: first_group.to_string(),
                    second_entry: source_label(&entries[second_idx]).into_owned(),
                    second_group: second_group.to_string(),
                });
            }
        };

        for &idx in members {
            out[idx] = Some(resolved.clone());
        }
    }

    Ok(out)
}

/// Build a deterministic `chain_{lowest_lex_id}` name from a component's
/// member indices.
///
/// Every multi-member component reaches this helper via an `after` edge,
/// and `after.ref` can only target an entry that carries an `id` (that's
/// how reference resolution works in [`build_id_index`]). Therefore the
/// component always has at least one `id`-bearing member, and
/// [`Iterator::next`] on the sorted id list is guaranteed to be `Some`.
fn auto_chain_name(members: &[usize], entries: &[ExpandedEntry]) -> String {
    let mut ids: Vec<&str> = members
        .iter()
        .filter_map(|&i| entries[i].id.as_deref())
        .collect();
    ids.sort();
    let first = ids.first().unwrap_or_else(|| {
        unreachable!(
            "multi-entry component has no id-bearing member — `after.ref` \
             resolution guarantees every linked entry carries an id"
        )
    });
    format!("chain_{first}")
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

/// Find a cycle in the directed dependency graph for error reporting.
///
/// Uses DFS with gray/black coloring — on encountering a back-edge to a
/// gray vertex, the recursion stack is replayed to reconstruct the cycle
/// path. The first and last entries in the returned vector are always
/// equal, giving a readable display like `A -> B -> C -> A`.
fn find_cycle(entries: &[ExpandedEntry], id_to_idx: &BTreeMap<&str, usize>) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let n = entries.len();
    let mut color = vec![Color::White; n];
    let mut stack: Vec<usize> = Vec::new();

    // Recursive DFS with a path-reconstruction vector: `stack` records the
    // current ancestor chain so that on a back-edge we can slice out the
    // cycle from `dep` to `node` without re-traversing the graph. Back-edge
    // detection is driven by `color[dep] == Gray`.
    fn dfs(
        node: usize,
        entries: &[ExpandedEntry],
        id_to_idx: &BTreeMap<&str, usize>,
        color: &mut [Color],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[node] = Color::Gray;
        stack.push(node);

        if let Some(clause) = &entries[node].after {
            if let Some(&dep) = id_to_idx.get(clause.ref_id.as_str()) {
                match color[dep] {
                    Color::White => {
                        if let Some(cycle) = dfs(dep, entries, id_to_idx, color, stack) {
                            return Some(cycle);
                        }
                    }
                    Color::Gray => {
                        // Back-edge: reconstruct the cycle from `dep` to `node`.
                        let start = stack.iter().position(|&x| x == dep).unwrap_or(0);
                        let mut cycle: Vec<usize> = stack[start..].to_vec();
                        cycle.push(dep);
                        return Some(cycle);
                    }
                    Color::Black => {}
                }
            }
        }

        color[node] = Color::Black;
        stack.pop();
        None
    }

    for start in 0..n {
        if color[start] == Color::White {
            if let Some(cycle) = dfs(start, entries, id_to_idx, &mut color, &mut stack) {
                return cycle
                    .into_iter()
                    .map(|i| source_label(&entries[i]).into_owned())
                    .collect();
            }
        }
    }

    vec!["<unknown cycle>".to_string()]
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Map an [`AfterOp`] (v2 AST) onto the alias-free [`Operator`] used by
/// the timing math.
fn operator_from(op: &AfterOp) -> Operator {
    match op {
        AfterOp::LessThan => Operator::LessThan,
        AfterOp::GreaterThan => Operator::GreaterThan,
    }
}

/// Parse a duration string into fractional seconds, emitting a compiler
/// error with field context on failure.
fn parse_duration_secs(
    input: &str,
    source_id: &str,
    field: &'static str,
) -> Result<f64, CompileAfterError> {
    parse_duration(input)
        .map(|d| d.as_secs_f64())
        .map_err(|e| CompileAfterError::InvalidDuration {
            source_id: source_id.to_string(),
            field,
            input: input.to_string(),
            reason: e.to_string(),
        })
}

/// Resolve an optional duration string to seconds, falling back to
/// `default_secs` when `None`.
///
/// Parse failures surface as [`TimingError::InvalidDuration`] tagged with
/// the alias parameter name (e.g. `"flap.up_duration"`). The outer
/// [`timing_to_error`] then maps this straight into
/// [`CompileAfterError::InvalidDuration`] — the same variant used for
/// top-level `after.delay` and `phase_offset` parse failures — so users
/// see consistent diagnostics for every duration-shaped input regardless
/// of where it appears on the generator config.
fn duration_or_default(
    input: Option<&str>,
    default_secs: f64,
    field: &'static str,
) -> Result<f64, TimingError> {
    match input {
        Some(s) => {
            parse_duration(s)
                .map(|d| d.as_secs_f64())
                .map_err(|e| TimingError::InvalidDuration {
                    field,
                    input: s.to_string(),
                    reason: e.to_string(),
                })
        }
        None => Ok(default_secs),
    }
}

/// Format an f64 seconds value as a duration string accepted by
/// [`parse_duration`].
///
/// The output prefers the shortest whole-unit representation (e.g. `"1m"`
/// for 60s, `"1h"` for 3600s) and falls back to fractional seconds for
/// values that cannot round-trip through a whole-unit form. Zero and
/// negative inputs normalize to `"0s"`. In debug builds a
/// `debug_assert!` guards against non-finite or negative inputs — these
/// can only arise from programmer error; the release fallback preserves
/// the `"0s"` normalization so production code never panics.
///
/// # Parity with the v1 story CLI
///
/// v1's `story::format_duration_secs` emits sub-second values in the
/// `"{ms}ms"` form (`0.5s` → `"500ms"`). This v2 helper emits fractional
/// seconds directly (`0.5s` → `"0.5s"`). Both strings round-trip through
/// [`parse_duration`] to the same [`std::time::Duration`], so parity
/// tests that parse the output back to seconds — like
/// `v2_story_parity::link_failover_compile_parity` — continue to agree.
/// Call sites that need byte-identical output to v1 must format locally.
pub fn format_duration_secs(secs: f64) -> String {
    debug_assert!(
        secs.is_finite() && secs >= 0.0,
        "format_duration_secs received non-finite or negative value: {secs}"
    );
    if !secs.is_finite() || secs <= 0.0 {
        return "0s".to_string();
    }

    // Prefer whole-unit forms.
    let ms = (secs * 1000.0).round() as u64;
    if ms.is_multiple_of(1000) {
        let whole_secs = ms / 1000;
        if whole_secs > 0 && whole_secs.is_multiple_of(3600) {
            return format!("{}h", whole_secs / 3600);
        }
        if whole_secs > 0 && whole_secs.is_multiple_of(60) {
            return format!("{}m", whole_secs / 60);
        }
        return format!("{whole_secs}s");
    }

    // Sub-millisecond: fall back to fractional seconds.
    if ms < 1 {
        return format!("{secs}s");
    }

    // Fractional seconds expressible in whole milliseconds → use seconds
    // with enough precision to round-trip through parse_duration. Three
    // decimals is sufficient because `ms` is already whole ms.
    let secs_rounded = (ms as f64) / 1000.0;
    format!("{secs_rounded}s")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::expand::expand;
    use crate::compiler::expand::InMemoryPackResolver;
    use crate::compiler::normalize::normalize;
    use crate::compiler::parse::parse;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Compile a v2 YAML string end-to-end through parse → normalize →
    /// expand → compile_after, using the provided pack resolver.
    fn compile(yaml: &str) -> Result<CompiledFile, String> {
        compile_with_resolver(yaml, &InMemoryPackResolver::new())
    }

    fn compile_with_resolver(
        yaml: &str,
        resolver: &InMemoryPackResolver,
    ) -> Result<CompiledFile, String> {
        let parsed = parse(yaml).map_err(|e| format!("parse: {e}"))?;
        let normalized = normalize(parsed).map_err(|e| format!("normalize: {e}"))?;
        let expanded = expand(normalized, resolver).map_err(|e| format!("expand: {e}"))?;
        compile_after(expanded).map_err(|e| format!("compile_after: {e}"))
    }

    // -----------------------------------------------------------------------
    // Reference resolution
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_ref_surfaces_available_ids() {
        let yaml = r#"
version: 2
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_saturation
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
  - id: log_entry
    signal_type: logs
    name: errors
    rate: 1
    log_generator: { type: template, templates: [{ message: "hi" }] }
    after: { ref: nonexistent, op: ">", value: 50 }
"#;
        let err = compile(yaml).expect_err("should fail");
        assert!(err.contains("nonexistent"), "got: {err}");
        assert!(err.contains("Available"), "got: {err}");
    }

    #[test]
    fn self_reference_is_rejected() {
        let yaml = r#"
version: 2
scenarios:
  - id: loop
    signal_type: metrics
    name: util
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: loop, op: ">", value: 50 }
"#;
        let err = compile(yaml).expect_err("self-ref is rejected");
        assert!(err.contains("references itself"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Simple crossing per operator/generator
    // -----------------------------------------------------------------------

    #[test]
    fn saturation_greater_than_sets_offset() {
        let yaml = r#"
version: 2
scenarios:
  - id: util
    signal_type: metrics
    name: util
    rate: 1
    generator: { type: saturation, baseline: 20, ceiling: 85, time_to_saturate: 120s }
  - id: follower
    signal_type: metrics
    name: latency
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: util, op: ">", value: 70 }
"#;
        let compiled = compile(yaml).expect("should compile");
        let follower = &compiled.entries[1];
        let expected_secs = (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        let expected_str = format_duration_secs(expected_secs);
        assert_eq!(
            follower.phase_offset.as_deref(),
            Some(expected_str.as_str())
        );
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    // Flap `<` crosses at the up_duration boundary (60s → "1m").
    #[case::flap_less_than(r#"
version: 2
scenarios:
  - id: link
    signal_type: metrics
    name: oper_state
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: util
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: link, op: "<", value: 1 }
"#, "1m")]
    // spike_event `<` crosses at the spike_duration boundary (10s).
    #[case::spike_event_less_than(r#"
version: 2
scenarios:
  - id: burst
    signal_type: metrics
    name: errs
    rate: 1
    generator: { type: spike_event, baseline: 0, spike_height: 100, spike_duration: 10s, spike_interval: 60s }
  - id: follower
    signal_type: metrics
    name: recovery
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: burst, op: "<", value: 50 }
"#, "10s")]
    // Step `>`: ceil((55-0)/10) = 6 ticks, rate=2 -> 3.0s.
    #[case::step_greater_than(r#"
version: 2
scenarios:
  - id: counter
    signal_type: metrics
    name: req_count
    rate: 2
    generator: { type: step, start: 0, step_size: 10 }
  - id: follower
    signal_type: metrics
    name: alert
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: counter, op: ">", value: 55 }
"#, "3s")]
    // Sequence `<`: index 2 (value 2) is the first < 3; rate=2 -> 1.0s.
    #[case::sequence_less_than(r#"
version: 2
scenarios:
  - id: seq
    signal_type: metrics
    name: values
    rate: 2
    generator: { type: sequence, values: [10, 5, 2, 1], repeat: false }
  - id: follower
    signal_type: metrics
    name: alert
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: seq, op: "<", value: 3 }
"#, "1s")]
    fn follower_phase_offset_matches_expected_crossing(
        #[case] yaml: &str,
        #[case] expected_offset: &str,
    ) {
        let compiled = compile(yaml).expect("should compile");
        assert_eq!(
            compiled.entries[1].phase_offset.as_deref(),
            Some(expected_offset)
        );
    }

    #[test]
    fn step_less_than_is_unsupported() {
        let yaml = r#"
version: 2
scenarios:
  - id: counter
    signal_type: metrics
    name: x
    rate: 1
    generator: { type: step, start: 0, step_size: 1 }
  - id: follower
    signal_type: metrics
    name: y
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: counter, op: "<", value: 5 }
"#;
        let err = compile(yaml).expect_err("step < is unsupported");
        assert!(err.contains("step"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Targets that cannot be resolved to a crossing time.
    //
    // `constant` values are out-of-range when the threshold is unreachable;
    // `sine`, `steady`, and `uniform` are blanket-unsupported because their
    // values never settle into a predictable threshold-crossing schedule.
    // Each error message must name the offending generator type so the
    // diagnostic points the user at the right signal.
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::constant(r#"
version: 2
scenarios:
  - id: k
    signal_type: metrics
    name: k
    rate: 1
    generator: { type: constant, value: 42 }
  - id: follower
    signal_type: metrics
    name: y
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: k, op: ">", value: 100 }
"#, "constant")]
    #[case::sine(r#"
version: 2
scenarios:
  - id: wave
    signal_type: metrics
    name: s
    rate: 1
    generator: { type: sine, amplitude: 10, period_secs: 60, offset: 50 }
  - id: follower
    signal_type: metrics
    name: f
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: wave, op: ">", value: 55 }
"#, "sine")]
    #[case::steady(r#"
version: 2
scenarios:
  - id: base
    signal_type: metrics
    name: s
    rate: 1
    generator: { type: steady, center: 50, amplitude: 5, period: 60s }
  - id: follower
    signal_type: metrics
    name: f
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: base, op: ">", value: 55 }
"#, "steady")]
    #[case::uniform(r#"
version: 2
scenarios:
  - id: u
    signal_type: metrics
    name: u
    rate: 1
    generator: { type: uniform, min: 0, max: 10, seed: 1 }
  - id: follower
    signal_type: metrics
    name: f
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: u, op: ">", value: 5 }
"#, "uniform")]
    fn unresolvable_target_generator_is_rejected(
        #[case] yaml: &str,
        #[case] expected_substring: &str,
    ) {
        let err = compile(yaml).expect_err("target generator must be rejected");
        assert!(
            err.contains(expected_substring),
            "expected error to mention {expected_substring:?}, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Transitive chains + delay additivity
    // -----------------------------------------------------------------------

    #[test]
    fn transitive_chain_accumulates() {
        let yaml = r#"
version: 2
scenarios:
  - id: a
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: b
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: saturation, baseline: 20, ceiling: 85, time_to_saturate: 120s }
    after: { ref: a, op: "<", value: 1 }
  - id: c
    signal_type: metrics
    name: c
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: b, op: ">", value: 70 }
"#;
        let compiled = compile(yaml).expect("chain compiles");
        let expected_b_secs = 60.0;
        let expected_c_secs = 60.0 + (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert_eq!(
            compiled.entries[1].phase_offset.as_deref(),
            Some(format_duration_secs(expected_b_secs).as_str())
        );
        assert_eq!(
            compiled.entries[2].phase_offset.as_deref(),
            Some(format_duration_secs(expected_c_secs).as_str())
        );
    }

    #[test]
    fn delay_is_added_to_crossing_time() {
        let yaml = r#"
version: 2
scenarios:
  - id: link
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: link, op: "<", value: 1, delay: 15s }
"#;
        let compiled = compile(yaml).expect("compile");
        assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("75s"));
    }

    #[test]
    fn explicit_phase_offset_is_added() {
        let yaml = r#"
version: 2
scenarios:
  - id: link
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    phase_offset: 10s
    after: { ref: link, op: "<", value: 1 }
"#;
        let compiled = compile(yaml).expect("compile");
        assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("70s"));
    }

    #[test]
    fn phase_offset_delay_and_crossing_sum() {
        let yaml = r#"
version: 2
scenarios:
  - id: link
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    phase_offset: 10s
    after: { ref: link, op: "<", value: 1, delay: 5s }
"#;
        let compiled = compile(yaml).expect("compile");
        // 10s + 60s crossing + 5s delay = 75s.
        assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("75s"));
    }

    // -----------------------------------------------------------------------
    // Cycle detection
    // -----------------------------------------------------------------------

    #[test]
    fn two_entry_cycle_is_detected() {
        let yaml = r#"
version: 2
scenarios:
  - id: a
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: b, op: ">", value: 1 }
  - id: b
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: a, op: ">", value: 1 }
"#;
        let err = compile(yaml).expect_err("cycle should fail");
        assert!(err.contains("circular"), "got: {err}");
        assert!(err.contains("a") && err.contains("b"), "got: {err}");
    }

    #[test]
    fn three_entry_cycle_path_is_returned() {
        let yaml = r#"
version: 2
scenarios:
  - id: a
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: c, op: ">", value: 1 }
  - id: b
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: a, op: ">", value: 1 }
  - id: c
    signal_type: metrics
    name: c
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
    after: { ref: b, op: ">", value: 1 }
"#;
        let err = compile(yaml).expect_err("cycle should fail");
        assert!(err.contains("circular"), "got: {err}");
        assert!(
            err.contains("a -> "),
            "cycle path should have an arrow. got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Clock-group assignment (matrix 11.15 / 11.16)
    // -----------------------------------------------------------------------

    #[test]
    fn clock_group_auto_assigned_as_chain_plus_lowest_id() {
        let yaml = r#"
version: 2
scenarios:
  - id: alpha
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: bravo
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: alpha, op: "<", value: 1 }
"#;
        let compiled = compile(yaml).expect("compile");
        assert_eq!(
            compiled.entries[0].clock_group.as_deref(),
            Some("chain_alpha")
        );
        assert_eq!(
            compiled.entries[1].clock_group.as_deref(),
            Some("chain_alpha")
        );
    }

    #[test]
    fn explicit_clock_group_propagates_to_chain_members() {
        let yaml = r#"
version: 2
scenarios:
  - id: alpha
    signal_type: metrics
    name: a
    rate: 1
    clock_group: failover
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: bravo
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: alpha, op: "<", value: 1 }
"#;
        let compiled = compile(yaml).expect("compile");
        assert_eq!(compiled.entries[0].clock_group.as_deref(), Some("failover"));
        assert_eq!(compiled.entries[1].clock_group.as_deref(), Some("failover"));
    }

    #[test]
    fn conflicting_clock_groups_are_rejected() {
        let yaml = r#"
version: 2
scenarios:
  - id: alpha
    signal_type: metrics
    name: a
    rate: 1
    clock_group: group_a
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: bravo
    signal_type: metrics
    name: b
    rate: 1
    clock_group: group_b
    generator: { type: constant, value: 1 }
    after: { ref: alpha, op: "<", value: 1 }
"#;
        let err = compile(yaml).expect_err("conflicting groups fail");
        assert!(err.contains("conflicting clock_group"), "got: {err}");
        assert!(
            err.contains("group_a") && err.contains("group_b"),
            "got: {err}"
        );
    }

    #[test]
    fn independent_signals_keep_no_clock_group() {
        let yaml = r#"
version: 2
scenarios:
  - id: independent
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 60s }
"#;
        let compiled = compile(yaml).expect("compile");
        assert!(compiled.entries[0].clock_group.is_none());
    }

    #[test]
    fn clock_group_empty_string_mixed_with_some_x_uses_x() {
        // An explicit empty string is filtered out (treated as "no value")
        // and the concrete `"x"` wins for the whole component — no conflict.
        let yaml = r#"
version: 2
scenarios:
  - id: alpha
    signal_type: metrics
    name: a
    rate: 1
    clock_group: ""
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: bravo
    signal_type: metrics
    name: b
    rate: 1
    clock_group: x
    generator: { type: constant, value: 1 }
    after: { ref: alpha, op: "<", value: 1 }
"#;
        let compiled = compile(yaml).expect("compile");
        assert_eq!(compiled.entries[0].clock_group.as_deref(), Some("x"));
        assert_eq!(compiled.entries[1].clock_group.as_deref(), Some("x"));
    }

    #[test]
    fn clock_group_whitespace_variants_conflict() {
        // `"x "` and `"x"` differ under string equality, so the component
        // carries two distinct non-empty values -> ConflictingClockGroup.
        let yaml = r#"
version: 2
scenarios:
  - id: alpha
    signal_type: metrics
    name: a
    rate: 1
    clock_group: "x "
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: bravo
    signal_type: metrics
    name: b
    rate: 1
    clock_group: x
    generator: { type: constant, value: 1 }
    after: { ref: alpha, op: "<", value: 1 }
"#;
        let err = compile(yaml).expect_err("trailing whitespace must conflict");
        assert!(err.contains("conflicting clock_group"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Cross-signal-type after (spec §3.5, matrix 11.11)
    // -----------------------------------------------------------------------

    #[test]
    fn log_signal_can_depend_on_metrics_target() {
        let yaml = r#"
version: 2
scenarios:
  - id: err_rate
    signal_type: metrics
    name: http_error_rate
    rate: 1
    generator: { type: saturation, baseline: 1, ceiling: 30, time_to_saturate: 90s }
  - id: err_logs
    signal_type: logs
    name: app_logs
    rate: 1
    log_generator: { type: template, templates: [{ message: "upstream timeout" }] }
    after: { ref: err_rate, op: ">", value: 10 }
"#;
        let compiled = compile(yaml).expect("cross-signal after compiles");
        assert!(compiled.entries[1].phase_offset.is_some());
    }

    #[test]
    fn metrics_entry_cannot_depend_on_logs_target() {
        let yaml = r#"
version: 2
scenarios:
  - id: log_src
    signal_type: logs
    name: lg
    rate: 1
    log_generator: { type: template, templates: [{ message: "hi" }] }
  - id: follower
    signal_type: metrics
    name: f
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: log_src, op: ">", value: 0 }
"#;
        let err = compile(yaml).expect_err("logs target rejected");
        assert!(err.contains("logs signal"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Alias desugaring correctness: `flap` after-math matches desugared
    // sequence.
    // -----------------------------------------------------------------------

    #[test]
    fn flap_alias_produces_expected_up_duration_offset() {
        let yaml_alias = r#"
version: 2
scenarios:
  - id: link
    signal_type: metrics
    name: s
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: f
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: link, op: "<", value: 1 }
"#;
        let compiled = compile(yaml_alias).expect("compile");
        assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("1m"));
    }

    // -----------------------------------------------------------------------
    // Pack sub-signal refs (matrix 11.7, 11.12, 11.13)
    // -----------------------------------------------------------------------

    fn resolver_with_test_pack() -> InMemoryPackResolver {
        // Simple pack with unique-by-name metrics.
        let yaml = r#"
name: testpack
category: test
description: test
metrics:
  - name: state_flap
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - name: util_sat
    generator: { type: saturation, baseline: 0, ceiling: 100, time_to_saturate: 120s }
"#;
        let pack =
            serde_yaml_ng::from_str::<crate::packs::MetricPackDef>(yaml).expect("pack parses");
        let mut r = InMemoryPackResolver::new();
        r.insert("testpack", pack);
        r
    }

    #[test]
    fn dotted_pack_ref_resolves() {
        let yaml = r#"
version: 2
scenarios:
  - id: dev
    signal_type: metrics
    rate: 1
    pack: testpack
  - id: follower
    signal_type: metrics
    name: alert
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: dev.state_flap, op: "<", value: 1 }
"#;
        let compiled = compile_with_resolver(yaml, &resolver_with_test_pack()).expect("compile");
        // Look for the follower entry (non-pack).
        let follower = compiled
            .entries
            .iter()
            .find(|e| e.id.as_deref() == Some("follower"))
            .expect("follower present");
        assert_eq!(follower.phase_offset.as_deref(), Some("1m"));
    }

    #[test]
    fn ambiguous_bare_pack_ref_is_rejected() {
        // Use a pack with two specs sharing the metric name.
        let pack_yaml = r#"
name: ambig
category: test
description: test
metrics:
  - name: cpu_util
    labels: { mode: user }
    generator: { type: sawtooth, min: 0, max: 100, period_secs: 60 }
  - name: cpu_util
    labels: { mode: system }
    generator: { type: sawtooth, min: 0, max: 100, period_secs: 60 }
"#;
        let pack =
            serde_yaml_ng::from_str::<crate::packs::MetricPackDef>(pack_yaml).expect("pack parses");
        let mut r = InMemoryPackResolver::new();
        r.insert("ambig", pack);

        let yaml = r#"
version: 2
scenarios:
  - id: host
    signal_type: metrics
    rate: 1
    pack: ambig
  - id: follower
    signal_type: metrics
    name: alert
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: host.cpu_util, op: ">", value: 50 }
"#;
        let err = compile_with_resolver(yaml, &r).expect_err("bare ref is ambiguous");
        assert!(err.contains("ambiguous"), "got: {err}");
        assert!(
            err.contains("host.cpu_util#0") && err.contains("host.cpu_util#1"),
            "candidates should be listed. got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // InvalidDuration coverage — every code path that can construct this
    // variant must have a dedicated regression test.
    //
    // Each case names the source id of the failing entry, the field that
    // flagged the malformed duration, and the literal input string so the
    // error round-trip is byte-exact.
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    // `compile_after` is the first validation pass that actually parses
    // `after.delay` as a `std::time::Duration` — the parser only checks
    // the shape of the YAML. A malformed delay string must surface as
    // `CompileAfterError::InvalidDuration` tagged with
    // `field == "after.delay"`.
    #[case::after_delay(r#"
version: 2
scenarios:
  - id: src
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: src, op: "<", value: 1, delay: "10seconds" }
"#, "follower", "after.delay", "10seconds")]
    // `phase_offset: "0s"` is a well-known `parse_duration` rejection
    // (zero durations are invalid). Because the entry's `phase_offset`
    // is parsed inside `compile_after`, this must surface as
    // `CompileAfterError::InvalidDuration` with `field == "phase_offset"`.
    #[case::phase_offset_zero(r#"
version: 2
scenarios:
  - id: src
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: 60s, down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    phase_offset: "0s"
    generator: { type: constant, value: 1 }
    after: { ref: src, op: "<", value: 1 }
"#, "follower", "phase_offset", "0s")]
    // Invalid alias duration params (e.g. `flap.up_duration: "oops"`) must
    // also route through `InvalidDuration` — historically these were
    // folded into `OutOfRangeThreshold` because `duration_or_default`
    // wrapped them as `TimingError::OutOfRange`. PR 5 review flagged the
    // mis-classification; this regression anchors the fix.
    #[case::alias_flap_up_duration(r#"
version: 2
scenarios:
  - id: src
    signal_type: metrics
    name: a
    rate: 1
    generator: { type: flap, up_duration: "oops", down_duration: 30s }
  - id: follower
    signal_type: metrics
    name: b
    rate: 1
    generator: { type: constant, value: 1 }
    after: { ref: src, op: "<", value: 1 }
"#, "follower", "flap.up_duration", "oops")]
    fn invalid_duration_surfaces_invalid_duration(
        #[case] yaml: &str,
        #[case] expected_source_id: &str,
        #[case] expected_field: &str,
        #[case] expected_input: &str,
    ) {
        let err = match compile_after_from_yaml(yaml) {
            Err(e) => e,
            Ok(_) => panic!("invalid duration must fail"),
        };
        match err {
            CompileAfterError::InvalidDuration {
                ref source_id,
                field,
                ref input,
                ..
            } => {
                assert_eq!(source_id, expected_source_id);
                assert_eq!(field, expected_field);
                assert_eq!(input, expected_input);
            }
            other => panic!("expected InvalidDuration, got {other:?}"),
        }
    }

    /// Pipe YAML straight to `compile_after` and return the typed error
    /// (rather than the stringified form the other helpers use). Enables
    /// the `InvalidDuration` tests above to pattern-match on the variant
    /// shape without redundant string assertions.
    fn compile_after_from_yaml(yaml: &str) -> Result<CompiledFile, CompileAfterError> {
        let parsed = parse(yaml).expect("parse");
        let normalized = normalize(parsed).expect("normalize");
        let expanded = expand(normalized, &InMemoryPackResolver::new()).expect("expand");
        compile_after(expanded)
    }

    // -----------------------------------------------------------------------
    // format_duration_secs round-trip
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::whole_seconds(30.0,            "30s")]
    #[case::whole_minutes(120.0,           "2m")]
    #[case::whole_hours(3600.0,            "1h")]
    // Exact zero (and the `-0.0` variant, which compares equal to 0.0)
    // both route through the `<= 0.0` fallback and emit `"0s"`.
    #[case::zero(0.0,                      "0s")]
    #[case::negative_zero(-0.0,            "0s")]
    fn format_duration_whole_units(#[case] secs: f64, #[case] expected: &str) {
        assert_eq!(format_duration_secs(secs), expected);
    }

    #[test]
    fn format_duration_fractional_seconds_round_trip() {
        let result = format_duration_secs(92.307);
        let dur = parse_duration(&result).expect("round-trip");
        assert!(
            (dur.as_secs_f64() - 92.307).abs() < 0.01,
            "got {}, expected ~92.307",
            dur.as_secs_f64()
        );
    }
}
