//! Config validation helpers: duration parsing and semantic checks.

use std::time::{Duration, SystemTime};

use crate::model::metric::{is_valid_label_key, is_valid_metric_name};
use crate::sink::retry::{RetryConfig, RetryPolicy};
use crate::sink::SinkConfig;
use crate::{ConfigError, SondaError};

use super::{
    BurstConfig, CardinalitySpikeConfig, DistributionConfig, DynamicLabelConfig,
    DynamicLabelStrategy, HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig,
    SummaryScenarioConfig,
};

/// Parse a human-readable duration string into a [`Duration`].
///
/// Supported units (fractional values accepted, e.g. `"1.5s"`, `"0.5m"`):
/// - `ms` — milliseconds (e.g. `"100ms"`)
/// - `s`  — seconds      (e.g. `"30s"`, `"1.5s"`)
/// - `m`  — minutes      (e.g. `"5m"`, `"0.5m"`)
/// - `h`  — hours        (e.g. `"1h"`)
///
/// Returns [`SondaError::Config`] if the string is empty, has no recognized
/// unit suffix, has a non-numeric prefix, or has a zero or negative value.
pub fn parse_duration(s: &str) -> Result<Duration, SondaError> {
    if s.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "duration must not be empty",
        )));
    }

    // Determine unit suffix and numeric portion.
    let (numeric_str, multiplier_ms): (&str, f64) = if let Some(stripped) = s.strip_suffix("ms") {
        (stripped, 1.0)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3_600_000.0)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60_000.0)
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1_000.0)
    } else {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "unrecognized duration unit in {:?}: expected one of ms, s, m, h",
            s
        ))));
    };

    if numeric_str.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "duration {:?} has no numeric value before the unit",
            s
        ))));
    }

    // Reject leading minus sign explicitly for a clear error message.
    if numeric_str.starts_with('-') {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "duration {:?} must be positive",
            s
        ))));
    }

    let value: f64 = numeric_str.parse().map_err(|_| {
        SondaError::Config(ConfigError::invalid(format!(
            "duration {:?} has an invalid numeric part {:?}",
            s, numeric_str
        )))
    })?;

    if value.is_nan() || value.is_infinite() || value <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "duration {:?} must be greater than zero",
            s
        ))));
    }

    let total_ms = value * multiplier_ms;
    // Guard against overflow: f64 can represent values well beyond u64 range.
    if total_ms > u64::MAX as f64 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "duration {:?} overflows millisecond representation",
            s
        ))));
    }

    Ok(Duration::from_micros((total_ms * 1_000.0) as u64))
}

/// Parse a `delay:` open/close duration, accepting zero as `Duration::ZERO`.
///
/// Retained name for the `delay:` clause; [`parse_offset_duration`] is the
/// same parser under the name that describes the whole class of fields it
/// serves.
pub fn parse_delay_duration(s: &str) -> Result<Duration, SondaError> {
    parse_offset_duration(s)
}

/// Parse a duration for which zero is a meaningful value, rather than an error.
///
/// [`parse_duration`] rejects zero, and must: a zero rate or a zero scenario
/// duration describes nothing that can run. Offsets are the opposite case —
/// they measure *from* somewhere, so zero means "at that point", not "absent":
///
/// - `delay:` — `0s` is no debounce on the transition.
/// - `gap_windows[].at` — `0s` is a scenario that begins inside the silence,
///   which is what a capture taken during an outage looks like.
///
/// Accepts `0`, `0s`, `0ms`, `0m`, `0h`; rejects negatives and garbage exactly
/// as `parse_duration` does.
pub fn parse_offset_duration(s: &str) -> Result<Duration, SondaError> {
    match parse_duration(s) {
        Ok(d) => Ok(d),
        Err(e) => {
            let trimmed = s.trim();
            if trimmed.starts_with('-') {
                return Err(e);
            }
            let numeric_str = trimmed
                .strip_suffix("ms")
                .or_else(|| trimmed.strip_suffix('h'))
                .or_else(|| trimmed.strip_suffix('m'))
                .or_else(|| trimmed.strip_suffix('s'))
                .unwrap_or(trimmed);
            if let Ok(v) = numeric_str.parse::<f64>() {
                if v == 0.0 {
                    return Ok(Duration::ZERO);
                }
            }
            Err(e)
        }
    }
}

/// Parse an optional duration string into a [`Duration`].
///
/// Unlike [`parse_duration`], this function accepts zero values (e.g. `"0s"`)
/// and returns `None` for them, since a zero duration is semantically
/// equivalent to "disabled" / "no delay".
pub fn parse_optional_duration(s: &str) -> Result<Option<Duration>, SondaError> {
    match parse_duration(s) {
        Ok(d) => Ok(Some(d)),
        Err(e) => {
            // Check if it was rejected because the value is zero.
            // Strip any recognized suffix, then parse the numeric part as f64.
            let trimmed = s.trim();
            let numeric_str = trimmed
                .strip_suffix("ms")
                .or_else(|| trimmed.strip_suffix('h'))
                .or_else(|| trimmed.strip_suffix('m'))
                .or_else(|| trimmed.strip_suffix('s'))
                .unwrap_or("");
            if let Ok(v) = numeric_str.parse::<f64>() {
                if v == 0.0 {
                    return Ok(None); // "0s", "0ms", "0m", "0h", "0.0s" all mean disabled
                }
            }
            Err(e)
        }
    }
}

/// Parse an optional phase offset string into a [`Duration`].
///
/// Unlike [`parse_duration`], this function accepts zero values (e.g. `"0s"`)
/// and returns `None` for them, since a zero offset is semantically equivalent
/// to no offset.
pub fn parse_phase_offset(s: &str) -> Result<Option<Duration>, SondaError> {
    parse_optional_duration(s).map_err(|_| {
        SondaError::Config(ConfigError::invalid(format!(
            "invalid phase_offset {:?}: {}",
            s,
            parse_duration(s).unwrap_err()
        )))
    })
}

/// Validate a single [`CardinalitySpikeConfig`] for semantic correctness.
///
/// Checks:
/// - `label` is a valid Prometheus label key.
/// - `every` and `for` are parseable duration strings.
/// - `for` is strictly less than `every`.
/// - `cardinality` is greater than zero.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_cardinality_spike_config(spike: &CardinalitySpikeConfig) -> Result<(), SondaError> {
    if !is_valid_label_key(&spike.label) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "invalid cardinality_spikes label {:?}: must match [a-zA-Z_][a-zA-Z0-9_]*",
            spike.label
        ))));
    }

    let every = parse_duration(&spike.every)
        .map_err(|e| prepend_context("invalid cardinality_spikes.every", &spike.every, e))?;
    let for_dur = parse_duration(&spike.r#for)
        .map_err(|e| prepend_context("invalid cardinality_spikes.for", &spike.r#for, e))?;

    if for_dur >= every {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "cardinality_spikes.for ({:?}) must be less than cardinality_spikes.every ({:?})",
            spike.r#for, spike.every
        ))));
    }

    if spike.cardinality == 0 {
        return Err(SondaError::Config(ConfigError::invalid(
            "cardinality_spikes.cardinality must be greater than zero",
        )));
    }

    Ok(())
}

/// Validate a [`DynamicLabelConfig`] for semantic correctness.
///
/// Checks:
/// - `key` is a valid Prometheus label key.
/// - For counter strategy: `cardinality` is greater than zero.
/// - For values-list strategy: the values list is non-empty.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_dynamic_label_config(dl: &DynamicLabelConfig) -> Result<(), SondaError> {
    if !is_valid_label_key(&dl.key) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "invalid dynamic_labels key {:?}: must match [a-zA-Z_][a-zA-Z0-9_]*",
            dl.key
        ))));
    }

    match &dl.strategy {
        DynamicLabelStrategy::Counter { cardinality, .. } => {
            if *cardinality == 0 {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "dynamic_labels[key={:?}].cardinality must be greater than zero",
                    dl.key
                ))));
            }
        }
        DynamicLabelStrategy::ValuesList { values } => {
            if values.is_empty() {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "dynamic_labels[key={:?}].values must not be empty",
                    dl.key
                ))));
            }
        }
    }

    Ok(())
}

/// Validate a [`ScenarioConfig`] for semantic correctness.
///
/// Checks:
/// - `rate` is strictly positive.
/// - `duration`, if provided, is a parseable duration string.
/// - If gaps are configured, `gap.for` is strictly less than `gap.every`.
/// - The metric name is a valid Prometheus metric name
///   (matches `[a-zA-Z_:][a-zA-Z0-9_:]*`).
///
/// Returns [`SondaError::Config`] with a descriptive message naming the field
/// and the invalid value.
pub fn validate_config(config: &ScenarioConfig) -> Result<(), SondaError> {
    // Rate must be strictly positive. Explicit NaN check ensures NaN is also rejected.
    if config.rate.is_nan() || config.rate <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "rate must be positive, got {}",
            config.rate
        ))));
    }

    // Duration must be parseable if provided.
    if let Some(ref dur_str) = config.duration {
        parse_duration(dur_str).map_err(|e| prepend_context("invalid duration", dur_str, e))?;
    }

    // start_time must be parseable if provided.
    if let Some(ref start_str) = config.start_time {
        parse_start_time(start_str)?;
    }

    // Gap consistency: gap_for < gap_every.
    if let Some(ref gap) = config.gaps {
        let every = parse_duration(&gap.every)
            .map_err(|e| prepend_context("invalid gaps.every", &gap.every, e))?;
        let for_dur = parse_duration(&gap.r#for)
            .map_err(|e| prepend_context("invalid gaps.for", &gap.r#for, e))?;
        if for_dur >= every {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gaps.for ({:?}) must be less than gaps.every ({:?})",
                gap.r#for, gap.every
            ))));
        }
    }

    // Burst consistency: multiplier > 0, burst.for < burst.every.
    if let Some(ref burst) = config.bursts {
        validate_burst_config(burst)?;
    }

    // Cardinality spike consistency: valid label key, parseable durations, for < every, cardinality > 0.
    if let Some(ref spikes) = config.cardinality_spikes {
        for spike in spikes {
            validate_cardinality_spike_config(spike)?;
        }
    }

    // Dynamic label consistency: valid key, non-zero cardinality / non-empty values.
    if let Some(ref dls) = config.dynamic_labels {
        for dl in dls {
            validate_dynamic_label_config(dl)?;
        }
    }

    // Metric name must be a valid Prometheus metric name.
    if !is_valid_metric_name(&config.name) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
            config.name
        ))));
    }

    // Jitter amplitude must be finite and non-negative.
    validate_jitter(config.base.jitter)?;

    // Encoder precision must not exceed 17 (f64 has ~15-17 significant digits).
    validate_encoder_precision(&config.encoder)?;

    #[cfg(feature = "gnmi")]
    validate_gnmi_encoder(config)?;

    #[cfg(feature = "snmp")]
    validate_snmp_encoder(config)?;

    validate_sink_config(&config.base.sink)?;

    validate_encoder_sink_pairing(&config.encoder, &config.base.sink)?;

    Ok(())
}

/// Require the `gnmi` encoder and the `gnmi_target` sink to be used together,
/// and the `snmp` encoder and the `snmp_agent` sink.
///
/// The gnmi encoder's output is one unframed notification per event, which
/// only the `gnmi_target` sink can carry: written back to back on a
/// byte-stream sink, two notifications decode as one. The snmp encoder's
/// output is a publish record only the `snmp_agent` sink reads. Each sink,
/// in turn, needs the encoder that produces its input. Either one of a pair
/// without the other is a [`SondaError::Config`] naming both.
///
/// Returns `Ok(())` for any other pairing.
pub fn validate_encoder_sink_pairing(
    encoder: &crate::encoder::EncoderConfig,
    sink: &SinkConfig,
) -> Result<(), SondaError> {
    #[cfg(feature = "gnmi")]
    {
        let gnmi_encoder = matches!(encoder, crate::encoder::EncoderConfig::Gnmi(_));
        let gnmi_sink = matches!(sink, SinkConfig::GnmiTarget(_));
        if gnmi_encoder && !gnmi_sink {
            return Err(SondaError::Config(ConfigError::invalid(
                "the gnmi encoder can only be used with the gnmi_target sink",
            )));
        }
        if gnmi_sink && !gnmi_encoder {
            return Err(SondaError::Config(ConfigError::invalid(
                "the gnmi_target sink can only be used with the gnmi encoder",
            )));
        }
    }
    #[cfg(feature = "snmp")]
    {
        let snmp_encoder = matches!(encoder, crate::encoder::EncoderConfig::Snmp(_));
        let snmp_sink = matches!(sink, SinkConfig::SnmpAgent(_));
        if snmp_encoder && !snmp_sink {
            return Err(SondaError::Config(ConfigError::invalid(
                "the snmp encoder can only be used with the snmp_agent sink",
            )));
        }
        if snmp_sink && !snmp_encoder {
            return Err(SondaError::Config(ConfigError::invalid(
                "the snmp_agent sink can only be used with the snmp encoder",
            )));
        }
    }
    let _ = (encoder, sink);
    Ok(())
}

/// Validate a [`BurstConfig`] for semantic correctness.
///
/// Checks:
/// - `multiplier` is strictly positive (not NaN, not zero, not negative).
/// - `burst.for` is strictly less than `burst.every`.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_burst_config(burst: &BurstConfig) -> Result<(), SondaError> {
    // Multiplier must be strictly positive.
    if burst.multiplier.is_nan() || burst.multiplier <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "bursts.multiplier must be positive, got {}",
            burst.multiplier
        ))));
    }

    // Parse both duration strings.
    let every = parse_duration(&burst.every)
        .map_err(|e| prepend_context("invalid bursts.every", &burst.every, e))?;
    let for_dur = parse_duration(&burst.r#for)
        .map_err(|e| prepend_context("invalid bursts.for", &burst.r#for, e))?;

    // burst.for must be strictly less than burst.every.
    if for_dur >= every {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "bursts.for ({:?}) must be less than bursts.every ({:?})",
            burst.r#for, burst.every
        ))));
    }

    Ok(())
}

/// Validate a [`LogScenarioConfig`] for semantic correctness.
///
/// Checks:
/// - `rate` is strictly positive and not NaN.
/// - `duration`, if provided, is a parseable duration string.
/// - If gaps are configured, `gap.for` is strictly less than `gap.every`.
/// - If bursts are configured, `burst.for` is strictly less than `burst.every`
///   and `burst.multiplier` is strictly positive.
///
/// Returns [`SondaError::Config`] with a descriptive message naming the field
/// and the invalid value.
pub fn validate_log_config(config: &LogScenarioConfig) -> Result<(), SondaError> {
    reject_gnmi_for_logs(&config.encoder, &config.base.sink)?;
    reject_snmp_for("logs", &config.encoder, &config.base.sink)?;
    if config.rate.is_nan() || config.rate <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "rate must be positive, got {}",
            config.rate
        ))));
    }

    if let Some(ref dur_str) = config.duration {
        parse_duration(dur_str).map_err(|e| prepend_context("invalid duration", dur_str, e))?;
    }

    if let Some(ref gap) = config.gaps {
        let every = parse_duration(&gap.every)
            .map_err(|e| prepend_context("invalid gaps.every", &gap.every, e))?;
        let for_dur = parse_duration(&gap.r#for)
            .map_err(|e| prepend_context("invalid gaps.for", &gap.r#for, e))?;
        if for_dur >= every {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gaps.for ({:?}) must be less than gaps.every ({:?})",
                gap.r#for, gap.every
            ))));
        }
    }

    if let Some(ref burst) = config.bursts {
        validate_burst_config(burst)?;
    }

    // Cardinality spike consistency: valid label key, parseable durations, for < every, cardinality > 0.
    if let Some(ref spikes) = config.cardinality_spikes {
        for spike in spikes {
            validate_cardinality_spike_config(spike)?;
        }
    }

    // Dynamic label consistency: valid key, non-zero cardinality / non-empty values.
    if let Some(ref dls) = config.dynamic_labels {
        for dl in dls {
            validate_dynamic_label_config(dl)?;
        }
    }

    // Jitter amplitude must be finite and non-negative.
    validate_jitter(config.base.jitter)?;

    // Encoder precision must not exceed 17 (f64 has ~15-17 significant digits).
    validate_encoder_precision(&config.encoder)?;

    validate_sink_config(&config.base.sink)?;

    Ok(())
}

/// Extract the precision value from an [`EncoderConfig`], if present.
fn encoder_precision(encoder: &crate::encoder::EncoderConfig) -> Option<u8> {
    match encoder {
        crate::encoder::EncoderConfig::PrometheusText { precision } => *precision,
        crate::encoder::EncoderConfig::InfluxLineProtocol { precision, .. } => *precision,
        crate::encoder::EncoderConfig::JsonLines { precision } => *precision,
        crate::encoder::EncoderConfig::Syslog { .. } => None,
        #[cfg(feature = "remote-write")]
        crate::encoder::EncoderConfig::RemoteWrite => None,
        #[cfg(feature = "otlp")]
        crate::encoder::EncoderConfig::Otlp => None,
        #[cfg(not(feature = "remote-write"))]
        crate::encoder::EncoderConfig::RemoteWriteDisabled { .. } => None,
        #[cfg(not(feature = "otlp"))]
        crate::encoder::EncoderConfig::OtlpDisabled { .. } => None,
        #[cfg(feature = "gnmi")]
        crate::encoder::EncoderConfig::Gnmi(_) => None,
        #[cfg(not(feature = "gnmi"))]
        crate::encoder::EncoderConfig::GnmiDisabled { .. } => None,
        #[cfg(feature = "snmp")]
        crate::encoder::EncoderConfig::Snmp(_) => None,
        #[cfg(not(feature = "snmp"))]
        crate::encoder::EncoderConfig::SnmpDisabled { .. } => None,
    }
}

/// Reject the `gnmi` encoder and the `gnmi_target` sink on a logs entry.
///
/// Both carry metric signals only: the encoder does not encode log events,
/// and the sink takes events only through the metric hook.
pub(crate) fn reject_gnmi_for_logs(
    encoder: &crate::encoder::EncoderConfig,
    sink: &SinkConfig,
) -> Result<(), SondaError> {
    #[cfg(feature = "gnmi")]
    {
        if matches!(encoder, crate::encoder::EncoderConfig::Gnmi(_)) {
            return Err(SondaError::Config(ConfigError::invalid(
                "the gnmi encoder carries metric signals only (metrics, histogram and \
                 summary entries), not logs entries",
            )));
        }
        if matches!(sink, SinkConfig::GnmiTarget(_)) {
            return Err(SondaError::Config(ConfigError::invalid(
                "the gnmi_target sink carries metric signals only (metrics, histogram and \
                 summary entries), not logs entries",
            )));
        }
    }
    #[cfg(not(feature = "gnmi"))]
    let _ = (encoder, sink);
    Ok(())
}

/// Reject the `snmp` encoder and the `snmp_agent` sink on an entry of
/// `kind` (`"logs"`, `"histogram"` or `"summary"`).
///
/// Both carry metrics entries only: a log, a histogram's buckets and a
/// summary's quantiles have no MIB shape.
pub(crate) fn reject_snmp_for(
    kind: &str,
    encoder: &crate::encoder::EncoderConfig,
    sink: &SinkConfig,
) -> Result<(), SondaError> {
    #[cfg(feature = "snmp")]
    {
        if matches!(encoder, crate::encoder::EncoderConfig::Snmp(_)) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "the snmp encoder carries metrics entries only, not {kind} entries: a \
                 {kind} entry has no MIB shape"
            ))));
        }
        if matches!(sink, SinkConfig::SnmpAgent(_)) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "the snmp_agent sink carries metrics entries only, not {kind} entries: a \
                 {kind} entry has no MIB shape"
            ))));
        }
    }
    #[cfg(not(feature = "snmp"))]
    let _ = (kind, encoder, sink);
    Ok(())
}

/// The system group's OID, which the `snmp_agent` sink serves itself.
#[cfg(feature = "snmp")]
const SNMP_SYSTEM_GROUP: [u32; 7] = [1, 3, 6, 1, 2, 1, 1];

/// The arcs one position of an OID template can render for an entry: some
/// listed arcs and the arcs of some counters.
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ArcSet {
    /// These arcs.
    listed: std::collections::BTreeSet<u32>,
    /// The arcs of each counter: `prefix` followed by each of `0` to
    /// `cardinality - 1` in decimal, those that are arcs.
    counters: Vec<(String, u64)>,
}

#[cfg(feature = "snmp")]
impl ArcSet {
    /// One literal arc.
    fn literal(arc: u32) -> Self {
        Self {
            listed: [arc].into(),
            counters: Vec::new(),
        }
    }

    /// The arcs a label renders as, from the values the entry gives it: its
    /// static value or each `values:` entry that is an
    /// [`index_arc`](crate::encoder::snmp::oid::index_arc); the values of a
    /// `counter` dynamic label or `counter` spike, a spike's together with
    /// the values outside its window; for a `random` spike, the values
    /// outside its window; none for a label the entry does not carry.
    fn of_label(set: Option<super::LabelValueSet>) -> Self {
        use super::LabelValueSet;
        use crate::encoder::snmp::oid::index_arc;
        let mut arcs = Self::default();
        let mut next = set;
        while let Some(set) = next {
            next = match set {
                LabelValueSet::Static(value) => {
                    arcs.listed.extend(index_arc(&value));
                    None
                }
                LabelValueSet::Values(values) => {
                    arcs.listed
                        .extend(values.iter().filter_map(|v| index_arc(v)));
                    None
                }
                LabelValueSet::Counter {
                    prefix,
                    cardinality,
                } => {
                    arcs.counters.push((prefix, cardinality));
                    None
                }
                LabelValueSet::SpikeCounter {
                    prefix,
                    cardinality,
                    outside,
                } => {
                    arcs.counters.push((prefix, cardinality));
                    outside.map(|set| *set)
                }
                LabelValueSet::RandomShape { outside, .. } => outside.map(|set| *set),
            };
        }
        arcs
    }

    /// Whether `arc` is in this set. O(listed + counters × digits).
    fn contains(&self, arc: u32) -> bool {
        self.listed.contains(&arc)
            || self
                .counters
                .iter()
                .any(|(prefix, cardinality)| counter_holds(prefix, *cardinality, arc))
    }

    /// Whether the set holds no arc: nothing listed, and no counter with a
    /// value. A counter's values are arcs once rule 4 has passed.
    fn is_empty(&self) -> bool {
        self.listed.is_empty()
            && self
                .counters
                .iter()
                .all(|(_, cardinality)| *cardinality == 0)
    }

    /// The arcs of this set that `other` also holds.
    ///
    /// Exact for listed arcs against anything (an arc is kept when `other`
    /// [contains](Self::contains) it) and for a counter against a listed
    /// set (the counter becomes the listed arcs of `other` it holds). A
    /// counter that meets a counter of `other` is kept whole, unchanged:
    /// the one conservative case, which can only keep an arc the exact
    /// intersection would not, so rule 6 may refuse two such templates but
    /// never misses a collision. O((listed + counters) × (listed + counters)
    /// × digits).
    fn narrow(&self, other: &ArcSet) -> ArcSet {
        let mut listed: std::collections::BTreeSet<u32> = self
            .listed
            .iter()
            .copied()
            .filter(|arc| other.contains(*arc))
            .collect();
        let mut counters = Vec::new();
        for (prefix, cardinality) in &self.counters {
            let meets_a_counter = other
                .counters
                .iter()
                .any(|(q, d)| counters_meet((prefix, *cardinality), (q, *d)).is_some());
            if meets_a_counter {
                counters.push((prefix.clone(), *cardinality));
            } else {
                listed.extend(
                    other
                        .listed
                        .iter()
                        .copied()
                        .filter(|arc| counter_holds(prefix, *cardinality, *arc)),
                );
            }
        }
        ArcSet { listed, counters }
    }

    /// The smallest arc in both sets, if they share one. O(listed ×
    /// counters × digits + counters²).
    fn common(&self, other: &Self) -> Option<u32> {
        let listed = |mine: &Self, theirs: &Self| {
            mine.listed
                .iter()
                .copied()
                .find(|arc| theirs.contains(*arc))
        };
        let counters = self.counters.iter().flat_map(|(p, c)| {
            other
                .counters
                .iter()
                .filter_map(move |(q, d)| counters_meet((p, *c), (q, *d)))
        });
        [listed(self, other), listed(other, self)]
            .into_iter()
            .flatten()
            .chain(counters)
            .min()
    }
}

/// Whether the counter `prefix` + `0..cardinality` holds the value that
/// renders as `arc`: `arc` in decimal is `prefix` followed by a number
/// below `cardinality`, written without leading zeros. O(digits).
#[cfg(feature = "snmp")]
fn counter_holds(prefix: &str, cardinality: u64, arc: u32) -> bool {
    let text = arc.to_string();
    text.strip_prefix(prefix)
        .and_then(crate::encoder::snmp::oid::index_arc)
        .is_some_and(|n| u64::from(n) < cardinality)
}

/// The smallest arc both counters render, if any. A value of both is the
/// longer prefix followed by a number `j`, and the longer prefix is the
/// shorter followed by digits `q`, so the shorter counter's number is `q`
/// followed by `j`, which is smallest at `j = 0`. O(digits).
#[cfg(feature = "snmp")]
fn counters_meet(a: (&String, u64), b: (&String, u64)) -> Option<u32> {
    use crate::encoder::snmp::oid::index_arc;
    let ((short, short_cardinality), (long, long_cardinality)) = if a.0.len() <= b.0.len() {
        (a, b)
    } else {
        (b, a)
    };
    if short_cardinality == 0 || long_cardinality == 0 {
        return None;
    }
    let q = long.strip_prefix(short.as_str())?;
    if !q.is_empty() {
        // The shorter counter's number q0: q without a leading zero.
        if q.starts_with('0') || !q.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number: u128 = format!("{q}0").parse().ok()?;
        if number >= u128::from(short_cardinality) {
            return None;
        }
    }
    index_arc(&format!("{long}0"))
}

/// `label`'s placeholder as `template` writes it: `{label:str}` for a string
/// index, else `{label}`.
#[cfg(feature = "snmp")]
fn placeholder_text(template: &crate::encoder::snmp::oid::OidTemplate, label: &str) -> String {
    use crate::encoder::snmp::oid::TemplateArc;
    let string = template
        .arcs()
        .any(|arc| arc == TemplateArc::StringLabel(label));
    if string {
        format!("{{{label}:str}}")
    } else {
        format!("{{{label}}}")
    }
}

/// A collision of two templates under rule 6 of [`validate_snmp_encoder`].
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum SnmpCollision {
    /// Both render this OID, when the labels take these values: each label
    /// once, with the arc it renders as in the first template it appears
    /// in and, when it renders as another in the second template, that arc
    /// too. `exact` is false when the OID rests on a counter
    /// [`ArcSet::narrow`] kept whole: the templates may render it.
    Oid {
        oid: String,
        values: Vec<(String, u32, Option<u32>)>,
        exact: bool,
    },
    /// A template has a string index and one object is equal to or a prefix
    /// of the other.
    Nested,
}

