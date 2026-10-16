//! Unified scenario launch API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::compiler::{DelayClause, WhileClause};
use crate::config::aliases::desugar_entry;
use crate::config::validate::{
    validate_config, validate_histogram_config, validate_log_config, validate_summary_config,
};
use crate::config::{expand_entry, PromMeta, PromMetricType, ScenarioEntry};
use crate::generator::GeneratorConfig;
use crate::schedule::core_loop::GateContext;
use crate::schedule::gate_bus::{GateBus, GateBusResolver};
use crate::schedule::handle::ScenarioHandle;
use crate::schedule::histogram_runner::run_with_sink_gated as run_histogram_with_sink_gated;
use crate::schedule::log_runner::run_logs_with_sink_gated;
use crate::schedule::runner::run_with_sink_gated;
use crate::schedule::stats::{ScenarioState, ScenarioStats};
use crate::schedule::summary_runner::run_with_sink_gated as run_summary_with_sink_gated;
use crate::schedule::LaunchId;
use crate::sink::create_sink_in;
use crate::{ConfigError, SondaError};

/// Extract the inner message from a [`SondaError`] for re-wrapping.
///
/// When the error is [`SondaError::Config`], extracts the inner
/// [`ConfigError`] message to avoid duplicating the "configuration error:"
/// prefix that `SondaError::Config`'s `Display` adds. For other variants,
/// falls back to the full `Display` representation.
fn inner_error_message(err: &SondaError) -> String {
    match err {
        SondaError::Config(config_err) => format!("{config_err}"),
        other => format!("{other}"),
    }
}

/// Validate any scenario entry (metrics, logs, histogram, or summary).
///
/// Dispatches to the appropriate validator based on the entry variant. This
/// centralises the `match ScenarioEntry { ... }` dispatch so that neither
/// the CLI nor the server needs to duplicate it.
///
/// # Errors
///
/// Returns [`SondaError`] if the entry's configuration is invalid.
pub fn validate_entry(entry: &ScenarioEntry) -> Result<(), SondaError> {
    match entry {
        ScenarioEntry::Metrics(config) => validate_config(config),
        ScenarioEntry::Logs(config) => validate_log_config(config),
        ScenarioEntry::Histogram(config) => validate_histogram_config(config),
        ScenarioEntry::Summary(config) => validate_summary_config(config),
    }
}

/// A validated scenario entry paired with its resolved phase offset.
///
/// Produced by [`prepare_entries`] after the expand, validate, and
/// phase-offset-parse pipeline completes. Consumers can iterate over a
/// `Vec<PreparedEntry>` to launch each scenario without repeating
/// validation or parsing logic.
#[derive(Debug)]
pub struct PreparedEntry {
    /// The validated scenario entry, ready to pass to [`launch_scenario`].
    pub entry: ScenarioEntry,
    /// Resolved start delay from the entry's `phase_offset` field.
    ///
    /// `None` when no phase offset was configured or when the offset is zero.
    pub start_delay: Option<Duration>,
    /// Compiler-assigned id used for `while:` / `after:` ref resolution.
    pub id: Option<String>,
    /// Continuous-coupling gate this entry waits on.
    pub while_clause: Option<WhileClause>,
    /// Open / close debounce windows applied to `while:` transitions.
    pub delay_clause: Option<DelayClause>,
}

/// Expand, validate, and resolve phase offsets for a batch of scenario entries.
///
/// This is the single authoritative implementation of the
/// expand -> validate -> parse_phase_offset pipeline. The CLI, multi-runner,
/// and HTTP server all call this function instead of duplicating the logic.
///
/// The function is atomic with respect to validation: if any entry fails
/// expansion, validation, or phase-offset parsing, an error is returned and
/// no entries are prepared. This enables callers to implement batch semantics
/// where nothing is launched unless everything is valid.
///
/// # Parameters
///
/// * `entries` — raw scenario entries, potentially containing multi-column
///   `csv_replay` generators that need expansion.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if any entry fails expansion, validation,
/// or phase-offset parsing. The error message includes the entry index for
/// diagnostics.
pub fn prepare_entries(entries: Vec<ScenarioEntry>) -> Result<Vec<PreparedEntry>, SondaError> {
    Ok(prepare_entries_grouped(entries)?
        .into_iter()
        .flatten()
        .collect())
}

/// Check entries against the sinks already running in this process,
/// starting nothing: a `gnmi_target` address that a running target serves
/// with other settings, or whose port a running target on another address
/// holds, is refused, as it would be when the entry's sink starts.
///
/// Only targets bound before the call are seen, so an entry that passes can
/// still be refused when its sink starts, if another target binds first.
///
/// # Errors
///
/// [`SondaError::Config`] for the first entry refused.
pub fn check_running_sinks<'a>(
    entries: impl IntoIterator<Item = &'a ScenarioEntry>,
) -> Result<(), SondaError> {
    #[cfg(feature = "gnmi")]
    for entry in entries {
        if let crate::sink::SinkConfig::GnmiTarget(cfg) = &entry.base().sink {
            crate::sink::gnmi::registry().check(cfg)?;
        }
    }
    #[cfg(not(feature = "gnmi"))]
    let _ = entries;
    Ok(())
}

/// [`prepare_entries`], with each authored entry's results kept together.
///
/// `result[i]` holds every entry that authored entry `i` expanded into, in
/// order. Usually one; a multi-column `csv_replay` produces one per series, and
/// an entry that expands to nothing produces an empty inner `Vec` — so the
/// outer length always equals the input length and index `i` is always the
/// authored entry `i`.
///
/// This exists because a caller that has to render an expanded entry alongside
/// something only the AUTHORED entry knows — `sonda --dry-run run` and the
/// `while:` / `after:` / `clock_group` fields the compiler resolves — needs the
/// mapping, and flattening throws it away. Returning the grouping rather than
/// stamping an index onto [`PreparedEntry`] keeps that struct's shape: it has
/// all-public fields and no `#[non_exhaustive]`, so adding one would break
/// every downstream struct literal (review #583 r2 M1).
pub fn prepare_entries_grouped(
    entries: Vec<ScenarioEntry>,
) -> Result<Vec<Vec<PreparedEntry>>, SondaError> {
    // Phase 1: expand csv_replay multi-column entries, tracking the original
    // input index for each expanded entry so error messages reference the
    // index the caller provided rather than the post-expansion position.
    let group_count = entries.len();
    let mut expanded: Vec<(usize, ScenarioEntry)> = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        let batch = expand_entry(entry).map_err(|e| {
            SondaError::Config(ConfigError::invalid(format!(
                "scenario[{i}]: {}",
                inner_error_message(&e)
            )))
        })?;
        for entry in batch {
            expanded.push((i, entry));
        }
    }

    // Phase 1.5: desugar operational generator aliases (flap, steady, leak,
    // etc.) into their underlying GeneratorConfig variants. This must happen
    // after expand (so csv_replay is resolved) and before validate (so the
    // concrete generator types pass validation).
    let expanded: Vec<(usize, ScenarioEntry)> = expanded
        .into_iter()
        .map(|(idx, entry)| {
            let desugared = desugar_entry(entry).map_err(|e| {
                SondaError::Config(ConfigError::invalid(format!(
                    "scenario[{idx}]: desugaring failed: {}",
                    inner_error_message(&e)
                )))
            })?;
            Ok((idx, desugared))
        })
        .collect::<Result<Vec<_>, SondaError>>()?;

    // Phase 1.75: rules that span more than one entry of the file.
    validate_file(
        &expanded
            .iter()
            .map(|(idx, entry)| (*idx, entry))
            .collect::<Vec<_>>(),
    )?;

    // Phase 2: validate all entries and resolve phase offsets, keeping each
    // authored entry's results in its own bucket.
    let mut prepared: Vec<Vec<PreparedEntry>> = Vec::with_capacity(group_count);
    prepared.resize_with(group_count, Vec::new);
    for (orig_idx, entry) in expanded {
        validate_entry(&entry).map_err(|e| {
            SondaError::Config(ConfigError::invalid(format!(
                "scenario[{orig_idx}]: {}",
                inner_error_message(&e)
            )))
        })?;

        let start_delay = match entry.phase_offset() {
            Some(offset) => crate::config::validate::parse_phase_offset(offset).map_err(|e| {
                SondaError::Config(ConfigError::invalid(format!(
                    "scenario[{orig_idx}] phase_offset: {}",
                    inner_error_message(&e)
                )))
            })?,
            None => None,
        };

        prepared[orig_idx].push(PreparedEntry {
            entry,
            start_delay,
            id: None,
            while_clause: None,
            delay_clause: None,
        });
    }

    Ok(prepared)
}

/// Check the rules that span more than one entry of a file.
///
/// `entries` is every entry of the file after `csv_replay` expansion and alias
/// desugaring, each paired with the index of the authored entry it came from;
/// errors name that index. [`prepare_entries`] and the gated launch path both
/// call this before they validate the entries one by one, so a file is held to
/// the same cross-entry rules on either path: entries sharing a `gnmi_target`
/// address agree on its settings, and every `gnmi` encoder agrees with the
/// entries that use it.
pub(crate) fn validate_file(entries: &[(usize, &ScenarioEntry)]) -> Result<(), SondaError> {
    #[cfg(feature = "gnmi")]
    {
        validate_gnmi_target_addresses(entries)?;
        validate_gnmi_file(entries)?;
    }
    let _ = entries;
    Ok(())
}

/// Check that the `gnmi_target` sinks of one file can share their listen
/// addresses.
///
/// Entries on one address share one target, so they must use the same
/// `max_subscribers`, `subscriber_buffer` and `sample_floor`. A wildcard
/// address such as `0.0.0.0:57400` and an address of a family it covers on
/// the same port cannot both listen (an IPv6 wildcard covers both
/// families). Port 0 always binds a target of its own and is never
/// compared, and a sink whose config does not resolve is left to per-entry
/// validation. Every failure is a [`SondaError::Config`] naming both entries.
#[cfg(feature = "gnmi")]
fn validate_gnmi_target_addresses(entries: &[(usize, &ScenarioEntry)]) -> Result<(), SondaError> {
    use crate::sink::gnmi::TargetSettings;
    use crate::sink::SinkConfig;

    let mut seen: Vec<(usize, TargetSettings)> = Vec::new();
    for (index, entry) in entries {
        let SinkConfig::GnmiTarget(cfg) = &entry.base().sink else {
            continue;
        };
        let Ok(settings) = cfg.settings() else {
            continue;
        };
        if settings.listen.port() == 0 {
            continue;
        }
        for (other, earlier) in &seen {
            if earlier.listen.port() != settings.listen.port() {
                continue;
            }
            if earlier.listen == settings.listen {
                if !earlier.serves(&settings) {
                    return Err(SondaError::Config(ConfigError::invalid(format!(
                        "scenario[{other}] and scenario[{index}] both use gnmi_target {} \
                         with different settings ({}); every sink on one address must use \
                         the same settings",
                        settings.listen,
                        earlier.beside(&settings),
                    ))));
                }
            } else if crate::sink::gnmi::listen_overlap(earlier.listen, settings.listen) {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "scenario[{other}] listens on gnmi_target {} and scenario[{index}] on {}; \
                     a wildcard address and another address cannot share port {}, so use one \
                     address for both",
                    earlier.listen,
                    settings.listen,
                    settings.listen.port(),
                ))));
            }
        }
        seen.push((*index, settings));
    }
    Ok(())
}

