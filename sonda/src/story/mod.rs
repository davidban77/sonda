//! Story compilation layer for multi-signal temporal scenarios.
//!
//! Stories are a concise YAML format that expresses multi-signal scenarios
//! with temporal causality. A story compiles down to the existing
//! `Vec<ScenarioEntry>` + `phase_offset` infrastructure at parse time —
//! there is no runtime reactivity.
//!
//! # Example Story YAML
//!
//! ```yaml
//! story: link_failover
//! description: "Edge router link failure with traffic shift to backup"
//! duration: 5m
//! rate: 1
//! encoder: { type: prometheus_text }
//! sink: { type: stdout }
//! labels:
//!   device: rtr-edge-01
//!   job: network
//!
//! signals:
//!   - metric: interface_oper_state
//!     behavior: flap
//!     up_duration: 60s
//!     down_duration: 30s
//!     labels:
//!       interface: GigabitEthernet0/0/0
//!
//!   - metric: backup_link_utilization
//!     behavior: saturation
//!     baseline: 20
//!     ceiling: 85
//!     time_to_saturate: 2m
//!     after: interface_oper_state < 1
//!     labels:
//!       interface: GigabitEthernet0/1/0
//! ```
//!
//! # Compilation Flow
//!
//! 1. Parse YAML into [`StoryConfig`]
//! 2. Resolve `after` clauses (topological sort + timing formulas)
//! 3. Expand each signal into a [`ScenarioEntry`] with shared fields,
//!    `phase_offset`, and `clock_group` injected
//! 4. Return `Vec<ScenarioEntry>` for the existing `prepare_entries` pipeline

pub mod after_resolve;
pub mod timing;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_yaml_ng::Value;
use sonda_core::config::ScenarioEntry;

use crate::yaml_helpers::{escape_yaml_double_quoted, needs_quoting};

use self::after_resolve::{parse_after_clause, resolve_offsets, AfterClause, SignalParams};

/// Top-level story configuration, parsed from YAML.
///
/// Contains shared fields that apply to all signals (unless overridden)
/// and the list of signal definitions.
#[derive(Debug)]
pub struct StoryConfig {
    /// Story identifier (used as `clock_group`).
    pub story: String,
    /// Human-readable description (currently parsed but not displayed).
    #[allow(dead_code)]
    pub description: Option<String>,
    /// Shared duration for all signals (e.g., `"5m"`).
    pub duration: Option<String>,
    /// Shared event rate in events per second.
    pub rate: Option<f64>,
    /// Shared encoder configuration (raw YAML value).
    pub encoder: Option<Value>,
    /// Shared sink configuration (raw YAML value).
    pub sink: Option<Value>,
    /// Shared labels applied to all signals.
    pub labels: Option<HashMap<String, String>>,
    /// The signal definitions.
    pub signals: Vec<SignalConfig>,
}

/// A single signal definition within a story.
///
/// Each signal maps to one `ScenarioEntry` after compilation.
#[derive(Debug)]
pub struct SignalConfig {
    /// Metric name for this signal.
    pub metric: String,
    /// Behavior alias (e.g., `"flap"`, `"saturation"`, `"degradation"`).
    pub behavior: String,
    /// Optional `after` clause for temporal sequencing.
    pub after: Option<String>,
    /// Per-signal labels (merged with story-level labels).
    pub labels: Option<HashMap<String, String>>,
    /// Per-signal rate override.
    pub rate: Option<f64>,
    /// Per-signal duration override.
    pub duration: Option<String>,
    /// Per-signal encoder override (raw YAML value).
    pub encoder: Option<Value>,
    /// Per-signal sink override (raw YAML value).
    pub sink: Option<Value>,
    /// All remaining flat parameters (behavior-specific).
    pub params: HashMap<String, Value>,
}

/// CLI overrides that can be applied to a story.
///
/// These override the story-level shared fields, not per-signal fields.
#[derive(Debug, Default)]
pub struct StoryOverrides {
    /// Override the story duration.
    pub duration: Option<String>,
    /// Override the story rate.
    pub rate: Option<f64>,
    /// Override the story sink type.
    pub sink: Option<String>,
    /// Override the sink endpoint.
    pub endpoint: Option<String>,
    /// Override the encoder format.
    pub encoder: Option<String>,
}

/// Load a story YAML file from disk and return its raw content.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn load_story_yaml(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read story file {}", path.display()))
}