/// Rule 6 of [`validate_snmp_encoder`] for one pair of templates: an OID
/// both can render for the entry `base`, with the label values that render
/// it, or `None`.
///
/// When either template has a string index, the two collide exactly when one
/// object is equal to or a prefix of the other ([`SnmpCollision::Nested`]).
/// Templates of different lengths never collide. Otherwise a label takes one
/// value at every position of its template, and the same label in the other
/// template is a value of its own, because the two render for different
/// series. Each label of each template starts with the arcs it renders as
/// for this entry ([`ArcSet::of_label`]); a pass visits every position
/// and narrows the set of each label there to the arcs the other side's set
/// at that position also holds ([`ArcSet::narrow`]; a literal is its one
/// arc), and passes repeat until no set changes, each pass but the last
/// removing an arc or turning a counter into listed arcs. The templates
/// collide when, after that, no label's set is empty and the two sides of
/// every position share an arc. The OID returned takes the smallest shared
/// arc at each position, which for listed arcs is each label's smallest
/// narrowed arc at every position it occupies, and names each label once.
/// Exact but for two counters that meet ([`ArcSet::narrow`]): a counter
/// kept whole decides exactly only at a position where it meets the other
/// side's counter and each of the two labels occupies no other position;
/// anywhere else the collision is not `exact`. O((labels + counters + arcs
/// listed) × arcs × values) for the passes.
#[cfg(feature = "snmp")]
fn snmp_collision(
    own: &crate::encoder::snmp::oid::OidTemplate,
    other: &crate::encoder::snmp::oid::OidTemplate,
    base: &super::BaseScheduleConfig,
) -> Option<SnmpCollision> {
    use crate::encoder::snmp::oid::TemplateArc;
    if own.has_string_index() || other.has_string_index() {
        let (a, b) = (own.object(), other.object());
        return (a.is_prefix_of(b) || b.is_prefix_of(a)).then_some(SnmpCollision::Nested);
    }
    let sides: [Vec<TemplateArc<'_>>; 2] = [own.arcs().collect(), other.arcs().collect()];
    if sides[0].len() != sides[1].len() {
        return None;
    }
    // One set per label of each side; each position holds, per side, its
    // arc and the index of the set of the label there.
    let mut keys: Vec<(usize, &str)> = Vec::new();
    let mut sets: Vec<ArcSet> = Vec::new();
    let mut positions: Vec<[(TemplateArc<'_>, Option<usize>); 2]> = Vec::new();
    for (&a, &b) in sides[0].iter().zip(&sides[1]) {
        let mut position = [(a, None), (b, None)];
        for (side, (arc, slot)) in position.iter_mut().enumerate() {
            let TemplateArc::Label(label) = *arc else {
                continue;
            };
            *slot = Some(match keys.iter().position(|key| *key == (side, label)) {
                Some(i) => i,
                None => {
                    keys.push((side, label));
                    sets.push(ArcSet::of_label(base.label_value_set(label)));
                    keys.len() - 1
                }
            });
        }
        positions.push(position);
    }
    let set_of = |sets: &[ArcSet], (arc, slot): (TemplateArc<'_>, Option<usize>)| match (slot, arc)
    {
        (Some(i), _) => sets[i].clone(),
        (None, TemplateArc::Literal(arc)) => ArcSet::literal(arc),
        // Neither template has a string index: answered above.
        (None, _) => ArcSet::default(),
    };
    loop {
        let mut changed = false;
        for position in &positions {
            for side in [0, 1] {
                let Some(i) = position[side].1 else {
                    continue;
                };
                let narrowed = sets[i].narrow(&set_of(&sets, position[1 - side]));
                if narrowed != sets[i] {
                    sets[i] = narrowed;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    if sets.iter().any(ArcSet::is_empty) {
        return None;
    }
    let occupied = |i: usize| {
        positions
            .iter()
            .flatten()
            .filter(|(_, slot)| *slot == Some(i))
            .count()
    };
    let exact = positions.iter().all(|position| {
        let kept_whole = position
            .iter()
            .any(|(_, slot)| slot.is_some_and(|i| !sets[i].counters.is_empty()));
        !kept_whole
            || position
                .iter()
                .all(|(_, slot)| slot.is_some_and(|i| occupied(i) == 1))
    });
    let mut oid: Vec<String> = Vec::new();
    // Each label of each side with its arc at its first position, in the
    // order the positions name them.
    let mut arcs: Vec<((usize, &str), u32)> = Vec::new();
    for position in &positions {
        let arc = set_of(&sets, position[0]).common(&set_of(&sets, position[1]))?;
        oid.push(arc.to_string());
        for (side, (template_arc, _)) in position.iter().enumerate() {
            if let TemplateArc::Label(label) = *template_arc {
                if !arcs.iter().any(|(key, _)| *key == (side, label)) {
                    arcs.push(((side, label), arc));
                }
            }
        }
    }
    let first_arc = |side: usize, label: &str| {
        arcs.iter()
            .find(|(key, _)| *key == (side, label))
            .map(|(_, arc)| *arc)
    };
    let mut values: Vec<(String, u32, Option<u32>)> = Vec::new();
    for ((_, label), _) in &arcs {
        if values.iter().any(|(named, _, _)| named == label) {
            continue;
        }
        let (own_arc, other_arc) = (first_arc(0, label), first_arc(1, label));
        let Some(arc) = own_arc.or(other_arc) else {
            continue;
        };
        let second = other_arc.filter(|second| own_arc.is_some_and(|first| first != *second));
        values.push((label.to_string(), arc, second));
    }
    Some(SnmpCollision::Oid {
        oid: oid.join("."),
        values,
        exact,
    })
}

/// How a rule 6 refusal says the two templates reach an OID: they "both
/// render" it when the collision is exact, and "may render" it when it rests
/// on a counter [`ArcSet::narrow`] kept whole.
#[cfg(feature = "snmp")]
fn renders(exact: bool) -> &'static str {
    if exact {
        "both render"
    } else {
        "may render"
    }
}

/// Validate a metrics entry's `snmp` encoder against the entry.
///
/// The rules, in this order, each failure a [`SondaError::Config`] with its
/// own message:
///
/// 1. **Pairing.** When the encoder is `snmp` or the sink is `snmp_agent`,
///    both are ([`validate_encoder_sink_pairing`]). Any other encoder is then
///    `Ok(())`.
/// 2. **Column present.** `oids:` has an entry for the entry's metric name.
///    Every `oids:` entry then parses as an
///    [`OidTemplate`](crate::encoder::snmp::oid::OidTemplate) (one with no
///    placeholder is a scalar and ends in `.0`) and every
///    `oids:` and `types:` key is a metric name
///    ([`SnmpEncoder::new`](crate::encoder::snmp::SnmpEncoder::new)).
///    Every `label_columns:` key is a label key, every entry's template
///    parses too, and no entry's type is the enum.
/// 3. **Placeholders are labels.** Every placeholder of the metric's
///    template names a key of the entry's `labels` or `dynamic_labels`. A
///    key only `cardinality_spikes` sets is refused: events carry it only
///    while the spike window is open. Every placeholder of a
///    `label_columns:` template is a placeholder of every `oids:` template.
///    Every `label_columns:` label is a static label of the entry, not a
///    `dynamic_labels` or `cardinality_spikes` label and not absent: series
///    that differ only in a label-column label share the metric's OID. A
///    typed column's type can serve that static value: it is written into
///    a scratch buffer by the encoder's own
///    [`write_label_text`](crate::encoder::snmp::value::write_label_text),
///    and the refusal names the label, the type and the value.
///    O(placeholders × templates + columns).
/// 4. **Index values are arcs.** Every value the label of each `{label}`
///    placeholder of the metric's template or a `label_columns:` template
///    can take is an [`index_arc`](crate::encoder::snmp::oid::index_arc):
///    the static value, unless a dynamic label of the same key replaces it;
///    every `values:` entry of a dynamic label; and the prefix followed by
///    `0` to `cardinality - 1` of a `counter` dynamic label or `counter`
///    spike, which are all arcs exactly when the first and the last are. A
///    `random` spike on the label is refused. A `{label:str}` placeholder
///    takes any value and is not checked here. O(values).
/// 5. **Coverage.** Every label key of the entry (`labels`,
///    `dynamic_labels`, `cardinality_spikes`) is a placeholder of the
///    template, a `label_columns:` label, the `agent_label`, or in
///    `drop_labels` ([`uncovered_label`](crate::encoder::snmp::uncovered_label)).
///    Series that differ only in an unreferenced label would share one OID;
///    series that differ only in a `label_columns:` label do share the
///    metric's, which is why rule 3 requires that label to be static.
///    O(keys).
/// 6. **No collision.** The templates the entry renders, its metric's and
///    each `label_columns:` one, can render no OID for this entry that
///    another template of the encoder, in `oids:` or `label_columns:`, can
///    render ([`snmp_collision`]): with a string index in either, one object
///    is equal to or a prefix of the other; otherwise the two have the same
///    number of arcs, and their arcs can be equal at every position at
///    once, a label taking one value at every position of its template.
///    Exact but for two counters whose values meet, which may be refused
///    when they cannot collide; such a refusal says the templates "may
///    render" the OID rather than "both render". O((metrics + columns)² × (labels + arcs
///    listed) × arcs × values).
///
/// Last, the **system group**: no `oids:` or `label_columns:` template can
/// render an OID under `1.3.6.1.2.1.1`, which the agent serves itself
/// ([`can_render_under`](crate::encoder::snmp::oid::OidTemplate::can_render_under)).
///
/// Returns `Ok(())` for any other encoder.
#[cfg(feature = "snmp")]
pub fn validate_snmp_encoder(config: &ScenarioConfig) -> Result<(), SondaError> {
    use super::{LabelSource, LabelValueSet};
    use crate::encoder::snmp::oid::{index_arc, Oid, OidTemplate, TemplateArc};
    use crate::encoder::snmp::{uncovered_label, SnmpEncoder};
    use crate::encoder::EncoderConfig;
    use std::collections::BTreeSet;

    let snmp_sink = matches!(config.base.sink, SinkConfig::SnmpAgent(_));
    if snmp_sink || matches!(config.encoder, EncoderConfig::Snmp(_)) {
        validate_encoder_sink_pairing(&config.encoder, &config.base.sink)?;
    }
    let EncoderConfig::Snmp(cfg) = &config.encoder else {
        return Ok(());
    };
    let name = config.name.as_str();
    let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));

    // 2. Column present, then every template parses.
    if !cfg.oids.contains_key(name) {
        return Err(invalid(format!(
            "snmp encoder has no oids: entry for metric {name:?}; set encoder.oids.{name}"
        )));
    }
    let encoder = SnmpEncoder::new(cfg)?;
    let template = encoder.template_for(name)?;
    let metrics: BTreeSet<&String> = cfg.oids.keys().collect();
    let mut columns: Vec<(&String, OidTemplate)> = Vec::new();
    for (label, column) in cfg
        .label_columns
        .iter()
        .collect::<std::collections::BTreeMap<_, _>>()
    {
        // SnmpEncoder::new parsed every one.
        let template = OidTemplate::parse(column.oid())
            .map_err(|e| invalid(format!("snmp encoder label_columns.{label}: {e}")))?;
        columns.push((label, template));
    }

    let mut entry_labels: Vec<&str> = Vec::new();
    let mut spike_labels: Vec<&str> = Vec::new();
    for (key, source) in config.base.emitted_label_keys() {
        match source {
            LabelSource::Static | LabelSource::Dynamic => entry_labels.push(key),
            LabelSource::Spike => spike_labels.push(key),
        }
    }

    // 3. Placeholders are labels.
    for placeholder in template.placeholders() {
        if entry_labels.contains(&placeholder) {
            continue;
        }
        let why = if spike_labels.contains(&placeholder) {
            "a cardinality_spikes label; events carry it only while the spike window is \
             open, so the OID could not be rendered outside it"
        } else {
            "which is not a label of this entry"
        };
        let written = placeholder_text(template, placeholder);
        return Err(invalid(format!(
            "snmp OID template for metric {name:?} uses placeholder {written}, {why}"
        )));
    }
    for (label, column) in &columns {
        for placeholder in column.placeholders() {
            for metric in &metrics {
                if !encoder
                    .template_for(metric)?
                    .placeholders()
                    .any(|p| p == placeholder)
                {
                    let written = placeholder_text(column, placeholder);
                    return Err(invalid(format!(
                        "snmp encoder label_columns.{label}: {column} uses placeholder \
                         {written}, which the oids: template of metric {metric:?} does not \
                         use; a label column uses only placeholders every oids: template uses"
                    )));
                }
            }
        }
    }
    let mut scratch = Vec::new();
    for (label, _) in &columns {
        let source = match config.base.label_value_set(label) {
            Some(LabelValueSet::Static(value)) => {
                let ty = cfg.label_columns.get(*label).and_then(|c| c.value_type());
                scratch.clear();
                crate::encoder::snmp::value::write_label_text(ty, label, &value, &mut scratch)
                    .map_err(|e| invalid(format!("snmp encoder label_columns.{label}: {e}")))?;
                continue;
            }
            Some(LabelValueSet::Values(_) | LabelValueSet::Counter { .. }) => {
                "a dynamic_labels label"
            }
            Some(LabelValueSet::SpikeCounter { .. } | LabelValueSet::RandomShape { .. }) => {
                "a cardinality_spikes label"
            }
            None => "not a label of this entry",
        };
        return Err(invalid(format!(
            "snmp encoder label_columns.{label}: label `{label}` is {source}; series that \
             differ only in a label-column label share the metric's OID, so its label must be \
             a static label of the entry"
        )));
    }

    // 4. Index values are arcs.
    let counter_values = |prefix: &str, cardinality: u64| {
        let first = format!("{prefix}0");
        let last = format!("{prefix}{}", cardinality.saturating_sub(1));
        let arcs = cardinality == 0 || (index_arc(&first).is_some() && index_arc(&last).is_some());
        (!arcs).then(|| {
            format!(
                "its counter values run from {first:?} to {last:?}; a counter renders as \
                 arcs with an empty or digit-only prefix (prefix: \"\")"
            )
        })
    };
    // Each `{label}` placeholder once, with the template it is first met in.
    let mut integer_placeholders: Vec<(&str, String)> = Vec::new();
    let sources = std::iter::once((format!("snmp OID template for metric {name:?}"), template))
        .chain(
            columns
                .iter()
                .map(|(label, column)| (format!("snmp encoder label_columns.{label}"), column)),
        );
    for (source, from) in sources {
        for arc in from.arcs() {
            if let TemplateArc::Label(label) = arc {
                if !integer_placeholders.iter().any(|(seen, _)| *seen == label) {
                    integer_placeholders.push((label, source.clone()));
                }
            }
        }
    }
    for (placeholder, source) in integer_placeholders {
        let mut next = config.base.label_value_set(placeholder);
        while let Some(set) = next {
            let (problem, outside) = match set {
                LabelValueSet::Static(value) => (
                    index_arc(&value)
                        .is_none()
                        .then(|| format!("its value {value:?} is not an unsigned 32-bit integer")),
                    None,
                ),
                LabelValueSet::Values(values) => (
                    values.iter().find(|v| index_arc(v).is_none()).map(|v| {
                        format!("its dynamic value {v:?} is not an unsigned 32-bit integer")
                    }),
                    None,
                ),
                LabelValueSet::Counter {
                    prefix,
                    cardinality,
                } => (counter_values(&prefix, cardinality), None),
                LabelValueSet::SpikeCounter {
                    prefix,
                    cardinality,
                    outside,
                } => (
                    counter_values(&prefix, cardinality),
                    outside.map(|set| *set),
                ),
                LabelValueSet::RandomShape { .. } => (
                    Some(
                        "a random cardinality spike sets it to values that are not integers"
                            .to_string(),
                    ),
                    None,
                ),
            };
            if let Some(problem) = problem {
                return Err(invalid(format!(
                    "{source} renders label `{placeholder}` as an OID arc (an unsigned \
                     32-bit integer in decimal, without leading zeros), but {problem}"
                )));
            }
            next = outside;
        }
    }

    // 5. Coverage.
    let all_labels = config.base.emitted_label_keys().map(|(key, _)| key);
    if let Some(label) = uncovered_label(cfg, template, all_labels) {
        return Err(invalid(format!(
            "snmp OID template for metric {name:?} does not reference label `{label}`, so \
             series that differ only in it would share one OID; reference it as a \
             placeholder, make it the encoder's agent_label, list it in its label_columns, \
             or list it in its drop_labels"
        )));
    }

    // 6. No collision within the entry: this metric's template, then each
    // label column's, against every other template of the encoder.
    // A template the refusal names, by what it is in the encoder block.
    type Named<'t> = (String, &'t OidTemplate);
    let others: Vec<Named<'_>> = metrics
        .iter()
        .filter(|metric| **metric != name)
        .map(|metric| Ok((format!("metric {metric:?}"), encoder.template_for(metric)?)))
        .collect::<Result<_, SondaError>>()?;
    let column_names: Vec<Named<'_>> = columns
        .iter()
        .map(|(label, column)| (format!("label_columns.{label}"), column))
        .collect();
    let own = (format!("metric {name:?}"), template);
    let mut pairs: Vec<(&Named<'_>, &Named<'_>)> = Vec::new();
    pairs.extend(others.iter().map(|other| (&own, other)));
    pairs.extend(column_names.iter().map(|column| (&own, column)));
    for (i, column) in column_names.iter().enumerate() {
        pairs.extend(others.iter().map(|other| (column, other)));
        pairs.extend(column_names[i + 1..].iter().map(|later| (column, later)));
    }
    for ((a, a_template), (b, b_template)) in pairs {
        // Two metrics keep the form `metrics "a" and "b"`.
        let both = match (a.strip_prefix("metric "), b.strip_prefix("metric ")) {
            (Some(a), Some(b)) => format!("metrics {a} and {b}"),
            _ => format!("{a} and {b}"),
        };
        match snmp_collision(a_template, b_template, &config.base) {
            None => {}
            Some(SnmpCollision::Oid { oid, values, exact }) => {
                let when = if values.is_empty() {
                    String::new()
                } else {
                    let values: Vec<String> = values
                        .iter()
                        .map(|(label, arc, second)| match second {
                            None => format!("{label} is {arc}"),
                            Some(second) => format!(
                                "{label} is {arc} in the first template and {second} in the \
                                 second"
                            ),
                        })
                        .collect();
                    format!(" when {}", values.join(" and "))
                };
                return Err(invalid(format!(
                    "snmp OID templates of {both} {} {oid} for this entry{when}; give each its \
                     own column",
                    renders(exact)
                )));
            }
            Some(SnmpCollision::Nested) => {
                return Err(invalid(format!(
                    "snmp OID templates of {both} can render the same OID: {a_template} and \
                     {b_template} have a string index and objects that are equal or one a \
                     prefix of the other; give each its own column"
                )));
            }
        }
    }

    // Not under the system group.
    let system = Oid::from_arcs(SNMP_SYSTEM_GROUP.to_vec())
        .ok_or_else(|| invalid("the system group OID is not encodable".to_string()))?;
    let mut named: Vec<(String, &OidTemplate)> = Vec::new();
    for metric in &metrics {
        named.push((format!("oids.{metric}"), encoder.template_for(metric)?));
    }
    for (label, column) in &columns {
        named.push((format!("label_columns.{label}"), column));
    }
    for (field, template) in named {
        if template.can_render_under(&system) {
            return Err(invalid(format!(
                "snmp encoder {field}: {template} can render OIDs under 1.3.6.1.2.1.1, the \
                 system group, which the snmp_agent sink serves itself"
            )));
        }
    }
    Ok(())
}

/// Validate a metrics entry's `gnmi` encoder against the entry.
///
/// Checks the entry's one series with [`validate_gnmi_series_set`]'s rules.
/// Every failure is a [`SondaError::Config`]. Returns `Ok(())` for any other
/// encoder.
#[cfg(feature = "gnmi")]
pub fn validate_gnmi_encoder(config: &ScenarioConfig) -> Result<(), SondaError> {
    validate_gnmi_series_set(
        &config.encoder,
        &config.base,
        &GnmiSeries::metric(&config.name),
    )
}

/// Validate a histogram entry's `gnmi` encoder against its series:
/// `<name>_bucket` (with `le`), `<name>_sum` and `<name>_count` (without).
#[cfg(feature = "gnmi")]
fn validate_gnmi_histogram(config: &HistogramScenarioConfig) -> Result<(), SondaError> {
    validate_gnmi_series_set(
        &config.encoder,
        &config.base,
        &GnmiSeries::histogram(config),
    )
}

/// Validate a summary entry's `gnmi` encoder against its series: `<name>`
/// (with `quantile`), `<name>_sum` and `<name>_count` (without).
#[cfg(feature = "gnmi")]
fn validate_gnmi_summary(config: &SummaryScenarioConfig) -> Result<(), SondaError> {
    validate_gnmi_series_set(&config.encoder, &config.base, &GnmiSeries::summary(config))
}

/// One series an entry emits, as the gnmi encoder sees it.
#[cfg(feature = "gnmi")]
pub(crate) struct GnmiSeries {
    /// The metric name on the series' events.
    pub(crate) name: String,
    /// A label the runner adds to every event of this series (`le` for a
    /// histogram bucket, `quantile` for a summary quantile), with the values
    /// it takes. Each value is a series of its own, so the template must
    /// reference it.
    pub(crate) injected: Option<(&'static str, super::LabelValueSet)>,
    /// The injected label of the entry's other series, which this one lacks.
    pub(crate) lacks: Option<&'static str>,
}

/// The schedule config, `gnmi` encoder config and series of a metrics,
/// histogram or summary entry that uses the `gnmi` encoder: the entry's name
/// for a metrics entry, `<name>_bucket`, `<name>_sum` and `<name>_count` for
/// a histogram, `<name>`, `<name>_sum` and `<name>_count` for a summary.
/// `None` for any other entry.
#[cfg(feature = "gnmi")]
pub(crate) fn gnmi_series(
    entry: &super::ScenarioEntry,
) -> Option<(
    &super::BaseScheduleConfig,
    &crate::encoder::gnmi::GnmiEncoderConfig,
    Vec<GnmiSeries>,
)> {
    use super::ScenarioEntry;
    use crate::encoder::EncoderConfig;

    let (base, encoder, series) = match entry {
        ScenarioEntry::Metrics(c) => (&c.base, &c.encoder, GnmiSeries::metric(&c.name)),
        ScenarioEntry::Histogram(c) => (&c.base, &c.encoder, GnmiSeries::histogram(c)),
        ScenarioEntry::Summary(c) => (&c.base, &c.encoder, GnmiSeries::summary(c)),
        ScenarioEntry::Logs(_) => return None,
    };
    match encoder {
        EncoderConfig::Gnmi(cfg) => Some((base, cfg, series)),
        _ => None,
    }
}

#[cfg(feature = "gnmi")]
impl GnmiSeries {
    /// A metrics entry's one series, named after the entry. The one
    /// definition the per-entry validation and [`gnmi_series`] share.
    fn metric(name: &str) -> Vec<GnmiSeries> {
        vec![GnmiSeries {
            name: name.to_string(),
            injected: None,
            lacks: None,
        }]
    }

    /// A histogram entry's series: `<name>_bucket` with `le`, taking the
    /// values of [`HistogramGenerator::le_values`](crate::generator::histogram::HistogramGenerator::le_values),
    /// then `<name>_sum` and `<name>_count` without it.
    fn histogram(config: &HistogramScenarioConfig) -> Vec<GnmiSeries> {
        let le = crate::generator::histogram::HistogramGenerator::from_config(config).le_values();
        Self::derived(&config.name, "_bucket", "le", le)
    }

    /// A summary entry's series: `<name>` with `quantile`, taking the values
    /// of [`SummaryGenerator::quantile_values`](crate::generator::summary::SummaryGenerator::quantile_values),
    /// then `<name>_sum` and `<name>_count` without it.
    fn summary(config: &SummaryScenarioConfig) -> Vec<GnmiSeries> {
        let quantile =
            crate::generator::summary::SummaryGenerator::from_config(config).quantile_values();
        Self::derived(&config.name, "", "quantile", quantile)
    }

    /// A histogram's or summary's series: `<name><suffix>` carrying `key`
    /// with `values`, then `<name>_sum` and `<name>_count` without it.
    fn derived(
        name: &str,
        suffix: &str,
        key: &'static str,
        values: Vec<String>,
    ) -> Vec<GnmiSeries> {
        vec![
            GnmiSeries {
                name: format!("{name}{suffix}"),
                injected: Some((key, super::LabelValueSet::Values(values))),
                lacks: None,
            },
            GnmiSeries {
                name: format!("{name}_sum"),
                injected: None,
                lacks: Some(key),
            },
            GnmiSeries {
                name: format!("{name}_count"),
                injected: None,
                lacks: Some(key),
            },
        ]
    }
}

/// Validate a `gnmi` encoder against every series an entry emits.
///
/// For each series:
/// - `origin` is non-empty, every `enum` map is non-empty, and `path` and
///   every `paths:` entry parse as templates (via
///   [`GnmiEncoder::new`](crate::encoder::gnmi::GnmiEncoder::new)).
/// - A template exists for the series' metric name: its `paths:` entry or `path`.
/// - Every required placeholder `{x}` in that template is `name`, a key of
///   the entry's `labels` or `dynamic_labels`, or the label the runner adds
///   to the series (`le` on a histogram's `_bucket` series, `quantile` on a
///   summary's quantile series) — labels every event of the series carries.
///   A `cardinality_spikes` label is not accepted as a required placeholder:
///   the runner adds it only while the spike window is open, so rendering
///   would fail once the window closed. An optional placeholder `{x?}` may
///   name any label: its key is left out of the path of an event that does
///   not carry it. A label the entry carries but the template misspells is
///   caught by the coverage rule below.
/// - The series with `le` or `quantile` references it, and `drop_labels` does
///   not list it: each value is a series of its own. The `_sum` and `_count`
///   series carry neither, so their template must not reference it as a
///   required placeholder; they need `paths:` entries of their own when
///   `path` does, unless `path` references it as `{le?}` or `{quantile?}`.
/// - A template that uses `{name}` is rejected when the entry also carries a
///   label called `name`, because `{name}` always renders the metric name and
///   every series would collapse onto one path. The exceptions are
///   `target_label: name`, where the label reaches the notification prefix so
///   series still differ, and `name` listed in `drop_labels`, which opts out
///   of the label on purpose.
/// - Every key in `labels` and `dynamic_labels` is referenced by the template,
///   is the encoder's `target_label`, or is listed in its `drop_labels`.
///   Otherwise series that differ only in that label would share one path.
/// - Every `cardinality_spikes` label is covered the same way: referenced as
///   an optional placeholder, the target label, a referenced static label of
///   the same key, or listed in `drop_labels`. A series whose events carry
///   the label inside the window then gets a path of its own there.
/// - Every value a label used as an element-name placeholder can render to
///   reads back as one element: non-empty, with no `/`, `[` or `]`. That is
///   the static value (unless a dynamic label of the same key replaces it),
///   every `values:` entry of a dynamic label, and the explicit `prefix:` of
///   a counter dynamic label or of a `cardinality_spikes` entry on that key.
///   Bucket bounds and quantiles render as numbers, which always are.
///   The rule is the one the encoder applies when it renders the path.
/// - The coverage rule is [`GnmiEncoder`](crate::encoder::gnmi::GnmiEncoder)'s own, which the encoder applies to
///   each new series at encode time, so a configuration that validates never
///   has an event rejected by it.
///
/// Every failure is a [`SondaError::Config`].
///
/// Returns `Ok(())` for any other encoder.
#[cfg(feature = "gnmi")]
fn validate_gnmi_series_set(
    encoder: &crate::encoder::EncoderConfig,
    config: &super::BaseScheduleConfig,
    series_set: &[GnmiSeries],
) -> Result<(), SondaError> {
    use crate::encoder::gnmi::GnmiEncoder;

    let crate::encoder::EncoderConfig::Gnmi(cfg) = encoder else {
        return Ok(());
    };
    let encoder = GnmiEncoder::new(cfg)?;
    for series in series_set {
        validate_gnmi_series(cfg, &encoder, config, series)?;
    }
    Ok(())
}

#[cfg(feature = "gnmi")]
fn validate_gnmi_series(
    cfg: &crate::encoder::gnmi::GnmiEncoderConfig,
    encoder: &crate::encoder::gnmi::GnmiEncoder,
    config: &super::BaseScheduleConfig,
    series: &GnmiSeries,
) -> Result<(), SondaError> {
    use super::{LabelSource, LabelValueSet};
    use crate::encoder::gnmi::path::{is_valid_element_name, NAME_PLACEHOLDER};

    let template = encoder.template_for(&series.name)?;

    // Static and dynamic labels are on every event, as is the label the
    // runner adds to this series, so a template may reference them; spike
    // labels exist only inside their window.
    let mut entry_labels: Vec<&str> = series.injected.iter().map(|(key, _)| *key).collect();
    let mut spike_labels: Vec<&str> = Vec::new();
    for (key, source) in config.emitted_label_keys() {
        match source {
            LabelSource::Static | LabelSource::Dynamic => entry_labels.push(key),
            LabelSource::Spike => spike_labels.push(key),
        }
    }
    entry_labels.sort_unstable();
    entry_labels.dedup();
    spike_labels.sort_unstable();
    spike_labels.dedup();
    let entry_label = |key: &str| entry_labels.binary_search(&key).is_ok();
    let spike_label = |key: &str| spike_labels.binary_search(&key).is_ok();

    if template.placeholders().any(|p| p.name == NAME_PLACEHOLDER)
        && !encoder.label_needs_no_reference(NAME_PLACEHOLDER)
        && (entry_label(NAME_PLACEHOLDER) || spike_label(NAME_PLACEHOLDER))
    {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "gnmi path template for series {:?} uses {{name}}, which always renders the \
             metric name, but this entry also has a label called `name`; rename the label \
             (for example to `ifName`) and reference it as {{ifName}}",
            series.name
        ))));
    }

    if let Some(key) = series.lacks {
        if template
            .placeholders()
            .any(|p| p.name == key && !p.optional)
        {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gnmi path template for series {:?} uses {{{key}}}, but that series carries \
                 no `{key}` label; give it its own paths: entry without {{{key}}}",
                series.name
            ))));
        }
    }
    if let Some(&(key, _)) = series.injected.as_ref() {
        if !template.placeholders().any(|p| p.name == key) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gnmi path template for series {:?} does not reference {{{key}}}; every \
                 `{key}` value is a series of its own, so the template must place it, for \
                 example [{key}={{{key}}}]",
                series.name
            ))));
        }
        if cfg.drop_labels.iter().any(|dropped| dropped == key) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gnmi encoder drop_labels lists `{key}`, which series {:?} must keep in its \
                 path; remove it from drop_labels",
                series.name
            ))));
        }
    }

    let known = |placeholder: &str| placeholder == NAME_PLACEHOLDER || entry_label(placeholder);
    if let Some(spiked) = template
        .placeholders()
        .find(|p| !p.optional && !known(p.name) && spike_label(p.name))
        .map(|p| p.name)
    {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "gnmi path template for series {:?} uses placeholder {{{spiked}}}, a \
             cardinality_spikes label; it exists only while the spike window is open, so \
             the path could not be rendered outside it",
            series.name
        ))));
    }
    if let Some(unknown) = template
        .placeholders()
        .find(|p| !p.optional && !known(p.name))
    {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "gnmi path template for series {:?} uses placeholder {{{}}}, which is \
             neither {{name}} nor a label of this entry",
            series.name, unknown.name
        ))));
    }

    // Every value an element-name placeholder can render to is in its label's
    // value set. Counter and spike values are a prefix plus digits or hex
    // digits, so the prefix alone decides whether they are valid. Values are
    // checked in config order: the static or dynamic value, then each spike.
    let value_clause =
        |value: &str| format!("its value {value:?} is empty or contains '/', '[' or ']'");
    let element_values = |label: &str| -> Vec<(String, String)> {
        let prefixed = |prefix: &str| {
            (
                format!("{prefix}0"),
                format!("values from its prefix {prefix:?} contain '/', '[' or ']'"),
            )
        };
        let mut layers = Vec::new();
        let mut next = config.label_value_set(label);
        while let Some(set) = next {
            let (layer, outside) = match set {
                LabelValueSet::Static(value) => {
                    let clause = value_clause(&value);
                    (vec![(value, clause)], None)
                }
                LabelValueSet::Values(list) => (
                    list.iter()
                        .map(|value| (value.clone(), value_clause(value)))
                        .collect(),
                    None,
                ),
                LabelValueSet::Counter { prefix, .. } => (vec![prefixed(&prefix)], None),
                LabelValueSet::SpikeCounter {
                    prefix, outside, ..
                }
                | LabelValueSet::RandomShape {
                    prefix, outside, ..
                } => (vec![prefixed(&prefix)], outside.map(|set| *set)),
            };
            layers.push(layer);
            next = outside;
        }
        layers.into_iter().rev().flatten().collect()
    };
    let bad_element = template
        .element_name_placeholders()
        .filter(|p| *p != NAME_PLACEHOLDER)
        .find_map(|p| {
            element_values(p)
                .into_iter()
                .find(|(rendered, _)| !is_valid_element_name(rendered))
                .map(|(_, shown)| (p, shown))
        });
    if let Some((label, shown)) = bad_element {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "gnmi path template for series {:?} uses label `{label}` as an element name, \
             but {shown}, so those events would be rejected; use it as a key value instead, for example \
             [name={{{label}}}]",
            series.name
        ))));
    }

    if let Some(unreferenced) = encoder.uncovered_label(template, entry_labels.iter().copied()) {
        // {name} always renders the metric name, so a `name` label cannot be
        // referenced by it.
        let advice = if unreferenced == NAME_PLACEHOLDER {
            "rename it (for example to `ifName`) and reference it as {ifName}, or list it \
             in the encoder's drop_labels"
                .to_string()
        } else {
            format!("reference it as {{{unreferenced}}} or list it in the encoder's drop_labels")
        };
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "gnmi path template for series {:?} does not reference label `{unreferenced}`, \
             so series that differ only in it would share one path; {advice}",
            series.name
        ))));
    }

    if let Some(spiked) = encoder.uncovered_label(template, spike_labels.iter().copied()) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "series {:?} has cardinality_spikes label `{spiked}`, which events carry while \
             the spike window is open but the gnmi path template does not reference; list it \
             in the encoder's drop_labels, or reference it as an optional key value, for \
             example [{spiked}={{{spiked}?}}]",
            series.name
        ))));
    }
    Ok(())
}