/// Check every `gnmi` encoder in a file against the entries that use it.
///
/// A metrics entry emits one series, named after the entry. A histogram
/// emits `<name>_bucket` (with `le`), `<name>_sum` and `<name>_count`; a
/// summary emits `<name>` (with `quantile`), `<name>_sum` and `<name>_count`.
///
/// - Entries that use equal encoder configs share one encoder. Every key in
///   that encoder's `values:` and `paths:` is exactly the name of one of
///   those entries' series. Otherwise the error names the key and, when one
///   is within two edits, the closest series name; a key that is a
///   histogram's own name points at its `<name>_bucket` series.
/// - Two series with different names must not write the same leaf on the
///   same target of the same listen address. A series' leaf is its `origin`
///   and its effective template with `{name}` rendered as the series' name;
///   every other element name and key value is a literal or a label
///   placeholder. Two leaves meet when they have the same origin, the same
///   number of elements and the same key names in each, and every element
///   name and key value can be equal: two literals when they are the same
///   text, a literal and a placeholder when the label can take that value,
///   and two placeholders when their labels can take a common value
///   ([`label_value_set`](crate::config::BaseScheduleConfig::label_value_set),
///   [`overlaps`](crate::config::LabelValueSet::overlaps)). A histogram's
///   `le` takes each bucket bound as the runner formats it and `+Inf`, and a
///   summary's `quantile` each quantile as the runner formats it. A series'
///   target is the value of the encoder's `target_label`, or empty on events
///   that do not carry it, and two targets meet when their value sets overlap,
///   whichever label each encoder takes its target from. Its listen address
///   is the `gnmi_target` sink's: entries on different addresses never share
///   a leaf, an entry on port 0 binds a target of its own, and an entry whose
///   `listen` does not parse is left to per-entry validation. Series on one
///   address are compared pairwise.
///
/// Two comparisons are approximate:
///
/// - Each element name, key value and target is compared on its own, so a
///   label used in two places is matched in each place separately. This can
///   reject two series that never write the same leaf, and never misses one.
/// - A `random` cardinality spike's values are compared one by one up to
///   [`MAX_ENUMERATED_SPIKE`](crate::config::MAX_ENUMERATED_SPIKE) values. A
///   larger spike's values are compared by their shape only, its prefix
///   followed by 16 lowercase hex digits, so it meets any value of that
///   shape, and the error says the two metrics may collide.
///
/// An entry whose encoder config does not build, or that has no template for
/// a series, is left to per-entry validation.
#[cfg(feature = "gnmi")]
fn validate_gnmi_file(entries: &[(usize, &ScenarioEntry)]) -> Result<(), SondaError> {
    use std::net::SocketAddr;

    use crate::config::validate::gnmi_series;
    use crate::config::{LabelValueSet, Overlap, MAX_ENUMERATED_SPIKE};
    use crate::encoder::gnmi::path::NAME_PLACEHOLDER;
    use crate::encoder::gnmi::proto::Path;
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
    use crate::model::metric::Labels;
    use crate::sink::SinkConfig;

    let invalid = |idx: usize, msg: String| {
        SondaError::Config(ConfigError::invalid(format!("scenario[{idx}]: {msg}")))
    };

    // Every gnmi entry: its position in `entries`, authored index, schedule
    // config, encoder config and series.
    let gnmi_entries: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(position, (idx, entry))| {
            let (base, cfg, series) = gnmi_series(entry)?;
            let histogram = matches!(entry, ScenarioEntry::Histogram(_)).then_some(&base.name);
            Some((position, *idx, base, cfg, series, histogram))
        })
        .collect();

    // One group per distinct encoder config: the index of its first entry,
    // the names of the series it serves, and the names of its histograms.
    let mut encoders: Vec<(&GnmiEncoderConfig, usize, Vec<&str>, Vec<&str>)> = Vec::new();
    for (_, idx, _, cfg, series, histogram) in &gnmi_entries {
        let group = match encoders.iter_mut().position(|(seen, ..)| *seen == *cfg) {
            Some(group) => group,
            None => {
                encoders.push((*cfg, *idx, Vec::new(), Vec::new()));
                encoders.len() - 1
            }
        };
        let (_, _, names, histograms) = &mut encoders[group];
        names.extend(series.iter().map(|s| s.name.as_str()));
        histograms.extend(histogram.map(String::as_str));
    }
    for (cfg, idx, mut names, histograms) in encoders {
        names.sort_unstable();
        names.dedup();
        for (field, mut keys) in [
            ("values", cfg.values.keys().collect::<Vec<_>>()),
            ("paths", cfg.paths.keys().collect::<Vec<_>>()),
        ] {
            keys.sort_unstable();
            let Some(key) = keys
                .into_iter()
                .find(|key| names.binary_search(&key.as_str()).is_err())
            else {
                continue;
            };
            let bucket = histograms
                .contains(&key.as_str())
                .then(|| format!("{key}_bucket"));
            let hint = match bucket.as_deref().or_else(|| nearest_name(key, &names)) {
                Some(near) => format!("; did you mean `{near}`?"),
                None => String::new(),
            };
            return Err(invalid(
                idx,
                format!(
                    "gnmi encoder {field}: key `{key}` names no metric this encoder serves{hint}"
                ),
            ));
        }
    }

    /// The target a series is served from: a listen address, a port-0
    /// target of the entry's own, or none when the sink is not a
    /// `gnmi_target`.
    #[derive(Clone, Copy, PartialEq)]
    enum Address {
        Listen(SocketAddr),
        Own(usize),
        None,
    }

    /// A series that writes one gnmi leaf.
    struct Writer<'a> {
        address: Address,
        name: &'a str,
        /// The leaf with `{name}` rendered and every other placeholder `p`
        /// rendered as `{p}`.
        leaf: Path,
        target_label: &'a str,
        /// The values of `Path.target`: the target label's values, and the
        /// empty target wherever an event does not carry it.
        target: LabelValueSet,
        /// Each label placeholder of the leaf, with its label's values; the
        /// label the runner adds to this series takes the values it gives it.
        labels: Vec<(String, Option<LabelValueSet>)>,
    }

    /// One element name or key value of a leaf.
    enum Term<'a> {
        Literal(&'a str),
        Label(&'a str, Option<&'a LabelValueSet>),
    }

    fn term<'a>(writer: &'a Writer<'_>, rendered: &'a str) -> Term<'a> {
        match rendered.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            None => Term::Literal(rendered),
            Some(label) => match writer.labels.iter().find(|(known, _)| known == label) {
                Some((known, set)) => Term::Label(known, set.as_ref()),
                None => Term::Label(label, None),
            },
        }
    }

    /// The label's values with the empty target wherever it is absent.
    fn empty_where_absent(set: Option<LabelValueSet>) -> LabelValueSet {
        let outside = |set: Option<Box<LabelValueSet>>| {
            Some(Box::new(empty_where_absent(set.map(|set| *set))))
        };
        match set {
            None => LabelValueSet::Static(String::new()),
            Some(LabelValueSet::SpikeCounter {
                prefix,
                cardinality,
                outside: closed,
            }) => LabelValueSet::SpikeCounter {
                prefix,
                cardinality,
                outside: outside(closed),
            },
            Some(LabelValueSet::RandomShape {
                prefix,
                seed,
                cardinality,
                outside: closed,
            }) => LabelValueSet::RandomShape {
                prefix,
                seed,
                cardinality,
                outside: outside(closed),
            },
            Some(set) => set,
        }
    }

    /// Whether element name or key value `x` of `a` can equal `y` of `b`.
    /// A meeting by a random spike's shape records the labels whose values
    /// are compared by shape in `shaped`.
    fn terms_meet<'a>(
        a: &'a Writer<'_>,
        x: &'a str,
        b: &'a Writer<'_>,
        y: &'a str,
        shaped: &mut Vec<(&'a str, &'a str)>,
    ) -> bool {
        let (left, right) = (term(a, x), term(b, y));
        let overlap = match (&left, &right) {
            (Term::Literal(x), Term::Literal(y)) if x == y => Overlap::Certain,
            (Term::Literal(value), Term::Label(_, Some(set)))
            | (Term::Label(_, Some(set)), Term::Literal(value)) => set.meets_value(value),
            (Term::Label(_, Some(set)), Term::Label(_, Some(other))) => set.meets(other),
            _ => Overlap::Disjoint,
        };
        if overlap == Overlap::ByShape {
            for (writer, side) in [(a, &left), (b, &right)] {
                if let Term::Label(label, Some(set)) = side {
                    if set.compared_by_shape() {
                        shaped.push((writer.name, label));
                    }
                }
            }
        }
        overlap != Overlap::Disjoint
    }

    /// The error for two series that can write the same leaf on the same
    /// target, or `None` when they cannot.
    fn collision(other: &Writer<'_>, writer: &Writer<'_>) -> Option<String> {
        let mut shaped = Vec::new();
        let leaves_meet = other.leaf.origin == writer.leaf.origin
            && other.leaf.elem.len() == writer.leaf.elem.len()
            && other.leaf.elem.iter().zip(&writer.leaf.elem).all(|(x, y)| {
                x.key.len() == y.key.len()
                    && terms_meet(other, &x.name, writer, &y.name, &mut shaped)
                    && x.key.iter().all(|(key, value)| {
                        y.key.get(key).is_some_and(|theirs| {
                            terms_meet(other, value, writer, theirs, &mut shaped)
                        })
                    })
            });
        if !leaves_meet {
            return None;
        }
        match other.target.meets(&writer.target) {
            Overlap::Disjoint => return None,
            Overlap::ByShape => {
                for side in [other, writer] {
                    if side.target.compared_by_shape() {
                        shaped.push((side.name, side.target_label));
                    }
                }
            }
            Overlap::Certain => {}
        }

        let spelled = if other.leaf == writer.leaf {
            String::new()
        } else {
            format!(" (`{}` writes it as `{}`)", writer.name, writer.leaf)
        };
        let on_target = match (&other.target, &writer.target) {
            (LabelValueSet::Static(value), LabelValueSet::Static(_)) if value.is_empty() => {
                " with no target".to_string()
            }
            (LabelValueSet::Static(value), LabelValueSet::Static(_)) => {
                format!(" on target `{value}`")
            }
            _ => {
                let varying: Vec<String> = [other, writer]
                    .into_iter()
                    .filter(|side| !matches!(side.target, LabelValueSet::Static(_)))
                    .map(|side| format!("`{}` takes `{}`", side.name, side.target_label))
                    .collect();
                let source = if varying.len() == 1 {
                    "a dynamic or spike label"
                } else {
                    "dynamic or spike labels"
                };
                format!(
                    " on a target they may share: {} from {source}",
                    varying.join(" and ")
                )
            }
        };
        let advice = "give each metric its own path, for example with {name} in the template \
                      or its own paths: entry";
        if shaped.is_empty() {
            return Some(format!(
                "metrics `{}` and `{}` would both write the gnmi leaf `{}`{spelled}{on_target}, \
                 each overwriting the other; {advice}",
                other.name, writer.name, other.leaf
            ));
        }
        shaped.sort_unstable();
        shaped.dedup();
        let random_labels: Vec<String> = shaped
            .iter()
            .map(|(metric, label)| format!("`{label}` of `{metric}`"))
            .collect();
        Some(format!(
            "metrics `{}` and `{}` may collide on the gnmi leaf `{}`{spelled}{on_target}: the \
             values of {} come from a random cardinality spike of more than {} values and are \
             compared by their prefix and 16 hex digits only; {advice}",
            other.name,
            writer.name,
            other.leaf,
            random_labels.join(" and "),
            MAX_ENUMERATED_SPIKE
        ))
    }

    let mut writers: Vec<Writer<'_>> = Vec::new();
    for (position, idx, base, cfg, series, _) in &gnmi_entries {
        let address = match &base.sink {
            SinkConfig::GnmiTarget(sink) => match sink.settings() {
                Ok(settings) if settings.listen.port() == 0 => Address::Own(*position),
                Ok(settings) => Address::Listen(settings.listen),
                Err(_) => continue,
            },
            _ => Address::None,
        };
        let Ok(encoder) = GnmiEncoder::new(cfg) else {
            continue;
        };
        let target = empty_where_absent(base.label_value_set(&cfg.target_label));

        for one in series {
            let name = one.name.as_str();
            let Ok(template) = encoder.template_for(name) else {
                continue;
            };
            let symbols: Vec<(&str, String)> = template
                .placeholders()
                .filter(|p| *p != NAME_PLACEHOLDER)
                .map(|p| (p, format!("{{{p}}}")))
                .collect();
            let pairs: Vec<(&str, &str)> = symbols.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let Ok(labels) = Labels::from_pairs(&pairs) else {
                continue;
            };
            let Ok(mut leaf) = template.render(name, &labels) else {
                continue;
            };
            leaf.origin.clone_from(&cfg.origin);
            let mut values: Vec<(String, Option<LabelValueSet>)> = Vec::new();
            for (label, _) in &symbols {
                if values.iter().any(|(known, _)| known == label) {
                    continue;
                }
                let set = match &one.injected {
                    Some((key, injected)) if key == label => Some(injected.clone()),
                    _ => base.label_value_set(label),
                };
                values.push((label.to_string(), set));
            }

            let writer = Writer {
                address,
                name,
                leaf,
                target_label: cfg.target_label.as_str(),
                target: target.clone(),
                labels: values,
            };
            if let Some(message) = writers
                .iter()
                .filter(|other| other.address == writer.address && other.name != name)
                .find_map(|other| collision(other, &writer))
            {
                return Err(invalid(*idx, message));
            }
            writers.push(writer);
        }
    }
    Ok(())
}