/// Parse a story YAML string into a [`StoryConfig`].
///
/// # Errors
///
/// Returns an error if the YAML structure is invalid or required fields
/// are missing.
pub fn parse_story(yaml: &str) -> Result<StoryConfig> {
    let root: Value = serde_yaml_ng::from_str(yaml).context("invalid YAML in story file")?;

    let mapping = root
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("story file must be a YAML mapping at the top level"))?;

    let story = get_string(mapping, "story")
        .ok_or_else(|| anyhow::anyhow!("story file must have a 'story' field (string)"))?;

    let description = get_string(mapping, "description");
    let duration = get_string(mapping, "duration");
    let rate = get_f64(mapping, "rate");
    let encoder = mapping.get(Value::String("encoder".to_string())).cloned();
    let sink = mapping.get(Value::String("sink".to_string())).cloned();
    let labels = parse_labels(mapping.get(Value::String("labels".to_string())));

    let signals_val = mapping
        .get(Value::String("signals".to_string()))
        .ok_or_else(|| anyhow::anyhow!("story file must have a 'signals' list"))?;

    let signals_seq = signals_val
        .as_sequence()
        .ok_or_else(|| anyhow::anyhow!("'signals' must be a YAML sequence"))?;

    if signals_seq.is_empty() {
        bail!("'signals' list must not be empty");
    }

    let mut signals = Vec::with_capacity(signals_seq.len());
    for (i, val) in signals_seq.iter().enumerate() {
        let sig = parse_signal(val).with_context(|| format!("error in signal[{i}]"))?;
        signals.push(sig);
    }

    // Check for duplicate metric names.
    let mut seen = HashMap::new();
    for (i, sig) in signals.iter().enumerate() {
        if let Some(prev) = seen.insert(&sig.metric, i) {
            bail!(
                "duplicate metric name {:?} in signals[{prev}] and signals[{i}]",
                sig.metric
            );
        }
    }

    Ok(StoryConfig {
        story,
        description,
        duration,
        rate,
        encoder,
        sink,
        labels,
        signals,
    })
}

/// Compile a [`StoryConfig`] into a `Vec<ScenarioEntry>`, resolving `after`
/// clauses into `phase_offset` values.
///
/// Applies `overrides` to story-level shared fields before expanding signals.
///
/// # Errors
///
/// Returns an error if `after` clause resolution fails (unknown references,
/// cycles, unsupported behaviors, out-of-range thresholds).
pub fn compile_story(
    config: &StoryConfig,
    overrides: &StoryOverrides,
) -> Result<Vec<ScenarioEntry>> {
    // Resolve effective shared fields (CLI overrides > story fields).
    let effective_rate = overrides.rate.or(config.rate).unwrap_or(1.0);
    let effective_duration = overrides.duration.as_deref().or(config.duration.as_deref());
    let effective_encoder = build_effective_encoder(overrides, config);
    let effective_sink = build_effective_sink(overrides, config);

    // Parse the total wall-clock cap (if any).
    let total_duration_secs = effective_duration
        .map(|d| {
            sonda_core::config::validate::parse_duration(d)
                .map(|dur| dur.as_secs_f64())
                .map_err(|e| anyhow::anyhow!("invalid story duration {:?}: {e}", d))
        })
        .transpose()?;

    // Build the signal list for after-clause resolution.
    let signal_tuples: Vec<(String, Option<AfterClause>, SignalParams)> = config
        .signals
        .iter()
        .map(|sig| {
            let after_clause = sig
                .after
                .as_deref()
                .map(parse_after_clause)
                .transpose()
                .map_err(|e| {
                    anyhow::anyhow!("signal {:?}: invalid after clause: {e}", sig.metric)
                })?;

            let params = SignalParams {
                behavior: sig.behavior.clone(),
                params: sig.params.clone(),
            };

            Ok((sig.metric.clone(), after_clause, params))
        })
        .collect::<Result<Vec<_>>>()?;

    // Resolve all offsets.
    let offsets = resolve_offsets(&signal_tuples).map_err(|e| anyhow::anyhow!("{e}"))?;

    // When a wall-clock duration cap is set, warn about signals that will be
    // skipped or truncated, and compute the effective per-signal duration.
    if let Some(cap_secs) = total_duration_secs {
        print_duration_cap_warnings(config, &offsets, cap_secs, effective_duration.unwrap_or(""));
    }

    // Expand each signal into a ScenarioEntry.
    let mut entries = Vec::with_capacity(config.signals.len());

    for sig in &config.signals {
        let offset_secs = offsets[&sig.metric];

        // When a wall-clock cap is active, skip signals whose phase_offset
        // meets or exceeds the total duration.
        if let Some(cap_secs) = total_duration_secs {
            if offset_secs >= cap_secs {
                continue;
            }
        }

        // Build merged labels: story-level + signal-level (signal wins on conflict).
        let merged_labels = merge_labels(config.labels.as_ref(), sig.labels.as_ref());

        // Determine per-signal rate, duration, encoder, sink.
        let sig_rate = sig.rate.unwrap_or(effective_rate);

        // Compute the per-signal emission duration. When a wall-clock cap is
        // active the effective duration is capped to (total_duration - offset).
        let sig_duration = compute_signal_duration(
            sig.duration.as_deref(),
            effective_duration,
            total_duration_secs,
            offset_secs,
        );

        let sig_encoder = sig.encoder.as_ref().unwrap_or(&effective_encoder);
        let sig_sink = sig.sink.as_ref().unwrap_or(&effective_sink);

        // Build the phase_offset string.
        let phase_offset = if offset_secs > 0.0 {
            Some(format_duration_secs(offset_secs))
        } else {
            None
        };

        // Build a YAML snippet for the generator config from behavior + flat params.
        let generator_yaml = build_generator_yaml(&sig.behavior, &sig.params);

        // Build the full scenario entry YAML and deserialize it.
        let entry_yaml = build_entry_yaml(&EntryYamlParams {
            name: &sig.metric,
            rate: sig_rate,
            duration: sig_duration.as_deref(),
            generator_yaml: &generator_yaml,
            encoder: sig_encoder,
            sink: sig_sink,
            labels: &merged_labels,
            phase_offset: phase_offset.as_deref(),
            clock_group: &config.story,
        })?;

        let entry: ScenarioEntry = serde_yaml_ng::from_str(&entry_yaml).with_context(|| {
            format!(
                "failed to deserialize compiled scenario for signal {:?}:\n{}",
                sig.metric, entry_yaml
            )
        })?;

        entries.push(entry);
    }

    if entries.is_empty() {
        bail!(
            "all signals were skipped because their phase offsets exceed \
             the story duration ({})",
            effective_duration.unwrap_or("0s")
        );
    }

    Ok(entries)
}