/// Validate the optional jitter amplitude for semantic correctness.
///
/// Checks:
/// - `jitter` must not be NaN.
/// - `jitter` must be finite (not infinite).
/// - `jitter` must be non-negative (>= 0).
///
/// Returns `Ok(())` when `jitter` is `None` (no jitter configured).
fn validate_jitter(jitter: Option<f64>) -> Result<(), SondaError> {
    if let Some(j) = jitter {
        if j.is_nan() {
            return Err(SondaError::Config(ConfigError::invalid(
                "jitter must not be NaN",
            )));
        }
        if j.is_infinite() {
            return Err(SondaError::Config(ConfigError::invalid(
                "jitter must be finite",
            )));
        }
        if j < 0.0 {
            return Err(SondaError::Config(ConfigError::invalid(
                "jitter must be non-negative",
            )));
        }
    }
    Ok(())
}

/// Validate that an encoder's precision (if set) does not exceed 17.
///
/// An `f64` has approximately 15-17 significant decimal digits. Precision values
/// above 17 produce meaningless trailing digits, so they are rejected as a
/// configuration error.
fn validate_encoder_precision(encoder: &crate::encoder::EncoderConfig) -> Result<(), SondaError> {
    if let Some(p) = encoder_precision(encoder) {
        if p > 17 {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "encoder precision must be 0..=17, got {}",
                p
            ))));
        }
    }
    Ok(())
}

/// Validate a [`DistributionConfig`] for semantic correctness.
///
/// Checks:
/// - Exponential: `rate` must be strictly positive.
/// - Normal: `stddev` must be strictly positive; `mean` must not be NaN.
/// - Uniform: `min` and `max` must not be NaN; `min` must be strictly less than `max`.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_distribution_config(dist: &DistributionConfig) -> Result<(), SondaError> {
    match dist {
        DistributionConfig::Exponential { rate } => {
            if rate.is_nan() || *rate <= 0.0 {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "distribution.rate must be positive, got {}",
                    rate
                ))));
            }
        }
        DistributionConfig::Normal { stddev, mean } => {
            if stddev.is_nan() || *stddev <= 0.0 {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "distribution.stddev must be positive, got {}",
                    stddev
                ))));
            }
            if mean.is_nan() {
                return Err(SondaError::Config(ConfigError::invalid(
                    "distribution.mean must not be NaN",
                )));
            }
        }
        DistributionConfig::Uniform { min, max } => {
            if min.is_nan() || max.is_nan() {
                return Err(SondaError::Config(ConfigError::invalid(
                    "distribution.min and distribution.max must not be NaN",
                )));
            }
            if *min >= *max {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "distribution.min ({}) must be strictly less than distribution.max ({})",
                    min, max
                ))));
            }
        }
    }
    Ok(())
}

/// Validate histogram bucket boundaries for semantic correctness.
///
/// Checks:
/// - Buckets must not be empty.
/// - All values must be strictly positive.
/// - Values must be strictly sorted (ascending, no duplicates).
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_buckets(buckets: &[f64]) -> Result<(), SondaError> {
    if buckets.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "buckets must not be empty",
        )));
    }
    for (i, &b) in buckets.iter().enumerate() {
        if b.is_nan() || b.is_infinite() || b <= 0.0 {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "buckets[{}] must be positive and finite, got {}",
                i, b
            ))));
        }
    }
    for window in buckets.windows(2) {
        if window[1] <= window[0] {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "buckets must be strictly sorted: {} >= {}",
                window[0], window[1]
            ))));
        }
    }
    Ok(())
}

/// Validate summary quantile targets for semantic correctness.
///
/// Checks:
/// - Quantiles must not be empty.
/// - All values must be in the open interval `(0, 1)`.
/// - Values must be strictly sorted (ascending, no duplicates).
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_quantiles(quantiles: &[f64]) -> Result<(), SondaError> {
    if quantiles.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "quantiles must not be empty",
        )));
    }
    for (i, &q) in quantiles.iter().enumerate() {
        if q.is_nan() || q <= 0.0 || q >= 1.0 {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "quantiles[{}] must be in (0, 1), got {}",
                i, q
            ))));
        }
    }
    for window in quantiles.windows(2) {
        if window[1] <= window[0] {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "quantiles must be strictly sorted: {} >= {}",
                window[0], window[1]
            ))));
        }
    }
    Ok(())
}

/// Validate a [`HistogramScenarioConfig`] for semantic correctness.
///
/// Checks:
/// - `rate` is strictly positive.
/// - `duration`, if provided, is a parseable duration string.
/// - Gap/burst/spike consistency (delegates to shared validators).
/// - Bucket boundaries are sorted, positive, non-empty.
/// - Distribution parameters are valid.
/// - `observations_per_tick` must be > 0 (when provided).
/// - Metric name is a valid Prometheus metric name.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_histogram_config(config: &HistogramScenarioConfig) -> Result<(), SondaError> {
    reject_snmp_for("histogram", &config.encoder, &config.base.sink)?;
    if config.rate.is_nan() || config.rate <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "rate must be positive, got {}",
            config.rate
        ))));
    }

    if let Some(ref dur_str) = config.duration {
        parse_duration(dur_str).map_err(|e| prepend_context("invalid duration", dur_str, e))?;
    }

    if let Some(ref gap) = config.gaps {
        let every = parse_duration(&gap.every)
            .map_err(|e| prepend_context("invalid gaps.every", &gap.every, e))?;
        let for_dur = parse_duration(&gap.r#for)
            .map_err(|e| prepend_context("invalid gaps.for", &gap.r#for, e))?;
        if for_dur >= every {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gaps.for ({:?}) must be less than gaps.every ({:?})",
                gap.r#for, gap.every
            ))));
        }
    }

    if let Some(ref burst) = config.bursts {
        validate_burst_config(burst)?;
    }

    if let Some(ref spikes) = config.cardinality_spikes {
        for spike in spikes {
            validate_cardinality_spike_config(spike)?;
        }
    }

    if let Some(ref dls) = config.dynamic_labels {
        for dl in dls {
            validate_dynamic_label_config(dl)?;
        }
    }

    if !is_valid_metric_name(&config.name) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
            config.name
        ))));
    }

    if let Some(ref buckets) = config.buckets {
        validate_buckets(buckets)?;
    }

    validate_distribution_config(&config.distribution)?;

    if let Some(obs) = config.observations_per_tick {
        if obs == 0 {
            return Err(SondaError::Config(ConfigError::invalid(
                "observations_per_tick must be greater than zero",
            )));
        }
    }

    validate_jitter(config.base.jitter)?;
    validate_encoder_precision(&config.encoder)?;

    #[cfg(feature = "gnmi")]
    validate_gnmi_histogram(config)?;

    validate_sink_config(&config.base.sink)?;

    validate_encoder_sink_pairing(&config.encoder, &config.base.sink)?;

    Ok(())
}

/// Validate a [`SummaryScenarioConfig`] for semantic correctness.
///
/// Checks:
/// - `rate` is strictly positive.
/// - `duration`, if provided, is a parseable duration string.
/// - Gap/burst/spike consistency (delegates to shared validators).
/// - Quantile targets are in `(0, 1)`, sorted, non-empty.
/// - Distribution parameters are valid.
/// - `observations_per_tick` must be > 0 (when provided).
/// - Metric name is a valid Prometheus metric name.
///
/// Returns [`SondaError::Config`] with a descriptive message if validation fails.
pub fn validate_summary_config(config: &SummaryScenarioConfig) -> Result<(), SondaError> {
    reject_snmp_for("summary", &config.encoder, &config.base.sink)?;
    if config.rate.is_nan() || config.rate <= 0.0 {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "rate must be positive, got {}",
            config.rate
        ))));
    }

    if let Some(ref dur_str) = config.duration {
        parse_duration(dur_str).map_err(|e| prepend_context("invalid duration", dur_str, e))?;
    }

    if let Some(ref gap) = config.gaps {
        let every = parse_duration(&gap.every)
            .map_err(|e| prepend_context("invalid gaps.every", &gap.every, e))?;
        let for_dur = parse_duration(&gap.r#for)
            .map_err(|e| prepend_context("invalid gaps.for", &gap.r#for, e))?;
        if for_dur >= every {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "gaps.for ({:?}) must be less than gaps.every ({:?})",
                gap.r#for, gap.every
            ))));
        }
    }

    if let Some(ref burst) = config.bursts {
        validate_burst_config(burst)?;
    }

    if let Some(ref spikes) = config.cardinality_spikes {
        for spike in spikes {
            validate_cardinality_spike_config(spike)?;
        }
    }

    if let Some(ref dls) = config.dynamic_labels {
        for dl in dls {
            validate_dynamic_label_config(dl)?;
        }
    }

    if !is_valid_metric_name(&config.name) {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
            config.name
        ))));
    }

    if let Some(ref quantiles) = config.quantiles {
        validate_quantiles(quantiles)?;
    }

    validate_distribution_config(&config.distribution)?;

    if let Some(obs) = config.observations_per_tick {
        if obs == 0 {
            return Err(SondaError::Config(ConfigError::invalid(
                "observations_per_tick must be greater than zero",
            )));
        }
    }

    validate_jitter(config.base.jitter)?;
    validate_encoder_precision(&config.encoder)?;

    #[cfg(feature = "gnmi")]
    validate_gnmi_summary(config)?;

    validate_sink_config(&config.base.sink)?;

    validate_encoder_sink_pairing(&config.encoder, &config.base.sink)?;

    Ok(())
}

/// Pre-parse every duration-string field on the sink so typos surface at
/// validation time, not at factory time.
pub fn validate_sink_config(sink: &SinkConfig) -> Result<(), SondaError> {
    match sink {
        SinkConfig::Stdout
        | SinkConfig::File { .. }
        | SinkConfig::Udp { .. }
        | SinkConfig::Memory { .. } => Ok(()),
        SinkConfig::Tcp { retry, .. } => validate_retry_config_opt(retry.as_ref()),
        #[cfg(feature = "http")]
        SinkConfig::HttpPush {
            max_buffer_age,
            retry,
            ..
        } => {
            validate_max_buffer_age(max_buffer_age.as_deref())?;
            validate_retry_config_opt(retry.as_ref())
        }
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite {
            max_buffer_age,
            retry,
            ..
        } => {
            validate_max_buffer_age(max_buffer_age.as_deref())?;
            validate_retry_config_opt(retry.as_ref())
        }
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka {
            max_buffer_age,
            retry,
            ..
        } => {
            validate_max_buffer_age(max_buffer_age.as_deref())?;
            validate_retry_config_opt(retry.as_ref())
        }
        #[cfg(feature = "http")]
        SinkConfig::Loki {
            max_buffer_age,
            retry,
            ..
        } => {
            validate_max_buffer_age(max_buffer_age.as_deref())?;
            validate_retry_config_opt(retry.as_ref())
        }
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc {
            max_buffer_age,
            retry,
            ..
        } => {
            validate_max_buffer_age(max_buffer_age.as_deref())?;
            validate_retry_config_opt(retry.as_ref())
        }
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled { .. } => Ok(()),
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled { .. } => Ok(()),
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled { .. } => Ok(()),
        #[cfg(not(feature = "http"))]
        SinkConfig::LokiDisabled { .. } => Ok(()),
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled { .. } => Ok(()),
        #[cfg(feature = "gnmi")]
        SinkConfig::GnmiTarget(cfg) => cfg.settings().map(|_| ()),
        #[cfg(not(feature = "gnmi"))]
        SinkConfig::GnmiTargetDisabled { .. } => Ok(()),
        #[cfg(feature = "snmp")]
        SinkConfig::SnmpAgent(cfg) => cfg.settings().map(|_| ()),
        #[cfg(not(feature = "snmp"))]
        SinkConfig::SnmpAgentDisabled { .. } => Ok(()),
    }
}

#[cfg(any(
    feature = "http",
    feature = "remote-write",
    feature = "kafka",
    feature = "otlp"
))]
fn validate_max_buffer_age(s: Option<&str>) -> Result<(), SondaError> {
    if let Some(s) = s {
        parse_optional_duration(s)?;
    }
    Ok(())
}

fn validate_retry_config_opt(retry: Option<&RetryConfig>) -> Result<(), SondaError> {
    if let Some(r) = retry {
        // Validate by *constructing* the policy the runtime will construct,
        // rather than re-listing its rules here. Two lists drift, and these
        // two had: this one parsed the two duration strings and stopped, so
        // `--dry-run` answered "Validation: OK" for `max_attempts: 0` and
        // for `max_backoff` below `initial_backoff` — both of which
        // `RetryPolicy::from_config` refuses moments later at sink
        // construction. A dry run that passes what the next real run
        // rejects is worse than no dry run: it is a check that reports on
        // something other than what it claims to check.
        //
        // The policy is discarded; only its refusals are wanted here.
        RetryPolicy::from_config(r)?;
    }
    Ok(())
}

/// Emission-time anchor parsed from a `start_time:` config string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartTime {
    /// Anchor at wall-clock now — the default, behaviour-identical to omitting the field.
    Now,
    /// Anchor at an absolute instant parsed from an RFC 3339 string.
    Absolute(SystemTime),
    /// Anchor at a signed offset from scenario start, resolved when the loop begins.
    Offset { forward: bool, by: Duration },
}

/// Parse a `start_time:` string into a [`StartTime`].
///
/// Accepts three forms: the literal `now`, an absolute RFC 3339 timestamp, or a
/// signed relative offset (`+`/`-` followed by a duration; `d` for days is
/// accepted in addition to the `ms`/`s`/`m`/`h` units of `parse_duration`).
pub fn parse_start_time(s: &str) -> Result<StartTime, SondaError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "start_time must not be empty",
        )));
    }
    if trimmed == "now" {
        return Ok(StartTime::Now);
    }
    if let Some(magnitude) = trimmed.strip_prefix('+') {
        return Ok(StartTime::Offset {
            forward: true,
            by: parse_offset_magnitude(magnitude)?,
        });
    }
    if let Some(magnitude) = trimmed.strip_prefix('-') {
        return Ok(StartTime::Offset {
            forward: false,
            by: parse_offset_magnitude(magnitude)?,
        });
    }
    parse_absolute_start_time(trimmed)
}

fn parse_offset_magnitude(s: &str) -> Result<Duration, SondaError> {
    if let Some(days) = s.strip_suffix('d') {
        let value: f64 = days.parse().map_err(|_| {
            SondaError::Config(ConfigError::invalid(format!(
                "start_time offset {:?} has an invalid numeric part",
                s
            )))
        })?;
        if !value.is_finite() || value <= 0.0 {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "start_time offset {:?} must be greater than zero",
                s
            ))));
        }
        return Duration::try_from_secs_f64(value * 86_400.0).map_err(|_| {
            SondaError::Config(ConfigError::invalid(format!(
                "start_time offset {:?} is too large",
                s
            )))
        });
    }
    parse_duration(s).map_err(|e| prepend_context("invalid start_time offset", s, e))
}

#[cfg(feature = "config")]
fn parse_absolute_start_time(s: &str) -> Result<StartTime, SondaError> {
    let parsed = chrono::DateTime::parse_from_rfc3339(s).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "start_time {:?} is not a valid RFC 3339 timestamp, signed offset, or \"now\": {}",
            s, e
        )))
    })?;
    Ok(StartTime::Absolute(SystemTime::from(parsed)))
}

#[cfg(not(feature = "config"))]
fn parse_absolute_start_time(s: &str) -> Result<StartTime, SondaError> {
    Err(SondaError::Config(ConfigError::invalid(format!(
        "start_time {:?} requires an RFC 3339 timestamp, signed offset, or \"now\"; \
         absolute timestamp parsing needs the `config` feature",
        s
    ))))
}