/// The name in `names` closest to `key` by [`edit_distance`], if one is within
/// two edits. Ties go to the earliest name in `names`.
#[cfg(feature = "gnmi")]
fn nearest_name<'a>(key: &str, names: &[&'a str]) -> Option<&'a str> {
    names
        .iter()
        .map(|name| (edit_distance(key, name), *name))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

/// Levenshtein distance between `a` and `b`: the fewest single-character
/// insertions, deletions and substitutions that turn one into the other.
#[cfg(feature = "gnmi")]
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for (i, a_char) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(a_char != *b_char);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Launch every entry as one launch, each as a tokio task with the id
/// `{id_prefix}-{i}` and a child token of `cancel`. Their sinks share what a
/// launch shares, such as a gnmi path two entries render.
///
/// Fails fast: when an entry cannot launch, the scenarios already launched
/// are stopped and joined (up to a second each, as `launch_multi_compiled`
/// does), and that entry's error is returned.
///
/// # Errors
///
/// The first entry's launch error.
pub async fn launch_prepared(
    entries: Vec<PreparedEntry>,
    id_prefix: &str,
    cancel: &CancellationToken,
) -> Result<Vec<ScenarioHandle>, SondaError> {
    let launch = LaunchId::next();
    let mut handles = Vec::with_capacity(entries.len());
    for (i, prepared) in entries.into_iter().enumerate() {
        let launched = launch_scenario_in(
            launch,
            format!("{id_prefix}-{i}"),
            prepared.entry,
            cancel.child_token(),
            prepared.start_delay,
        )
        .await;
        match launched {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                for handle in &handles {
                    handle.stop();
                }
                for mut handle in handles {
                    let _ = handle.join_async(Some(Duration::from_secs(1))).await;
                }
                return Err(e);
            }
        }
    }
    Ok(handles)
}

/// Launch a single scenario as a tokio task, as a launch of one: its sink
/// shares nothing with any other scenario's. Use [`launch_prepared`] for the
/// entries of one file.
pub async fn launch_scenario(
    id: String,
    entry: ScenarioEntry,
    cancel: CancellationToken,
    start_delay: Option<Duration>,
) -> Result<ScenarioHandle, SondaError> {
    launch_scenario_in(LaunchId::next(), id, entry, cancel, start_delay).await
}

/// [`launch_scenario`] as part of `launch`.
pub(crate) async fn launch_scenario_in(
    launch: LaunchId,
    id: String,
    entry: ScenarioEntry,
    cancel: CancellationToken,
    start_delay: Option<Duration>,
) -> Result<ScenarioHandle, SondaError> {
    launch_scenario_with_gates_in(
        launch,
        id,
        None,
        entry,
        cancel,
        start_delay,
        None,
        None,
        None,
    )
    .await
}

/// Launch a scenario with optional `while:` / `after:` gating wired in, as a
/// launch of its own.
#[allow(clippy::too_many_arguments)]
pub async fn launch_scenario_with_gates(
    id: String,
    scenario_name: Option<String>,
    entry: ScenarioEntry,
    cancel: CancellationToken,
    start_delay: Option<Duration>,
    upstream_bus: Option<Arc<GateBus>>,
    gate_ctx: Option<GateContext>,
    resolver: Option<Arc<dyn GateBusResolver>>,
) -> Result<ScenarioHandle, SondaError> {
    launch_scenario_with_gates_in(
        LaunchId::next(),
        id,
        scenario_name,
        entry,
        cancel,
        start_delay,
        upstream_bus,
        gate_ctx,
        resolver,
    )
    .await
}