/// Compute the effective per-signal emission duration, respecting the
/// wall-clock duration cap when set.
///
/// When `total_duration_secs` is `Some`, each signal's effective duration is
/// capped to `total_duration - offset_secs`. If the signal has its own
/// per-signal duration, the effective value is the minimum of that duration
/// and the remaining wall-clock budget.
fn compute_signal_duration(
    per_signal_duration: Option<&str>,
    story_duration: Option<&str>,
    total_duration_secs: Option<f64>,
    offset_secs: f64,
) -> Option<String> {
    let base_duration_str = per_signal_duration.or(story_duration);

    let Some(cap_secs) = total_duration_secs else {
        // No wall-clock cap — use the signal or story duration as-is.
        return base_duration_str.map(|s| s.to_string());
    };

    let remaining = cap_secs - offset_secs;
    if remaining <= 0.0 {
        // Should not happen (caller filters these out), but be defensive.
        return Some("0s".to_string());
    }

    match base_duration_str {
        Some(dur_str) => {
            // Parse the per-signal duration and cap it.
            if let Ok(dur) = sonda_core::config::validate::parse_duration(dur_str) {
                let dur_secs = dur.as_secs_f64();
                if dur_secs > remaining {
                    Some(format_duration_secs(remaining))
                } else {
                    Some(dur_str.to_string())
                }
            } else {
                // Unparseable duration — pass through and let validation catch it.
                Some(dur_str.to_string())
            }
        }
        None => {
            // No explicit duration but there is a wall-clock cap — set one.
            Some(format_duration_secs(remaining))
        }
    }
}

/// Print warnings to stderr when the story duration cap causes signals to be
/// skipped or truncated.
fn print_duration_cap_warnings(
    config: &StoryConfig,
    offsets: &HashMap<String, f64>,
    cap_secs: f64,
    duration_str: &str,
) {
    let mut skipped: Vec<(&str, f64)> = Vec::new();
    let mut truncated: Vec<(&str, f64, f64)> = Vec::new();

    for sig in &config.signals {
        let offset = offsets[&sig.metric];
        if offset >= cap_secs {
            skipped.push((&sig.metric, offset));
        } else {
            // Check if the signal's emission time would be truncated.
            let remaining = cap_secs - offset;
            let sig_dur = sig
                .duration
                .as_deref()
                .or(config.duration.as_deref())
                .and_then(|d| sonda_core::config::validate::parse_duration(d).ok())
                .map(|d| d.as_secs_f64());
            if let Some(dur_secs) = sig_dur {
                if dur_secs > remaining {
                    truncated.push((&sig.metric, offset, remaining));
                }
            }
        }
    }

    if skipped.is_empty() && truncated.is_empty() {
        return;
    }

    eprintln!("warning: story duration ({duration_str}) is shorter than some phase offsets:");
    for (name, offset) in &skipped {
        eprintln!(
            "  - {:?} needs {} before it starts -- skipped",
            name,
            format_duration_human(*offset),
        );
    }
    for (name, _offset, remaining) in &truncated {
        eprintln!(
            "  - {:?} will run for {} instead of its full duration",
            name,
            format_duration_human(*remaining),
        );
    }

    // Suggest the minimum duration needed to include all signals.
    let max_offset = offsets.values().cloned().fold(0.0_f64, f64::max);
    eprintln!(
        "  hint: use --duration {} or longer to include all signals",
        format_duration_human(max_offset + 60.0),
    );
}

/// Format a duration in seconds as a human-readable string for warning messages.
///
/// Uses minutes and seconds notation (e.g., "2m32s") for values >= 60s.
fn format_duration_human(secs: f64) -> String {
    if secs < 0.0 {
        return "0s".to_string();
    }
    let total_secs = secs.round() as u64;
    if total_secs == 0 {
        // Sub-second: use milliseconds.
        let ms = (secs * 1000.0).round() as u64;
        return format!("{ms}ms");
    }
    let m = total_secs / 60;
    let s = total_secs % 60;
    if m == 0 {
        format!("{s}s")
    } else if s == 0 {
        format!("{m}m")
    } else {
        format!("{m}m{s}s")
    }
}

/// Build a YAML snippet for the generator from a behavior alias and flat params.
///
/// This produces something like:
/// ```yaml
/// type: flap
/// up_duration: "60s"
/// down_duration: "30s"
/// ```
fn build_generator_yaml(behavior: &str, params: &HashMap<String, Value>) -> String {
    let mut lines = Vec::with_capacity(params.len() + 1);
    lines.push(format!("type: {behavior}"));

    // Sort keys for deterministic output.
    let mut sorted: Vec<(&String, &Value)> = params.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);

    for (key, value) in sorted {
        let val_str = format_yaml_value(value);
        lines.push(format!("{key}: {val_str}"));
    }

    lines.join("\n")
}