/// Wrap a `SondaError::Config` from `parse_duration` with additional field context.
///
/// Extracts the inner message string from the error so the final error reads
/// `"<label> <value_quoted>: <original message>"` without double-prefixing.
pub(crate) fn prepend_context(label: &str, value: &str, err: SondaError) -> SondaError {
    let inner_msg = match err {
        SondaError::Config(ref e) => e.to_string(),
        _ => err.to_string(),
    };
    SondaError::Config(ConfigError::invalid(format!(
        "{} {:?}: {}",
        label, value, inner_msg
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BaseScheduleConfig, GapConfig, ScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    #[cfg(feature = "gnmi")]
    use crate::sink::gnmi::GnmiTargetConfig;
    use crate::sink::SinkConfig;

    // ---- parse_duration: happy path ------------------------------------------

    #[test]
    fn parse_duration_seconds() {
        let d = parse_duration("30s").expect("30s must parse");
        assert_eq!(d.as_secs(), 30);
        assert_eq!(d.subsec_millis(), 0);
    }

    #[test]
    fn parse_duration_minutes() {
        let d = parse_duration("5m").expect("5m must parse");
        assert_eq!(d.as_secs(), 300);
    }

    #[test]
    fn parse_duration_hours() {
        let d = parse_duration("1h").expect("1h must parse");
        assert_eq!(d.as_secs(), 3600);
    }

    #[test]
    fn parse_duration_milliseconds() {
        let d = parse_duration("100ms").expect("100ms must parse");
        assert_eq!(d.as_millis(), 100);
        assert_eq!(d.as_secs(), 0);
    }

    #[test]
    fn parse_duration_large_value() {
        let d = parse_duration("120m").expect("120m must parse");
        assert_eq!(d.as_secs(), 7200);
    }

    #[test]
    fn parse_duration_one_second() {
        let d = parse_duration("1s").expect("1s must parse");
        assert_eq!(d.as_secs(), 1);
    }

    #[test]
    fn parse_duration_one_millisecond() {
        let d = parse_duration("1ms").expect("1ms must parse");
        assert_eq!(d.as_millis(), 1);
    }

    // ---- parse_duration: error cases -----------------------------------------

    #[test]
    fn parse_duration_empty_string_returns_err() {
        let result = parse_duration("");
        assert!(
            result.is_err(),
            "empty string must return Err, got {result:?}"
        );
    }

    #[test]
    fn parse_duration_no_unit_returns_err() {
        let result = parse_duration("abc");
        assert!(result.is_err(), "'abc' must return Err");
    }

    #[test]
    fn parse_duration_numeric_only_returns_err() {
        let result = parse_duration("30");
        assert!(result.is_err(), "'30' (no unit) must return Err");
    }

    #[test]
    fn parse_duration_negative_seconds_returns_err() {
        let result = parse_duration("-5s");
        assert!(result.is_err(), "'-5s' must return Err");
    }

    #[test]
    fn parse_duration_negative_milliseconds_returns_err() {
        let result = parse_duration("-100ms");
        assert!(result.is_err(), "'-100ms' must return Err");
    }

    #[test]
    fn parse_duration_zero_seconds_returns_err() {
        let result = parse_duration("0s");
        assert!(result.is_err(), "'0s' must return Err (zero duration)");
    }

    #[test]
    fn parse_duration_zero_minutes_returns_err() {
        let result = parse_duration("0m");
        assert!(result.is_err(), "'0m' must return Err (zero duration)");
    }

    #[test]
    fn parse_duration_unit_only_no_number_returns_err() {
        let result = parse_duration("s");
        assert!(result.is_err(), "'s' (no numeric part) must return Err");
    }

    #[test]
    fn parse_duration_fractional_seconds() {
        let d = parse_duration("1.5s").expect("1.5s must parse");
        assert_eq!(d.as_millis(), 1500);
    }

    #[test]
    fn parse_duration_fractional_minutes() {
        let d = parse_duration("0.5m").expect("0.5m must parse");
        assert_eq!(d.as_secs(), 30);
    }

    #[test]
    fn parse_duration_fractional_hours() {
        let d = parse_duration("0.5h").expect("0.5h must parse");
        assert_eq!(d.as_secs(), 1800);
    }

    #[test]
    fn parse_duration_fractional_milliseconds() {
        let d = parse_duration("1.5ms").expect("1.5ms must parse");
        // 1.5ms = 1500 microseconds
        assert_eq!(d.as_micros(), 1500);
    }

    #[test]
    fn parse_duration_fractional_preserves_sub_millisecond_precision() {
        let d = parse_duration("0.1s").expect("0.1s must parse");
        assert_eq!(d.as_millis(), 100);
    }

    #[test]
    fn parse_duration_integer_values_still_work_after_f64_switch() {
        // Ensure backward compatibility: integer values parse identically.
        let d = parse_duration("30s").expect("30s must parse");
        assert_eq!(d.as_secs(), 30);
        let d = parse_duration("100ms").expect("100ms must parse");
        assert_eq!(d.as_millis(), 100);
    }

    #[test]
    fn parse_duration_zero_fractional_returns_err() {
        let result = parse_duration("0.0s");
        assert!(result.is_err(), "'0.0s' must return Err (zero duration)");
    }

    #[test]
    fn parse_duration_infinity_returns_err() {
        let result = parse_duration("infs");
        assert!(result.is_err(), "'infs' must return Err (infinite)");
    }

    #[test]
    fn parse_duration_unknown_unit_returns_err() {
        let result = parse_duration("10d");
        assert!(result.is_err(), "'10d' must return Err (unknown unit)");
    }

    #[test]
    fn parse_delay_duration_accepts_zero_seconds() {
        let d = parse_delay_duration("0s").expect("'0s' must parse for delay");
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn parse_delay_duration_accepts_zero_milliseconds() {
        let d = parse_delay_duration("0ms").expect("'0ms' must parse for delay");
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn parse_delay_duration_accepts_bare_zero() {
        let d = parse_delay_duration("0").expect("'0' must parse for delay");
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn parse_delay_duration_accepts_positive_value() {
        let d = parse_delay_duration("5s").expect("'5s' must parse for delay");
        assert_eq!(d.as_secs(), 5);
    }

    #[test]
    fn parse_delay_duration_rejects_negative_value() {
        let result = parse_delay_duration("-1s");
        assert!(result.is_err(), "negative delay must be rejected");
    }

    #[test]
    fn parse_delay_duration_rejects_garbage() {
        let result = parse_delay_duration("not_a_duration");
        assert!(result.is_err(), "non-duration string must be rejected");
    }

    // ---- parse_offset_duration -----------------------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    // Zero is a value, not an absence: an offset of zero means "at the start".
    #[case::bare_zero("0", Some(0))]
    #[case::zero_seconds("0s", Some(0))]
    #[case::zero_millis("0ms", Some(0))]
    #[case::zero_minutes("0m", Some(0))]
    #[case::zero_hours("0h", Some(0))]
    // Positive values behave exactly as `parse_duration` does.
    #[case::positive_seconds("5s", Some(5_000))]
    #[case::positive_millis("250ms", Some(250))]
    #[case::positive_minutes("2m", Some(120_000))]
    // Everything `parse_duration` rejects stays rejected.
    #[case::negative("-1s", None)]
    #[case::negative_zero_suffix("-0s", None)]
    #[case::garbage("not_a_duration", None)]
    #[case::empty("", None)]
    #[case::unknown_unit("10d", None)]
    #[case::no_numeric_part("s", None)]
    fn parse_offset_duration_cases(#[case] input: &str, #[case] expected_ms: Option<u64>) {
        match (parse_offset_duration(input), expected_ms) {
            (Ok(d), Some(ms)) => assert_eq!(d.as_millis() as u64, ms, "input {input:?}"),
            (Err(_), None) => {}
            (Ok(d), None) => panic!("{input:?} must be rejected, parsed as {d:?}"),
            (Err(e), Some(_)) => panic!("{input:?} must parse, got {e}"),
        }
    }

    /// `parse_delay_duration` is the retained name for the same parser, and
    /// must stay that way.
    ///
    /// Two names for one rule is how the two `gap_windows` callers came to
    /// disagree in the first place. This fails the moment the wrapper grows
    /// behaviour of its own rather than delegating.
    #[test]
    fn parse_delay_duration_agrees_with_parse_offset_duration() {
        for input in [
            "0",
            "0s",
            "0ms",
            "0m",
            "0h",
            "5s",
            "250ms",
            "2m",
            "-1s",
            "not_a_duration",
            "",
            "10d",
        ] {
            let delay = parse_delay_duration(input).map_err(|e| e.to_string());
            let offset = parse_offset_duration(input).map_err(|e| e.to_string());
            assert_eq!(delay, offset, "the two names disagreed on {input:?}");
        }
    }

    // ---- parse_optional_duration ---------------------------------------------

    #[test]
    fn parse_optional_duration_positive_value_is_some() {
        let d = parse_optional_duration("5s").expect("5s must parse");
        assert_eq!(d, Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_optional_duration_zero_is_none() {
        assert_eq!(parse_optional_duration("0s").expect("0s must parse"), None);
        assert_eq!(
            parse_optional_duration("0ms").expect("0ms must parse"),
            None
        );
    }

    #[test]
    fn parse_optional_duration_garbage_returns_err() {
        assert!(parse_optional_duration("garbage").is_err());
    }

    // ---- validate_config: rate validation ------------------------------------

    #[test]
    fn validate_config_rate_zero_returns_err() {
        let config = minimal_config_with_rate(0.0);
        let result = validate_config(&config);
        assert!(result.is_err(), "rate=0 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("rate"),
            "error must mention 'rate', got: {msg}"
        );
    }

    #[test]
    fn validate_config_rate_negative_returns_err() {
        let config = minimal_config_with_rate(-1.0);
        let result = validate_config(&config);
        assert!(result.is_err(), "rate=-1 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("rate"),
            "error must mention 'rate', got: {msg}"
        );
    }

    #[test]
    fn validate_config_rate_positive_is_valid() {
        let config = minimal_config_with_rate(1000.0);
        assert!(validate_config(&config).is_ok(), "rate=1000 must be valid");
    }

    #[test]
    fn validate_config_rate_fractional_positive_is_valid() {
        let config = minimal_config_with_rate(0.5);
        assert!(
            validate_config(&config).is_ok(),
            "rate=0.5 (sub-hertz) must be valid"
        );
    }

    #[test]
    fn validate_config_rate_nan_returns_err() {
        let config = minimal_config_with_rate(f64::NAN);
        let result = validate_config(&config);
        assert!(result.is_err(), "rate=NaN must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("rate"),
            "error must mention 'rate', got: {msg}"
        );
    }

    // ---- validate_config: duration -------------------------------------------

    #[test]
    fn validate_config_invalid_duration_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.duration = Some("abc".to_string());
        let result = validate_config(&config);
        assert!(result.is_err(), "unparseable duration must be rejected");
    }

    #[test]
    fn validate_config_valid_duration_is_accepted() {
        let mut config = minimal_config_with_rate(100.0);
        config.duration = Some("30s".to_string());
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_none_duration_is_accepted() {
        let mut config = minimal_config_with_rate(100.0);
        config.duration = None;
        assert!(
            validate_config(&config).is_ok(),
            "no duration (run forever) must be valid"
        );
    }

    // ---- validate_config: gap consistency ------------------------------------

    #[test]
    fn validate_config_gap_for_less_than_every_is_valid() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
        });
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_gap_for_equal_to_every_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "10s".to_string(),
        });
        let result = validate_config(&config);
        assert!(result.is_err(), "gap_for == gap_every must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("gaps"),
            "error must mention 'gaps', got: {msg}"
        );
    }

    #[test]
    fn validate_config_gap_for_greater_than_every_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "20s".to_string(),
        });
        let result = validate_config(&config);
        assert!(result.is_err(), "gap_for > gap_every must be rejected");
    }

    #[test]
    fn validate_config_gap_invalid_every_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "bad".to_string(),
            r#for: "5s".to_string(),
        });
        let result = validate_config(&config);
        assert!(result.is_err(), "invalid gaps.every must be rejected");
    }

    #[test]
    fn validate_config_gap_invalid_for_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "bad".to_string(),
        });
        let result = validate_config(&config);
        assert!(result.is_err(), "invalid gaps.for must be rejected");
    }

    // ---- validate_config: metric name ----------------------------------------

    #[test]
    fn validate_config_valid_metric_name_up() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "up".to_string();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_valid_metric_name_with_underscores() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "http_requests_total".to_string();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_valid_metric_name_double_underscore_prefix() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "__internal".to_string();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_valid_metric_name_colon_separator() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "namespace:subsystem:metric".to_string();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_invalid_metric_name_starts_with_digit_returns_err() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "123bad".to_string();
        let result = validate_config(&config);
        assert!(result.is_err(), "'123bad' must be rejected as metric name");
        let msg = err_msg(result);
        assert!(
            msg.contains("name") || msg.contains("metric"),
            "error must mention name/metric, got: {msg}"
        );
    }

    #[test]
    fn validate_config_invalid_metric_name_contains_hyphen_returns_err() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = "has-dash".to_string();
        let result = validate_config(&config);
        assert!(
            result.is_err(),
            "'has-dash' must be rejected as metric name"
        );
    }

    #[test]
    fn validate_config_invalid_metric_name_empty_returns_err() {
        let mut config = minimal_config_with_rate(1.0);
        config.name = String::new();
        let result = validate_config(&config);
        assert!(result.is_err(), "empty metric name must be rejected");
    }

    // ---- ScenarioConfig YAML deserialization ---------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_minimal_scenario_config() {
        let yaml = r#"
