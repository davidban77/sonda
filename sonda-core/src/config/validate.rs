//! Config validation helpers: duration parsing and semantic checks.

use std::time::Duration;

use crate::model::metric::{is_valid_label_key, is_valid_metric_name};
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

/// Parse an optional phase offset string into a [`Duration`].
///
/// Unlike [`parse_duration`], this function accepts zero values (e.g. `"0s"`)
/// and returns `None` for them, since a zero offset is semantically equivalent
/// to no offset.
pub fn parse_phase_offset(s: &str) -> Result<Option<Duration>, SondaError> {
    // Try parse_duration first — it handles non-zero values.
    match parse_duration(s) {
        Ok(d) => Ok(Some(d)),
        Err(_) => {
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
                    return Ok(None); // "0s", "0ms", "0m", "0h", "0.0s" all mean no delay
                }
            }
            Err(SondaError::Config(ConfigError::invalid(format!(
                "invalid phase_offset {:?}: {}",
                s,
                parse_duration(s).unwrap_err()
            ))))
        }
    }
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
    }
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
/// - Normal: `stddev` must be strictly positive.
/// - Uniform: no constraints beyond NaN (both `min` and `max` are valid as-is).
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

    Ok(())
}

/// Wrap a `SondaError::Config` from `parse_duration` with additional field context.
///
/// Extracts the inner message string from the error so the final error reads
/// `"<label> <value_quoted>: <original message>"` without double-prefixing.
fn prepend_context(label: &str, value: &str, err: SondaError) -> SondaError {
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
    #[test]
    fn round_trip_creates_generator_encoder_sink_successfully() {
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

        let sink = create_sink(&config.sink, None);
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
                v >= 0.0 && v <= 1.0,
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
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        }
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
                jitter: Some(f64::NAN),
                jitter_seed: None,
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
                jitter: Some(-0.5),
                jitter_seed: None,
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
                jitter: None,
                jitter_seed: None,
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
                jitter: None,
                jitter_seed: None,
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

    // ---- validate_histogram_config ------------------------------------------

    /// Helper to build a minimal valid HistogramScenarioConfig.
    fn make_histogram_config() -> HistogramScenarioConfig {
        HistogramScenarioConfig {
            base: BaseScheduleConfig {
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
                jitter: None,
                jitter_seed: None,
            },
            buckets: None,
            distribution: DistributionConfig::Exponential { rate: 10.0 },
            observations_per_tick: Some(100),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
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

    // ---- validate_summary_config --------------------------------------------

    /// Helper to build a minimal valid SummaryScenarioConfig.
    fn make_summary_config() -> SummaryScenarioConfig {
        SummaryScenarioConfig {
            base: BaseScheduleConfig {
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
                jitter: None,
                jitter_seed: None,
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
}