/// Format a serde_yaml_ng::Value for inline YAML output.
fn format_yaml_value(value: &Value) -> String {
    match value {
        Value::String(s) => {
            // Quote strings that might be misinterpreted by YAML.
            if needs_quoting(s) {
                format!("{s:?}")
            } else {
                s.clone()
            }
        }
        Value::Number(n) => format!("{n}"),
        Value::Bool(b) => format!("{b}"),
        Value::Null => "null".to_string(),
        _ => serde_yaml_ng::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Parameters for building a ScenarioEntry YAML string.
struct EntryYamlParams<'a> {
    name: &'a str,
    rate: f64,
    duration: Option<&'a str>,
    generator_yaml: &'a str,
    encoder: &'a Value,
    sink: &'a Value,
    labels: &'a Option<HashMap<String, String>>,
    phase_offset: Option<&'a str>,
    clock_group: &'a str,
}

/// Build a full ScenarioEntry YAML string from compiled signal fields.
fn build_entry_yaml(p: &EntryYamlParams<'_>) -> Result<String> {
    let mut lines = Vec::new();

    lines.push("signal_type: metrics".to_string());
    if needs_quoting(p.name) {
        let escaped = escape_yaml_double_quoted(p.name);
        lines.push(format!("name: \"{escaped}\""));
    } else {
        lines.push(format!("name: {}", p.name));
    }
    lines.push(format!("rate: {}", p.rate));

    if let Some(dur) = p.duration {
        lines.push(format!("duration: {dur}"));
    }

    // Generator block.
    lines.push("generator:".to_string());
    for gen_line in p.generator_yaml.lines() {
        lines.push(format!("  {gen_line}"));
    }

    // Encoder.
    let encoder_str =
        serde_yaml_ng::to_string(p.encoder).context("failed to serialize encoder config")?;
    lines.push("encoder:".to_string());
    for enc_line in encoder_str.trim().lines() {
        lines.push(format!("  {enc_line}"));
    }

    // Sink.
    let sink_str = serde_yaml_ng::to_string(p.sink).context("failed to serialize sink config")?;
    lines.push("sink:".to_string());
    for sink_line in sink_str.trim().lines() {
        lines.push(format!("  {sink_line}"));
    }

    // Labels.
    if let Some(ref lbl) = p.labels {
        if !lbl.is_empty() {
            lines.push("labels:".to_string());
            let mut sorted_labels: Vec<_> = lbl.iter().collect();
            sorted_labels.sort_by_key(|(k, _)| *k);
            for (k, v) in sorted_labels {
                if needs_quoting(v) {
                    let escaped = escape_yaml_double_quoted(v);
                    lines.push(format!("  {k}: \"{escaped}\""));
                } else {
                    lines.push(format!("  {k}: {v}"));
                }
            }
        }
    }

    // Phase offset.
    if let Some(offset) = p.phase_offset {
        lines.push(format!("phase_offset: {offset}"));
    }

    // Clock group.
    lines.push(format!("clock_group: {}", p.clock_group));

    Ok(lines.join("\n"))
}

/// Merge story-level labels with signal-level labels.
///
/// Signal labels override story labels on key conflict.
fn merge_labels(
    story_labels: Option<&HashMap<String, String>>,
    signal_labels: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    match (story_labels, signal_labels) {
        (None, None) => None,
        (Some(s), None) => Some(s.clone()),
        (None, Some(s)) => Some(s.clone()),
        (Some(story), Some(signal)) => {
            let mut merged = story.clone();
            for (k, v) in signal {
                merged.insert(k.clone(), v.clone());
            }
            Some(merged)
        }
    }
}

/// Build the effective encoder value from overrides and config.
fn build_effective_encoder(overrides: &StoryOverrides, config: &StoryConfig) -> Value {
    if let Some(ref enc) = overrides.encoder {
        // Build a simple encoder YAML value from the string.
        let mut map = serde_yaml_ng::Mapping::new();
        map.insert(
            Value::String("type".to_string()),
            Value::String(enc.clone()),
        );
        Value::Mapping(map)
    } else {
        config.encoder.clone().unwrap_or_else(|| {
            let mut map = serde_yaml_ng::Mapping::new();
            map.insert(
                Value::String("type".to_string()),
                Value::String("prometheus_text".to_string()),
            );
            Value::Mapping(map)
        })
    }
}

/// Build the effective sink value from overrides and config.
fn build_effective_sink(overrides: &StoryOverrides, config: &StoryConfig) -> Value {
    if let Some(ref sink_type) = overrides.sink {
        let mut map = serde_yaml_ng::Mapping::new();
        map.insert(
            Value::String("type".to_string()),
            Value::String(sink_type.clone()),
        );
        if let Some(ref endpoint) = overrides.endpoint {
            // Different sinks use different endpoint field names,
            // matching the SinkConfig variant fields in sonda-core.
            let field = match sink_type.as_str() {
                "http_push" | "remote_write" | "loki" => "url",
                "tcp" | "udp" => "address",
                "otlp_grpc" => "endpoint",
                "file" => "path",
                "kafka" => "brokers",
                _ => "url",
            };
            map.insert(
                Value::String(field.to_string()),
                Value::String(endpoint.clone()),
            );
        }
        Value::Mapping(map)
    } else {
        config.sink.clone().unwrap_or_else(|| {
            let mut map = serde_yaml_ng::Mapping::new();
            map.insert(
                Value::String("type".to_string()),
                Value::String("stdout".to_string()),
            );
            Value::Mapping(map)
        })
    }
}

/// Format a duration in seconds as a human-readable string.
///
/// Uses the largest whole unit that divides evenly, otherwise falls back
/// to fractional seconds with millisecond precision.
fn format_duration_secs(secs: f64) -> String {
    if secs <= 0.0 {
        return "0s".to_string();
    }

    // Try whole seconds first.
    let ms = (secs * 1000.0).round() as u64;
    if ms.is_multiple_of(1000) {
        let whole_secs = ms / 1000;
        if whole_secs.is_multiple_of(3600) && whole_secs > 0 {
            return format!("{}h", whole_secs / 3600);
        }
        if whole_secs.is_multiple_of(60) && whole_secs > 0 {
            return format!("{}m", whole_secs / 60);
        }
        return format!("{whole_secs}s");
    }

    // Fractional seconds — use milliseconds if sub-second, else fractional seconds.
    if ms < 1000 {
        return format!("{ms}ms");
    }

    // Use fractional seconds with up to 3 decimal places.
    let rounded = (secs * 1000.0).round() / 1000.0;
    format!("{rounded}s")
}

/// Parse a signal definition from a YAML value.
fn parse_signal(val: &Value) -> Result<SignalConfig> {
    let mapping = val
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("each signal must be a YAML mapping"))?;

    let metric = get_string(mapping, "metric")
        .ok_or_else(|| anyhow::anyhow!("signal must have a 'metric' field"))?;

    let behavior = get_string(mapping, "behavior")
        .ok_or_else(|| anyhow::anyhow!("signal must have a 'behavior' field"))?;

    let after = get_string(mapping, "after");
    let labels = parse_labels(mapping.get(Value::String("labels".to_string())));
    let rate = get_f64(mapping, "rate");
    let duration = get_string(mapping, "duration");
    let encoder = mapping.get(Value::String("encoder".to_string())).cloned();
    let sink = mapping.get(Value::String("sink".to_string())).cloned();

    // Collect all remaining keys as behavior params.
    let reserved = [
        "metric",
        "behavior",
        "after",
        "labels",
        "rate",
        "duration",
        "encoder",
        "sink",
        "signal_type",
    ];
    let mut params = HashMap::new();
    for (k, v) in mapping {
        if let Value::String(key) = k {
            if !reserved.contains(&key.as_str()) {
                params.insert(key.clone(), v.clone());
            }
        }
    }

    Ok(SignalConfig {
        metric,
        behavior,
        after,
        labels,
        rate,
        duration,
        encoder,
        sink,
        params,
    })
}