/// [`launch_scenario_with_gates`] as part of `launch`: the scenario's sink is
/// created in that launch.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_scenario_with_gates_in(
    launch: LaunchId,
    id: String,
    scenario_name: Option<String>,
    entry: ScenarioEntry,
    cancel: CancellationToken,
    start_delay: Option<Duration>,
    upstream_bus: Option<Arc<GateBus>>,
    gate_ctx: Option<GateContext>,
    resolver: Option<Arc<dyn GateBusResolver>>,
) -> Result<ScenarioHandle, SondaError> {
    let stats = Arc::new(RwLock::new(ScenarioStats::default()));
    let stats_for_task = Arc::clone(&stats);
    let alive = Arc::new(AtomicBool::new(true));
    let alive_for_task = Arc::clone(&alive);
    let cancel_for_task = cancel.clone();

    let (name, target_rate) = match &entry {
        ScenarioEntry::Metrics(c) => (c.name.clone(), c.rate),
        ScenarioEntry::Logs(c) => (c.name.clone(), c.rate),
        ScenarioEntry::Histogram(c) => (c.name.clone(), c.rate),
        ScenarioEntry::Summary(c) => (c.name.clone(), c.rate),
    };
    let labels = Arc::new(entry.base().labels.clone().unwrap_or_default());
    let prometheus_meta = derive_prometheus_meta(&entry);
    let sink_type = entry.base().sink.kind();

    let started_at = Instant::now();

    let stats_for_state = Arc::clone(&stats);
    let is_gated = gate_ctx.is_some();
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let cleaned_up_for_task = Arc::clone(&cleaned_up);
    let resolver_for_task = resolver.clone();
    let scenario_name_for_task = scenario_name.clone();
    let entry_id_for_task = id.clone();

    // Cross-POST scenarios broadcast via resolver.unregister_entries; broadcasting
    // here too races the registry's subscriber migration.
    let bus_for_finish_guard = if scenario_name_for_task.is_some() && resolver_for_task.is_some() {
        None
    } else {
        upstream_bus.as_ref().map(Arc::clone)
    };

    let task = tokio::task::spawn(async move {
        // Drop guard clears the alive flag on every exit path, including
        // panics — observers polling `is_alive()` see a single clean
        // transition `true → false` regardless of how the task terminates.
        let _alive_guard = AliveGuard {
            flag: alive_for_task,
        };

        // Marks Finished on drop; unregisters this entry's bus when a resolver is wired.
        let _state_guard = StateGuard {
            stats: Arc::clone(&stats_for_state),
            resolver: resolver_for_task,
            scenario_name: scenario_name_for_task,
            entry_id: entry_id_for_task,
            cleaned_up: cleaned_up_for_task,
        };

        // Drops before _state_guard so downstreams see UpstreamGone
        // before the scenario flips to Finished.
        let _bus_finish_guard = BusFinishGuard {
            bus: bus_for_finish_guard,
        };

        let cancel_for_outer = cancel_for_task.clone();
        let runner = async move {
            if let Some(delay) = start_delay {
                tokio::select! {
                    _ = cancel_for_task.cancelled() => {
                        return Ok::<_, SondaError>(());
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }

            if !is_gated {
                if let Ok(mut st) = stats_for_state.write() {
                    st.transition_state(ScenarioState::Running);
                }
            }

            let sink_labels = entry.base().labels.clone();
            let mut sink = create_sink_in(&entry.base().sink, sink_labels.as_ref(), launch).await?;

            match entry {
                ScenarioEntry::Metrics(config) => {
                    run_with_sink_gated(
                        &config,
                        &mut sink,
                        &cancel_for_task,
                        Some(Arc::clone(&stats_for_task)),
                        upstream_bus,
                        gate_ctx,
                    )
                    .await
                }
                ScenarioEntry::Logs(config) => {
                    run_logs_with_sink_gated(
                        &config,
                        &mut sink,
                        &cancel_for_task,
                        Some(Arc::clone(&stats_for_task)),
                        gate_ctx,
                    )
                    .await
                }
                ScenarioEntry::Histogram(config) => {
                    run_histogram_with_sink_gated(
                        &config,
                        &mut sink,
                        &cancel_for_task,
                        Some(Arc::clone(&stats_for_task)),
                        gate_ctx,
                    )
                    .await
                }
                ScenarioEntry::Summary(config) => {
                    run_summary_with_sink_gated(
                        &config,
                        &mut sink,
                        &cancel_for_task,
                        Some(Arc::clone(&stats_for_task)),
                        gate_ctx,
                    )
                    .await
                }
            }
        };

        // `biased` polls the cancel arm first, so an in-flight `tokio::time::sleep`
        // inside the runner is dropped on cancel rather than awaited to completion.
        tokio::select! {
            biased;
            _ = cancel_for_outer.cancelled() => Ok::<_, SondaError>(()),
            result = runner => result,
        }
    });

    Ok(ScenarioHandle::new(
        id,
        name,
        scenario_name,
        cancel,
        Some(task),
        started_at,
        stats,
        target_rate,
        alive,
        labels,
        prometheus_meta,
        cleaned_up,
    )
    .with_sink_type(sink_type))
}

fn derive_prometheus_meta(entry: &ScenarioEntry) -> Option<Arc<PromMeta>> {
    match entry {
        ScenarioEntry::Metrics(c) => {
            let metric_type = c.metric_type.unwrap_or(match c.generator {
                GeneratorConfig::Step { .. } => PromMetricType::Counter,
                _ => PromMetricType::Gauge,
            });
            Some(Arc::new(PromMeta::new(metric_type, c.help.clone())))
        }
        ScenarioEntry::Histogram(c) => Some(Arc::new(PromMeta::new(
            c.metric_type.unwrap_or(PromMetricType::Histogram),
            c.help.clone(),
        ))),
        ScenarioEntry::Summary(c) => Some(Arc::new(PromMeta::new(
            c.metric_type.unwrap_or(PromMetricType::Summary),
            c.help.clone(),
        ))),
        ScenarioEntry::Logs(_) => None,
    }
}

struct AliveGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

struct StateGuard {
    stats: Arc<RwLock<ScenarioStats>>,
    resolver: Option<Arc<dyn GateBusResolver>>,
    scenario_name: Option<String>,
    entry_id: String,
    cleaned_up: Arc<AtomicBool>,
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        if let Ok(mut st) = self.stats.write() {
            if st.state != ScenarioState::Finished {
                st.transition_state(ScenarioState::Finished);
            }
        }
        // delete_scenario sets cleaned_up first; skip duplicate unregister.
        if self.cleaned_up.swap(true, Ordering::SeqCst) {
            return;
        }
        if let (Some(name), Some(resolver)) =
            (self.scenario_name.as_deref(), self.resolver.as_ref())
        {
            resolver.unregister_entries(name, &[self.entry_id.as_str()]);
        }
    }
}

struct BusFinishGuard {
    bus: Option<Arc<GateBus>>,
}

impl Drop for BusFinishGuard {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.take() {
            bus.broadcast_upstream_gone();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::config::{
        BaseScheduleConfig, DistributionConfig, GapConfig, HistogramScenarioConfig,
        LogScenarioConfig, ScenarioConfig, ScenarioEntry, SummaryScenarioConfig,
    };
    use crate::encoder::EncoderConfig;
    use crate::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use crate::sink::SinkConfig;
    #[cfg(all(feature = "gnmi", feature = "config"))]
    use crate::{config::MAX_ENUMERATED_SPIKE, util::random_spike_value};

    // ---- Helpers ------------------------------------------------------------

    /// Build a short-lived metrics `ScenarioEntry` (runs for 200ms then stops).
    fn metrics_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 50.0,
                duration: Some("200ms".to_string()),
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
        })
    }

    /// Build a short-lived logs `ScenarioEntry` (runs for 200ms then stops).
    fn logs_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 50.0,
                duration: Some("200ms".to_string()),
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
                templates: vec![TemplateConfig {
                    message: "test log".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        })
    }

    /// Build an indefinitely-running metrics entry (no duration).
    fn metrics_entry_indefinite(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 100.0,
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
        })
    }

    /// Build an indefinitely-running logs entry (no duration).
    fn logs_entry_indefinite(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 100.0,
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
                templates: vec![TemplateConfig {
                    message: "indefinite log".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(1),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        })
    }

    // ---- validate_entry: dispatches to the correct validator ----------------

    /// validate_entry dispatches to validate_config for a Metrics entry.
    #[test]
    fn validate_entry_accepts_valid_metrics_entry() {
        let entry = metrics_entry("valid_metrics");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid metrics entry: {result:?}"
        );
    }

    /// validate_entry dispatches to validate_log_config for a Logs entry.
    #[test]
    fn validate_entry_accepts_valid_logs_entry() {
        let entry = logs_entry("valid_logs");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid logs entry: {result:?}"
        );
    }

    /// validate_entry returns Err for a Metrics entry with rate = 0 (invalid).
    #[test]
    fn validate_entry_rejects_metrics_entry_with_zero_rate() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "bad_metrics".to_string(),
                rate: 0.0, // invalid
                duration: Some("1s".to_string()),
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
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject a metrics entry with rate=0"
        );
    }

    /// validate_entry returns Err for a Metrics entry with negative rate.
    #[test]
    fn validate_entry_rejects_metrics_entry_with_negative_rate() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "neg_rate".to_string(),
                rate: -5.0,
                duration: Some("1s".to_string()),
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
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject negative rate for metrics entry"
        );
    }

    /// validate_entry returns Err for a Logs entry with rate = 0 (invalid).
    #[test]
    fn validate_entry_rejects_logs_entry_with_zero_rate() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "bad_logs".to_string(),
                rate: 0.0, // invalid
                duration: Some("1s".to_string()),
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
                templates: vec![TemplateConfig {
                    message: "msg".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject a logs entry with rate=0"
        );
    }

    /// validate_entry returns Err for a Metrics entry with an invalid duration.
    #[test]
    fn validate_entry_rejects_metrics_entry_with_bad_duration() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "bad_dur".to_string(),
                rate: 10.0,
                duration: Some("not_a_duration".to_string()),
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
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject an invalid duration string"
        );
    }

    // ---- launch_scenario: returns a running handle --------------------------

    /// launch_scenario with a metrics entry returns a handle whose thread is alive.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_scenario_metrics_returns_running_handle() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry_indefinite("launch_metrics");

        let mut handle = launch_scenario("test-id-1".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed for valid metrics entry");

        // The thread must be alive immediately after launch.
        assert!(
            handle.is_running(),
            "handle must report is_running() == true immediately after launch"
        );

        // Verify the handle fields are populated correctly.
        assert_eq!(handle.id, "test-id-1");
        assert_eq!(handle.name, "launch_metrics");

        // Shut down cleanly.
        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed after stop");
    }

    /// launch_scenario with a logs entry returns a handle whose thread is alive.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_scenario_logs_returns_running_handle() {
        let cancel = CancellationToken::new();
        let entry = logs_entry_indefinite("launch_logs");

        let mut handle = launch_scenario("test-id-2".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed for valid logs entry");

        assert!(
            handle.is_running(),
            "handle must report is_running() == true for a launched logs scenario"
        );
        assert_eq!(handle.name, "launch_logs");

        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed after stop");
    }

    // ---- stop() + join() exits cleanly --------------------------------------

    /// stop() followed by join() on a metrics scenario exits cleanly (Ok).
    #[tokio::test(flavor = "multi_thread")]
    async fn stop_then_join_metrics_scenario_returns_ok() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry_indefinite("stop_join_metrics");
        let mut handle = launch_scenario("id-stop-1".to_string(), entry, cancel, None)
            .await
            .expect("launch must succeed");

        handle.stop();
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "join after stop must return Ok for metrics: {result:?}"
        );
        assert!(
            !handle.is_running(),
            "is_running must be false after stop + join"
        );
    }

    /// stop() followed by join() on a logs scenario exits cleanly (Ok).
    #[tokio::test(flavor = "multi_thread")]
    async fn stop_then_join_logs_scenario_returns_ok() {
        let cancel = CancellationToken::new();
        let entry = logs_entry_indefinite("stop_join_logs");
        let mut handle = launch_scenario("id-stop-2".to_string(), entry, cancel, None)
            .await
            .expect("launch must succeed");

        handle.stop();
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "join after stop must return Ok for logs: {result:?}"
        );
    }

    /// A finite-duration scenario exits on its own and join() returns Ok.
    #[tokio::test(flavor = "multi_thread")]
    async fn finite_duration_scenario_exits_naturally_and_join_returns_ok() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry("natural_exit");
        let mut handle = launch_scenario("id-natural".to_string(), entry, cancel, None)
            .await
            .expect("launch must succeed");

        // 200ms duration — wait for it to finish, then join.
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "natural exit must result in Ok join: {result:?}"
        );
    }

    // ---- stats_snapshot(): non-zero total_events after brief run ------------

    /// After letting a launched scenario run briefly, stats show non-zero events.
    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_shows_nonzero_events_after_brief_run() {
        use std::thread;

        let cancel = CancellationToken::new();
        // High rate so events accumulate quickly.
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "stats_test".to_string(),
                rate: 500.0,
                duration: None, // indefinite — we stop it manually
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

        let mut handle = launch_scenario("id-stats".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed");

        // Wait long enough for at least a few events to be emitted and stats updated.
        thread::sleep(Duration::from_millis(200));

        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "stats_snapshot must show non-zero total_events after running for 200ms, got {}",
            snap.total_events
        );
        assert!(
            snap.bytes_emitted > 0,
            "stats_snapshot must show non-zero bytes_emitted, got {}",
            snap.bytes_emitted
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    /// Stats are also tracked for logs scenarios.
    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_shows_nonzero_events_for_logs_scenario() {
        use std::thread;

        let cancel = CancellationToken::new();
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "logs_stats_test".to_string(),
                rate: 500.0,
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
                templates: vec![TemplateConfig {
                    message: "stat tracking log".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(42),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });

        let mut handle = launch_scenario("id-log-stats".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed");

        thread::sleep(Duration::from_millis(200));

        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "log scenario stats must show non-zero total_events, got {}",
            snap.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- elapsed(): positive duration after launch --------------------------

    /// elapsed() is positive immediately after launch.
    #[tokio::test(flavor = "multi_thread")]
    async fn elapsed_is_positive_after_launch() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry_indefinite("elapsed_test");
        let mut handle = launch_scenario("id-elapsed".to_string(), entry, cancel, None)
            .await
            .expect("launch must succeed");

        let d = handle.elapsed();
        assert!(
            d >= Duration::ZERO,
            "elapsed must be non-negative right after launch, got {d:?}"
        );

        handle.stop();
        handle.join(None).ok();
    }

    // ---- start_delay: None starts immediately -------------------------------

    /// launch_scenario with start_delay=None starts emitting events immediately.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_with_no_start_delay_emits_events_immediately() {
        use std::thread;

        let cancel = CancellationToken::new();
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "no_delay_test".to_string(),
                rate: 500.0,
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

        let mut handle = launch_scenario("id-nodelay".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed");

        // Wait briefly and check events have already been emitted.
        thread::sleep(Duration::from_millis(200));
        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "with no start_delay, events should be emitted within 200ms, got {}",
            snap.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- start_delay: Some(Duration) delays the start -----------------------

    /// launch_scenario with start_delay=Some(500ms) does not emit events for
    /// the first ~400ms (allowing margin for thread scheduling).
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_with_start_delay_does_not_emit_during_delay() {
        use std::thread;

        let cancel = CancellationToken::new();
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "delay_test".to_string(),
                rate: 500.0,
                duration: Some("1s".to_string()),
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

        let delay = Duration::from_millis(500);
        let mut handle =
            launch_scenario("id-delay".to_string(), entry, cancel.clone(), Some(delay))
                .await
                .expect("launch must succeed");

        // Check after 100ms — should still be in the delay period.
        thread::sleep(Duration::from_millis(100));
        let snap_early = handle.stats_snapshot();
        assert_eq!(
            snap_early.total_events, 0,
            "during start_delay, total_events should be 0, got {}",
            snap_early.total_events
        );

        // Wait for the delay to expire and some events to be emitted.
        thread::sleep(Duration::from_millis(600));
        let snap_after = handle.stats_snapshot();
        assert!(
            snap_after.total_events > 0,
            "after start_delay expires, events should be emitted, got {}",
            snap_after.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- Shutdown during start_delay exits cleanly --------------------------

    /// Sending shutdown during start_delay causes the thread to exit cleanly
    /// without hanging.
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_during_start_delay_exits_cleanly() {
        use std::thread;
        use std::time::Instant;

        let cancel = CancellationToken::new();
        let entry = metrics_entry_indefinite("shutdown_delay");

        // Set a long delay (10 seconds) so we can verify the shutdown works.
        let delay = Duration::from_secs(10);
        let mut handle = launch_scenario(
            "id-shutdown-delay".to_string(),
            entry,
            cancel.clone(),
            Some(delay),
        )
        .await
        .expect("launch must succeed");

        // Wait 100ms then signal shutdown.
        thread::sleep(Duration::from_millis(100));
        handle.stop();

        // Join should succeed quickly, well within 2 seconds.
        let start = Instant::now();
        let result = handle.join(Some(Duration::from_secs(2)));
        let elapsed = start.elapsed();

        assert!(
            result.is_ok(),
            "join after shutdown during delay must return Ok: {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "thread must exit promptly after shutdown during delay, took {:?}",
            elapsed
        );

        // No events should have been emitted since we stopped during the delay.
        let snap = handle.stats_snapshot();
        assert_eq!(
            snap.total_events, 0,
            "no events should be emitted when shutdown during delay, got {}",
            snap.total_events
        );
    }

    // ---- start_delay with logs scenario -------------------------------------

    /// launch_scenario with start_delay works for logs scenarios too.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_logs_with_start_delay_does_not_emit_during_delay() {
        use std::thread;

        let cancel = CancellationToken::new();
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "log_delay_test".to_string(),
                rate: 500.0,
                duration: Some("1s".to_string()),
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
                templates: vec![TemplateConfig {
                    message: "delayed log".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });

        let delay = Duration::from_millis(500);
        let mut handle = launch_scenario(
            "id-log-delay".to_string(),
            entry,
            cancel.clone(),
            Some(delay),
        )
        .await
        .expect("launch must succeed");

        // Check during the delay.
        thread::sleep(Duration::from_millis(100));
        let snap_early = handle.stats_snapshot();
        assert_eq!(
            snap_early.total_events, 0,
            "logs scenario should not emit during start_delay, got {}",
            snap_early.total_events
        );

        // Wait for delay to expire.
        thread::sleep(Duration::from_millis(600));
        let snap_after = handle.stats_snapshot();
        assert!(
            snap_after.total_events > 0,
            "logs scenario should emit after delay, got {}",
            snap_after.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- Histogram helpers --------------------------------------------------

    /// Build a short-lived histogram `ScenarioEntry` (runs for 200ms then stops).
    fn histogram_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Histogram(HistogramScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 50.0,
                duration: Some("200ms".to_string()),
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
            buckets: None,
            distribution: DistributionConfig::Exponential { rate: 10.0 },
            observations_per_tick: Some(50),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        })
    }

    /// Build a short-lived summary `ScenarioEntry` (runs for 200ms then stops).
    fn summary_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Summary(SummaryScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 50.0,
                duration: Some("200ms".to_string()),
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
            quantiles: None,
            distribution: DistributionConfig::Normal {
                mean: 0.1,
                stddev: 0.02,
            },
            observations_per_tick: Some(50),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        })
    }

    // ---- launch_scenario: histogram runs to completion ----------------------

    /// A short histogram scenario launches, runs to completion, and join returns Ok.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_histogram_scenario_runs_to_completion() {
        let cancel = CancellationToken::new();
        let entry = histogram_entry("launch_histogram");
        let mut handle = launch_scenario("id-histogram".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed for valid histogram entry");

        let result = handle.join(Some(Duration::from_secs(5)));
        assert!(
            result.is_ok(),
            "histogram scenario must run to completion: {result:?}"
        );
    }

    // ---- launch_scenario: summary runs to completion -----------------------

    /// A short summary scenario launches, runs to completion, and join returns Ok.
    #[tokio::test(flavor = "multi_thread")]
    async fn launch_summary_scenario_runs_to_completion() {
        let cancel = CancellationToken::new();
        let entry = summary_entry("launch_summary");
        let mut handle = launch_scenario("id-summary".to_string(), entry, cancel.clone(), None)
            .await
            .expect("launch must succeed for valid summary entry");

        let result = handle.join(Some(Duration::from_secs(5)));
        assert!(
            result.is_ok(),
            "summary scenario must run to completion: {result:?}"
        );
    }

    // ---- validate_entry: histogram and summary dispatching ------------------

    /// validate_entry dispatches to validate_histogram_config for a Histogram entry.
    #[test]
    fn validate_entry_accepts_valid_histogram_entry() {
        let entry = histogram_entry("valid_histogram");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid histogram entry: {result:?}"
        );
    }

    /// validate_entry dispatches to validate_summary_config for a Summary entry.
    #[test]
    fn validate_entry_accepts_valid_summary_entry() {
        let entry = summary_entry("valid_summary");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid summary entry: {result:?}"
        );
    }

    // ---- prepare_entries tests ------------------------------------------------

    /// prepare_entries accepts an empty list and returns an empty result.
    #[test]
    fn prepare_entries_empty_list_returns_empty() {
        let result = prepare_entries(vec![]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    /// prepare_entries accepts a single valid metrics entry.
    #[test]
    fn prepare_entries_single_valid_entry() {
        let entries = vec![metrics_entry("prep_metric")];
        let result = prepare_entries(entries);
        assert!(result.is_ok(), "must accept valid entry: {result:?}");
        let prepared = result.unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].entry.base().name, "prep_metric");
        assert!(
            prepared[0].start_delay.is_none(),
            "entry without phase_offset must have no start_delay"
        );
    }

    /// prepare_entries accepts multiple valid entries of mixed signal types.
    #[test]
    fn prepare_entries_mixed_signal_types() {
        let entries = vec![
            metrics_entry("prep_m"),
            logs_entry("prep_l"),
            histogram_entry("prep_h"),
            summary_entry("prep_s"),
        ];
        let result = prepare_entries(entries);
        assert!(result.is_ok(), "must accept mixed entries: {result:?}");
        assert_eq!(result.unwrap().len(), 4);
    }

    /// prepare_entries rejects a batch with an invalid entry and returns error.
    #[test]
    fn prepare_entries_rejects_invalid_entry() {
        let invalid = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "invalid_rate".to_string(),
                rate: 0.0, // invalid
                duration: Some("1s".to_string()),
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

        let entries = vec![metrics_entry("good"), invalid];
        let result = prepare_entries(entries);
        assert!(result.is_err(), "must reject batch with invalid entry");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("scenario[1]"),
            "error must include the entry index, got: {err_msg}"
        );
    }

    /// prepare_entries resolves phase_offset into start_delay.
    #[test]
    fn prepare_entries_resolves_phase_offset() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "offset_test".to_string(),
                rate: 10.0,
                duration: Some("200ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("500ms".to_string()),
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

        let result = prepare_entries(vec![entry]);
        assert!(result.is_ok());
        let prepared = result.unwrap();
        assert_eq!(
            prepared[0].start_delay,
            Some(Duration::from_millis(500)),
            "500ms phase_offset must resolve to 500ms start_delay"
        );
    }

    /// prepare_entries treats "0s" phase_offset as no delay.
    #[test]
    fn prepare_entries_zero_phase_offset_is_none() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "zero_offset".to_string(),
                rate: 10.0,
                duration: Some("200ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("0s".to_string()),
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

        let result = prepare_entries(vec![entry]);
        assert!(result.is_ok());
        let prepared = result.unwrap();
        assert!(
            prepared[0].start_delay.is_none(),
            "0s phase_offset must resolve to None (no delay)"
        );
    }

    /// prepare_entries rejects invalid phase_offset strings.
    #[test]
    fn prepare_entries_rejects_invalid_phase_offset() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "bad_offset".to_string(),
                rate: 10.0,
                duration: Some("200ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("not_a_duration".to_string()),
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

        let result = prepare_entries(vec![entry]);
        assert!(result.is_err(), "must reject invalid phase_offset");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("phase_offset"),
            "error must mention phase_offset, got: {err_msg}"
        );
    }

    /// PreparedEntry is Debug.
    #[test]
    fn prepared_entry_is_debuggable() {
        let entry = metrics_entry("debug_test");
        let prepared = prepare_entries(vec![entry]).unwrap();
        let s = format!("{:?}", prepared[0]);
        assert!(s.contains("PreparedEntry"));
    }

    #[test]
    fn alive_guard_clears_flag_on_panic() {
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_thread = Arc::clone(&alive);

        let thread = std::thread::Builder::new()
            .name("alive-guard-panic".to_string())
            .spawn(move || {
                let _guard = AliveGuard {
                    flag: alive_for_thread,
                };
                panic!("intentional panic for AliveGuard test");
            })
            .expect("spawn must succeed");

        let join_result = thread.join();
        assert!(
            join_result.is_err(),
            "thread must panic and surface Err on join"
        );
        assert!(
            !alive.load(Ordering::SeqCst),
            "AliveGuard Drop must clear the alive flag even when the thread panics"
        );
    }

    #[test]
    fn state_guard_writes_finished_on_drop() {
        let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
        {
            let mut st = stats.write().unwrap();
            st.state = ScenarioState::Running;
        }
        {
            let _g = StateGuard {
                stats: Arc::clone(&stats),
                resolver: None,
                scenario_name: None,
                entry_id: String::new(),
                cleaned_up: Arc::new(AtomicBool::new(false)),
            };
        }
        assert_eq!(stats.read().unwrap().state, ScenarioState::Finished);
    }

    #[test]
    fn state_guard_writes_finished_on_thread_panic() {
        let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
        let stats_for_thread = Arc::clone(&stats);

        let thread = std::thread::Builder::new()
            .name("state-guard-panic".to_string())
            .spawn(move || {
                let _g = StateGuard {
                    stats: stats_for_thread,
                    resolver: None,
                    scenario_name: None,
                    entry_id: String::new(),
                    cleaned_up: Arc::new(AtomicBool::new(false)),
                };
                panic!("intentional panic for StateGuard test");
            })
            .expect("spawn must succeed");

        let result = thread.join();
        assert!(result.is_err(), "thread must panic");
        assert_eq!(
            stats.read().unwrap().state,
            ScenarioState::Finished,
            "StateGuard Drop must write Finished even on panic"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn state_remains_pending_during_start_delay_then_transitions_to_running() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry_indefinite("delayed_state");
        let mut handle = launch_scenario(
            "delayed-state-id".to_string(),
            entry,
            cancel.clone(),
            Some(Duration::from_millis(200)),
        )
        .await
        .expect("launch must succeed");

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            handle.stats_snapshot().state,
            ScenarioState::Pending,
            "scenario must report Pending while inside start_delay"
        );

        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            handle.stats_snapshot().state,
            ScenarioState::Running,
            "scenario must report Running once start_delay has elapsed"
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_non_gated_scenario_state_transitions_through_running_to_finished() {
        let cancel = CancellationToken::new();
        let entry = metrics_entry("state_lifecycle");
        let mut handle = launch_scenario("state-id".to_string(), entry, cancel, None)
            .await
            .expect("launch must succeed");

        let mut saw_running = false;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if handle.stats_snapshot().state == ScenarioState::Running {
                saw_running = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_running, "non-gated scenario must reach Running");

        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed");
        assert_eq!(
            handle.stats_snapshot().state,
            ScenarioState::Finished,
            "non-gated scenario must end in Finished"
        );
    }

    #[test]
    fn alive_guard_clears_flag_on_clean_exit() {
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_thread = Arc::clone(&alive);

        let thread = std::thread::Builder::new()
            .name("alive-guard-clean".to_string())
            .spawn(move || {
                let _guard = AliveGuard {
                    flag: alive_for_thread,
                };
            })
            .expect("spawn must succeed");

        thread.join().expect("clean thread must join Ok");
        assert!(
            !alive.load(Ordering::SeqCst),
            "AliveGuard Drop must clear the alive flag on clean exit"
        );
    }

    /// prepare_entries error messages reference the original input index, not
    /// the post-expansion index. When entry 0 is valid and entry 1 is invalid,
    /// the error should say "scenario[1]" regardless of how many entries the
    /// expansion of entry 0 produced.
    #[test]
    fn prepare_entries_error_index_refers_to_original_input_index() {
        let valid = metrics_entry("valid_a");
        let invalid = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "invalid_b".to_string(),
                rate: 0.0, // invalid
                duration: Some("1s".to_string()),
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

        // Entry 0 is valid, entry 1 is invalid. Even though entry 0 does not
        // expand (single column), the error for entry 1 should reference
        // "scenario[1]", not a shifted index.
        let entries = vec![valid, invalid];
        let result = prepare_entries(entries);
        assert!(result.is_err(), "must reject batch with invalid entry");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("scenario[1]"),
            "error must reference original input index 1, got: {err_msg}"
        );
    }

    /// prepare_entries error message for phase_offset references the original
    /// input index.
    #[test]
    fn prepare_entries_phase_offset_error_references_original_index() {
        let valid = metrics_entry("valid_first");
        let bad_offset = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "bad_offset".to_string(),
                rate: 10.0,
                duration: Some("1s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("not-a-duration".to_string()),
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

        let entries = vec![valid, bad_offset];
        let result = prepare_entries(entries);
        assert!(result.is_err(), "must reject invalid phase_offset");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("scenario[1]"),
            "error must reference original input index 1, got: {err_msg}"
        );
        assert!(
            err_msg.contains("phase_offset"),
            "error must mention phase_offset, got: {err_msg}"
        );
    }

    // ---- prometheus_meta derivation ----------------------------------------

    fn entry_with_metric_type(metric_type: Option<PromMetricType>) -> ScenarioEntry {
        let mut entry = metrics_entry("meta_metrics");
        if let ScenarioEntry::Metrics(c) = &mut entry {
            c.metric_type = metric_type;
        }
        entry
    }

    fn step_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate: 50.0,
                duration: Some("200ms".to_string()),
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
            generator: GeneratorConfig::Step {
                start: None,
                step_size: 1.0,
                max: None,
            },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_metrics_scenario_with_no_type_defaults_to_gauge() {
        let entry = entry_with_metric_type(None);
        let mut handle = launch_scenario(
            "id-meta-gauge".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        let meta = handle
            .prometheus_meta
            .as_ref()
            .expect("metrics entry must have prometheus_meta");
        assert_eq!(meta.metric_type, PromMetricType::Gauge);
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_metrics_scenario_with_step_generator_defaults_to_counter() {
        let entry = step_entry("step_counter");
        let mut handle = launch_scenario(
            "id-meta-step".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        let meta = handle
            .prometheus_meta
            .as_ref()
            .expect("metrics entry must have prometheus_meta");
        assert_eq!(meta.metric_type, PromMetricType::Counter);
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_histogram_scenario_defaults_to_histogram() {
        let entry = histogram_entry("hist_default");
        let mut handle = launch_scenario(
            "id-meta-hist".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        let meta = handle
            .prometheus_meta
            .as_ref()
            .expect("histogram entry must have prometheus_meta");
        assert_eq!(meta.metric_type, PromMetricType::Histogram);
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_summary_scenario_defaults_to_summary() {
        let entry = summary_entry("summary_default");
        let mut handle = launch_scenario(
            "id-meta-sum".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        let meta = handle
            .prometheus_meta
            .as_ref()
            .expect("summary entry must have prometheus_meta");
        assert_eq!(meta.metric_type, PromMetricType::Summary);
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_scenario_explicit_metric_type_overrides_default() {
        let entry = entry_with_metric_type(Some(PromMetricType::Counter));
        let mut handle = launch_scenario(
            "id-meta-explicit".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        let meta = handle
            .prometheus_meta
            .as_ref()
            .expect("metrics entry must have prometheus_meta");
        assert_eq!(meta.metric_type, PromMetricType::Counter);
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_log_scenario_has_no_prometheus_meta() {
        let entry = logs_entry("log_no_meta");
        let mut handle = launch_scenario(
            "id-meta-logs".to_string(),
            entry,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("launch must succeed");
        assert!(
            handle.prometheus_meta.is_none(),
            "log scenario must not have prometheus_meta"
        );
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_during_gap_sleep_exits_within_200ms() {
        let cancel = CancellationToken::new();
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "cancel_during_gap_sleep".to_string(),
                rate: 100.0,
                duration: None,
                gaps: Some(GapConfig {
                    every: "3s".to_string(),
                    r#for: "2900ms".to_string(),
                }),
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

        let mut handle = launch_scenario(
            "cancel-gap-sleep-id".to_string(),
            entry,
            cancel.clone(),
            None,
        )
        .await
        .expect("launch must succeed");

        tokio::time::sleep(Duration::from_millis(150)).await;

        let cancel_at = Instant::now();
        handle.stop();
        let join_result = handle.join_async(Some(Duration::from_millis(500))).await;
        let exit_duration = cancel_at.elapsed();

        assert!(
            join_result.is_ok(),
            "join_async must complete within 500ms; got {join_result:?}"
        );
        assert!(
            exit_duration < Duration::from_millis(200),
            "cancel-to-exit must be under 200ms; took {exit_duration:?}"
        );
    }

    // ---- Whole-file gnmi checks ------------------------------------------------

    /// The per-entry pairing rule rejects the gnmi encoder with every sink in
    /// this build. A file that reaches it has passed the whole-file checks,
    /// which run first.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const GNMI_PAIRING_ERROR: &str = "the gnmi encoder can only be used with the gnmi_target sink";

    /// A file with two metrics entries, `in_octets` and `out_octets`, sharing
    /// one gnmi encoder whose body (after `type: gnmi`) is `encoder`.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn two_counters(encoder: &str) -> String {
        format!(
            r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  labels:
    device: leaf-1
    ifName: eth0
  encoder:
    type: gnmi
{encoder}
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {{type: constant, value: 1.0}}
  - signal_type: metrics
    name: out_octets
    generator: {{type: constant, value: 1.0}}
"#
        )
    }

    /// Compile a v2 file into the entries `prepare_entries` receives.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn compile_entries(
        yaml: &str,
        resolver: &crate::compiler::expand::InMemoryPackResolver,
    ) -> Vec<ScenarioEntry> {
        crate::compile::compile_scenario_file(yaml, resolver).expect("fixture must compile")
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn prepare_error(entries: Vec<ScenarioEntry>) -> String {
        prepare_entries(entries)
            .expect_err("a gnmi file must not prepare in this build")
            .to_string()
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_values_key_naming_no_metric_is_rejected() {
        let yaml = two_counters(
            "    path: \"/interfaces/interface[name={ifName}]/state/counters/{name}\"\n    \
             values:\n      in_octet: uint",
        );
        let entries = compile_entries(&yaml, &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");

        let msg = prepare_error(entries);
        assert!(msg.contains("values: key `in_octet`"), "{msg}");
        assert!(msg.contains("did you mean `in_octets`?"), "{msg}");
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_paths_key_naming_no_metric_is_rejected() {
        let yaml = two_counters(
            "    path: \"/interfaces/interface[name={ifName}]/state/counters/{name}\"\n    \
             paths:\n      out_octet: \"/interfaces/interface[name={ifName}]/state/out\"",
        );
        let entries = compile_entries(&yaml, &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");

        let msg = prepare_error(entries);
        assert!(msg.contains("paths: key `out_octet`"), "{msg}");
        assert!(msg.contains("did you mean `out_octets`?"), "{msg}");
    }

    /// A key more than two edits from every metric name gets no suggestion.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_values_key_far_from_every_metric_is_rejected_without_a_suggestion() {
        let yaml = two_counters(
            "    path: \"/interfaces/interface[name={ifName}]/state/counters/{name}\"\n    \
             values:\n      if_speed: uint",
        );
        let entries = compile_entries(&yaml, &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");

        let msg = prepare_error(entries);
        assert!(msg.contains("values: key `if_speed`"), "{msg}");
        assert!(!msg.contains("did you mean"), "{msg}");
    }

    /// Keys may name metrics that appear only once a pack has expanded.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_keys_naming_pack_expanded_metrics_are_accepted() {
        let pack: crate::packs::MetricPackDef = serde_yaml_ng::from_str(
            r#"
name: interface_counters
description: test
category: network
metrics:
  - name: in_octets
    generator: {type: constant, value: 1.0}
  - name: out_octets
    generator: {type: constant, value: 1.0}
"#,
        )
        .expect("pack must parse");
        let mut resolver = crate::compiler::expand::InMemoryPackResolver::new();
        resolver.insert("interface_counters", pack);
        let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  labels:
    device: leaf-1
    ifName: eth0
  encoder:
    type: gnmi
    path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
    values:
      in_octets: uint
    paths:
      out_octets: "/interfaces/interface[name={ifName}]/state/out-octets"
scenarios:
  - signal_type: metrics
    pack: interface_counters
"#;
        let entries = compile_entries(yaml, &resolver);
        let names: Vec<&str> = entries.iter().map(|e| e.base().name.as_str()).collect();
        assert_eq!(
            names,
            ["in_octets", "out_octets"],
            "the pack entry must expand to the metrics the keys name"
        );

        let msg = prepare_error(entries);
        assert!(msg.contains(GNMI_PAIRING_ERROR), "{msg}");
    }

    /// Two metrics whose paths render to the same leaf on the same target are
    /// rejected, whether they share a template without `{name}`, one copies
    /// the other's rendered `{name}` into its `paths:` entry, or they list the
    /// same keys in a different order. The error names both metrics, the leaf
    /// and the target.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::shared_template_without_name(
        "    path: \"/interfaces/interface[name={ifName}]/state\"",
        "openconfig:/interfaces/interface[name={ifName}]/state",
    )]
    #[case::override_copies_the_rendered_name(
        "    path: \"/interfaces/interface[name={ifName}]/state/counters/{name}\"\n    paths:\n      out_octets: \"/interfaces/interface[name={ifName}]/state/counters/in-octets\"",
        "openconfig:/interfaces/interface[name={ifName}]/state/counters/in-octets",
    )]
    #[case::same_keys_in_another_order(
        "    path: \"/interfaces/interface[name={ifName}][type=eth]/state\"\n    paths:\n      out_octets: \"/interfaces/interface[type=eth][name={ifName}]/state\"",
        "openconfig:/interfaces/interface[name={ifName}][type=eth]/state",
    )]
    fn gnmi_metrics_writing_the_same_leaf_are_rejected(#[case] encoder: &str, #[case] leaf: &str) {
        let entries = compile_entries(&two_counters(encoder), &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");

        let msg = prepare_error(entries);
        assert!(msg.contains("metrics `in_octets` and `out_octets`"), "{msg}");
        assert!(msg.contains(&format!("gnmi leaf `{leaf}` on target `leaf-1`")), "{msg}");
    }

    /// Two metrics sharing a template without `{name}`; `in_octets` and
    /// `out_octets` take the given blocks, which decide their target. A block
    /// may also give its entry its own encoder, such as [`HOST_ENCODER`].
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn two_targets(in_octets: &str, out_octets: &str) -> String {
        format!(
            r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  labels:
    ifName: eth0
  encoder:
    type: gnmi
    path: "/interfaces/interface[name={{ifName}}]/state"
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {{type: constant, value: 1.0}}
{in_octets}  - signal_type: metrics
    name: out_octets
    generator: {{type: constant, value: 1.0}}
{out_octets}"#
        )
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_1: &str = "    labels:\n      device: leaf-1\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_2: &str = "    labels:\n      device: leaf-2\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const DYNAMIC_LEAF_1_OR_2: &str =
        "    dynamic_labels:\n      - key: device\n        values: [leaf-1, leaf-2]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_9_REPLACED_BY_DYNAMIC: &str = "    labels:\n      device: leaf-9\n    dynamic_labels:\n      - key: device\n        values: [leaf-1, leaf-2]\n";
    /// A spike over `device` renders `leaf-0` and `leaf-1` while it lasts.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_2_SPIKED: &str = "    labels:\n      device: leaf-2\n    cardinality_spikes:\n      - label: device\n        every: 10s\n        for: 5s\n        cardinality: 2\n        prefix: leaf-\n";
    /// The same template, from an encoder that takes its target from `host`.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const HOST_ENCODER: &str = "    encoder:\n      type: gnmi\n      target_label: host\n      path: \"/interfaces/interface[name={ifName}]/state\"\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const HOST_LEAF_1: &str = "    labels:\n      host: leaf-1\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const HOST_LEAF_2: &str = "    labels:\n      host: leaf-2\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const DYNAMIC_HOST_LEAF_1_OR_2: &str =
        "    dynamic_labels:\n      - key: host\n        values: [leaf-1, leaf-2]\n";
    /// A static `device` with a dynamic label on another key, here a path
    /// placeholder, which does not make the target vary.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_1_DYNAMIC_IFNAME: &str = "    labels:\n      device: leaf-1\n    dynamic_labels:\n      - key: ifName\n        values: [eth0, eth1]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const STATIC_LEAF_2_DYNAMIC_IFNAME: &str = "    labels:\n      device: leaf-2\n    dynamic_labels:\n      - key: ifName\n        values: [eth0, eth1]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const OUT_OCTETS_TARGET_VARIES: &str =
        " on a target they may share: `out_octets` takes `device` from a dynamic or spike label";

    /// Two metrics writing the same leaf collide only when their targets can
    /// match, whichever label each encoder takes its target from. A static
    /// target label is one target, a missing one is the empty target, and a
    /// dynamic or spike one can be any target, including a static one it
    /// replaces. `None` means the file passes the whole-file check; otherwise
    /// the error names the leaf followed by the given clause.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::different_static_targets(STATIC_LEAF_1, STATIC_LEAF_2, None)]
    #[case::absent_and_static_targets("", STATIC_LEAF_1, None)]
    #[case::both_targets_absent("", "", Some(" with no target"))]
    #[case::static_and_dynamic_targets(STATIC_LEAF_1, DYNAMIC_LEAF_1_OR_2, Some(OUT_OCTETS_TARGET_VARIES))]
    #[case::dynamic_replaces_a_different_static(STATIC_LEAF_1, STATIC_LEAF_9_REPLACED_BY_DYNAMIC, Some(OUT_OCTETS_TARGET_VARIES))]
    #[case::static_and_spike_targets(STATIC_LEAF_1, STATIC_LEAF_2_SPIKED, Some(OUT_OCTETS_TARGET_VARIES))]
    #[case::both_targets_dynamic(DYNAMIC_LEAF_1_OR_2, DYNAMIC_LEAF_1_OR_2, Some(" on a target they may share: `in_octets` takes `device` and `out_octets` takes `device` from dynamic or spike labels"))]
    #[case::different_labels_with_different_values(STATIC_LEAF_1, &format!("{HOST_LEAF_2}{HOST_ENCODER}"), None)]
    #[case::different_labels_with_the_same_value(STATIC_LEAF_1, &format!("{HOST_LEAF_1}{HOST_ENCODER}"), Some(" on target `leaf-1`"))]
    #[case::different_labels_both_absent("", HOST_ENCODER, Some(" with no target"))]
    #[case::dynamic_host_and_static_device(&format!("{DYNAMIC_HOST_LEAF_1_OR_2}{HOST_ENCODER}"), STATIC_LEAF_1, Some(" on a target they may share: `in_octets` takes `host` from a dynamic or spike label"))]
    #[case::different_static_targets_with_a_dynamic_placeholder(STATIC_LEAF_1_DYNAMIC_IFNAME, STATIC_LEAF_2_DYNAMIC_IFNAME, None)]
    fn gnmi_metrics_sharing_a_leaf_collide_only_on_a_shared_target(
        #[case] in_octets: &str,
        #[case] out_octets: &str,
        #[case] rejected_with: Option<&str>,
    ) {
        let entries = compile_entries(&two_targets(in_octets, out_octets), &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");
        for (entry, block) in entries.iter().zip([in_octets, out_octets]) {
            let ScenarioEntry::Metrics(config) = entry else {
                panic!("both entries must be metrics entries");
            };
            let crate::encoder::EncoderConfig::Gnmi(gnmi) = &config.encoder else {
                panic!("{}: the entry must use the gnmi encoder", config.name);
            };
            let target = gnmi.target_label.as_str();
            let expected_target = if block.contains("target_label: host") { "host" } else { "device" };
            assert_eq!(target, expected_target, "{}: the target label must be as the case describes", config.name);
            assert_eq!(
                config.labels.as_ref().is_some_and(|l| l.contains_key(target)),
                block.contains(&format!("    labels:\n      {target}: ")),
                "{}: the static target label must be as the case describes",
                config.name
            );
            assert_eq!(
                config.dynamic_labels.iter().flatten().any(|dl| dl.key == target),
                block.contains(&format!("- key: {target}\n")),
                "{}: the dynamic target label must be as the case describes",
                config.name
            );
            assert_eq!(
                config.cardinality_spikes.iter().flatten().any(|spike| spike.label == target),
                block.contains(&format!("- label: {target}\n")),
                "{}: the spike over the target label must be as the case describes",
                config.name
            );
        }

        let msg = prepare_error(entries);
        match rejected_with {
            None => assert!(msg.contains(GNMI_PAIRING_ERROR), "{msg}"),
            Some(clause) => {
                assert!(msg.contains("metrics `in_octets` and `out_octets`"), "{msg}");
                assert!(
                    msg.contains(&format!(
                        "gnmi leaf `openconfig:/interfaces/interface[name={{ifName}}]/state`{clause}, "
                    )),
                    "{msg}"
                );
            }
        }
    }

    /// A key must name a metric that its own encoder serves. `out_octets` has
    /// its own encoder, so `in_octets`'s encoder naming it is rejected.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_key_naming_a_metric_of_another_encoder_is_rejected() {
        let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  labels:
    device: leaf-1
    ifName: eth0
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {type: constant, value: 1.0}
    encoder:
      type: gnmi
      path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
      values:
        out_octets: uint
  - signal_type: metrics
    name: out_octets
    generator: {type: constant, value: 1.0}
    encoder:
      type: gnmi
      path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
"#;
        let entries = compile_entries(yaml, &Default::default());
        let names: Vec<&str> = entries.iter().map(|e| e.base().name.as_str()).collect();
        assert_eq!(
            names,
            ["in_octets", "out_octets"],
            "the file must expand to both metrics"
        );

        let msg = prepare_error(entries);
        assert!(
            msg.contains("scenario[0]: gnmi encoder values: key `out_octets` names no metric this encoder serves"),
            "{msg}"
        );
    }

    /// Metrics that share a template are accepted when `{name}` tells their
    /// paths apart, and metrics with their own `paths:` entries never share one.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::name_in_the_shared_template("    path: \"/interfaces/interface[name={ifName}]/state/{name}\"")]
    #[case::own_paths_entries("    paths:\n      in_octets: \"/interfaces/interface[name={ifName}]/state/in-octets\"\n      out_octets: \"/interfaces/interface[name={ifName}]/state/out-octets\"")]
    fn gnmi_metrics_with_distinct_paths_are_accepted(#[case] encoder: &str) {
        let entries = compile_entries(&two_counters(encoder), &Default::default());
        assert_eq!(entries.len(), 2, "the file must expand to two entries");

        let msg = prepare_error(entries);
        assert!(msg.contains(GNMI_PAIRING_ERROR), "{msg}");
    }

    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::equal("in_octets", "in_octets", 0)]
    #[case::one_insertion("in_octet", "in_octets", 1)]
    #[case::two_edits("flaw", "lawn", 2)]
    #[case::three_edits("kitten", "sitting", 3)]
    #[case::from_empty("", "abc", 3)]
    #[case::to_empty("abc", "", 3)]
    fn edit_distance_counts_single_character_edits(#[case] a: &str, #[case] b: &str, #[case] expected: usize) {
        assert_eq!(edit_distance(a, b), expected);
    }

    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::within_two_edits("in_octe", &["in_octets", "out_octets"], Some("in_octets"))]
    #[case::three_edits_away("in_oct", &["in_octets", "out_octets"], None)]
    #[case::tie_goes_to_the_first_name("aa", &["ab", "ba"], Some("ab"))]
    #[case::no_names("in_octets", &[], None)]
    fn nearest_name_suggests_only_within_two_edits(#[case] key: &str, #[case] names: &[&str], #[case] expected: Option<&str>) {
        assert_eq!(nearest_name(key, names), expected);
    }

    /// A metrics entry on a `gnmi_target` sink listening on `listen`.
    #[cfg(feature = "gnmi")]
    fn gnmi_entry(name: &str, listen: &str, max_subscribers: Option<usize>) -> ScenarioEntry {
        let mut entry = metrics_entry(name);
        if let ScenarioEntry::Metrics(ref mut config) = entry {
            let mut sink = crate::sink::gnmi::GnmiTargetConfig::new(listen);
            sink.max_subscribers = max_subscribers;
            config.base.sink = SinkConfig::GnmiTarget(sink);
            config.encoder = EncoderConfig::Gnmi(crate::encoder::gnmi::GnmiEncoderConfig {
                path: Some("/a/{name}".to_string()),
                ..Default::default()
            });
        }
        entry
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn entries_sharing_an_address_must_agree() {
        prepare_entries(vec![
            gnmi_entry("a", "127.0.0.1:57400", Some(4)),
            gnmi_entry("b", "127.0.0.1:57400", Some(4)),
            gnmi_entry("c", "127.0.0.2:57400", None),
        ])
        .expect("identical settings on one address, and another specific address");

        let err = prepare_entries(vec![
            gnmi_entry("a", "127.0.0.1:57400", Some(4)),
            gnmi_entry("b", "127.0.0.1:57400", Some(8)),
        ])
        .expect_err("different settings on one address");
        let msg = err.to_string();
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(msg.contains("scenario[0] and scenario[1]"), "got {msg}");
        assert!(msg.contains("max_subscribers 4 and 8"), "got {msg}");

        let err = prepare_entries(vec![
            gnmi_entry("a", "0.0.0.0:57400", None),
            gnmi_entry("b", "127.0.0.1:57400", None),
        ])
        .expect_err("a wildcard and a specific address on one port");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(err.to_string().contains("wildcard"), "got {err}");

        prepare_entries(vec![
            gnmi_entry("a", "127.0.0.1:0", Some(4)),
            gnmi_entry("b", "127.0.0.1:0", Some(8)),
        ])
        .expect("port 0 binds a target per entry, so nothing is shared");

        prepare_entries(vec![
            gnmi_entry("a", "0.0.0.0:57400", None),
            gnmi_entry("b", "[::1]:57400", None),
        ])
        .expect("an IPv4 wildcard and an IPv6 address bind side by side");

        // An IPv4-mapped address is the IPv4 address it maps.
        prepare_entries(vec![
            gnmi_entry("a", "[::ffff:127.0.0.1]:57400", Some(4)),
            gnmi_entry("b", "127.0.0.1:57400", Some(4)),
        ])
        .expect("an IPv4-mapped address and the IPv4 address it maps are one address");
        let err = prepare_entries(vec![
            gnmi_entry("a", "[::ffff:127.0.0.1]:57400", Some(4)),
            gnmi_entry("b", "127.0.0.1:57400", Some(8)),
        ])
        .expect_err("one address, different settings");
        let msg = err.to_string();
        assert!(
            msg.contains("both use gnmi_target 127.0.0.1:57400"),
            "got {msg}"
        );
        assert!(!msg.contains("wildcard"), "got {msg}");
    }

    /// A file whose entries share one gnmi encoder through `defaults:`.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn shared_encoder_file(paths: &str, scenarios: &str) -> String {
        format!(
            "version: 2\nkind: runnable\ndefaults:\n  rate: 1\n  duration: 1s\n  encoder:\n    \
             type: gnmi\n    paths:\n{paths}  sink:\n    type: gnmi_target\n    listen: \
             127.0.0.1:57400\nscenarios:\n{scenarios}"
        )
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    const THREE_KINDS: &str = "  - signal_type: metrics\n    name: in_octets\n    generator: {type: constant, value: 1.0}\n  - signal_type: histogram\n    name: latency\n    distribution: {type: exponential, rate: 10.0}\n  - signal_type: summary\n    name: rpc\n    distribution: {type: exponential, rate: 10.0}\n";

    #[cfg(all(feature = "gnmi", feature = "config"))]
    const THREE_KINDS_PATHS: &str = "      in_octets: \"/interfaces/{name}\"\n      latency_bucket: \"/latency[le={le}]/{name}\"\n      latency_sum: \"/latency/{name}\"\n      latency_count: \"/latency/{name}\"\n      rpc: \"/rpc[quantile={quantile}]/{name}\"\n      rpc_sum: \"/rpc/{name}\"\n      rpc_count: \"/rpc/{name}\"\n";

    /// Histogram and summary series are series of the encoder they share
    /// with a metrics entry, so `paths:` keys may name them.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_keys_may_name_histogram_and_summary_series() {
        let yaml = shared_encoder_file(THREE_KINDS_PATHS, THREE_KINDS);
        let entries = compile_entries(&yaml, &Default::default());
        assert_eq!(entries.len(), 3, "metrics, histogram and summary");
        prepare_entries(entries).expect("every key names a series this encoder serves");
    }

    /// A key names a series exactly: a histogram's own name is not one of
    /// its series, and the hint names its bucket series.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_key_naming_a_histogram_points_at_its_bucket_series() {
        let paths = THREE_KINDS_PATHS.replace("latency_bucket:", "latency:");
        let yaml = shared_encoder_file(&paths, THREE_KINDS);
        let msg = prepare_error(compile_entries(&yaml, &Default::default()));
        assert!(
            msg.contains("paths: key `latency` names no metric this encoder serves"),
            "{msg}"
        );
        assert!(msg.contains("did you mean `latency_bucket`?"), "{msg}");
    }

    /// Two histograms whose bucket series render to one leaf collide, like
    /// two metrics would.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_histogram_series_take_part_in_the_leaf_check() {
        let paths = "      a_bucket: \"/latency[le={le}]/bucket\"\n      a_sum: \"/a/{name}\"\n      a_count: \"/a/{name}\"\n      b_bucket: \"/latency[le={le}]/bucket\"\n      b_sum: \"/b/{name}\"\n      b_count: \"/b/{name}\"\n";
        let scenarios = "  - signal_type: histogram\n    name: a\n    distribution: {type: exponential, rate: 10.0}\n  - signal_type: histogram\n    name: b\n    distribution: {type: exponential, rate: 10.0}\n";
        let yaml = shared_encoder_file(paths, scenarios);
        let msg = prepare_error(compile_entries(&yaml, &Default::default()));
        assert!(msg.contains("`a_bucket` and `b_bucket`"), "{msg}");
        assert!(msg.contains("le={le}"), "{msg}");
    }

    /// Two metrics entries on one literal leaf collide only when they serve
    /// it from the same listen address.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rstest::rstest]
    #[case::same_address("127.0.0.1:57400", "127.0.0.1:57400", false)]
    #[case::different_ports("127.0.0.1:57400", "127.0.0.1:57401", true)]
    #[case::different_hosts("127.0.0.1:57400", "127.0.0.2:57400", true)]
    #[case::each_on_port_zero("127.0.0.1:0", "127.0.0.1:0", true)]
    fn gnmi_leaf_check_is_per_listen_address(
        #[case] first: &str,
        #[case] second: &str,
        #[case] accepted: bool,
    ) {
        let entry = |name: &str, listen: &str| {
            format!(
                "  - signal_type: metrics\n    name: {name}\n    generator: {{type: constant, value: 1.0}}\n    sink:\n      type: gnmi_target\n      listen: \"{listen}\"\n"
            )
        };
        let paths = "      up: \"/system/state\"\n      down: \"/system/state\"\n";
        let scenarios = format!("{}{}", entry("up", first), entry("down", second));
        let yaml = shared_encoder_file(paths, &scenarios);
        let entries = compile_entries(&yaml, &Default::default());
        let result = prepare_entries(entries);
        if accepted {
            result.expect("different targets never share a leaf");
        } else {
            let msg = result.expect_err("one target, one leaf").to_string();
            assert!(msg.contains("would both write the gnmi leaf"), "{msg}");
        }
    }

    /// Two metrics on one gnmi_target address, sharing a template without
    /// `{name}` unless a block gives its entry its own encoder;
    /// `in_octets` and `out_octets` take the given blocks.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn one_address(in_octets: &str, out_octets: &str) -> String {
        format!(
            r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  encoder:
    type: gnmi
    path: "/interfaces/interface[name={{ifName}}]/state"
  sink:
    type: gnmi_target
    listen: 127.0.0.1:57400
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {{type: constant, value: 1.0}}
{in_octets}  - signal_type: metrics
    name: out_octets
    generator: {{type: constant, value: 1.0}}
{out_octets}"#
        )
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_ETH0: &str = "    labels:\n      device: leaf-1\n      ifName: eth0\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_LITERAL_ETH0: &str = "    labels:\n      device: leaf-1\n    encoder:\n      type: gnmi\n      path: \"/interfaces/interface[name=eth0]/state\"\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_PORT_ETH0: &str = "    labels:\n      device: leaf-1\n      port: eth0\n    encoder:\n      type: gnmi\n      path: \"/interfaces/interface[name={port}]/state\"\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_ETH1: &str = "    labels:\n      device: leaf-1\n      ifName: eth1\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_DYNAMIC_ETH1_ETH2: &str = "    labels:\n      device: leaf-1\n    dynamic_labels:\n      - key: ifName\n        values: [eth1, eth2]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_DYNAMIC_ETH0_ETH1: &str = "    labels:\n      device: leaf-1\n    dynamic_labels:\n      - key: ifName\n        values: [eth0, eth1]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_3_ETH0: &str = "    labels:\n      device: leaf-3\n      ifName: eth0\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const DYNAMIC_LEAF_1_LEAF_2_ETH0: &str = "    labels:\n      ifName: eth0\n    dynamic_labels:\n      - key: device\n        values: [leaf-1, leaf-2]\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const NO_TARGET_ETH0: &str = "    labels:\n      ifName: eth0\n";
    /// A counter spike over `device` with the default prefix: `device_0`
    /// and `device_1` while it is open, `leaf-2` otherwise.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_2_DEFAULT_SPIKE_ETH0: &str = "    labels:\n      device: leaf-2\n      ifName: eth0\n    cardinality_spikes:\n      - label: device\n        every: 10s\n        for: 5s\n        cardinality: 2\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_2_ETH0: &str = "    labels:\n      device: leaf-2\n      ifName: eth0\n";
    /// A random spike of `cardinality` values over `device`: `pod-` and 16
    /// hex digits while it is open, `leaf-2` otherwise.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn leaf_2_random_spike_eth0(cardinality: u64) -> String {
        format!("    labels:\n      device: leaf-2\n      ifName: eth0\n    cardinality_spikes:\n      - label: device\n        every: 10s\n        for: 5s\n        cardinality: {cardinality}\n        strategy: random\n        prefix: pod-\n")
    }
    /// `device` set to `device`, `ifName` to `eth0`.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn device_eth0(device: &str) -> String {
        format!("    labels:\n      device: {device}\n      ifName: eth0\n")
    }
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const POD_SHAPED_ETH0: &str =
        "    labels:\n      device: pod-0123456789abcdef\n      ifName: eth0\n";
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const POD_3_ETH0: &str = "    labels:\n      device: pod-3\n      ifName: eth0\n";
    /// A random spike of `cardinality` values over `ifName`: `eth` and 16
    /// hex digits while it is open, `eth1` otherwise.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn leaf_1_random_ifname(cardinality: u64) -> String {
        format!("    labels:\n      device: leaf-1\n      ifName: eth1\n    cardinality_spikes:\n      - label: ifName\n        every: 10s\n        for: 5s\n        cardinality: {cardinality}\n        strategy: random\n        prefix: eth\n")
    }
    /// `device` set to `leaf-1`, `ifName` to `if_name`.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn leaf_1_ifname(if_name: &str) -> String {
        format!("    labels:\n      device: leaf-1\n      ifName: {if_name}\n")
    }
    #[cfg(all(feature = "gnmi", feature = "config"))]
    const LEAF_1_ETH_SHAPED: &str =
        "    labels:\n      device: leaf-1\n      ifName: eth0123456789abcdef\n";

    /// Leaves and targets meet when the values their labels can take meet.
    /// `None` means the file prepares; otherwise the error contains the
    /// given text.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::literal_and_placeholder_with_its_value(LEAF_1_ETH0, LEAF_1_LITERAL_ETH0, Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` (`out_octets` writes it as `openconfig:/interfaces/interface[name=eth0]/state`) on target `leaf-1`, each overwriting the other"))]
    #[case::two_placeholders_with_one_value(LEAF_1_ETH0, LEAF_1_PORT_ETH0, Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` (`out_octets` writes it as `openconfig:/interfaces/interface[name={port}]/state`) on target `leaf-1`, each overwriting the other"))]
    #[case::one_template_eth0_and_eth1(LEAF_1_ETH0, LEAF_1_ETH1, None)]
    #[case::static_eth0_and_dynamic_eth1_eth2(LEAF_1_ETH0, LEAF_1_DYNAMIC_ETH1_ETH2, None)]
    #[case::static_leaf_3_and_dynamic_leaf_1_leaf_2(LEAF_3_ETH0, DYNAMIC_LEAF_1_LEAF_2_ETH0, None)]
    #[case::static_eth0_and_dynamic_eth0_eth1(LEAF_1_ETH0, LEAF_1_DYNAMIC_ETH0_ETH1, Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on target `leaf-1`, each overwriting the other"))]
    #[case::static_leaf_1_and_default_spike_over_leaf_2(LEAF_1_ETH0, LEAF_2_DEFAULT_SPIKE_ETH0, None)]
    #[case::no_target_and_dynamic_leaf_1_leaf_2(NO_TARGET_ETH0, DYNAMIC_LEAF_1_LEAF_2_ETH0, None)]
    #[case::random_spike_target_and_its_first_value(&device_eth0(&random_spike_value("pod-", 0, 0)), &leaf_2_random_spike_eth0(2), Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on a target they may share: `out_octets` takes `device` from a dynamic or spike label, each overwriting the other"))]
    #[case::random_spike_target_and_an_unlisted_value_of_its_shape(POD_SHAPED_ETH0, &leaf_2_random_spike_eth0(2), None)]
    #[case::random_spike_target_past_the_cap(POD_SHAPED_ETH0, &leaf_2_random_spike_eth0(MAX_ENUMERATED_SPIKE + 1), Some("metrics `in_octets` and `out_octets` may collide on the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on a target they may share: `out_octets` takes `device` from a dynamic or spike label: the values of `device` of `out_octets` come from a random cardinality spike of more than 65536 values"))]
    #[case::random_spike_target_past_the_cap_and_a_value_of_another_shape(POD_3_ETH0, &leaf_2_random_spike_eth0(MAX_ENUMERATED_SPIKE + 1), None)]
    #[case::random_spike_placeholder_and_its_second_value(&leaf_1_ifname(&random_spike_value("eth", 0, 1)), &leaf_1_random_ifname(2), Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on target `leaf-1`, each overwriting the other"))]
    #[case::random_spike_placeholder_and_an_unlisted_value_of_its_shape(LEAF_1_ETH_SHAPED, &leaf_1_random_ifname(2), None)]
    #[case::random_spike_placeholder_past_the_cap(LEAF_1_ETH_SHAPED, &leaf_1_random_ifname(MAX_ENUMERATED_SPIKE + 1), Some("metrics `in_octets` and `out_octets` may collide on the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on target `leaf-1`: the values of `ifName` of `out_octets` come from a random cardinality spike of more than 65536 values"))]
    #[case::static_leaf_2_and_a_spike_over_leaf_2(LEAF_2_ETH0, LEAF_2_DEFAULT_SPIKE_ETH0, Some("metrics `in_octets` and `out_octets` would both write the gnmi leaf `openconfig:/interfaces/interface[name={ifName}]/state` on a target they may share: `out_octets` takes `device` from a dynamic or spike label, each overwriting the other"))]
    fn gnmi_leaves_and_targets_meet_by_label_values(
        #[case] in_octets: &str,
        #[case] out_octets: &str,
        #[case] rejected_with: Option<&str>,
    ) {

        let entries = compile_entries(&one_address(in_octets, out_octets), &Default::default());
        let names: Vec<&str> = entries.iter().map(|e| e.base().name.as_str()).collect();
        assert_eq!(names, ["in_octets", "out_octets"], "the file must compile to both metrics");
        match rejected_with {
            None => {
                prepare_entries(entries).expect("the two metrics never write one leaf on one target");
            }
            Some(text) => {
                let msg = prepare_entries(entries)
                    .expect_err("the two metrics can write one leaf on one target")
                    .to_string();
                assert!(msg.contains(text), "{msg}");
            }
        }
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn metric_yaml(name: &str) -> String {
        format!("  - signal_type: metrics\n    name: {name}\n    generator: {{type: constant, value: 1.0}}\n")
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn histogram_yaml(name: &str, buckets: &str) -> String {
        format!("  - signal_type: histogram\n    name: {name}\n    buckets: {buckets}\n    distribution: {{type: exponential, rate: 10.0}}\n")
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn summary_yaml(name: &str, quantiles: &str) -> String {
        format!("  - signal_type: summary\n    name: {name}\n    quantiles: {quantiles}\n    distribution: {{type: exponential, rate: 10.0}}\n")
    }

    /// `paths:` entries giving `series` the template `leaf` and each of
    /// `derived`'s `_sum` and `_count` series a leaf of its own.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    fn leaf_paths(series: &[(&str, &str)], derived: &[&str]) -> String {
        let mut paths: String = series
            .iter()
            .map(|(name, leaf)| format!("      {name}: \"{leaf}\"\n"))
            .collect();
        for name in derived {
            paths.push_str(&format!("      {name}_sum: \"/{name}/{{name}}\"\n      {name}_count: \"/{name}/{{name}}\"\n"));
        }
        paths
    }

    /// A histogram's `le` and a summary's `quantile` take the values their
    /// runners give them: each meets a literal or another label holding one
    /// of those values, `le` and `quantile` meet where a bound and a
    /// quantile format alike, and two histograms always share `+Inf`.
    /// `None` means the file prepares; otherwise the error contains the
    /// given text.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::literal_le_and_a_bucket_of_that_bound(
        leaf_paths(&[("m", "/latency[le=0.5]/bucket"), ("h_bucket", "/latency[le={le}]/bucket")], &["h"]),
        metric_yaml("m") + &histogram_yaml("h", "[0.5, 2.0]"),
        Some("metrics `m` and `h_bucket` would both write the gnmi leaf `openconfig:/latency[le=0.5]/bucket` (`h_bucket` writes it as `openconfig:/latency[le={le}]/bucket`) with no target"),
    )]
    #[case::literal_le_and_no_bucket_of_that_bound(
        leaf_paths(&[("m", "/latency[le=0.5]/bucket"), ("h_bucket", "/latency[le={le}]/bucket")], &["h"]),
        metric_yaml("m") + &histogram_yaml("h", "[0.25, 1]"),
        None,
    )]
    #[case::literal_quantile_and_a_quantile_of_that_value(
        leaf_paths(&[("m", "/rpc[quantile=0.5]/q"), ("s", "/rpc[quantile={quantile}]/q")], &["s"]),
        metric_yaml("m") + &summary_yaml("s", "[0.5, 0.9]"),
        Some("metrics `m` and `s` would both write the gnmi leaf `openconfig:/rpc[quantile=0.5]/q` (`s` writes it as `openconfig:/rpc[quantile={quantile}]/q`) with no target"),
    )]
    #[case::literal_quantile_and_no_quantile_of_that_value(
        leaf_paths(&[("m", "/rpc[quantile=0.5]/q"), ("s", "/rpc[quantile={quantile}]/q")], &["s"]),
        metric_yaml("m") + &summary_yaml("s", "[0.25, 0.75]"),
        None,
    )]
    #[case::le_and_quantile_formatting_alike(
        leaf_paths(&[("h_bucket", "/stats[v={le}]/x"), ("s", "/stats[v={quantile}]/x")], &["h", "s"]),
        histogram_yaml("h", "[0.5, 2.0]") + &summary_yaml("s", "[0.5, 0.9]"),
        Some("metrics `h_bucket` and `s` would both write the gnmi leaf `openconfig:/stats[v={le}]/x` (`s` writes it as `openconfig:/stats[v={quantile}]/x`) with no target"),
    )]
    #[case::le_and_quantile_formatting_apart(
        leaf_paths(&[("h_bucket", "/stats[v={le}]/x"), ("s", "/stats[v={quantile}]/x")], &["h", "s"]),
        histogram_yaml("h", "[0.25, 2.0]") + &summary_yaml("s", "[0.5, 0.9]"),
        None,
    )]
    #[case::two_histograms_with_no_finite_bound_in_common(
        leaf_paths(&[("a_bucket", "/latency[le={le}]/bucket"), ("b_bucket", "/latency[le={le}]/bucket")], &["a", "b"]),
        histogram_yaml("a", "[0.25]") + &histogram_yaml("b", "[0.5]"),
        Some("metrics `a_bucket` and `b_bucket` would both write the gnmi leaf `openconfig:/latency[le={le}]/bucket` with no target"),
    )]
    fn gnmi_le_and_quantile_meet_by_their_values(
        #[case] paths: String,
        #[case] scenarios: String,
        #[case] rejected_with: Option<&str>,
    ) {
        let entries = compile_entries(&shared_encoder_file(&paths, &scenarios), &Default::default());
        let names: Vec<&str> = entries.iter().map(|e| e.base().name.as_str()).collect();
        assert_eq!(names.len(), 2, "the file must compile to both entries: {names:?}");
        match rejected_with {
            None => {
                prepare_entries(entries).expect("the two entries never write one leaf");
            }
            Some(text) => {
                let msg = prepare_entries(entries)
                    .expect_err("the two entries can write one leaf")
                    .to_string();
                assert!(msg.contains(text), "{msg}");
            }
        }
    }
}