name: up
rate: 10.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("minimal YAML must deserialize");
        assert_eq!(config.name, "up");
        assert_eq!(config.rate, 10.0);
        assert!(config.duration.is_none());
        assert!(config.gaps.is_none());
        assert!(config.labels.is_none());
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_minimal_config_encoder_defaults_to_prometheus_text() {
        let yaml = r#"
name: up
rate: 10.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("minimal YAML must deserialize");
        assert!(
            matches!(config.encoder, EncoderConfig::PrometheusText { .. }),
            "default encoder must be PrometheusText"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_minimal_config_sink_defaults_to_stdout() {
        let yaml = r#"
name: up
rate: 10.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("minimal YAML must deserialize");
        assert!(
            matches!(config.sink, SinkConfig::Stdout),
            "default sink must be Stdout"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_full_scenario_config_from_architecture_example() {
        // This YAML is taken directly from docs/architecture.md Section 6.
        let yaml = r#"
name: interface_oper_state
rate: 1000
duration: 30s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
gaps:
  every: 2m
  for: 20s
labels:
  hostname: t0-a1
  zone: eu1
encoder:
  type: prometheus_text
sink:
  type: stdout
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("architecture example YAML must deserialize");
        assert_eq!(config.name, "interface_oper_state");
        assert_eq!(config.rate, 1000.0);
        assert_eq!(config.duration.as_deref(), Some("30s"));

        // Check gap config
        let gap = config.gaps.as_ref().expect("gaps must be present");
        assert_eq!(gap.every, "2m");
        assert_eq!(gap.r#for, "20s");

        // Check labels
        let labels = config.labels.as_ref().expect("labels must be present");
        assert_eq!(labels.get("hostname").map(String::as_str), Some("t0-a1"));
        assert_eq!(labels.get("zone").map(String::as_str), Some("eu1"));

        // Check encoder and sink defaults via explicit YAML values
        assert!(matches!(
            config.encoder,
            EncoderConfig::PrometheusText { .. }
        ));
        assert!(matches!(config.sink, SinkConfig::Stdout));
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_config_with_labels() {
        let yaml = r#"
name: up
rate: 1.0
generator:
  type: constant
  value: 1.0
labels:
  env: prod
  region: us-east-1
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("YAML with labels must deserialize");
        let labels = config.base.labels.expect("labels must be present");
        assert_eq!(labels.get("env").map(String::as_str), Some("prod"));
        assert_eq!(labels.get("region").map(String::as_str), Some("us-east-1"));
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_config_with_gap() {
        let yaml = r#"
name: up
rate: 100.0
generator:
  type: constant
  value: 1.0
gaps:
  every: 2m
  for: 20s
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("YAML with gaps must deserialize");
        let gap = config.base.gaps.expect("gaps must be present");
        assert_eq!(gap.every, "2m");
        assert_eq!(gap.r#for, "20s");
    }

    // ---- validate_config: full architecture example round-trip ---------------

    #[cfg(feature = "config")]
    #[test]
    fn validate_architecture_example_config_passes() {
        let yaml = r#"
name: interface_oper_state
rate: 1000
duration: 30s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
gaps:
  every: 2m
  for: 20s
labels:
  hostname: t0-a1
  zone: eu1
encoder:
  type: prometheus_text
sink:
  type: stdout
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        assert!(
            validate_config(&config).is_ok(),
            "architecture example must pass validation"
        );
    }

    // ---- Round-trip: deserialize -> validate -> create factories -------------

    #[cfg(feature = "config")]
    #[tokio::test]
    async fn round_trip_creates_generator_encoder_sink_successfully() {
        use crate::encoder::create_encoder;
        use crate::generator::create_generator;
        use crate::sink::create_sink;

        let yaml = r#"
name: up
rate: 100.0
duration: 5s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 10.0
  offset: 10.0
gaps:
  every: 30s
  for: 5s
labels:
  env: test
encoder:
  type: prometheus_text
sink:
  type: stdout
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        assert!(validate_config(&config).is_ok(), "must validate");

        let gen = create_generator(&config.generator, config.rate).expect("generator factory");
        // Generator must produce a value at tick 0
        let _ = gen.value(0);

        let encoder = create_encoder(&config.encoder);
        // Encoder must exist (just check it does not panic on creation)
        drop(encoder);

        let sink = create_sink(&config.sink, None).await;
        assert!(sink.is_ok(), "sink must be created without error");
    }

    #[cfg(feature = "config")]
    #[test]
    fn round_trip_constant_generator_produces_expected_value() {
        use crate::generator::create_generator;

        let yaml = r#"
name: up
rate: 10.0
generator:
  type: constant
  value: 42.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        assert!(validate_config(&config).is_ok());
        let gen = create_generator(&config.generator, config.rate).expect("constant factory");
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(999), 42.0);
    }

    #[cfg(feature = "config")]
    #[test]
    fn round_trip_uniform_generator_values_in_range() {
        use crate::generator::create_generator;

        let yaml = r#"
name: noise
rate: 100.0
generator:
  type: uniform
  min: 0.0
  max: 1.0
  seed: 42
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        assert!(validate_config(&config).is_ok());
        let gen = create_generator(&config.generator, config.rate).expect("uniform factory");
        for tick in 0..1000 {
            let v = gen.value(tick);
            assert!(
                (0.0..=1.0).contains(&v),
                "value {v} out of [0,1] at tick {tick}"
            );
        }
    }

    // ---- ScenarioConfig: Clone and Debug contracts ---------------------------

    #[cfg(feature = "config")]
    #[test]
    fn scenario_config_is_cloneable() {
        let yaml = r#"
name: up
rate: 1.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        let cloned = config.clone();
        assert_eq!(cloned.name, config.name);
        assert_eq!(cloned.rate, config.rate);
    }

    #[cfg(feature = "config")]
    #[test]
    fn scenario_config_is_debuggable() {
        let yaml = r#"
name: up
rate: 1.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("must deserialize");
        let debug_str = format!("{config:?}");
        assert!(
            debug_str.contains("up"),
            "Debug output must contain the metric name"
        );
    }

    // ---- GapConfig: Debug and Clone ------------------------------------------

    #[test]
    fn gap_config_is_cloneable_and_debuggable() {
        let gap = GapConfig {
            every: "2m".to_string(),
            r#for: "20s".to_string(),
        };
        let cloned = gap.clone();
        assert_eq!(cloned.every, "2m");
        assert_eq!(cloned.r#for, "20s");
        let debug_str = format!("{gap:?}");
        assert!(debug_str.contains("2m"));
    }

    // ---- Error messages: no double "configuration error:" prefix ------------

    #[test]
    fn validate_config_gap_invalid_every_error_has_no_double_prefix() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "bad".to_string(),
            r#for: "5s".to_string(),
        });
        let msg = err_msg(validate_config(&config));
        // The message must start with "configuration error:" exactly once.
        // If prepend_context was broken it would produce
        // "configuration error: ... configuration error: ..." which contains
        // the prefix a second time after the first colon.
        let first_pos = msg
            .find("configuration error:")
            .expect("must contain prefix");
        let second_pos = msg[first_pos + 1..].find("configuration error:");
        assert!(
            second_pos.is_none(),
            "error message must not double-prefix 'configuration error:': {msg}"
        );
    }

    #[test]
    fn validate_config_gap_invalid_for_error_has_no_double_prefix() {
        let mut config = minimal_config_with_rate(100.0);
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "bad".to_string(),
        });
        let msg = err_msg(validate_config(&config));
        let first_pos = msg
            .find("configuration error:")
            .expect("must contain prefix");
        let second_pos = msg[first_pos + 1..].find("configuration error:");
        assert!(
            second_pos.is_none(),
            "error message must not double-prefix 'configuration error:': {msg}"
        );
    }

    #[test]
    fn validate_config_invalid_duration_error_has_no_double_prefix() {
        let mut config = minimal_config_with_rate(100.0);
        config.duration = Some("bad".to_string());
        let msg = err_msg(validate_config(&config));
        let first_pos = msg
            .find("configuration error:")
            .expect("must contain prefix");
        let second_pos = msg[first_pos + 1..].find("configuration error:");
        assert!(
            second_pos.is_none(),
            "error message must not double-prefix 'configuration error:': {msg}"
        );
    }

    // ---- validate_burst_config: multiplier validation ------------------------

    #[test]
    fn validate_burst_config_multiplier_zero_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 0.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "multiplier=0 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("multiplier"),
            "error must mention 'multiplier', got: {msg}"
        );
    }

    #[test]
    fn validate_burst_config_multiplier_negative_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: -1.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "multiplier=-1 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("multiplier"),
            "error must mention 'multiplier', got: {msg}"
        );
    }

    #[test]
    fn validate_burst_config_multiplier_nan_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: f64::NAN,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "multiplier=NaN must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("multiplier"),
            "error must mention 'multiplier', got: {msg}"
        );
    }

    #[test]
    fn validate_burst_config_burst_for_equal_to_every_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "10s".to_string(),
            multiplier: 5.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "burst.for == burst.every must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("bursts"),
            "error must mention 'bursts', got: {msg}"
        );
    }

    #[test]
    fn validate_burst_config_burst_for_greater_than_every_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "20s".to_string(),
            multiplier: 5.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "burst.for > burst.every must be rejected");
    }

    #[test]
    fn validate_burst_config_valid_values_pass() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 5.0,
        };
        assert!(
            validate_burst_config(&burst).is_ok(),
            "valid burst config must pass validation"
        );
    }

    #[test]
    fn validate_burst_config_fractional_multiplier_passes() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 0.5,
        };
        assert!(
            validate_burst_config(&burst).is_ok(),
            "fractional positive multiplier must be valid"
        );
    }

    #[test]
    fn validate_burst_config_invalid_every_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "bad".to_string(),
            r#for: "2s".to_string(),
            multiplier: 5.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "invalid bursts.every must be rejected");
    }

    #[test]
    fn validate_burst_config_invalid_for_returns_err() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "bad".to_string(),
            multiplier: 5.0,
        };
        let result = validate_burst_config(&burst);
        assert!(result.is_err(), "invalid bursts.for must be rejected");
    }

    // ---- validate_config: burst config integration --------------------------

    #[test]
    fn validate_config_with_valid_burst_passes() {
        let mut config = minimal_config_with_rate(100.0);
        config.bursts = Some(crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 5.0,
        });
        assert!(
            validate_config(&config).is_ok(),
            "config with valid burst must pass validation"
        );
    }

    #[test]
    fn validate_config_burst_multiplier_zero_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.bursts = Some(crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 0.0,
        });
        let result = validate_config(&config);
        assert!(result.is_err(), "multiplier=0 in config must be rejected");
    }

    #[test]
    fn validate_config_burst_for_greater_than_every_returns_err() {
        let mut config = minimal_config_with_rate(100.0);
        config.bursts = Some(crate::config::BurstConfig {
            every: "5s".to_string(),
            r#for: "10s".to_string(),
            multiplier: 2.0,
        });
        let result = validate_config(&config);
        assert!(
            result.is_err(),
            "burst.for > burst.every in config must be rejected"
        );
    }

    // ---- ScenarioConfig: burst YAML deserialization -------------------------

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_config_with_burst() {
        let yaml = r#"
name: up
rate: 100.0
generator:
  type: constant
  value: 1.0
bursts:
  every: 10s
  for: 2s
  multiplier: 5.0
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("YAML with bursts must deserialize");
        let burst = config.base.bursts.expect("bursts must be present");
        assert_eq!(burst.every, "10s");
        assert_eq!(burst.r#for, "2s");
        assert_eq!(burst.multiplier, 5.0);
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_config_without_burst_has_none_bursts() {
        let yaml = r#"
name: up
rate: 10.0
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig =
            serde_yaml_ng::from_str(yaml).expect("YAML without bursts must deserialize");
        assert!(
            config.bursts.is_none(),
            "bursts field must be None when not provided"
        );
    }

    #[test]
    fn burst_config_is_cloneable_and_debuggable() {
        let burst = crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 5.0,
        };
        let cloned = burst.clone();
        assert_eq!(cloned.every, "10s");
        assert_eq!(cloned.r#for, "2s");
        assert_eq!(cloned.multiplier, 5.0);
        let debug_str = format!("{burst:?}");
        assert!(debug_str.contains("10s"));
    }

    // ---- Error messages contain field names ----------------------------------

    #[test]
    fn validate_config_error_messages_are_descriptive() {
        // Rate error should mention the value and "rate"
        let config = minimal_config_with_rate(-5.0);
        let msg = err_msg(validate_config(&config));
        assert!(
            msg.contains("rate"),
            "rate error must mention 'rate': {msg}"
        );

        // Invalid metric name error should mention the bad name
        let mut config2 = minimal_config_with_rate(1.0);
        config2.name = "123bad".to_string();
        let msg2 = err_msg(validate_config(&config2));
        assert!(
            msg2.contains("123bad"),
            "metric name error must include the bad value: {msg2}"
        );
    }

    // ---- Helpers -------------------------------------------------------------

    /// Build a minimal valid ScenarioConfig overriding only the rate.
    fn minimal_config_with_rate(rate: f64) -> ScenarioConfig {
        ScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "up".to_string(),
                rate,
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
        }
    }

    // ---- validate_gnmi_encoder ----------------------------------------------

    #[cfg(feature = "gnmi")]
    fn gnmi_config(path: Option<&str>) -> ScenarioConfig {
        let mut config = minimal_config_with_rate(10.0);
        config.base.name = "in_octets".to_string();
        config.base.labels = Some(std::collections::HashMap::from([
            ("device".to_string(), "rtr-1".to_string()),
            ("ifName".to_string(), "Gi0/0/0".to_string()),
        ]));
        config.encoder = EncoderConfig::Gnmi(crate::encoder::gnmi::GnmiEncoderConfig {
            path: path.map(str::to_string),
            ..Default::default()
        });
        config
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_template_using_entry_labels_and_name_is_accepted() {
        let config = gnmi_config(Some(
            "/interfaces/interface[name={ifName}]/state/counters/{name}",
        ));
        validate_gnmi_encoder(&config).expect("known placeholders must validate");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_unknown_placeholder_is_rejected_and_named() {
        let config = gnmi_config(Some("/interfaces/interface[name={ifname}]/state"));
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("{ifname}"),
            "error must name the placeholder: {msg}"
        );
        assert!(
            msg.contains("in_octets"),
            "error must name the metric: {msg}"
        );
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_config_without_a_template_for_the_metric_is_rejected() {
        let mut config = gnmi_config(None);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("no path template"), "got: {msg}");
        assert!(
            msg.contains("in_octets"),
            "error must name the metric: {msg}"
        );

        // A `paths:` entry for another metric does not cover this one.
        let crate::encoder::EncoderConfig::Gnmi(ref mut cfg) = config.encoder else {
            unreachable!()
        };
        cfg.paths.insert("out_octets".to_string(), "/a".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("no path template"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_paths_entry_alone_is_enough_and_is_checked() {
        let mut config = gnmi_config(None);
        let crate::encoder::EncoderConfig::Gnmi(ref mut cfg) = config.encoder else {
            unreachable!()
        };
        cfg.paths.insert(
            "in_octets".to_string(),
            "/a[k={ifName}]/{missing}".to_string(),
        );
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("{missing}"), "got: {msg}");

        let crate::encoder::EncoderConfig::Gnmi(ref mut cfg) = config.encoder else {
            unreachable!()
        };
        cfg.paths
            .insert("in_octets".to_string(), "/a[k={ifName}]".to_string());
        validate_gnmi_encoder(&config).expect("paths entry with known labels must validate");
    }

    #[cfg(feature = "gnmi")]
    fn spike(label: &str) -> CardinalitySpikeConfig {
        CardinalitySpikeConfig {
            label: label.to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 5,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        }
    }

    #[cfg(feature = "gnmi")]
    fn counter_label(key: &str) -> DynamicLabelConfig {
        DynamicLabelConfig {
            key: key.to_string(),
            strategy: DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 3,
            },
        }
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_dynamic_label_key_is_a_known_placeholder() {
        let mut config = gnmi_config(Some("/a[if={ifName}][pod={pod}]/{name}"));
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("{pod}"), "the key is not declared yet: {msg}");
        config.base.dynamic_labels = Some(vec![counter_label("pod")]);
        validate_gnmi_encoder(&config).expect("a dynamic label is on every event");
    }

    /// A static label value is known at validation time, so one that cannot be
    /// an element name is rejected there rather than on every event.
    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::slash(        "Gi0/0/0")]
    #[case::empty(        "")]
    #[case::open_bracket( "a[b")]
    #[case::close_bracket("a]b")]
    fn gnmi_static_label_value_must_be_a_valid_element_name(#[case] value: &str) {
        let mut config = gnmi_config(Some("/interfaces/{ifName}/state/{name}"));
        config.base.labels.get_or_insert_with(Default::default)
            .insert("ifName".to_string(), value.to_string());

        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label `ifName` as an element name"), "got: {msg}");
        assert!(msg.contains(&format!("{value:?}")), "the value must be named: {msg}");
        assert!(msg.contains("in_octets"), "the metric must be named: {msg}");

        // Positive control: the same value as a key value is fine.
        gnmi_cfg(&mut config).path =
            Some("/interfaces/interface[name={ifName}]/state/{name}".to_string());
        validate_gnmi_encoder(&config).expect("any value may be a key value");
    }

    /// Dynamic and spike values are known at validation time too, so a bad one
    /// is rejected there. The template uses `ifName` as an element name; the
    /// static value is valid, so only the dynamic or spike source can fail.
    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::values_list(   Some(values_label(&["Gi0", "Gi0/0/1"])), None,         Some("value \"Gi0/0/1\""))]
    #[case::counter_prefix(Some(prefixed_counter(Some("Gi0/"))),     None,         Some("values from its prefix \"Gi0/\" contain"))]
    #[case::spike_prefix(  None,                                     Some("a["),   Some("values from its prefix \"a[\" contain"))]
    #[case::valid_values(  Some(values_label(&["Gi0", "Gi1"])),      None,         None)]
    #[case::default_prefix(Some(prefixed_counter(None)),             None,         None)]
    #[case::empty_prefix(  Some(prefixed_counter(Some(""))),         None,         None)]
    #[case::valid_spike(   None,                                     Some("eth"),  None)]
    fn gnmi_dynamic_and_spike_values_must_be_valid_element_names(
        #[case] dynamic: Option<DynamicLabelConfig>,
        #[case] spike_prefix: Option<&str>,
        #[case] rejected: Option<&str>,
    ) {
        let mut config = gnmi_config(Some("/interfaces/{ifName}/state/{name}"));
        config.base.labels.get_or_insert_with(Default::default)
            .insert("ifName".to_string(), "Gi0".to_string());
        validate_gnmi_encoder(&config).expect("baseline: a valid static value");

        config.base.dynamic_labels = dynamic.map(|dl| vec![dl]);
        if let Some(prefix) = spike_prefix {
            let mut window = spike("ifName");
            window.prefix = Some(prefix.to_string());
            config.base.cardinality_spikes = Some(vec![window]);
        }
        match rejected {
            Some(shown) => {
                let msg = err_msg(validate_gnmi_encoder(&config));
                assert!(msg.contains("label `ifName` as an element name"), "got: {msg}");
                assert!(msg.contains(shown), "the offending {shown} must be named: {msg}");
            }
            None => validate_gnmi_encoder(&config).expect("every value is a valid element name"),
        }
    }

    #[cfg(feature = "gnmi")]
    fn values_label(values: &[&str]) -> DynamicLabelConfig {
        DynamicLabelConfig {
            key: "ifName".to_string(),
            strategy: DynamicLabelStrategy::ValuesList {
                values: values.iter().map(|v| v.to_string()).collect(),
            },
        }
    }

    #[cfg(feature = "gnmi")]
    fn prefixed_counter(prefix: Option<&str>) -> DynamicLabelConfig {
        DynamicLabelConfig {
            key: "ifName".to_string(),
            strategy: DynamicLabelStrategy::Counter {
                prefix: prefix.map(str::to_string),
                cardinality: 3,
            },
        }
    }

    /// A dynamic label of the same key replaces the static one on every event,
    /// so the static value is never rendered and is not checked.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_shadowed_static_label_value_is_not_an_element_name() {
        let mut config = gnmi_config(Some("/interfaces/{ifName}/state/{name}"));
        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert("ifName".to_string(), "Gi0/0/0".to_string());
        assert!(err_msg(validate_gnmi_encoder(&config)).contains("element name"));

        config.base.dynamic_labels = Some(vec![counter_label("ifName")]);
        validate_gnmi_encoder(&config).expect("the dynamic value is what renders");
    }

    /// Events carry a spike label while its window is open, and the template
    /// may not reference it, so it must be dropped.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_spike_label_must_be_dropped() {
        let mut config = gnmi_config(Some("/a[if={ifName}]/{name}"));
        validate_gnmi_encoder(&config).expect("baseline without spikes must validate");

        config.base.cardinality_spikes = Some(vec![spike("burst")]);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("cardinality_spikes label `burst`"),
            "got: {msg}"
        );
        assert!(msg.contains("drop_labels"), "got: {msg}");

        gnmi_cfg(&mut config).drop_labels.push("burst".to_string());
        validate_gnmi_encoder(&config).expect("a dropped spike label is allowed");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_spike_label_is_rejected_as_a_placeholder() {
        let mut config = gnmi_config(Some("/a[if={ifName}][burst={burst}]/{name}"));
        config.base.cardinality_spikes = Some(vec![spike("burst")]);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("{burst}"),
            "error must name the placeholder: {msg}"
        );
        assert!(msg.contains("spike window"), "error must say why: {msg}");

        // The same key as a static label is on every event, so it is fine.
        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert("burst".to_string(), "x".to_string());
        validate_gnmi_encoder(&config).expect("a static label of the same key is always present");
    }

    /// A spike label referenced as an optional key value needs no
    /// drop_labels: its key is left out while the window is closed. Referenced
    /// as `{burst}` it is still refused with the message above, and not
    /// referenced at all it still must be dropped.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_spike_label_referenced_as_an_optional_key_needs_no_drop_labels() {
        let mut config = gnmi_config(Some("/a[if={ifName}][burst={burst?}]/{name}"));
        config.base.cardinality_spikes = Some(vec![spike("burst")]);
        validate_gnmi_encoder(&config).expect("an optional key may name a spike label");
        assert!(
            gnmi_cfg(&mut config).drop_labels.is_empty(),
            "the premise: nothing dropped"
        );

        gnmi_cfg(&mut config).path = Some("/a[if={ifName}][burst={burst}]/{name}".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains(
                "uses placeholder {burst}, a cardinality_spikes label; it exists only \
                          while the spike window is open"
            ),
            "a required reference keeps its message: {msg}"
        );

        gnmi_cfg(&mut config).path = Some("/a[if={ifName}]/{name}".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("cardinality_spikes label `burst`"),
            "got: {msg}"
        );
        assert!(
            msg.contains("list it in the encoder's drop_labels"),
            "got: {msg}"
        );
        assert!(
            msg.contains("[burst={burst?}]"),
            "the optional form is named: {msg}"
        );
    }

    /// An optional placeholder may name a label the entry never carries:
    /// its key is always left out, so a template with `[spike={spike?}]`
    /// can serve entries without a spike. A misspelled reference to a spike
    /// label the entry does carry leaves that label unreferenced, which the
    /// coverage rule refuses. A required placeholder still names a label.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_optional_placeholder_may_name_a_label_the_entry_never_carries() {
        let mut config = gnmi_config(Some("/a[if={ifName}][spike={spike?}]/{name}"));
        validate_gnmi_encoder(&config).expect("no event carries `spike`; its key is left out");

        config.base.cardinality_spikes = Some(vec![spike("spike")]);
        validate_gnmi_encoder(&config).expect("the spike's events render the key");
        gnmi_cfg(&mut config).path = Some("/a[if={ifName}][spike={spkie?}]/{name}".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("cardinality_spikes label `spike`"),
            "got: {msg}"
        );

        gnmi_cfg(&mut config).path = Some("/a[if={ifName}][k={nope}]/{name}".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("placeholder {nope}"), "got: {msg}");
        assert!(msg.contains("neither {name} nor a label"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::static_label(|c: &mut ScenarioConfig| {
        c.base.labels.get_or_insert_with(Default::default).insert("name".to_string(), "Gi0/0/0".to_string());
    })]
    #[case::dynamic_label(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![counter_label("name")]))]
    #[case::spike_label(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![spike("name")]))]
    fn gnmi_name_placeholder_with_a_name_label_is_rejected(
        #[case] add_name_label: fn(&mut ScenarioConfig),
    ) {
        let mut config = gnmi_config(Some("/interfaces/interface[name={name}][if={ifName}]/state"));
        validate_gnmi_encoder(&config).expect("without a `name` label, {name} is the metric name");

        add_name_label(&mut config);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label called `name`"), "got: {msg}");
    }

    /// With `target_label: name` the label reaches the prefix, so `{name}`
    /// in the path collides with nothing. The same config with the default
    /// target is the collision.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_name_placeholder_is_fine_when_name_is_the_target_label() {
        let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state/{name}"));
        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert("name".to_string(), "rtr-1".to_string());
        gnmi_cfg(&mut config).drop_labels.push("device".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label called `name`"), "got: {msg}");

        gnmi_cfg(&mut config).target_label = "name".to_string();
        validate_gnmi_encoder(&config).expect("`name` as the target label collides with nothing");
    }

    /// A `name` label listed in `drop_labels` is deliberately out of the path,
    /// so `{name}` collides with nothing. For a spike label called `name` this
    /// is the only valid configuration.
    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::static_label(|c: &mut ScenarioConfig| {
        c.base.labels.get_or_insert_with(Default::default).insert("name".to_string(), "x".to_string());
    })]
    #[case::spike_label(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![spike("name")]))]
    fn gnmi_name_placeholder_is_fine_when_name_is_dropped(
        #[case] add_name_label: fn(&mut ScenarioConfig),
    ) {
        let mut config = gnmi_config(Some("/interfaces/interface[if={ifName}]/state/{name}"));
        add_name_label(&mut config);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label called `name`"), "got: {msg}");

        gnmi_cfg(&mut config).drop_labels.push("name".to_string());
        validate_gnmi_encoder(&config).expect("a dropped `name` label collides with nothing");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn validate_log_config_rejects_the_gnmi_encoder() {
        let config = crate::config::LogScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "test".to_string(),
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
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        };
        validate_log_config(&config).expect("the baseline must validate");

        let mut with_encoder = config.clone();
        with_encoder.encoder = gnmi_encoder_for("test", "le");
        let msg = err_msg(validate_log_config(&with_encoder));
        assert!(
            msg.contains("the gnmi encoder carries metric signals only"),
            "got: {msg}"
        );
        assert!(msg.contains("not logs entries"), "got: {msg}");

        let mut with_sink = config;
        with_sink.base.sink = gnmi_target_sink();
        let msg = err_msg(validate_log_config(&with_sink));
        assert!(
            msg.contains("the gnmi_target sink carries metric signals only"),
            "got: {msg}"
        );
        assert!(msg.contains("not logs entries"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_name_label_without_the_name_placeholder_needs_drop_labels() {
        let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state"));
        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert("name".to_string(), "x".to_string());
        // {name} is never the label, so a `name` label can only be dropped.
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label `name`"), "got: {msg}");
        assert!(
            !msg.contains("reference it as {name}"),
            "{{name}} renders the metric name, so it cannot be the advice: {msg}"
        );
        assert!(msg.contains("rename it"), "got: {msg}");

        gnmi_cfg(&mut config).drop_labels.push("name".to_string());
        validate_gnmi_encoder(&config).expect("a dropped `name` label collides with nothing");
    }

    #[cfg(feature = "gnmi")]
    fn gnmi_cfg(config: &mut ScenarioConfig) -> &mut crate::encoder::gnmi::GnmiEncoderConfig {
        match config.encoder {
            EncoderConfig::Gnmi(ref mut cfg) => cfg,
            _ => panic!("test config must use the gnmi encoder"),
        }
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_unreferenced_static_label_is_rejected_and_named() {
        let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state"));
        validate_gnmi_encoder(&config).expect("baseline: ifName referenced, device is the target");

        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert("job".to_string(), "edge".to_string());
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains("label `job`"),
            "error must name the label: {msg}"
        );
        assert!(
            msg.contains("drop_labels"),
            "error must name the opt-out: {msg}"
        );

        gnmi_cfg(&mut config).drop_labels.push("job".to_string());
        validate_gnmi_encoder(&config).expect("a label listed in drop_labels is allowed");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_unreferenced_dynamic_label_is_rejected_and_named() {
        let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state"));
        config.base.dynamic_labels = Some(vec![counter_label("pod")]);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label `pod`"), "got: {msg}");

        gnmi_cfg(&mut config).drop_labels.push("pod".to_string());
        validate_gnmi_encoder(&config).expect("a dropped dynamic label is allowed");
    }

    /// `device` passes unreferenced only because it is the target label:
    /// moving the target elsewhere makes the same config fail.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_target_label_is_exempt_from_the_reference_rule() {
        let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state"));
        validate_gnmi_encoder(&config).expect("device is the default target label");

        gnmi_cfg(&mut config).target_label = "host".to_string();
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains("label `device`"), "got: {msg}");
    }

    /// Two unreferenced labels: the error names the first in key order, every time.
    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_unreferenced_label_error_is_deterministic() {
        for _ in 0..64 {
            let mut config = gnmi_config(Some("/interfaces/interface[name={ifName}]/state"));
            let labels = config.base.labels.get_or_insert_with(Default::default);
            labels.insert("zone".to_string(), "z".to_string());
            labels.insert("job".to_string(), "j".to_string());
            let msg = err_msg(validate_gnmi_encoder(&config));
            assert!(msg.contains("label `job`"), "got: {msg}");
        }
    }

    #[cfg(feature = "gnmi")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::empty_origin(crate::encoder::gnmi::GnmiEncoderConfig {
        origin: String::new(),
        path: Some("/a".into()),
        ..Default::default()
    }, "origin")]
    #[case::empty_enum(crate::encoder::gnmi::GnmiEncoderConfig {
        path: Some("/a".into()),
        values: std::collections::HashMap::from([(
            "in_octets".to_string(),
            crate::encoder::gnmi::GnmiValueType::Enum(Default::default()),
        )]),
        ..Default::default()
    }, "enum map")]
    #[case::bad_grammar(crate::encoder::gnmi::GnmiEncoderConfig {
        path: Some("/a[k".into()),
        ..Default::default()
    }, "unclosed")]
    fn gnmi_encoder_field_errors_are_rejected(
        #[case] cfg: crate::encoder::gnmi::GnmiEncoderConfig,
        #[case] needle: &str,
    ) {
        let mut config = gnmi_config(None);
        config.encoder = EncoderConfig::Gnmi(cfg);
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(msg.contains(needle), "got: {msg}");
    }

    // ---- validate_config: gnmi encoder and sink pairing ----------------------

    #[cfg(feature = "gnmi")]
    fn valid_gnmi_config_with_sink(sink: SinkConfig) -> ScenarioConfig {
        let mut config = gnmi_config(Some(
            "/interfaces/interface[name={ifName}]/state/counters/{name}",
        ));
        config.base.sink = sink;
        config
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::stdout(SinkConfig::Stdout)]
    #[case::file(SinkConfig::File { path: "/tmp/x".to_string() })]
    #[case::udp(SinkConfig::Udp { address: "127.0.0.1:9999".to_string() })]
    #[case::tcp(SinkConfig::Tcp { address: "127.0.0.1:9999".to_string(), retry: None })]
    fn gnmi_encoder_is_rejected_with_every_sink(#[case] sink: SinkConfig) {
        let config = valid_gnmi_config_with_sink(sink);
        // Positive controls: the encoder block is valid, and the same entry
        // with another encoder validates, so only the pairing is rejected.
        validate_gnmi_encoder(&config).expect("the gnmi block itself is valid");
        let mut other = config.clone();
        other.encoder = EncoderConfig::PrometheusText { precision: None };
        validate_config(&other).expect("the entry validates with another encoder");

        let msg = err_msg(validate_config(&config));
        assert!(
            msg.contains("can only be used with the gnmi_target sink"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_encoder_with_the_gnmi_target_sink_validates() {
        let config = valid_gnmi_config_with_sink(gnmi_target_sink());
        validate_config(&config).expect("the gnmi encoder and the gnmi_target sink pair");
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::prometheus(EncoderConfig::PrometheusText { precision: None })]
    #[case::json(EncoderConfig::JsonLines { precision: None })]
    fn gnmi_target_sink_is_rejected_with_every_other_encoder(#[case] encoder: EncoderConfig) {
        let mut config = valid_gnmi_config_with_sink(gnmi_target_sink());
        config.encoder = encoder;
        let msg = err_msg(validate_config(&config));
        assert!(
            msg.contains("the gnmi_target sink can only be used with the gnmi encoder"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::listen_not_an_address(|c: &mut GnmiTargetConfig| c.listen = "localhost".into(), "not a socket address")]
    #[case::listen_without_port(|c: &mut GnmiTargetConfig| c.listen = "127.0.0.1".into(), "not a socket address")]
    #[case::max_subscribers_zero(|c: &mut GnmiTargetConfig| c.max_subscribers = Some(0), "max_subscribers must be at least 1")]
    #[case::subscriber_buffer_zero(|c: &mut GnmiTargetConfig| c.subscriber_buffer = Some(0), "subscriber_buffer must be between 1 and 65536, got 0")]
    #[case::subscriber_buffer_too_large(|c: &mut GnmiTargetConfig| c.subscriber_buffer = Some(65_537), "got 65537")]
    #[case::sample_floor_zero(|c: &mut GnmiTargetConfig| c.sample_floor = Some("0s".into()), "invalid gnmi_target sample_floor \"0s\"")]
    #[case::sample_floor_below_a_microsecond(|c: &mut GnmiTargetConfig| c.sample_floor = Some("0.0001ms".into()), "invalid gnmi_target sample_floor \"0.0001ms\": must be at least 0.001ms")]
    #[case::sample_floor_garbage(|c: &mut GnmiTargetConfig| c.sample_floor = Some("soon".into()), "invalid gnmi_target sample_floor \"soon\"")]
    #[rustfmt::skip]
    fn gnmi_target_sink_settings_are_validated(
        #[case] mutate: fn(&mut GnmiTargetConfig),
        #[case] needle: &str,
    ) {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:57400");
        validate_sink_config(&SinkConfig::GnmiTarget(cfg.clone())).expect("the baseline validates");
        mutate(&mut cfg);
        let msg = err_msg(validate_sink_config(&SinkConfig::GnmiTarget(cfg.clone())));
        assert!(msg.contains(needle), "got: {msg}");

        // The same defect fails a whole entry, not only the sink on its own.
        let config = valid_gnmi_config_with_sink(SinkConfig::GnmiTarget(cfg));
        assert!(err_msg(validate_config(&config)).contains(needle));
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_target_sink_settings_at_their_bounds_validate() {
        let cfg = GnmiTargetConfig {
            listen: "[::1]:0".into(),
            max_subscribers: Some(1),
            subscriber_buffer: Some(65_536),
            sample_floor: Some("100ms".into()),
            tls: None,
            credentials: None,
        };
        validate_sink_config(&SinkConfig::GnmiTarget(cfg)).expect("every field at a bound");
    }

    /// The generated schema accepts any scalar for a text field, so the
    /// parser takes one too and validation names what is wrong with it.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_target_text_fields_take_any_scalar() {
        let sink: SinkConfig = serde_yaml_ng::from_str(
            "type: gnmi_target\nlisten: 57400\nsample_floor: 5\nmax_subscribers: 2\n",
        )
        .expect("numbers parse as text");
        let SinkConfig::GnmiTarget(cfg) = &sink else {
            panic!("got {sink:?}");
        };
        assert_eq!(cfg.listen, "57400");
        assert_eq!(cfg.sample_floor.as_deref(), Some("5"));
        let msg = err_msg(validate_sink_config(&sink));
        assert!(
            msg.contains("listen \"57400\" is not a socket address"),
            "got: {msg}"
        );

        let sink: SinkConfig =
            serde_yaml_ng::from_str("type: gnmi_target\nlisten: 127.0.0.1:0\nsample_floor: null\n")
                .expect("an explicit null is the default");
        assert!(
            matches!(&sink, SinkConfig::GnmiTarget(cfg) if cfg.sample_floor.is_none()),
            "got {sink:?}"
        );
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_target_sink_rejects_unknown_keys() {
        let ok: SinkConfig =
            serde_yaml_ng::from_str("type: gnmi_target\nlisten: 127.0.0.1:57400\n")
                .expect("the tag is not an unknown key");
        assert!(
            matches!(&ok, SinkConfig::GnmiTarget(cfg) if *cfg == GnmiTargetConfig::new("127.0.0.1:57400")),
            "got {ok:?}"
        );
        let err = serde_yaml_ng::from_str::<SinkConfig>(
            "type: gnmi_target\nlisten: 127.0.0.1:57400\nlisten_addr: 0.0.0.0:57400\n",
        )
        .expect_err("a misspelt key is refused");
        assert!(err.to_string().contains("listen_addr"), "got: {err}");
    }

    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rstest::rstest]
    #[case::tls("tls: {cert: a.pem, key: b.pem, client-ca: c.pem}", "client-ca")]
    #[case::credentials("credentials: {username: u, pasword: p}", "pasword")]
    fn gnmi_target_tls_and_credentials_reject_unknown_keys(
        #[case] block: &str,
        #[case] misspelt: &str,
    ) {
        let err = serde_yaml_ng::from_str::<SinkConfig>(&format!(
            "type: gnmi_target\nlisten: 127.0.0.1:57400\n{block}\n"
        ))
        .expect_err("a misspelt key is refused");
        assert!(err.to_string().contains(misspelt), "got: {err}");
    }

    /// Every field of the two blocks parses from YAML; numbers parse as text,
    /// as the schema allows.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[test]
    fn gnmi_target_tls_and_credentials_parse() {
        let sink: SinkConfig = serde_yaml_ng::from_str(
            "type: gnmi_target\nlisten: 127.0.0.1:57400\n\
             tls: {cert: a.pem, key: b.pem, client_ca: c.pem}\n\
             credentials: {username: sonda, password: 1234}\n",
        )
        .expect("parses");
        let SinkConfig::GnmiTarget(cfg) = &sink else {
            panic!("got {sink:?}");
        };
        assert_eq!(
            cfg.tls,
            Some(crate::sink::gnmi::GnmiTlsConfig {
                cert: "a.pem".into(),
                key: "b.pem".into(),
                client_ca: Some("c.pem".into()),
            })
        );
        let credentials = cfg.credentials.as_ref().expect("credentials");
        assert_eq!(
            (credentials.username.as_str(), credentials.password.as_str()),
            ("sonda", "1234")
        );
    }

    /// A `credentials:` value that is not a map is refused with an error that
    /// does not repeat the value.
    #[cfg(all(feature = "gnmi", feature = "config"))]
    #[rstest::rstest]
    #[case::string("\"sonda:Hunter2Secret\"", "Hunter2Secret")]
    #[case::number("424242", "424242")]
    fn gnmi_target_credentials_that_are_not_a_map_are_not_echoed(
        #[case] value: &str,
        #[case] secret: &str,
    ) {
        let err = serde_yaml_ng::from_str::<SinkConfig>(&format!(
            "type: gnmi_target\nlisten: 127.0.0.1:57400\n\
             tls: {{cert: a.pem, key: b.pem}}\ncredentials: {value}\n"
        ))
        .expect_err("a scalar is not credentials");
        let msg = err.to_string();
        assert!(
            msg.contains("credentials must be a map with username and password"),
            "got: {msg}"
        );
        assert!(!msg.contains(secret), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_target_credentials_without_tls_are_refused() {
        let cfg = GnmiTargetConfig {
            credentials: Some(crate::sink::gnmi::GnmiCredentials {
                username: "sonda".into(),
                password: "hunter2".into(),
            }),
            ..GnmiTargetConfig::new("127.0.0.1:57400")
        };
        let msg = err_msg(validate_sink_config(&SinkConfig::GnmiTarget(cfg)));
        assert!(
            msg.contains("credentials need tls")
                && msg.contains("the password would cross the wire in clear"),
            "got: {msg}"
        );
        assert!(!msg.contains("hunter2"), "got: {msg}");
    }

    /// Each `tls` file that cannot be read, or does not hold what its field
    /// needs, fails validation with an error naming the field and the file.
    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::cert_missing("cert", None, "cannot be read")]
    #[case::cert_not_pem("cert", Some("not a certificate\n"), "holds no PEM certificate")]
    #[case::cert_holds_a_key("cert", Some("@key"), "holds no PEM certificate")]
    #[case::key_missing("key", None, "cannot be read")]
    #[case::key_holds_a_certificate("key", Some("@cert"), "holds no PEM private key")]
    #[case::key_of_another_certificate("key", Some("@client-key"), "cannot serve TLS")]
    #[case::client_ca_missing("client_ca", None, "cannot be read")]
    #[case::client_ca_not_pem("client_ca", Some("garbage\n"), "holds no PEM certificate")]
    #[rustfmt::skip]
    fn gnmi_target_tls_files_are_checked_at_validation(
        #[case] field: &str,
        #[case] contents: Option<&str>,
        #[case] needle: &str,
    ) {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let mut cfg = GnmiTargetConfig {
            tls: Some(files.mutual_tls()),
            ..GnmiTargetConfig::new("127.0.0.1:57400")
        };
        validate_sink_config(&SinkConfig::GnmiTarget(cfg.clone())).expect("the baseline validates");

        let pem = |name: &str| std::fs::read_to_string(files.path(name)).expect("a test file");
        let path = match contents {
            None => files.path("absent.pem"),
            Some("@key") => files.write("replaced.pem", &pem("server-key.pem")),
            Some("@cert") => files.write("replaced.pem", &pem("server.pem")),
            Some("@client-key") => files.write("replaced.pem", &files.client_key_pem),
            Some(text) => files.write("replaced.pem", text),
        };
        let tls = cfg.tls.as_mut().expect("tls");
        match field {
            "cert" => tls.cert = path.clone(),
            "key" => tls.key = path.clone(),
            _ => tls.client_ca = Some(path.clone()),
        }
        let msg = err_msg(validate_sink_config(&SinkConfig::GnmiTarget(cfg)));
        assert!(msg.contains(needle), "got: {msg}");
        assert!(msg.contains(&format!("{path:?}")), "names the file: {msg}");
        let named_field = match field {
            // The pair is named together when only the combination fails.
            "key" if needle == "cannot serve TLS" => "key".to_string(),
            field => format!("tls {field} "),
        };
        assert!(msg.contains(&named_field), "names the field: {msg}");
    }

    /// `validate_config` runs the gnmi checks, so an invalid block reports its
    /// own defect rather than only the pairing.
    #[cfg(feature = "gnmi")]
    #[test]
    fn validate_config_reports_gnmi_block_errors_before_the_pairing() {
        let mut config = valid_gnmi_config_with_sink(SinkConfig::Stdout);
        gnmi_cfg(&mut config).path = Some("/a/{nope}".into());
        let msg = err_msg(validate_config(&config));
        assert!(msg.contains("nope"), "got: {msg}");
        assert!(!msg.contains("gnmi_target"), "got: {msg}");
    }

    #[test]
    fn pairing_accepts_every_other_encoder() {
        let encoder = EncoderConfig::PrometheusText { precision: None };
        validate_encoder_sink_pairing(&encoder, &SinkConfig::Stdout)
            .expect("only the gnmi encoder has a pairing rule");
    }

    // ---- validate_config: encoder precision validation ------------------------

    #[test]
    fn precision_18_rejected() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::PrometheusText {
            precision: Some(18),
        };
        let result = validate_config(&config);
        assert!(result.is_err(), "precision=18 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("precision"),
            "error must mention 'precision', got: {msg}"
        );
    }

    #[test]
    fn precision_17_accepted() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::PrometheusText {
            precision: Some(17),
        };
        assert!(
            validate_config(&config).is_ok(),
            "precision=17 must be accepted"
        );
    }

    #[test]
    fn precision_0_accepted() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::PrometheusText { precision: Some(0) };
        assert!(
            validate_config(&config).is_ok(),
            "precision=0 must be accepted"
        );
    }

    #[test]
    fn precision_none_accepted() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::PrometheusText { precision: None };
        assert!(
            validate_config(&config).is_ok(),
            "precision=None must be accepted"
        );
    }

    #[test]
    fn precision_255_rejected() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::JsonLines {
            precision: Some(255),
        };
        let result = validate_config(&config);
        assert!(result.is_err(), "precision=255 must be rejected");
    }

    #[test]
    fn precision_influx_18_rejected() {
        let mut config = minimal_config_with_rate(10.0);
        config.encoder = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: Some(18),
        };
        let result = validate_config(&config);
        assert!(result.is_err(), "precision=18 on influx must be rejected");
    }

    /// Extract the error message string from a Result.
    fn err_msg(result: Result<(), crate::SondaError>) -> String {
        match result {
            Err(e) => e.to_string(),
            Ok(()) => panic!("expected Err but got Ok"),
        }
    }

    // ---- validate_cardinality_spike_config: happy path -----------------------

    #[test]
    fn valid_spike_config_counter_returns_ok() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "2m".to_string(),
            r#for: "30s".to_string(),
            cardinality: 500,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_ok());
    }

    #[test]
    fn valid_spike_config_random_returns_ok() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "error_msg".to_string(),
            every: "5m".to_string(),
            r#for: "1m".to_string(),
            cardinality: 1000,
            strategy: crate::config::SpikeStrategy::Random,
            prefix: None,
            seed: Some(42),
        };
        assert!(validate_cardinality_spike_config(&spike).is_ok());
    }

    // ---- validate_cardinality_spike_config: error cases ----------------------

    #[test]
    fn spike_config_invalid_label_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "123-bad".to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        let result = validate_cardinality_spike_config(&spike);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("123-bad"),
            "error should mention bad label: {msg}"
        );
    }

    #[test]
    fn spike_config_empty_label_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "".to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_err());
    }

    #[test]
    fn spike_config_unparseable_every_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod".to_string(),
            every: "bad".to_string(),
            r#for: "10s".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_err());
    }

    #[test]
    fn spike_config_unparseable_for_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod".to_string(),
            every: "1m".to_string(),
            r#for: "bad".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_err());
    }

    #[test]
    fn spike_config_for_not_less_than_every_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod".to_string(),
            every: "1m".to_string(),
            r#for: "2m".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_err());
    }

    #[test]
    fn spike_config_for_equal_to_every_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod".to_string(),
            every: "1m".to_string(),
            r#for: "1m".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        assert!(validate_cardinality_spike_config(&spike).is_err());
    }

    #[test]
    fn spike_config_zero_cardinality_returns_error() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod".to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 0,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        };
        let result = validate_cardinality_spike_config(&spike);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cardinality"),
            "error should mention cardinality: {msg}"
        );
    }

    // ---- validate_config with cardinality_spikes ----

    #[test]
    fn validate_config_with_valid_spike_returns_ok() {
        let mut config = minimal_config_with_rate(10.0);
        config.cardinality_spikes = Some(vec![crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "2m".to_string(),
            r#for: "30s".to_string(),
            cardinality: 500,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        }]);
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_with_invalid_spike_returns_error() {
        let mut config = minimal_config_with_rate(10.0);
        config.cardinality_spikes = Some(vec![crate::config::CardinalitySpikeConfig {
            label: "123bad".to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        }]);
        assert!(validate_config(&config).is_err());
    }

    // ---- validate_config: jitter validation ----------------------------------

    #[test]
    fn validate_config_jitter_none_is_accepted() {
        let config = minimal_config_with_rate(10.0);
        assert!(
            validate_config(&config).is_ok(),
            "jitter=None must be accepted"
        );
    }

    #[test]
    fn validate_config_jitter_zero_is_accepted() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(0.0);
        assert!(
            validate_config(&config).is_ok(),
            "jitter=0.0 must be accepted"
        );
    }

    #[test]
    fn validate_config_jitter_positive_is_accepted() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(5.0);
        assert!(
            validate_config(&config).is_ok(),
            "jitter=5.0 must be accepted"
        );
    }

    #[test]
    fn validate_config_jitter_nan_returns_err() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(f64::NAN);
        let result = validate_config(&config);
        assert!(result.is_err(), "jitter=NaN must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("NaN"),
            "error must mention 'jitter' and 'NaN', got: {msg}"
        );
    }

    #[test]
    fn validate_config_jitter_positive_infinity_returns_err() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(f64::INFINITY);
        let result = validate_config(&config);
        assert!(result.is_err(), "jitter=+Inf must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("finite"),
            "error must mention 'jitter' and 'finite', got: {msg}"
        );
    }

    #[test]
    fn validate_config_jitter_negative_infinity_returns_err() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(f64::NEG_INFINITY);
        let result = validate_config(&config);
        assert!(result.is_err(), "jitter=-Inf must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("finite"),
            "error must mention 'jitter' and 'finite', got: {msg}"
        );
    }

    #[test]
    fn validate_config_jitter_negative_returns_err() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.jitter = Some(-1.0);
        let result = validate_config(&config);
        assert!(result.is_err(), "jitter=-1.0 must be rejected");
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("non-negative"),
            "error must mention 'jitter' and 'non-negative', got: {msg}"
        );
    }

    #[test]
    fn validate_log_config_jitter_nan_returns_err() {
        let log_config = crate::config::LogScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "logs".to_string(),
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
                jitter: Some(f64::NAN),
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: None,
            },
            encoder: crate::encoder::EncoderConfig::JsonLines { precision: None },
        };
        let result = validate_log_config(&log_config);
        assert!(
            result.is_err(),
            "log config with jitter=NaN must be rejected"
        );
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("NaN"),
            "error must mention 'jitter' and 'NaN', got: {msg}"
        );
    }

    #[test]
    fn validate_log_config_jitter_negative_returns_err() {
        let log_config = crate::config::LogScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "logs".to_string(),
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
                jitter: Some(-0.5),
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: None,
            },
            encoder: crate::encoder::EncoderConfig::JsonLines { precision: None },
        };
        let result = validate_log_config(&log_config);
        assert!(
            result.is_err(),
            "log config with jitter=-0.5 must be rejected"
        );
        let msg = err_msg(result);
        assert!(
            msg.contains("jitter") && msg.contains("non-negative"),
            "error must mention 'jitter' and 'non-negative', got: {msg}"
        );
    }

    // ---- validate_dynamic_label_config: happy path ------------------------------

    #[test]
    fn valid_dynamic_label_counter_returns_ok() {
        let dl = crate::config::DynamicLabelConfig {
            key: "hostname".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: Some("host-".to_string()),
                cardinality: 10,
            },
        };
        assert!(validate_dynamic_label_config(&dl).is_ok());
    }

    #[test]
    fn valid_dynamic_label_values_list_returns_ok() {
        let dl = crate::config::DynamicLabelConfig {
            key: "region".to_string(),
            strategy: crate::config::DynamicLabelStrategy::ValuesList {
                values: vec!["us-east".to_string(), "eu-west".to_string()],
            },
        };
        assert!(validate_dynamic_label_config(&dl).is_ok());
    }

    #[test]
    fn valid_dynamic_label_counter_no_prefix_returns_ok() {
        let dl = crate::config::DynamicLabelConfig {
            key: "pod".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 5,
            },
        };
        assert!(validate_dynamic_label_config(&dl).is_ok());
    }

    // ---- validate_dynamic_label_config: error cases -----------------------------

    #[test]
    fn dynamic_label_invalid_key_returns_error() {
        let dl = crate::config::DynamicLabelConfig {
            key: "123-bad".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 10,
            },
        };
        let result = validate_dynamic_label_config(&dl);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("123-bad"),
            "error should mention bad key: {msg}"
        );
    }

    #[test]
    fn dynamic_label_empty_key_returns_error() {
        let dl = crate::config::DynamicLabelConfig {
            key: String::new(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 10,
            },
        };
        assert!(validate_dynamic_label_config(&dl).is_err());
    }

    #[test]
    fn dynamic_label_counter_zero_cardinality_returns_error() {
        let dl = crate::config::DynamicLabelConfig {
            key: "host".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 0,
            },
        };
        let result = validate_dynamic_label_config(&dl);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cardinality"),
            "error should mention cardinality: {msg}"
        );
    }

    #[test]
    fn dynamic_label_values_list_empty_returns_error() {
        let dl = crate::config::DynamicLabelConfig {
            key: "region".to_string(),
            strategy: crate::config::DynamicLabelStrategy::ValuesList { values: Vec::new() },
        };
        let result = validate_dynamic_label_config(&dl);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("values"), "error should mention values: {msg}");
    }

    // ---- validate_config with dynamic_labels -----------------------------------

    #[test]
    fn validate_config_with_valid_dynamic_labels_returns_ok() {
        let mut config = minimal_config_with_rate(10.0);
        config.dynamic_labels = Some(vec![crate::config::DynamicLabelConfig {
            key: "hostname".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: Some("host-".to_string()),
                cardinality: 10,
            },
        }]);
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_with_invalid_dynamic_label_key_returns_error() {
        let mut config = minimal_config_with_rate(10.0);
        config.dynamic_labels = Some(vec![crate::config::DynamicLabelConfig {
            key: "bad-key".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 5,
            },
        }]);
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_log_config_with_valid_dynamic_labels_returns_ok() {
        let log_config = crate::config::LogScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "test".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: Some(vec![crate::config::DynamicLabelConfig {
                    key: "pod".to_string(),
                    strategy: crate::config::DynamicLabelStrategy::ValuesList {
                        values: vec!["a".to_string(), "b".to_string()],
                    },
                }]),
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
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: crate::encoder::EncoderConfig::JsonLines { precision: None },
        };
        assert!(validate_log_config(&log_config).is_ok());
    }

    #[test]
    fn validate_log_config_with_invalid_dynamic_label_returns_error() {
        let log_config = crate::config::LogScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                gap_windows: None,
                name: "test".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: Some(vec![crate::config::DynamicLabelConfig {
                    key: "pod".to_string(),
                    strategy: crate::config::DynamicLabelStrategy::Counter {
                        prefix: None,
                        cardinality: 0,
                    },
                }]),
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
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: crate::encoder::EncoderConfig::JsonLines { precision: None },
        };
        assert!(validate_log_config(&log_config).is_err());
    }

    // ---- validate_buckets ---------------------------------------------------

    #[test]
    fn validate_buckets_accepts_sorted_positive_values() {
        assert!(validate_buckets(&[0.005, 0.01, 0.1, 1.0, 10.0]).is_ok());
    }

    #[test]
    fn validate_buckets_rejects_empty() {
        assert!(validate_buckets(&[]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_negative_value() {
        assert!(validate_buckets(&[-1.0, 0.1, 1.0]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_zero_value() {
        assert!(validate_buckets(&[0.0, 0.1, 1.0]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_unsorted() {
        assert!(validate_buckets(&[1.0, 0.5, 2.0]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_duplicates() {
        assert!(validate_buckets(&[0.1, 0.5, 0.5, 1.0]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_nan() {
        assert!(validate_buckets(&[0.1, f64::NAN, 1.0]).is_err());
    }

    #[test]
    fn validate_buckets_rejects_infinity() {
        assert!(validate_buckets(&[0.1, f64::INFINITY]).is_err());
    }

    // ---- validate_quantiles -------------------------------------------------

    #[test]
    fn validate_quantiles_accepts_valid_targets() {
        assert!(validate_quantiles(&[0.5, 0.9, 0.95, 0.99]).is_ok());
    }

    #[test]
    fn validate_quantiles_rejects_empty() {
        assert!(validate_quantiles(&[]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_zero() {
        assert!(validate_quantiles(&[0.0, 0.5, 0.99]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_one() {
        assert!(validate_quantiles(&[0.5, 1.0]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_negative() {
        assert!(validate_quantiles(&[-0.1, 0.5]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_greater_than_one() {
        assert!(validate_quantiles(&[0.5, 1.5]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_unsorted() {
        assert!(validate_quantiles(&[0.99, 0.5]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_duplicates() {
        assert!(validate_quantiles(&[0.5, 0.5, 0.99]).is_err());
    }

    #[test]
    fn validate_quantiles_rejects_nan() {
        assert!(validate_quantiles(&[0.5, f64::NAN]).is_err());
    }

    // ---- validate_distribution_config ---------------------------------------

    #[test]
    fn validate_distribution_exponential_positive_rate() {
        let dist = DistributionConfig::Exponential { rate: 10.0 };
        assert!(validate_distribution_config(&dist).is_ok());
    }

    #[test]
    fn validate_distribution_exponential_zero_rate() {
        let dist = DistributionConfig::Exponential { rate: 0.0 };
        assert!(validate_distribution_config(&dist).is_err());
    }

    #[test]
    fn validate_distribution_exponential_negative_rate() {
        let dist = DistributionConfig::Exponential { rate: -1.0 };
        assert!(validate_distribution_config(&dist).is_err());
    }

    #[test]
    fn validate_distribution_normal_positive_stddev() {
        let dist = DistributionConfig::Normal {
            mean: 0.1,
            stddev: 0.02,
        };
        assert!(validate_distribution_config(&dist).is_ok());
    }

    #[test]
    fn validate_distribution_normal_zero_stddev() {
        let dist = DistributionConfig::Normal {
            mean: 0.1,
            stddev: 0.0,
        };
        assert!(validate_distribution_config(&dist).is_err());
    }

    #[test]
    fn validate_distribution_normal_negative_stddev() {
        let dist = DistributionConfig::Normal {
            mean: 0.1,
            stddev: -1.0,
        };
        assert!(validate_distribution_config(&dist).is_err());
    }

    #[test]
    fn validate_distribution_uniform_valid() {
        let dist = DistributionConfig::Uniform { min: 0.0, max: 1.0 };
        assert!(validate_distribution_config(&dist).is_ok());
    }

    #[test]
    fn validate_distribution_uniform_nan_min() {
        let dist = DistributionConfig::Uniform {
            min: f64::NAN,
            max: 1.0,
        };
        assert!(validate_distribution_config(&dist).is_err());
    }

    #[test]
    fn validate_distribution_uniform_min_greater_than_max() {
        let dist = DistributionConfig::Uniform { min: 5.0, max: 1.0 };
        let result = validate_distribution_config(&dist);
        assert!(
            result.is_err(),
            "min > max must be rejected, got {result:?}"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("less than"),
            "error should mention 'less than', got: {err_msg}"
        );
    }

    #[test]
    fn validate_distribution_uniform_min_equal_to_max() {
        let dist = DistributionConfig::Uniform { min: 3.0, max: 3.0 };
        assert!(
            validate_distribution_config(&dist).is_err(),
            "min == max must be rejected"
        );
    }

    // ---- validate_histogram_config ------------------------------------------

    /// Helper to build a minimal valid HistogramScenarioConfig.
    fn make_histogram_config() -> HistogramScenarioConfig {
        HistogramScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "http_request_duration_seconds".to_string(),
                rate: 10.0,
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
            buckets: None,
            distribution: DistributionConfig::Exponential { rate: 10.0 },
            observations_per_tick: Some(100),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        }
    }

    #[test]
    fn validate_histogram_config_accepts_valid() {
        assert!(validate_histogram_config(&make_histogram_config()).is_ok());
    }

    #[test]
    fn validate_histogram_config_rejects_zero_rate() {
        let mut config = make_histogram_config();
        config.base.rate = 0.0;
        assert!(validate_histogram_config(&config).is_err());
    }

    #[test]
    fn validate_histogram_config_rejects_zero_observations() {
        let mut config = make_histogram_config();
        config.observations_per_tick = Some(0);
        assert!(validate_histogram_config(&config).is_err());
    }

    #[test]
    fn validate_histogram_config_rejects_unsorted_buckets() {
        let mut config = make_histogram_config();
        config.buckets = Some(vec![1.0, 0.5, 2.0]);
        assert!(validate_histogram_config(&config).is_err());
    }

    #[test]
    fn validate_histogram_config_rejects_invalid_metric_name() {
        let mut config = make_histogram_config();
        config.base.name = "123-invalid".to_string();
        assert!(validate_histogram_config(&config).is_err());
    }

    /// A gnmi encoder for a histogram or summary called `name`: `path` places
    /// `key`, and `_sum` and `_count` have `paths:` entries of their own.
    #[cfg(feature = "gnmi")]
    fn gnmi_encoder_for(name: &str, key: &str) -> EncoderConfig {
        EncoderConfig::Gnmi(crate::encoder::gnmi::GnmiEncoderConfig {
            path: Some(format!("/state/{name}[{key}={{{key}}}]/{{name}}")),
            paths: std::collections::HashMap::from([
                (format!("{name}_sum"), "/state/{name}".to_string()),
                (format!("{name}_count"), "/state/{name}".to_string()),
            ]),
            ..Default::default()
        })
    }

    #[cfg(feature = "gnmi")]
    fn gnmi_target_sink() -> SinkConfig {
        SinkConfig::GnmiTarget(crate::sink::gnmi::GnmiTargetConfig::new("127.0.0.1:57400"))
    }

    #[cfg(feature = "gnmi")]
    fn gnmi_histogram_config() -> HistogramScenarioConfig {
        let mut config = make_histogram_config();
        config.encoder = gnmi_encoder_for("latency", "le");
        config.base.name = "latency".to_string();
        config.base.sink = gnmi_target_sink();
        config
    }

    #[cfg(feature = "gnmi")]
    fn gnmi_summary_config() -> SummaryScenarioConfig {
        let mut config = make_summary_config();
        config.encoder = gnmi_encoder_for("latency", "quantile");
        config.base.name = "latency".to_string();
        config.base.sink = gnmi_target_sink();
        config
    }

    #[cfg(feature = "gnmi")]
    fn gnmi_entry_cfg(encoder: &mut EncoderConfig) -> &mut crate::encoder::gnmi::GnmiEncoderConfig {
        match encoder {
            EncoderConfig::Gnmi(cfg) => cfg,
            _ => panic!("test config must use the gnmi encoder"),
        }
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn validate_histogram_config_accepts_the_gnmi_encoder() {
        validate_histogram_config(&gnmi_histogram_config())
            .expect("{le} on the bucket series and paths for _sum and _count validate");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn validate_summary_config_accepts_the_gnmi_encoder() {
        validate_summary_config(&gnmi_summary_config())
            .expect("{quantile} on the quantile series and paths for _sum and _count validate");
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::sum("latency_sum")]
    #[case::count("latency_count")]
    fn gnmi_histogram_without_a_path_for_a_derived_series_is_rejected(#[case] series: &str) {
        let mut config = gnmi_histogram_config();
        gnmi_entry_cfg(&mut config.encoder).paths.remove(series);
        let msg = err_msg(validate_histogram_config(&config));
        assert!(msg.contains(&format!("{series:?}")), "got: {msg}");
        assert!(msg.contains("{le}"), "got: {msg}");
        assert!(msg.contains("paths:"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::sum("latency_sum")]
    #[case::count("latency_count")]
    fn gnmi_summary_without_a_path_for_a_derived_series_is_rejected(#[case] series: &str) {
        let mut config = gnmi_summary_config();
        gnmi_entry_cfg(&mut config.encoder).paths.remove(series);
        let msg = err_msg(validate_summary_config(&config));
        assert!(msg.contains(&format!("{series:?}")), "got: {msg}");
        assert!(msg.contains("{quantile}"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_histogram_template_without_le_is_rejected() {
        let mut config = gnmi_histogram_config();
        gnmi_entry_cfg(&mut config.encoder).path = Some("/state/latency/{name}".to_string());
        let msg = err_msg(validate_histogram_config(&config));
        assert!(msg.contains("\"latency_bucket\""), "got: {msg}");
        assert!(msg.contains("does not reference {le}"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_summary_template_without_quantile_is_rejected() {
        let mut config = gnmi_summary_config();
        gnmi_entry_cfg(&mut config.encoder).path = Some("/state/latency/{name}".to_string());
        let msg = err_msg(validate_summary_config(&config));
        assert!(msg.contains("\"latency\""), "got: {msg}");
        assert!(msg.contains("does not reference {quantile}"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_histogram_cannot_drop_le() {
        let mut config = gnmi_histogram_config();
        gnmi_entry_cfg(&mut config.encoder)
            .drop_labels
            .push("le".to_string());
        let msg = err_msg(validate_histogram_config(&config));
        assert!(msg.contains("drop_labels lists `le`"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_summary_cannot_drop_quantile() {
        let mut config = gnmi_summary_config();
        gnmi_entry_cfg(&mut config.encoder)
            .drop_labels
            .push("quantile".to_string());
        let msg = err_msg(validate_summary_config(&config));
        assert!(msg.contains("drop_labels lists `quantile`"), "got: {msg}");
    }

    #[cfg(feature = "gnmi")]
    #[rstest::rstest]
    #[case::le("le")]
    #[case::quantile("quantile")]
    fn gnmi_metrics_entry_cannot_use_an_injected_placeholder(#[case] key: &str) {
        let config = gnmi_config(Some(&format!(
            "/interfaces/interface[name={{ifName}}]/x[k={{{key}}}]"
        )));
        let msg = err_msg(validate_gnmi_encoder(&config));
        assert!(
            msg.contains(&format!("placeholder {{{key}}}")),
            "got: {msg}"
        );
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_histogram_and_summary_need_the_gnmi_target_sink() {
        let mut histogram = gnmi_histogram_config();
        histogram.base.sink = SinkConfig::Stdout;
        let msg = err_msg(validate_histogram_config(&histogram));
        assert!(
            msg.contains("can only be used with the gnmi_target sink"),
            "got: {msg}"
        );

        let mut summary = gnmi_summary_config();
        summary.base.sink = SinkConfig::Stdout;
        let msg = err_msg(validate_summary_config(&summary));
        assert!(
            msg.contains("can only be used with the gnmi_target sink"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn gnmi_target_sink_needs_the_gnmi_encoder_on_histogram_and_summary() {
        let mut histogram = make_histogram_config();
        histogram.base.sink = gnmi_target_sink();
        let msg = err_msg(validate_histogram_config(&histogram));
        assert!(
            msg.contains("can only be used with the gnmi encoder"),
            "got: {msg}"
        );

        let mut summary = make_summary_config();
        summary.base.sink = gnmi_target_sink();
        let msg = err_msg(validate_summary_config(&summary));
        assert!(
            msg.contains("can only be used with the gnmi encoder"),
            "got: {msg}"
        );
    }

    // ---- validate_snmp_encoder -----------------------------------------------

    /// A metrics entry the snmp encoder accepts: two Counter64 columns of
    /// ifXTable, interface 1, on an snmp_agent sink.
    #[cfg(feature = "snmp")]
    fn snmp_config() -> ScenarioConfig {
        let mut config = minimal_config_with_rate(10.0);
        config.base.name = "ifHCInOctets".to_string();
        config.base.labels = Some(std::collections::HashMap::from([
            ("device".to_string(), "rtr-1".to_string()),
            ("ifIndex".to_string(), "1".to_string()),
        ]));
        config.base.sink =
            SinkConfig::SnmpAgent(crate::sink::snmp::SnmpAgentConfig::new("127.0.0.1:1161"));
        config.encoder = EncoderConfig::Snmp(crate::encoder::snmp::SnmpEncoderConfig {
            oids: std::collections::HashMap::from([
                (
                    "ifHCInOctets".to_string(),
                    "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
                ),
                (
                    "ifHCOutOctets".to_string(),
                    "1.3.6.1.2.1.31.1.1.1.10.{ifIndex}".to_string(),
                ),
            ]),
            ..Default::default()
        });
        config
    }

    #[cfg(feature = "snmp")]
    fn snmp_cfg(config: &mut ScenarioConfig) -> &mut crate::encoder::snmp::SnmpEncoderConfig {
        match &mut config.encoder {
            EncoderConfig::Snmp(cfg) => cfg,
            _ => panic!("test config must use the snmp encoder"),
        }
    }

    #[cfg(feature = "snmp")]
    fn set_oid(config: &mut ScenarioConfig, metric: &str, template: &str) {
        snmp_cfg(config)
            .oids
            .insert(metric.to_string(), template.to_string());
    }

    #[cfg(feature = "snmp")]
    fn set_column(config: &mut ScenarioConfig, label: &str, template: &str) {
        snmp_cfg(config)
            .label_columns
            .insert(label.to_string(), template.into());
    }

    /// `label` served by `template` as `ty`.
    #[cfg(feature = "snmp")]
    fn set_typed_column(
        config: &mut ScenarioConfig,
        label: &str,
        template: &str,
        ty: crate::encoder::snmp::SnmpValueType,
    ) {
        snmp_cfg(config).label_columns.insert(
            label.to_string(),
            crate::encoder::snmp::LabelColumn::Typed {
                oid: template.to_string(),
                r#type: ty,
            },
        );
    }

    /// `ifIndex` a counter of `prefix` and `cardinality`, its column
    /// `…2.1.{ifIndex}.{slot}` with `slot` 0, beside the scalar
    /// `…2.1.<literal>.0`.
    #[cfg(feature = "snmp")]
    fn counter_beside(config: &mut ScenarioConfig, prefix: &str, cardinality: u64, literal: &str) {
        set_label(config, "slot", "0");
        config.base.dynamic_labels = Some(vec![snmp_dynamic(
            "ifIndex",
            snmp_counter(Some(prefix), cardinality),
        )]);
        set_oid(
            config,
            "ifHCInOctets",
            "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{slot}",
        );
        set_oid(
            config,
            "ifHCOutOctets",
            &format!("1.3.6.1.4.1.8072.9999.2.1.{literal}.0"),
        );
    }

    /// `ifIndex` 9 outside a counter spike of `0` to `3`, its column
    /// `…2.1.{ifIndex}.{slot}` with `slot` 0, beside the scalar
    /// `…2.1.<literal>.0`.
    #[cfg(feature = "snmp")]
    fn spike_beside(config: &mut ScenarioConfig, literal: &str) {
        set_label(config, "slot", "0");
        set_label(config, "ifIndex", "9");
        config.base.cardinality_spikes = Some(vec![snmp_spike(
            "ifIndex",
            crate::config::SpikeStrategy::Counter,
            Some(""),
        )]);
        set_oid(
            config,
            "ifHCInOctets",
            "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{slot}",
        );
        set_oid(
            config,
            "ifHCOutOctets",
            &format!("1.3.6.1.4.1.8072.9999.2.1.{literal}.0"),
        );
    }

    #[cfg(feature = "snmp")]
    fn set_label(config: &mut ScenarioConfig, key: &str, value: &str) {
        config
            .base
            .labels
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), value.to_string());
    }

    #[cfg(feature = "snmp")]
    fn snmp_spike(
        label: &str,
        strategy: crate::config::SpikeStrategy,
        prefix: Option<&str>,
    ) -> CardinalitySpikeConfig {
        CardinalitySpikeConfig {
            label: label.to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 4,
            strategy,
            prefix: prefix.map(str::to_string),
            seed: None,
        }
    }

    #[cfg(feature = "snmp")]
    fn snmp_dynamic(key: &str, strategy: DynamicLabelStrategy) -> DynamicLabelConfig {
        DynamicLabelConfig {
            key: key.to_string(),
            strategy,
        }
    }

    #[cfg(feature = "snmp")]
    fn snmp_counter(prefix: Option<&str>, cardinality: u64) -> DynamicLabelStrategy {
        DynamicLabelStrategy::Counter {
            prefix: prefix.map(str::to_string),
            cardinality,
        }
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn snmp_baseline_is_accepted() {
        validate_snmp_encoder(&snmp_config()).expect("the baseline validates");
        validate_config(&snmp_config()).expect("and passes the whole metrics validation");
    }

    /// Each case adds exactly one defect to [`snmp_config`], which validates.
    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest::rstest]
    // 1. Pairing, both directions.
    #[case::r1_encoder_without_agent_sink(|c: &mut ScenarioConfig| c.base.sink = SinkConfig::Stdout,
        "the snmp encoder can only be used with the snmp_agent sink")]
    #[case::r1_agent_sink_without_encoder(|c: &mut ScenarioConfig| c.encoder = EncoderConfig::PrometheusText { precision: None },
        "the snmp_agent sink can only be used with the snmp encoder")]
    // 2. Column present.
    #[case::r2_no_oids_entry(|c: &mut ScenarioConfig| { snmp_cfg(c).oids.remove("ifHCInOctets"); },
        "no oids: entry for metric \"ifHCInOctets\"")]
    // 3. Placeholders are labels.
    #[case::r3_unknown_placeholder(|c: &mut ScenarioConfig| set_oid(c, "ifHCInOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifindex}"),
        "uses placeholder {ifindex}, which is not a label of this entry")]
    #[case::r3_spike_only_placeholder(|c: &mut ScenarioConfig| {
            set_oid(c, "ifHCInOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}.{slot}");
            c.base.cardinality_spikes = Some(vec![snmp_spike("slot", crate::config::SpikeStrategy::Counter, Some(""))]);
        },
        "uses placeholder {slot}, a cardinality_spikes label")]
    // 4. Index values are arcs: static, dynamic values, dynamic counter, spike counter, random spike.
    #[case::r4_static_value(|c: &mut ScenarioConfig| set_label(c, "ifIndex", "Gi0/0/0"),
        "snmp OID template for metric \"ifHCInOctets\" renders label `ifIndex` as an OID arc (an unsigned 32-bit integer in decimal, without leading zeros), but its value \"Gi0/0/0\"")]
    #[case::r4_static_leading_zero(|c: &mut ScenarioConfig| set_label(c, "ifIndex", "01"),
        "but its value \"01\"")]
    #[case::r4_dynamic_values(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex",
            DynamicLabelStrategy::ValuesList { values: vec!["1".to_string(), "eth2".to_string()] })]),
        "but its dynamic value \"eth2\"")]
    #[case::r4_dynamic_counter_default_prefix(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(None, 4))]),
        "but its counter values run from \"ifIndex_0\" to \"ifIndex_3\"")]
    #[case::r4_dynamic_counter_past_u32(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 4_294_967_297))]),
        "run from \"0\" to \"4294967296\"")]
    #[case::r4_spike_counter_default_prefix(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![
            snmp_spike("ifIndex", crate::config::SpikeStrategy::Counter, None)]),
        "but its counter values run from \"ifIndex_0\" to \"ifIndex_3\"")]
    #[case::r4_spike_random(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![
            snmp_spike("ifIndex", crate::config::SpikeStrategy::Random, Some(""))]),
        "a random cardinality spike sets it to values that are not integers")]
    #[case::r4_spike_outside_value(|c: &mut ScenarioConfig| {
            set_label(c, "ifIndex", "x");
            c.base.cardinality_spikes = Some(vec![snmp_spike("ifIndex", crate::config::SpikeStrategy::Counter, Some(""))]);
        },
        "but its value \"x\"")]
    // 5. Coverage: a static, a dynamic and a spike label.
    #[case::r5_static_label(|c: &mut ScenarioConfig| set_label(c, "job", "snmp"),
        "does not reference label `job`, so series that differ only in it would share one OID")]
    #[case::r5_dynamic_label(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("pod", snmp_counter(None, 2))]),
        "does not reference label `pod`")]
    #[case::r5_spike_label(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![
            snmp_spike("slot", crate::config::SpikeStrategy::Counter, None)]),
        "does not reference label `slot`")]
    // 6. No collision: a forgotten placeholder, and a copied template.
    #[case::r6_forgotten_placeholder(|c: &mut ScenarioConfig| { set_label(c, "ifIndex", "0");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.0"); },
        "metrics \"ifHCInOctets\" and \"ifHCOutOctets\" both render 1.3.6.1.2.1.31.1.1.1.6.0 for this entry")]
    #[case::r6_copied_template(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"),
        "both render 1.3.6.1.2.1.31.1.1.1.6.1 for this entry when ifIndex is 1")]
    // 6, arc-wise: a dynamic value, a counter and a shared static value each meet the other arc.
    #[case::r6_dynamic_value_meets_a_literal(|c: &mut ScenarioConfig| { c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex",
            DynamicLabelStrategy::ValuesList { values: vec!["0".to_string(), "1".to_string()] })]);
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.0"); },
        "both render 1.3.6.1.2.1.31.1.1.1.6.0 for this entry when ifIndex is 0")]
    #[case::r6_counter_meets_any_literal(|c: &mut ScenarioConfig| { set_label(c, "slot", "0");
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 48))]);
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{slot}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.2.1.7.0"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.7.0 for this entry when ifIndex is 7 and slot is 0")]
    #[case::r6_placeholders_share_a_value(|c: &mut ScenarioConfig| { set_label(c, "slot", "1");
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{slot}"); },
        "both render 1.3.6.1.2.1.31.1.1.1.6.1 for this entry when ifIndex is 1 and slot is 1")]
    #[case::r6_counters_kept_whole_may_render(|c: &mut ScenarioConfig| {
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 30)),
                snmp_dynamic("slot", snmp_counter(Some("1"), 1)), snmp_dynamic("port", snmp_counter(Some("2"), 1))]);
            snmp_cfg(c).drop_labels.extend(["slot".to_string(), "port".to_string()]);
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{ifIndex}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.2.1.{slot}.{port}"); },
        "snmp OID templates of metrics \"ifHCInOctets\" and \"ifHCOutOctets\" may render 1.3.6.1.4.1.8072.9999.2.1.10.20 for this entry when ifIndex is 10 and slot is 10 and port is 20; give each its own column")]
    #[case::r6_a_label_renders_apart_in_each_template(|c: &mut ScenarioConfig| { set_label(c, "slot", "2");
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex",
                DynamicLabelStrategy::ValuesList { values: vec!["1".to_string(), "2".to_string()] })]);
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.2.1.1.{ifIndex}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{slot}"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.1.2 for this entry when ifIndex is 2 in the first template and 1 in the second and slot is 2")]
    // The literal part parses as an OID, and nothing renders under the system group.
    #[case::r2_oid_of_one_arc(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.{ifIndex}"),
        "oids.ifHCOutOctets: OID template \"1.{ifIndex}\": OID \"1\" has one arc")]
    #[case::r2_oid_not_encodable(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "3.40.{ifIndex}"),
        "oids.ifHCOutOctets: OID template \"3.40.{ifIndex}\": OID \"3.40\" cannot be encoded")]
    #[case::r2_scalar_without_zero(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.1.1"),
        "oids.ifHCOutOctets: OID template \"1.3.6.1.4.1.8072.9999.1.1\" has no placeholder, so it names a scalar, and a scalar's instance ends in .0")]
    #[case::system_group_scalar(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.1.5.0"),
        "oids.ifHCOutOctets: 1.3.6.1.2.1.1.5.0 can render OIDs under 1.3.6.1.2.1.1, the system group")]
    #[case::system_group_reached_by_an_index(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.{ifIndex}"),
        "oids.ifHCOutOctets: 1.3.6.1.2.1.{ifIndex} can render OIDs under 1.3.6.1.2.1.1")]
    // Label columns: placeholders every oids: template uses, parsed, not under the system group.
    #[case::r3_label_column_placeholder_no_oids_template_uses(|c: &mut ScenarioConfig| { set_label(c, "slot", "2");
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{slot}"); },
        "label_columns.ifName: 1.3.6.1.2.1.31.1.1.1.1.{slot} uses placeholder {slot}, which the oids: template of metric \"ifHCInOctets\" does not use")]
    #[case::r3_label_column_placeholder_one_oids_template_lacks(|c: &mut ScenarioConfig| { set_label(c, "slot", "2");
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{slot}");
            set_column(c, "ifName", "1.3.6.1.4.1.8072.9999.2.2.{ifIndex}.{slot}"); },
        "which the oids: template of metric \"ifHCOutOctets\" does not use")]
    #[case::r2_label_column_not_a_template(|c: &mut ScenarioConfig| set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1"),
        "label_columns.ifName: OID template \"1.3.6.1.2.1.31.1.1.1.1\" has no placeholder")]
    #[case::r4_label_column_integer_index(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0/0/0"); set_label(c, "ifAlias", "uplink");
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.3.2.{ifName:str}");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string());
            set_column(c, "ifAlias", "1.3.6.1.4.1.8072.9999.3.3.{ifName}"); },
        "snmp encoder label_columns.ifAlias renders label `ifName` as an OID arc (an unsigned 32-bit integer in decimal, without leading zeros), but its value \"Gi0/0/0\"")]
    #[case::system_group_label_column(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            set_column(c, "ifName", "1.3.6.1.2.1.1.9.1.3.{ifIndex}"); },
        "label_columns.ifName: 1.3.6.1.2.1.1.9.1.3.{ifIndex} can render OIDs under 1.3.6.1.2.1.1")]
    // 3, a label column's label is a static label of the entry.
    #[case::r3_label_column_of_a_dynamic_label(|c: &mut ScenarioConfig| {
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifName",
                DynamicLabelStrategy::ValuesList { values: vec!["Gi0".to_string(), "Gi9".to_string()] })]);
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"); },
        "snmp encoder label_columns.ifName: label `ifName` is a dynamic_labels label; series that differ only in a label-column label share the metric's OID, so its label must be a static label of the entry")]
    #[case::r3_label_column_of_a_dynamic_label_over_a_static_one(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifName", snmp_counter(None, 2))]);
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"); },
        "label `ifName` is a dynamic_labels label")]
    #[case::r3_label_column_of_a_spike_label(|c: &mut ScenarioConfig| {
            c.base.cardinality_spikes = Some(vec![snmp_spike("ifName", crate::config::SpikeStrategy::Random, None)]);
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"); },
        "snmp encoder label_columns.ifName: label `ifName` is a cardinality_spikes label")]
    #[case::r3_label_column_of_a_static_label_under_a_spike(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            c.base.cardinality_spikes = Some(vec![snmp_spike("ifName", crate::config::SpikeStrategy::Counter, None)]);
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"); },
        "label `ifName` is a cardinality_spikes label")]
    #[case::r3_label_column_of_a_label_the_entry_lacks(|c: &mut ScenarioConfig| set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"),
        "snmp encoder label_columns.ifName: label `ifName` is not a label of this entry")]
    // 3, a typed column: its type serves the static value; never the enum.
    #[case::r3_typed_column_of_text_that_is_not_a_number(|c: &mut ScenarioConfig| { set_label(c, "ifType", "GigabitEthernet");
            set_typed_column(c, "ifType", "1.3.6.1.2.1.2.2.1.3.{ifIndex}", crate::encoder::snmp::SnmpValueType::Integer); },
        "snmp encoder label_columns.ifType: label `ifType` has value \"GigabitEthernet\", which its integer column cannot serve: it is not a number")]
    #[case::r3_typed_column_of_a_number_outside_its_type(|c: &mut ScenarioConfig| { set_label(c, "ifType", "3000000000");
            set_typed_column(c, "ifType", "1.3.6.1.2.1.2.2.1.3.{ifIndex}", crate::encoder::snmp::SnmpValueType::Integer); },
        "snmp encoder label_columns.ifType: label `ifType` has value \"3000000000\", which its integer column cannot serve: value 3000000000 is outside the integer range")]
    #[case::r3_typed_gauge_column_of_a_negative_number(|c: &mut ScenarioConfig| { set_label(c, "speed", "-1");
            set_typed_column(c, "speed", "1.3.6.1.2.1.2.2.1.5.{ifIndex}", crate::encoder::snmp::SnmpValueType::Gauge32); },
        "label `speed` has value \"-1\", which its gauge32 column cannot serve: value -1 is outside the gauge32 range")]
    #[case::r3_typed_column_of_a_fractional_number(|c: &mut ScenarioConfig| { set_label(c, "ifType", "6.5");
            set_typed_column(c, "ifType", "1.3.6.1.2.1.2.2.1.3.{ifIndex}", crate::encoder::snmp::SnmpValueType::Integer); },
        "snmp encoder label_columns.ifType: label `ifType` has value \"6.5\", which its integer column cannot serve: it has a fractional part")]
    #[case::r2_enum_label_column(|c: &mut ScenarioConfig| { set_label(c, "ifType", "6");
            set_typed_column(c, "ifType", "1.3.6.1.2.1.2.2.1.3.{ifIndex}",
                crate::encoder::snmp::SnmpValueType::Enum(std::collections::BTreeMap::from([(6, "ethernetCsmacd".to_string())]))); },
        "snmp encoder label_columns.ifType: the enum type is refused for a label column")]
    // 3, a string placeholder is named as written.
    #[case::r3_spike_only_string_placeholder(|c: &mut ScenarioConfig| {
            c.base.cardinality_spikes = Some(vec![snmp_spike("ifName", crate::config::SpikeStrategy::Counter, None)]);
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifIndex}.{ifName:str}"); },
        "uses placeholder {ifName:str}, a cardinality_spikes label")]
    #[case::r3_label_column_string_placeholder_no_oids_template_uses(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            snmp_cfg(c).drop_labels.push("ifName".to_string());
            set_column(c, "ifAlias", "1.3.6.1.4.1.8072.9999.3.4.{ifName:str}"); },
        "label_columns.ifAlias: 1.3.6.1.4.1.8072.9999.3.4.{ifName:str} uses placeholder {ifName:str}, which the oids: template")]
    // 6, label columns: against the metric, each other and another metric.
    #[case::r6_label_column_on_the_metrics_column(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"); },
        "snmp OID templates of metric \"ifHCInOctets\" and label_columns.ifName both render 1.3.6.1.2.1.31.1.1.1.6.1 for this entry when ifIndex is 1")]
    #[case::r6_two_label_columns_on_one_column(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0"); set_label(c, "ifAlias", "uplink");
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}");
            set_column(c, "ifAlias", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"); },
        "snmp OID templates of label_columns.ifAlias and label_columns.ifName both render 1.3.6.1.2.1.31.1.1.1.1.1 for this entry when ifIndex is 1")]
    #[case::r6_label_column_on_another_metrics_column(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.10.{ifIndex}"); },
        "snmp OID templates of label_columns.ifName and metric \"ifHCOutOctets\" both render 1.3.6.1.2.1.31.1.1.1.10.1 for this entry when ifIndex is 1")]
    #[case::r6_label_column_nested_with_a_string_index(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string());
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.3.2.{ifName:str}");
            set_column(c, "ifAlias", "1.3.6.1.4.1.8072.9999.3.{ifName:str}"); set_label(c, "ifAlias", "uplink"); },
        "snmp OID templates of metric \"ifHCInOctets\" and label_columns.ifAlias can render the same OID: 1.3.6.1.4.1.8072.9999.3.1.{ifName:str} and 1.3.6.1.4.1.8072.9999.3.{ifName:str}")]
    // 6, the counter arm: prefix followed by 0 to cardinality - 1.
    #[case::r6_counter_reaches_a_literal_below_its_cardinality(|c: &mut ScenarioConfig| { counter_beside(c, "", 4, "3"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.3.0 for this entry when ifIndex is 3 and slot is 0")]
    #[case::r6_digit_prefix_counter_reaches_its_values(|c: &mut ScenarioConfig| { counter_beside(c, "1", 4, "12"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.12.0 for this entry when ifIndex is 12 and slot is 0")]
    #[case::r6_counters_share_a_value(|c: &mut ScenarioConfig| {
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 20)),
                snmp_dynamic("slot", snmp_counter(Some("1"), 4))]);
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{slot}"); },
        "both render 1.3.6.1.2.1.31.1.1.1.6.10 for this entry when ifIndex is 10 and slot is 10")]
    #[case::r6_spike_counter_reaches_a_literal_in_its_window(|c: &mut ScenarioConfig| { spike_beside(c, "2"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.2.0 for this entry when ifIndex is 2 and slot is 0")]
    #[case::r6_spike_counter_reaches_its_outside_value(|c: &mut ScenarioConfig| { spike_beside(c, "9"); },
        "both render 1.3.6.1.4.1.8072.9999.2.1.9.0 for this entry when ifIndex is 9 and slot is 0")]
    // 6, a string index: objects equal or nested.
    #[case::r6_string_index_column_inside_another(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0/0/0");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string());
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.{ifName:str}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}"); },
        "metrics \"ifHCInOctets\" and \"ifHCOutOctets\" can render the same OID: 1.3.6.1.4.1.8072.9999.3.{ifName:str} and 1.3.6.1.4.1.8072.9999.3.1.{ifName:str}")]
    #[case::r6_string_index_beside_an_integer_column_on_one_object(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0/0/0");
            snmp_cfg(c).drop_labels.push("ifName".to_string());
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{ifName:str}"); },
        "metrics \"ifHCInOctets\" and \"ifHCOutOctets\" can render the same OID")]
    fn snmp_validation_refuses(#[case] defect: fn(&mut ScenarioConfig), #[case] fragment: &str) {
        // Guard: without the one defect the config is accepted.
        validate_snmp_encoder(&snmp_config()).expect("the baseline validates");
        let mut config = snmp_config();
        defect(&mut config);
        let msg = err_msg(validate_snmp_encoder(&config));
        assert!(msg.contains(fragment), "expected {fragment:?} in: {msg}");
    }

    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::counter_with_empty_prefix(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 48))]))]
    #[case::counter_with_digit_prefix(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some("1"), 10))]))]
    #[case::counter_to_u32_max(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 4_294_967_296))]))]
    #[case::values_all_integers(|c: &mut ScenarioConfig| c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex",
            DynamicLabelStrategy::ValuesList { values: vec!["1".to_string(), "4294967295".to_string()] })]))]
    #[case::spike_counter_with_empty_prefix(|c: &mut ScenarioConfig| c.base.cardinality_spikes = Some(vec![
            snmp_spike("ifIndex", crate::config::SpikeStrategy::Counter, Some(""))]))]
    #[case::dropped_label(|c: &mut ScenarioConfig| { set_label(c, "job", "snmp"); snmp_cfg(c).drop_labels.push("job".to_string()); })]
    #[case::dropped_spike_label(|c: &mut ScenarioConfig| { c.base.cardinality_spikes = Some(vec![
            snmp_spike("slot", crate::config::SpikeStrategy::Random, None)]); snmp_cfg(c).drop_labels.push("slot".to_string()); })]
    #[case::other_agent_label(|c: &mut ScenarioConfig| { set_label(c, "host", "rtr-1"); snmp_cfg(c).agent_label = "host".to_string();
            snmp_cfg(c).drop_labels.push("device".to_string()); })]
    #[case::scalar_template(|c: &mut ScenarioConfig| { set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.1.1.0");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string()); })]
    #[case::two_index_arcs(|c: &mut ScenarioConfig| { set_label(c, "slot", "2");
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.2.1.{slot}.{ifIndex}"); })]
    #[case::above_system_group_short(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.{x}"))]
    #[case::scalar_beside_system_group(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.0"))]
    #[case::r6_dynamic_values_miss_the_literal(|c: &mut ScenarioConfig| { c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex",
            DynamicLabelStrategy::ValuesList { values: vec!["1".to_string(), "2".to_string()] })]);
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.0"); })]
    #[case::r6_placeholders_with_disjoint_values(|c: &mut ScenarioConfig| { set_label(c, "slot", "2");
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{slot}"); })]
    #[case::r6_templates_of_different_lengths(|c: &mut ScenarioConfig| set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.1.0"))]
    // Label columns: a label named there needs no drop_labels entry.
    #[case::label_column_needs_no_drop_labels(|c: &mut ScenarioConfig| { set_label(c, "ifName", "GigabitEthernet0/0/0");
            set_label(c, "ifAlias", "uplink");
            set_column(c, "ifName", "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}");
            set_column(c, "ifAlias", "1.3.6.1.2.1.31.1.1.1.18.{ifIndex}"); })]
    // Typed label columns whose types serve the static values, the index's own among them.
    #[case::typed_label_columns(|c: &mut ScenarioConfig| { set_label(c, "ifType", "6"); set_label(c, "ifDescr", "Gi1/0/1");
            set_label(c, "speed", "1000");
            set_typed_column(c, "ifIndex", "1.3.6.1.2.1.2.2.1.1.{ifIndex}", crate::encoder::snmp::SnmpValueType::Integer);
            set_typed_column(c, "ifType", "1.3.6.1.2.1.2.2.1.3.{ifIndex}", crate::encoder::snmp::SnmpValueType::Integer);
            set_typed_column(c, "ifDescr", "1.3.6.1.2.1.2.2.1.2.{ifIndex}", crate::encoder::snmp::SnmpValueType::String);
            set_typed_column(c, "speed", "1.3.6.1.2.1.2.2.1.5.{ifIndex}", crate::encoder::snmp::SnmpValueType::Gauge32); })]
    #[case::scalar_label_column_beside_scalars(|c: &mut ScenarioConfig| { set_label(c, "site", "lab-1");
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.1.1.0");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.1.2.0");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string());
            set_column(c, "site", "1.3.6.1.4.1.8072.9999.1.3.0"); })]
    // 4 skips a string index: any static value, a dynamic list, a random spike.
    #[case::string_index_takes_any_static_value(|c: &mut ScenarioConfig| { set_label(c, "ifName", "GigabitEthernet0/0/0");
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifIndex}.{ifName:str}"); })]
    #[case::string_index_takes_dynamic_values(|c: &mut ScenarioConfig| { c.base.dynamic_labels = Some(vec![snmp_dynamic("ifName",
            DynamicLabelStrategy::ValuesList { values: vec!["Gi0/0/0".to_string(), "Gi0/0/1".to_string()] })]);
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifIndex}.{ifName:str}"); })]
    #[case::string_index_takes_a_random_spike(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0/0/0");
            c.base.cardinality_spikes = Some(vec![snmp_spike("ifName", crate::config::SpikeStrategy::Random, None)]);
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifIndex}.{ifName:str}"); })]
    // 6, the counter arm: a literal at or past the cardinality, a digit prefix's other values.
    #[case::r6_counter_misses_a_literal_at_its_cardinality(|c: &mut ScenarioConfig| { counter_beside(c, "", 4, "4"); })]
    #[case::r6_counter_misses_a_literal_past_its_cardinality(|c: &mut ScenarioConfig| { counter_beside(c, "", 4, "1000"); })]
    #[case::r6_digit_prefix_counter_misses_its_bare_numbers(|c: &mut ScenarioConfig| { counter_beside(c, "1", 4, "3"); })]
    #[case::r6_spike_counter_misses_a_literal_outside_both(|c: &mut ScenarioConfig| { spike_beside(c, "5"); })]
    #[case::r6_counters_with_disjoint_values(|c: &mut ScenarioConfig| {
            c.base.dynamic_labels = Some(vec![snmp_dynamic("ifIndex", snmp_counter(Some(""), 10)),
                snmp_dynamic("slot", snmp_counter(Some("1"), 4))]);
            snmp_cfg(c).drop_labels.push("slot".to_string());
            set_oid(c, "ifHCOutOctets", "1.3.6.1.2.1.31.1.1.1.6.{slot}"); })]
    // 6, a string index: columns whose objects differ in their last literal arc.
    #[case::r6_string_index_columns_beside_each_other(|c: &mut ScenarioConfig| { set_label(c, "ifName", "Gi0/0/0");
            snmp_cfg(c).drop_labels.push("ifIndex".to_string());
            set_oid(c, "ifHCInOctets", "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
            set_oid(c, "ifHCOutOctets", "1.3.6.1.4.1.8072.9999.3.2.{ifName:str}"); })]
    fn snmp_validation_accepts(#[case] change: fn(&mut ScenarioConfig)) {
        let mut config = snmp_config();
        change(&mut config);
        validate_snmp_encoder(&config).expect("this variant validates");
    }

    /// Rule 6 holds one value for a label used twice in one template:
    /// `{ifIndex}.{ifIndex}` renders `0.0` and `1.1`, never `1.0`, and the
    /// refusal names the label once.
    #[cfg(feature = "snmp")]
    #[test]
    fn a_label_repeated_in_a_template_is_named_once() {
        let mut config = snmp_config();
        config.base.dynamic_labels = Some(vec![snmp_dynamic(
            "ifIndex",
            DynamicLabelStrategy::ValuesList {
                values: vec!["0".to_string(), "1".to_string()],
            },
        )]);
        set_oid(
            &mut config,
            "ifHCInOctets",
            "1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{ifIndex}",
        );
        set_oid(
            &mut config,
            "ifHCOutOctets",
            "1.3.6.1.4.1.8072.9999.2.1.3.0",
        );
        validate_snmp_encoder(&config).expect("guard: ifIndex never renders 3");
        set_oid(
            &mut config,
            "ifHCOutOctets",
            "1.3.6.1.4.1.8072.9999.2.1.1.0",
        );
        validate_snmp_encoder(&config).expect("ifIndex renders 1.1 or 0.0, never 1.0");
        set_oid(
            &mut config,
            "ifHCOutOctets",
            "1.3.6.1.4.1.8072.9999.2.1.0.0",
        );
        let msg = err_msg(validate_snmp_encoder(&config));
        assert!(
            msg.contains(
                "both render 1.3.6.1.4.1.8072.9999.2.1.0.0 for this entry when ifIndex is 0;"
            ),
            "got: {msg}"
        );
        assert_eq!(msg.matches("ifIndex is").count(), 1, "named once: {msg}");
    }

    /// A label a collision names, its arc, and its arc in the second template
    /// when that differs.
    #[cfg(feature = "snmp")]
    type NamedArcs<'a> = (&'a str, u32, Option<u32>);

    /// [`snmp_collision`] of `own` and `other` under [`snmp_config`] with
    /// each of `values` (key, comma-separated values) a static label (one
    /// value) or a `values` dynamic label, and each of `counters` (key, prefix, cardinality) a `counter`
    /// dynamic label.
    #[cfg(feature = "snmp")]
    fn collision_of(
        own: &str,
        other: &str,
        values: &[(&str, &str)],
        counters: &[(&str, &str, u64)],
    ) -> Option<SnmpCollision> {
        use crate::encoder::snmp::oid::OidTemplate;
        let mut config = snmp_config();
        let mut dynamic = Vec::new();
        for (key, values) in values {
            let values: Vec<String> = values.split(',').map(str::to_string).collect();
            match &values[..] {
                [value] => set_label(&mut config, key, value),
                _ => dynamic.push(snmp_dynamic(
                    key,
                    DynamicLabelStrategy::ValuesList { values },
                )),
            }
        }
        for (key, prefix, cardinality) in counters {
            dynamic.push(snmp_dynamic(key, snmp_counter(Some(prefix), *cardinality)));
        }
        if !dynamic.is_empty() {
            config.base.dynamic_labels = Some(dynamic);
        }
        let parse = |text: &str| OidTemplate::parse(text).expect("a template");
        snmp_collision(&parse(own), &parse(other), &config.base)
    }

    /// Rule 6 holds one value per label across the positions of its
    /// template, and a label of the other template is a value of its own.
    /// Each case without a collision has a sibling, one value or arc apart,
    /// that collides. `repeated_label_against_*_apart` were refused before
    /// the rule held one value per label: position by position each arc
    /// could be equal.
    ///
    /// The two `counters_*` cases are the conservative one, `ArcSet::narrow`:
    /// "A counter that meets a counter of `other` is kept whole, unchanged:
    /// the one conservative case, which can only keep an arc the exact
    /// intersection would not, so rule 6 may refuse two such templates but
    /// never misses a collision." The first collides (`0.0`); the second is
    /// refused though `{a}.{a}` never renders `10.20`. Both say the templates
    /// "may render" the OID; every exact collision says they "both render" it.
    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::repeated_label_against_a_scalar_apart(  "1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.1.0", &[("a", "0,1")], &[], None)]
    #[case::repeated_label_against_a_scalar_it_renders("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.0.0", &[("a", "0,1")], &[],
        Some(("both render", "1.3.6.1.4.1.8072.9999.0.0", &[("a", 0, None)][..])))]
    #[case::repeated_label_against_a_literal_and_a_label_apart("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.1.{b}",
        &[("a", "1,2"), ("b", "2")], &[], None)]
    #[case::repeated_label_against_a_literal_and_a_label_that_meet("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.1.{b}",
        &[("a", "1,2"), ("b", "1,2")], &[],
        Some(("both render", "1.3.6.1.4.1.8072.9999.1.1", &[("a", 1, None), ("b", 1, None)][..])))]
    #[case::repeated_label_against_two_labels_apart("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.{b}.{c}",
        &[("a", "1,2"), ("b", "1"), ("c", "2")], &[], None)]
    #[case::repeated_label_against_two_labels_that_meet("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.{b}.{c}",
        &[("a", "1,2"), ("b", "1"), ("c", "1,2")], &[],
        Some(("both render", "1.3.6.1.4.1.8072.9999.1.1", &[("a", 1, None), ("b", 1, None), ("c", 1, None)][..])))]
    #[case::repeated_counter_against_a_literal_and_a_label_apart("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.7.{b}",
        &[("b", "8")], &[("a", "", 48)], None)]
    #[case::repeated_counter_against_a_literal_and_a_label_that_meet("1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.7.{b}",
        &[("b", "7,8")], &[("a", "", 48)],
        Some(("both render", "1.3.6.1.4.1.8072.9999.7.7", &[("a", 7, None), ("b", 7, None)][..])))]
    #[case::counters_meet(                        "1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.{b}.{c}", &[],
        &[("a", "", 3), ("b", "", 3), ("c", "", 3)],
        Some(("may render", "1.3.6.1.4.1.8072.9999.0.0", &[("a", 0, None), ("b", 0, None), ("c", 0, None)][..])))]
    #[case::counters_kept_whole_refuse(           "1.3.6.1.4.1.8072.9999.{a}.{a}", "1.3.6.1.4.1.8072.9999.{b}.{c}", &[],
        &[("a", "", 30), ("b", "1", 1), ("c", "2", 1)],
        Some(("may render", "1.3.6.1.4.1.8072.9999.10.20", &[("a", 10, None), ("b", 10, None), ("c", 20, None)][..])))]
    #[case::a_label_in_each_template_apart(      "1.3.6.1.4.1.8072.9999.1.{a}", "1.3.6.1.4.1.8072.9999.{a}.{b}",
        &[("a", "1"), ("b", "2")], &[], None)]
    #[case::a_label_in_each_template_meets(      "1.3.6.1.4.1.8072.9999.1.{a}", "1.3.6.1.4.1.8072.9999.{a}.{b}",
        &[("a", "1,2"), ("b", "2")], &[],
        Some(("both render", "1.3.6.1.4.1.8072.9999.1.2", &[("a", 2, Some(1)), ("b", 2, None)][..])))]
    fn rule_6_holds_one_value_per_label(
        #[case] own: &str,
        #[case] other: &str,
        #[case] values: &[(&str, &str)],
        #[case] counters: &[(&str, &str, u64)],
        #[case] expected: Option<(&str, &str, &[NamedArcs<'_>])>,
    ) {
        let found = collision_of(own, other, values, counters).map(|collision| match collision {
            SnmpCollision::Oid { oid, values, exact } => (renders(exact), oid, values),
            SnmpCollision::Nested => panic!("no string index here"),
        });
        let expected = expected.map(|(verb, oid, values)| {
            let values = values
                .iter()
                .map(|(label, arc, second)| (label.to_string(), *arc, *second))
                .collect::<Vec<_>>();
            (verb, oid.to_string(), values)
        });
        assert_eq!(found, expected);
    }

    /// `narrow` keeps a listed arc the other set contains, turns a counter
    /// into the listed arcs it holds, keeps a counter that meets another
    /// whole, and drops one that meets none.
    #[cfg(feature = "snmp")]
    #[test]
    fn narrow_is_exact_but_for_two_counters_that_meet() {
        let listed = |arcs: &[u32]| ArcSet {
            listed: arcs.iter().copied().collect(),
            counters: Vec::new(),
        };
        let counter = |prefix: &str, cardinality: u64| ArcSet {
            listed: Default::default(),
            counters: vec![(prefix.to_string(), cardinality)],
        };
        assert_eq!(
            listed(&[1, 5, 9]).narrow(&listed(&[5, 9, 11])),
            listed(&[5, 9])
        );
        assert_eq!(listed(&[1, 5, 12]).narrow(&counter("1", 3)), listed(&[12]));
        assert_eq!(counter("", 10).narrow(&listed(&[3, 10, 42])), listed(&[3]));
        assert_eq!(counter("", 30).narrow(&counter("2", 1)), counter("", 30));
        assert_eq!(counter("", 10).narrow(&counter("2", 1)), listed(&[]));
        assert!(listed(&[]).is_empty() && counter("", 0).is_empty());
        assert!(!counter("", 1).is_empty() && !listed(&[0]).is_empty());
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn validate_config_runs_the_snmp_rules() {
        let mut config = snmp_config();
        set_label(&mut config, "ifIndex", "eth0");
        let msg = err_msg(validate_config(&config));
        assert!(
            msg.contains("renders label `ifIndex` as an OID arc"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn other_encoders_are_not_snmp_validated() {
        let config = minimal_config_with_rate(10.0);
        assert!(matches!(
            config.encoder,
            EncoderConfig::PrometheusText { .. }
        ));
        validate_snmp_encoder(&config).expect("not an snmp entry");
    }

    #[cfg(feature = "snmp")]
    fn snmp_log_config(
        encoder: EncoderConfig,
        sink: SinkConfig,
    ) -> crate::config::LogScenarioConfig {
        let mut base = minimal_config_with_rate(10.0).base;
        base.sink = sink;
        crate::config::LogScenarioConfig {
            base,
            generator: crate::generator::LogGeneratorConfig::Template {
                templates: vec![crate::generator::TemplateConfig {
                    message: "test".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: None,
            },
            encoder,
        }
    }

    /// Histogram, summary and logs entries refuse the encoder and the sink,
    /// each named; the same entries validate without them.
    #[cfg(feature = "snmp")]
    #[test]
    fn histogram_summary_and_logs_refuse_snmp() {
        let snmp_encoder = || snmp_config().encoder;
        let agent = || snmp_config().base.sink;

        validate_histogram_config(&make_histogram_config())
            .expect("guard: the histogram validates");
        let mut histogram = make_histogram_config();
        histogram.encoder = snmp_encoder();
        let msg = err_msg(validate_histogram_config(&histogram));
        assert!(
            msg.contains("snmp encoder carries metrics entries only, not histogram entries"),
            "got: {msg}"
        );
        let mut histogram = make_histogram_config();
        histogram.base.sink = agent();
        let msg = err_msg(validate_histogram_config(&histogram));
        assert!(
            msg.contains("snmp_agent sink carries metrics entries only, not histogram entries"),
            "got: {msg}"
        );

        validate_summary_config(&make_summary_config()).expect("guard: the summary validates");
        let mut summary = make_summary_config();
        summary.encoder = snmp_encoder();
        let msg = err_msg(validate_summary_config(&summary));
        assert!(msg.contains("not summary entries"), "got: {msg}");
        let mut summary = make_summary_config();
        summary.base.sink = agent();
        let msg = err_msg(validate_summary_config(&summary));
        assert!(
            msg.contains("snmp_agent sink carries metrics entries only, not summary entries"),
            "got: {msg}"
        );

        let json = EncoderConfig::JsonLines { precision: None };
        validate_log_config(&snmp_log_config(json.clone(), SinkConfig::Stdout))
            .expect("guard: the log entry validates");
        let msg = err_msg(validate_log_config(&snmp_log_config(
            snmp_encoder(),
            SinkConfig::Stdout,
        )));
        assert!(
            msg.contains("snmp encoder carries metrics entries only, not logs entries"),
            "got: {msg}"
        );
        let msg = err_msg(validate_log_config(&snmp_log_config(json, agent())));
        assert!(
            msg.contains("snmp_agent sink carries metrics entries only, not logs entries"),
            "got: {msg}"
        );
    }

    // ---- validate_summary_config --------------------------------------------

    /// Helper to build a minimal valid SummaryScenarioConfig.
    fn make_summary_config() -> SummaryScenarioConfig {
        SummaryScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: "rpc_duration_seconds".to_string(),
                rate: 10.0,
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
            quantiles: None,
            distribution: DistributionConfig::Normal {
                mean: 0.1,
                stddev: 0.02,
            },
            observations_per_tick: Some(100),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
        }
    }

    #[test]
    fn validate_summary_config_accepts_valid() {
        assert!(validate_summary_config(&make_summary_config()).is_ok());
    }

    #[test]
    fn validate_summary_config_rejects_zero_rate() {
        let mut config = make_summary_config();
        config.base.rate = 0.0;
        assert!(validate_summary_config(&config).is_err());
    }

    #[test]
    fn validate_summary_config_rejects_zero_observations() {
        let mut config = make_summary_config();
        config.observations_per_tick = Some(0);
        assert!(validate_summary_config(&config).is_err());
    }

    #[test]
    fn validate_summary_config_rejects_out_of_range_quantiles() {
        let mut config = make_summary_config();
        config.quantiles = Some(vec![0.5, 1.5]);
        assert!(validate_summary_config(&config).is_err());
    }

    #[test]
    fn validate_summary_config_rejects_invalid_metric_name() {
        let mut config = make_summary_config();
        config.base.name = "123-invalid".to_string();
        assert!(validate_summary_config(&config).is_err());
    }

    // ---- validate_sink_config ------------------------------------------------

    use crate::sink::retry::RetryConfig;

    fn good_retry() -> RetryConfig {
        RetryConfig {
            max_attempts: 3,
            initial_backoff: "100ms".to_string(),
            max_backoff: "5s".to_string(),
        }
    }

    /// Validation runs the `snmp_agent` sink's one field check, so a value
    /// only `settings()` refuses fails the entry, naming the field.
    #[cfg(feature = "snmp")]
    #[test]
    fn validate_config_runs_the_snmp_agent_settings_check() {
        validate_config(&snmp_config()).expect("the baseline validates");
        let mut config = snmp_config();
        let SinkConfig::SnmpAgent(sink) = &mut config.base.sink else {
            panic!("the baseline is on an snmp_agent sink");
        };
        sink.max_message_size = 483;
        let msg = err_msg(validate_config(&config));
        assert!(msg.contains("max_message_size"), "{msg}");
    }

    /// Validation refuses an `snmp_agent` sink's empty or repeated
    /// `versions` through `settings()`, and two entries of one file on one
    /// address whose `versions` differ, naming both lists; the same versions
    /// in another order share the address.
    #[cfg(feature = "snmp")]
    #[test]
    fn validation_checks_the_snmp_agent_versions() {
        use crate::config::ScenarioEntry;
        use crate::schedule::launch::prepare_entries;
        use crate::sink::snmp::SnmpVersion::{V2c, V1};

        let with = |versions: &[crate::sink::snmp::SnmpVersion]| {
            let mut config = snmp_config();
            let SinkConfig::SnmpAgent(sink) = &mut config.base.sink else {
                panic!("the baseline is on an snmp_agent sink");
            };
            sink.versions = versions.to_vec();
            config
        };
        validate_config(&with(&[V1, V2c])).expect("v1 and v2c validate");
        let msg = err_msg(validate_config(&with(&[])));
        assert!(
            msg.contains("versions must name at least one version"),
            "{msg}"
        );
        let msg = err_msg(validate_config(&with(&[V1, V1])));
        assert!(msg.contains("versions names v1 more than once"), "{msg}");

        let file = |a: &[_], b: &[_]| {
            prepare_entries(vec![
                ScenarioEntry::Metrics(with(a)),
                ScenarioEntry::Metrics(with(b)),
            ])
            .map(|_| ())
        };
        file(&[V1, V2c], &[V2c, V1]).expect("the same versions share the address");
        let msg = err_msg(file(&[V1, V2c], &[V2c]));
        assert!(
            msg.contains("snmp_agent 127.0.0.1:1161 with different settings")
                && msg.contains("versions [v1, v2c] and [v2c]"),
            "{msg}"
        );
    }

    /// Validation refuses an `snmp_agent` sink naming `v3` without users and
    /// a user's short password through `settings()`, and two entries of one
    /// file on one address whose users differ, never naming a password.
    #[cfg(feature = "snmp")]
    #[test]
    fn validation_checks_the_snmp_agent_users() {
        use crate::config::ScenarioEntry;
        use crate::schedule::launch::prepare_entries;
        use crate::sink::snmp::{AuthProtocol, SnmpVersion, UsmAuth, UsmUser};

        let with = |users: &[(&str, &str)]| {
            let mut config = snmp_config();
            let SinkConfig::SnmpAgent(sink) = &mut config.base.sink else {
                panic!("the baseline is on an snmp_agent sink");
            };
            sink.versions = vec![SnmpVersion::V2c, SnmpVersion::V3];
            sink.users = users
                .iter()
                .map(|(name, password)| UsmUser {
                    name: name.to_string(),
                    auth: Some(UsmAuth {
                        protocol: AuthProtocol::Sha256,
                        password: password.to_string(),
                    }),
                    privacy: None,
                })
                .collect();
            config
        };
        validate_config(&with(&[("telegraf", "lab-password")])).expect("one user validates");
        let msg = err_msg(validate_config(&with(&[])));
        assert!(
            msg.contains("versions names v3, which needs at least one user"),
            "{msg}"
        );
        let msg = err_msg(validate_config(&with(&[("telegraf", "short")])));
        assert!(
            msg.contains("auth.password must be at least 8 bytes"),
            "{msg}"
        );

        let file = |a: &[(&str, &str)], b: &[(&str, &str)]| {
            prepare_entries(vec![
                ScenarioEntry::Metrics(with(a)),
                ScenarioEntry::Metrics(with(b)),
            ])
            .map(|_| ())
        };
        file(
            &[("telegraf", "lab-password")],
            &[("telegraf", "lab-password")],
        )
        .expect("the same users share the address");
        let msg = err_msg(file(
            &[("telegraf", "lab-password")],
            &[("telegraf", "lab-other-1")],
        ));
        assert!(
            msg.contains("snmp_agent 127.0.0.1:1161 with different settings")
                && msg.contains("users with another password"),
            "{msg}"
        );
        assert!(
            !msg.contains("lab-password") && !msg.contains("lab-other-1"),
            "{msg}"
        );
    }

    #[test]
    fn validate_sink_config_stdout_is_ok() {
        assert!(validate_sink_config(&SinkConfig::Stdout).is_ok());
    }

    #[test]
    fn validate_sink_config_file_is_ok() {
        let sink = SinkConfig::File {
            path: "/tmp/x".to_string(),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[test]
    fn validate_sink_config_udp_is_ok() {
        let sink = SinkConfig::Udp {
            address: "127.0.0.1:9999".to_string(),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[test]
    fn validate_sink_config_tcp_with_no_retry_is_ok() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: None,
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[test]
    fn validate_sink_config_tcp_with_good_retry_is_ok() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(good_retry()),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[test]
    fn validate_sink_config_tcp_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        let result = validate_sink_config(&sink);
        let msg = err_msg(result);
        assert!(
            msg.contains("garbage"),
            "error must mention the offending value, got: {msg}"
        );
    }

    #[test]
    fn validate_sink_config_tcp_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
        };
        let result = validate_sink_config(&sink);
        let msg = err_msg(result);
        assert!(
            msg.contains("garbage"),
            "error must mention the offending value, got: {msg}"
        );
    }

    // `RetryPolicy::from_config` has always refused these two, but only at
    // sink construction. Config validation — the thing `--dry-run` runs —
    // checked the duration strings and nothing else, so `--dry-run` printed
    // "Validation: OK" for a file the next real run refused. These assert
    // the two layers now agree.
    #[test]
    fn validate_sink_config_rejects_zero_max_attempts() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 0,
                initial_backoff: "100ms".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        let msg = err_msg(validate_sink_config(&sink));
        assert!(
            msg.contains("max_attempts"),
            "error must name the offending field, got: {msg}"
        );
    }

    #[test]
    fn validate_sink_config_rejects_backoff_ceiling_below_its_floor() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "30s".to_string(),
                max_backoff: "1s".to_string(),
            }),
        };
        let msg = err_msg(validate_sink_config(&sink));
        assert!(
            msg.contains("max_backoff") && msg.contains("initial_backoff"),
            "error must name both bounds so the reader can see which way round, got: {msg}"
        );
    }

    #[test]
    fn one_retry_attempt_is_the_documented_minimum_and_is_accepted() {
        // The boundary itself, in the direction that would break users: the
        // doc comment promises "at least 1", so 1 must pass. A guard written
        // as `<= 1` would reject the value its own documentation blesses.
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 1,
                initial_backoff: "100ms".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_http_push_with_good_durations_is_ok() {
        let sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: Some("5s".to_string()),
            headers: None,
            retry: Some(good_retry()),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_http_push_with_zero_buffer_age_is_ok() {
        let sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: Some("0s".to_string()),
            headers: None,
            retry: None,
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_http_push_with_garbage_max_buffer_age_returns_err() {
        let sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: Some("garbage".to_string()),
            headers: None,
            retry: None,
        };
        let result = validate_sink_config(&sink);
        let msg = err_msg(result);
        assert!(
            msg.contains("garbage"),
            "error must mention the offending value, got: {msg}"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_http_push_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_http_push_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_loki_with_good_durations_is_ok() {
        let sink = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: Some("5s".to_string()),
            retry: Some(good_retry()),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_loki_with_garbage_max_buffer_age_returns_err() {
        let sink = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
        };
        let result = validate_sink_config(&sink);
        let msg = err_msg(result);
        assert!(
            msg.contains("garbage"),
            "error must mention the offending value, got: {msg}"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_loki_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "http")]
    #[test]
    fn validate_sink_config_loki_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::Loki {
            url: "http://localhost:3100".to_string(),
            batch_size: None,
            max_streams_per_push: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn validate_sink_config_remote_write_with_good_durations_is_ok() {
        let sink = SinkConfig::RemoteWrite {
            url: "http://localhost:8428/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: Some("5s".to_string()),
            retry: Some(good_retry()),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn validate_sink_config_remote_write_with_garbage_max_buffer_age_returns_err() {
        let sink = SinkConfig::RemoteWrite {
            url: "http://localhost:8428/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn validate_sink_config_remote_write_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::RemoteWrite {
            url: "http://localhost:8428/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn validate_sink_config_remote_write_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::RemoteWrite {
            url: "http://localhost:8428/api/v1/write".to_string(),
            batch_size: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn validate_sink_config_kafka_with_good_durations_is_ok() {
        let sink = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: Some("5s".to_string()),
            retry: Some(good_retry()),
            tls: None,
            sasl: None,
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn validate_sink_config_kafka_with_garbage_max_buffer_age_returns_err() {
        let sink = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
            tls: None,
            sasl: None,
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn validate_sink_config_kafka_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
            tls: None,
            sasl: None,
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn validate_sink_config_kafka_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-test".to_string(),
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
            tls: None,
            sasl: None,
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn validate_sink_config_otlp_grpc_with_good_durations_is_ok() {
        let sink = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: crate::sink::otlp_grpc::OtlpSignalType::Metrics,
            batch_size: None,
            max_buffer_age: Some("5s".to_string()),
            retry: Some(good_retry()),
        };
        assert!(validate_sink_config(&sink).is_ok());
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn validate_sink_config_otlp_grpc_with_garbage_max_buffer_age_returns_err() {
        let sink = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: crate::sink::otlp_grpc::OtlpSignalType::Metrics,
            batch_size: None,
            max_buffer_age: Some("garbage".to_string()),
            retry: None,
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn validate_sink_config_otlp_grpc_with_garbage_initial_backoff_returns_err() {
        let sink = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: crate::sink::otlp_grpc::OtlpSignalType::Metrics,
            batch_size: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "garbage".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn validate_sink_config_otlp_grpc_with_garbage_max_backoff_returns_err() {
        let sink = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: crate::sink::otlp_grpc::OtlpSignalType::Metrics,
            batch_size: None,
            max_buffer_age: None,
            retry: Some(RetryConfig {
                max_attempts: 3,
                initial_backoff: "100ms".to_string(),
                max_backoff: "garbage".to_string(),
            }),
        };
        assert!(validate_sink_config(&sink).is_err());
    }

    // ---- end-to-end: validate_config rejects bad sink durations -------------

    #[cfg(feature = "http")]
    #[test]
    fn validate_config_rejects_bad_sink_max_buffer_age() {
        let mut config = minimal_config_with_rate(10.0);
        config.base.sink = SinkConfig::HttpPush {
            url: "http://localhost:9090/push".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: Some("garbage".to_string()),
            headers: None,
            retry: None,
        };
        assert!(validate_config(&config).is_err());
    }

    // ---- parse_start_time ----------------------------------------------------

    #[test]
    fn parse_start_time_now_returns_now_variant() {
        assert_eq!(parse_start_time("now").unwrap(), StartTime::Now);
    }

    #[test]
    fn parse_start_time_now_tolerates_surrounding_whitespace() {
        assert_eq!(parse_start_time("  now  ").unwrap(), StartTime::Now);
    }

    #[test]
    fn parse_start_time_positive_hours_offset_is_forward() {
        assert_eq!(
            parse_start_time("+24h").unwrap(),
            StartTime::Offset {
                forward: true,
                by: Duration::from_secs(24 * 3600),
            }
        );
    }

    #[test]
    fn parse_start_time_negative_days_offset_is_backward() {
        assert_eq!(
            parse_start_time("-7d").unwrap(),
            StartTime::Offset {
                forward: false,
                by: Duration::from_secs(7 * 86_400),
            }
        );
    }

    #[test]
    fn parse_start_time_positive_days_offset_is_forward() {
        assert_eq!(
            parse_start_time("+3d").unwrap(),
            StartTime::Offset {
                forward: true,
                by: Duration::from_secs(3 * 86_400),
            }
        );
    }

    #[test]
    fn parse_start_time_offset_accepts_minutes_and_seconds() {
        assert_eq!(
            parse_start_time("-90m").unwrap(),
            StartTime::Offset {
                forward: false,
                by: Duration::from_secs(90 * 60),
            }
        );
        assert_eq!(
            parse_start_time("+30s").unwrap(),
            StartTime::Offset {
                forward: true,
                by: Duration::from_secs(30),
            }
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn parse_start_time_rfc3339_returns_exact_instant() {
        let parsed = parse_start_time("2026-05-08T14:00:00Z").unwrap();
        let expected = std::time::UNIX_EPOCH + Duration::from_secs(1_778_248_800);
        assert_eq!(parsed, StartTime::Absolute(expected));
    }

    #[cfg(feature = "config")]
    #[test]
    fn parse_start_time_rfc3339_with_offset_normalizes_to_utc_instant() {
        // 14:00:00+02:00 is the same instant as 12:00:00Z.
        let with_offset = parse_start_time("2026-05-08T14:00:00+02:00").unwrap();
        let utc = parse_start_time("2026-05-08T12:00:00Z").unwrap();
        assert_eq!(with_offset, utc);
    }

    #[test]
    fn parse_start_time_empty_string_is_err() {
        assert!(parse_start_time("").is_err());
        assert!(parse_start_time("   ").is_err());
    }

    #[test]
    fn parse_start_time_garbage_is_err() {
        assert!(parse_start_time("garbage").is_err());
    }

    #[test]
    fn parse_start_time_offset_without_unit_is_err() {
        assert!(parse_start_time("+abc").is_err());
    }

    #[test]
    fn parse_start_time_zero_offset_is_err() {
        assert!(parse_start_time("+0d").is_err());
    }

    #[test]
    fn parse_start_time_overflowing_day_offset_is_err() {
        assert!(parse_start_time("+1e15d").is_err());
    }
}