/// Extract a string value from a YAML mapping.
fn get_string(mapping: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            // Handle bare numbers/bools that YAML might auto-parse.
            Value::Number(n) => Some(format!("{n}")),
            Value::Bool(b) => Some(format!("{b}")),
            _ => None,
        })
}

/// Extract an f64 value from a YAML mapping.
fn get_f64(mapping: &serde_yaml_ng::Mapping, key: &str) -> Option<f64> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
}

/// Parse a labels YAML value into a `HashMap<String, String>`.
fn parse_labels(val: Option<&Value>) -> Option<HashMap<String, String>> {
    val.and_then(|v| v.as_mapping()).map(|m| {
        m.iter()
            .filter_map(|(k, v)| {
                let key = match k {
                    Value::String(s) => s.clone(),
                    _ => return None,
                };
                let value = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => format!("{n}"),
                    Value::Bool(b) => format!("{b}"),
                    _ => return None,
                };
                Some((key, value))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_story
    // -----------------------------------------------------------------------

    #[test]
    fn parse_minimal_story() {
        let yaml = r#"
story: test_story
signals:
  - metric: cpu_usage
    behavior: steady
"#;
        let config = parse_story(yaml).expect("should parse minimal story");
        assert_eq!(config.story, "test_story");
        assert_eq!(config.signals.len(), 1);
        assert_eq!(config.signals[0].metric, "cpu_usage");
        assert_eq!(config.signals[0].behavior, "steady");
    }

    #[test]
    fn parse_story_with_all_shared_fields() {
        let yaml = r#"
story: link_failover
description: "Edge router link failure"
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
labels:
  device: rtr-edge-01
  job: network
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
"#;
        let config = parse_story(yaml).expect("should parse");
        assert_eq!(config.story, "link_failover");
        assert_eq!(
            config.description.as_deref(),
            Some("Edge router link failure")
        );
        assert_eq!(config.duration.as_deref(), Some("5m"));
        assert!((config.rate.unwrap() - 1.0).abs() < f64::EPSILON);
        assert!(config.labels.is_some());
        let labels = config.labels.as_ref().unwrap();
        assert_eq!(labels.get("device").unwrap(), "rtr-edge-01");
        assert_eq!(labels.get("job").unwrap(), "network");
    }

    #[test]
    fn parse_story_missing_story_field() {
        let yaml = r#"
signals:
  - metric: cpu
    behavior: steady
"#;
        let err = parse_story(yaml).expect_err("should fail");
        assert!(err.to_string().contains("story"), "got: {}", err);
    }

    #[test]
    fn parse_story_empty_signals() {
        let yaml = r#"
story: test
signals: []
"#;
        let err = parse_story(yaml).expect_err("should fail");
        assert!(err.to_string().contains("empty"), "got: {}", err);
    }

    #[test]
    fn parse_story_duplicate_metrics() {
        let yaml = r#"
story: test
signals:
  - metric: cpu_usage
    behavior: steady
  - metric: cpu_usage
    behavior: flap
"#;
        let err = parse_story(yaml).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"), "got: {}", err);
    }

    #[test]
    fn parse_signal_flat_params() {
        let yaml = r#"
story: test
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
    up_value: 1.0
    down_value: 0.0
    labels:
      interface: GigabitEthernet0/0/0
"#;
        let config = parse_story(yaml).expect("should parse");
        let sig = &config.signals[0];
        assert_eq!(sig.params.len(), 4);
        assert!(sig.labels.is_some());
        let labels = sig.labels.as_ref().unwrap();
        assert_eq!(labels.get("interface").unwrap(), "GigabitEthernet0/0/0");
    }

    // -----------------------------------------------------------------------
    // compile_story
    // -----------------------------------------------------------------------

    #[test]
    fn compile_story_no_after_clauses() {
        let yaml = r#"
story: test_compile
duration: 30s
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: cpu_usage
    behavior: steady
    center: 75.0
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 1);

        // Verify the entry has the correct clock_group.
        assert_eq!(entries[0].clock_group(), Some("test_compile"));
        // No phase_offset since no after clause.
        assert!(entries[0].phase_offset().is_none());
    }

    #[test]
    fn compile_story_with_after_clause() {
        let yaml = r#"
story: failover
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
  - metric: backup_link_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 2m
    after: interface_oper_state < 1
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 2);

        // First signal: no offset.
        assert!(entries[0].phase_offset().is_none());
        // Second signal: offset = 60s (up_duration of flap).
        // format_duration_secs(60.0) produces "1m".
        assert!(entries[1].phase_offset().is_some());
        let offset = entries[1].phase_offset().unwrap();
        assert_eq!(offset, "1m", "expected 1m offset, got {offset}");

        // Both should share the same clock_group.
        assert_eq!(entries[0].clock_group(), Some("failover"));
        assert_eq!(entries[1].clock_group(), Some("failover"));
    }

    #[test]
    fn compile_story_with_label_merging() {
        let yaml = r#"
story: label_test
rate: 1
duration: 10s
encoder: { type: prometheus_text }
sink: { type: stdout }
labels:
  device: rtr-01
  job: network
signals:
  - metric: cpu_usage
    behavior: steady
    labels:
      interface: eth0
      device: rtr-02
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 1);

        // Check the labels on the entry.
        let base = entries[0].base();
        let labels = base.labels.as_ref().expect("should have labels");
        // Signal's device=rtr-02 should override story's device=rtr-01.
        assert_eq!(labels.get("device").unwrap(), "rtr-02");
        // Story's job=network should be present.
        assert_eq!(labels.get("job").unwrap(), "network");
        // Signal's interface=eth0 should be present.
        assert_eq!(labels.get("interface").unwrap(), "eth0");
    }

    #[test]
    fn compile_story_with_cli_overrides() {
        let yaml = r#"
story: test_override
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: cpu_usage
    behavior: steady
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("2m".to_string()),
            rate: Some(10.0),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        assert_eq!(entries.len(), 1);

        let base = entries[0].base();
        assert!((base.rate - 10.0).abs() < f64::EPSILON);
        assert_eq!(base.duration.as_deref(), Some("2m"));
    }

    #[test]
    fn compile_story_with_transitive_after() {
        let yaml = r#"
story: transitive
duration: 10m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
  - metric: backup_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 120s
    after: interface_oper_state < 1
  - metric: latency_ms
    behavior: degradation
    baseline: 5
    ceiling: 150
    time_to_degrade: 3m
    after: backup_utilization > 70
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 3);

        // Verify offsets: A=0, B=60s (formatted as 1m), C=60s + (70-20)/(85-20)*120s
        assert!(entries[0].phase_offset().is_none());
        assert_eq!(entries[1].phase_offset().unwrap(), "1m");
        // C offset = 60 + 92.307... = 152.307...s
        let c_offset = entries[2].phase_offset().expect("should have offset");
        // Parse it back to verify.
        let c_dur =
            sonda_core::config::validate::parse_duration(c_offset).expect("should parse offset");
        let expected_c = 60.0 + (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert!(
            (c_dur.as_secs_f64() - expected_c).abs() < 0.01,
            "expected ~{expected_c}s, got {}s from {:?}",
            c_dur.as_secs_f64(),
            c_offset
        );
    }

    // -----------------------------------------------------------------------
    // format_duration_secs
    // -----------------------------------------------------------------------

    #[test]
    fn format_duration_whole_seconds() {
        assert_eq!(format_duration_secs(30.0), "30s");
    }

    #[test]
    fn format_duration_whole_minutes() {
        assert_eq!(format_duration_secs(120.0), "2m");
    }

    #[test]
    fn format_duration_whole_hours() {
        assert_eq!(format_duration_secs(3600.0), "1h");
    }

    #[test]
    fn format_duration_fractional_seconds() {
        let result = format_duration_secs(92.307);
        // Should produce a parseable duration string.
        let dur = sonda_core::config::validate::parse_duration(&result)
            .expect("formatted duration should be parseable");
        assert!(
            (dur.as_secs_f64() - 92.307).abs() < 0.01,
            "got {}, expected ~92.307",
            dur.as_secs_f64()
        );
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration_secs(0.0), "0s");
    }

    #[test]
    fn format_duration_sub_second() {
        assert_eq!(format_duration_secs(0.5), "500ms");
    }

    // -----------------------------------------------------------------------
    // merge_labels
    // -----------------------------------------------------------------------

    #[test]
    fn merge_labels_both_none() {
        assert!(merge_labels(None, None).is_none());
    }

    #[test]
    fn merge_labels_story_only() {
        let story = HashMap::from([("a".to_string(), "1".to_string())]);
        let result = merge_labels(Some(&story), None).unwrap();
        assert_eq!(result.get("a").unwrap(), "1");
    }

    #[test]
    fn merge_labels_signal_overrides() {
        let story = HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let signal = HashMap::from([("a".to_string(), "override".to_string())]);
        let result = merge_labels(Some(&story), Some(&signal)).unwrap();
        assert_eq!(result.get("a").unwrap(), "override");
        assert_eq!(result.get("b").unwrap(), "2");
    }

    // -----------------------------------------------------------------------
    // Duration cap (Fix 1)
    // -----------------------------------------------------------------------

    #[test]
    fn duration_cap_skips_signals_beyond_cap() {
        // Story has 3 signals: A at t=0, B at t=60s, C at ~152s.
        // With --duration 30s, only A should survive.
        let yaml = r#"
story: cap_test
duration: 10m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
  - metric: backup_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 120s
    after: interface_oper_state < 1
  - metric: latency_ms
    behavior: degradation
    baseline: 5
    ceiling: 150
    time_to_degrade: 3m
    after: backup_utilization > 70
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("30s".to_string()),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        // Only the first signal (offset=0) fits within 30s.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().name, "interface_oper_state");
        // Its duration should be capped to 30s.
        assert_eq!(entries[0].base().duration.as_deref(), Some("30s"));
    }

    #[test]
    fn duration_cap_truncates_signal_duration() {
        // Signal A starts at t=0 with a 5m story duration, but cap is 2m.
        // Signal A's effective duration should be capped to 2m.
        let yaml = r#"
story: truncate_test
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: cpu_usage
    behavior: steady
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("2m".to_string()),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().duration.as_deref(), Some("2m"));
    }

    #[test]
    fn duration_cap_keeps_signals_within_budget() {
        // Signal A at t=0, B at t=60s. Cap is 3m.
        // Both should be included: A gets 3m, B gets 2m.
        let yaml = r#"
story: budget_test
duration: 10m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
  - metric: backup_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 120s
    after: interface_oper_state < 1
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("3m".to_string()),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        assert_eq!(entries.len(), 2);
        // A: duration = min(10m, 3m - 0) = 3m
        assert_eq!(entries[0].base().duration.as_deref(), Some("3m"));
        // B: duration = min(10m, 3m - 60s) = 2m
        assert_eq!(entries[1].base().duration.as_deref(), Some("2m"));
    }

    #[test]
    fn duration_cap_skips_dependent_signals_only() {
        // Story: A at offset=0, B at offset=60s. Cap=30s.
        // B should be skipped, A survives with truncated duration.
        let yaml = r#"
story: skip_dependent
duration: 10m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
  - metric: backup_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 120s
    after: interface_oper_state < 1
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("30s".to_string()),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        // Only A (offset=0) survives.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().name, "interface_oper_state");
    }

    #[test]
    fn duration_cap_no_cap_passes_duration_through() {
        // Without a duration cap, signal gets the story duration unchanged.
        let yaml = r#"
story: no_cap_test
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: cpu_usage
    behavior: steady
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().duration.as_deref(), Some("5m"));
    }

    #[test]
    fn duration_cap_per_signal_duration_respected_if_shorter() {
        // Signal has its own duration (10s) which is shorter than cap (2m).
        // Per-signal duration should be kept as-is.
        let yaml = r#"
story: per_signal_dur
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
signals:
  - metric: cpu_usage
    behavior: steady
    duration: 10s
"#;
        let config = parse_story(yaml).expect("should parse");
        let overrides = StoryOverrides {
            duration: Some("2m".to_string()),
            ..Default::default()
        };
        let entries = compile_story(&config, &overrides).expect("should compile");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().duration.as_deref(), Some("10s"));
    }

    // -----------------------------------------------------------------------
    // YAML label quoting (Fix 5)
    // -----------------------------------------------------------------------

    #[test]
    fn label_values_with_special_chars_are_quoted() {
        let yaml = r#"
story: quote_test
rate: 1
duration: 10s
encoder: { type: prometheus_text }
sink: { type: stdout }
labels:
  url: "http://example.com:8080"
  flag: "true"
signals:
  - metric: cpu_usage
    behavior: steady
"#;
        let config = parse_story(yaml).expect("should parse");
        let entries = compile_story(&config, &StoryOverrides::default()).expect("should compile");
        assert_eq!(entries.len(), 1);
        let base = entries[0].base();
        let labels = base.labels.as_ref().expect("should have labels");
        assert_eq!(labels.get("url").unwrap(), "http://example.com:8080");
        assert_eq!(labels.get("flag").unwrap(), "true");
    }

    // -----------------------------------------------------------------------
    // Sink endpoint field mapping (Fix 3)
    // -----------------------------------------------------------------------

    #[test]
    fn build_effective_sink_otlp_grpc_uses_endpoint_field() {
        let config = StoryConfig {
            story: "test".to_string(),
            description: None,
            duration: None,
            rate: None,
            encoder: None,
            sink: None,
            labels: None,
            signals: vec![],
        };
        let overrides = StoryOverrides {
            sink: Some("otlp_grpc".to_string()),
            endpoint: Some("http://localhost:4317".to_string()),
            ..Default::default()
        };
        let sink = build_effective_sink(&overrides, &config);
        let map = sink.as_mapping().expect("should be a mapping");
        // otlp_grpc should use "endpoint" field, not "url".
        assert!(map.get(Value::String("endpoint".to_string())).is_some());
        assert!(map.get(Value::String("url".to_string())).is_none());
    }

    #[test]
    fn build_effective_sink_file_uses_path_field() {
        let config = StoryConfig {
            story: "test".to_string(),
            description: None,
            duration: None,
            rate: None,
            encoder: None,
            sink: None,
            labels: None,
            signals: vec![],
        };
        let overrides = StoryOverrides {
            sink: Some("file".to_string()),
            endpoint: Some("/tmp/output.txt".to_string()),
            ..Default::default()
        };
        let sink = build_effective_sink(&overrides, &config);
        let map = sink.as_mapping().expect("should be a mapping");
        assert!(map.get(Value::String("path".to_string())).is_some());
        assert!(map.get(Value::String("url".to_string())).is_none());
    }

    #[test]
    fn build_effective_sink_kafka_uses_brokers_field() {
        let config = StoryConfig {
            story: "test".to_string(),
            description: None,
            duration: None,
            rate: None,
            encoder: None,
            sink: None,
            labels: None,
            signals: vec![],
        };
        let overrides = StoryOverrides {
            sink: Some("kafka".to_string()),
            endpoint: Some("localhost:9092".to_string()),
            ..Default::default()
        };
        let sink = build_effective_sink(&overrides, &config);
        let map = sink.as_mapping().expect("should be a mapping");
        assert!(map.get(Value::String("brokers".to_string())).is_some());
        assert!(map.get(Value::String("url".to_string())).is_none());
    }

    // -----------------------------------------------------------------------
    // format_duration_human
    // -----------------------------------------------------------------------

    #[test]
    fn format_duration_human_seconds() {
        assert_eq!(format_duration_human(30.0), "30s");
    }

    #[test]
    fn format_duration_human_minutes_and_seconds() {
        assert_eq!(format_duration_human(152.0), "2m32s");
    }

    #[test]
    fn format_duration_human_exact_minutes() {
        assert_eq!(format_duration_human(60.0), "1m");
    }

    #[test]
    fn format_duration_human_zero() {
        // Sub-second rounding to 0.
        assert_eq!(format_duration_human(0.0), "0ms");
    }

    // -----------------------------------------------------------------------
    // compute_signal_duration
    // -----------------------------------------------------------------------

    #[test]
    fn compute_signal_duration_no_cap() {
        let result = compute_signal_duration(None, Some("5m"), None, 0.0);
        assert_eq!(result.as_deref(), Some("5m"));
    }

    #[test]
    fn compute_signal_duration_cap_truncates() {
        // Story duration 5m, cap 2m, offset 0 -> effective = 2m.
        let result = compute_signal_duration(Some("5m"), Some("5m"), Some(120.0), 0.0);
        assert_eq!(result.as_deref(), Some("2m"));
    }

    #[test]
    fn compute_signal_duration_cap_with_offset() {
        // Cap 3m, offset 60s -> remaining = 2m.
        let result = compute_signal_duration(Some("5m"), Some("5m"), Some(180.0), 60.0);
        assert_eq!(result.as_deref(), Some("2m"));
    }

    #[test]
    fn compute_signal_duration_per_signal_shorter_than_cap() {
        // Per-signal 10s, cap 2m, offset 0 -> keep 10s.
        let result = compute_signal_duration(Some("10s"), Some("5m"), Some(120.0), 0.0);
        assert_eq!(result.as_deref(), Some("10s"));
    }

    #[test]
    fn compute_signal_duration_no_explicit_uses_remaining() {
        // No per-signal or story duration string, but cap is 90s, offset 30s.
        let result = compute_signal_duration(None, None, Some(90.0), 30.0);
        assert_eq!(result.as_deref(), Some("1m"));
    }
}
