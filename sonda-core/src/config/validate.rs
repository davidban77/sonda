//! Config validation helpers: duration parsing and semantic checks.

use std::time::Duration;

use crate::SondaError;

use super::ScenarioConfig;

/// Parse a human-readable duration string into a [`Duration`].
///
/// Supported units:
/// - `ms` — milliseconds (e.g. `"100ms"`)
/// - `s`  — seconds      (e.g. `"30s"`)
/// - `m`  — minutes      (e.g. `"5m"`)
/// - `h`  — hours        (e.g. `"1h"`)
///
/// Returns [`SondaError::Config`] if the string is empty, has no recognized
/// unit suffix, has a non-numeric prefix, or has a zero or negative value.
pub fn parse_duration(s: &str) -> Result<Duration, SondaError> {
    if s.is_empty() {
        return Err(SondaError::Config("duration must not be empty".to_string()));
    }

    // Determine unit suffix and numeric portion.
    let (numeric_str, multiplier_ms): (&str, u64) = if let Some(stripped) = s.strip_suffix("ms") {
        (stripped, 1)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3_600_000)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60_000)
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1_000)
    } else {
        return Err(SondaError::Config(format!(
            "unrecognized duration unit in {:?}: expected one of ms, s, m, h",
            s
        )));
    };

    if numeric_str.is_empty() {
        return Err(SondaError::Config(format!(
            "duration {:?} has no numeric value before the unit",
            s
        )));
    }

    // Reject leading minus sign explicitly for a clear error message.
    if numeric_str.starts_with('-') {
        return Err(SondaError::Config(format!(
            "duration {:?} must be positive",
            s
        )));
    }

    let value: u64 = numeric_str.parse().map_err(|_| {
        SondaError::Config(format!(
            "duration {:?} has an invalid numeric part {:?}",
            s, numeric_str
        ))
    })?;

    if value == 0 {
        return Err(SondaError::Config(format!(
            "duration {:?} must be greater than zero",
            s
        )));
    }

    Ok(Duration::from_millis(value * multiplier_ms))
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
    // Rate must be strictly positive.
    if config.rate <= 0.0 {
        return Err(SondaError::Config(format!(
            "rate must be positive, got {}",
            config.rate
        )));
    }

    // Duration must be parseable if provided.
    if let Some(ref dur_str) = config.duration {
        parse_duration(dur_str)
            .map_err(|e| SondaError::Config(format!("invalid duration {:?}: {}", dur_str, e)))?;
    }

    // Gap consistency: gap_for < gap_every.
    if let Some(ref gap) = config.gaps {
        let every = parse_duration(&gap.every).map_err(|e| {
            SondaError::Config(format!("invalid gaps.every {:?}: {}", gap.every, e))
        })?;
        let for_dur = parse_duration(&gap.r#for)
            .map_err(|e| SondaError::Config(format!("invalid gaps.for {:?}: {}", gap.r#for, e)))?;
        if for_dur >= every {
            return Err(SondaError::Config(format!(
                "gaps.for ({:?}) must be less than gaps.every ({:?})",
                gap.r#for, gap.every
            )));
        }
    }

    // Metric name must be a valid Prometheus metric name.
    if !is_valid_metric_name(&config.name) {
        return Err(SondaError::Config(format!(
            "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
            config.name
        )));
    }

    Ok(())
}

/// Returns `true` if `s` is a valid Prometheus metric name.
///
/// Valid metric names match `[a-zA-Z_:][a-zA-Z0-9_:]*` and must not be empty.
fn is_valid_metric_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}
