//! YAML scenario generation from detected patterns.
//!
//! Converts pattern detection results into valid sonda scenario YAML that
//! uses operational vocabulary aliases (`steady`, `spike_event`, `leak`,
//! `saturation`, `flap`) or base generators (`sawtooth`, `step`) where
//! appropriate.
//!
//! The generated YAML is designed to be loadable by `sonda run --scenario`.

use std::collections::HashMap;

use super::csv_reader::ColumnMeta;
use super::pattern::Pattern;
use crate::yaml_helpers::{format_float, format_rate, needs_quoting, ParamValue};

/// A single scenario derived from one CSV column.
#[derive(Debug)]
pub struct ScenarioSpec {
    /// Metric name for this scenario.
    pub name: String,
    /// Generator type name (operational alias or base generator).
    pub generator_type: String,
    /// Generator parameters as key-value pairs.
    pub generator_params: Vec<(String, ParamValue)>,
    /// Static labels for this scenario.
    pub labels: HashMap<String, String>,
}

/// Convert a pattern and column metadata into a scenario specification.
///
/// The `rate` parameter is needed to convert point-based durations into
/// time-based durations for generator parameters. The `duration` string
/// (e.g., `"60s"`, `"5m"`) is forwarded to patterns like `Climb` that
/// map to the `leak` alias, where `time_to_ceiling` must be >= the
/// scenario duration to pass sonda-core validation.
pub fn pattern_to_spec(
    pattern: &Pattern,
    meta: &ColumnMeta,
    rate: f64,
    duration: &str,
) -> ScenarioSpec {
    let name = meta
        .metric_name
        .clone()
        .unwrap_or_else(|| format!("column_{}", meta.index));

    let labels = meta.labels.clone();

    let (generator_type, generator_params) = match pattern {
        Pattern::Steady { center, amplitude } => {
            // 60s is a reasonable default period for steady oscillation:
            // it matches common scrape intervals (Prometheus default 15-60s)
            // and produces visually smooth waves at typical emission rates.
            // The pattern detector does not estimate period from the data
            // because a steady signal's frequency is not its defining trait.
            let period = "60s".to_string();
            let params = vec![
                ("center".to_string(), ParamValue::Float(*center)),
                ("amplitude".to_string(), ParamValue::Float(*amplitude)),
                ("period".to_string(), ParamValue::String(period)),
            ];
            ("steady".to_string(), params)
        }
        Pattern::Spike {
            baseline,
            spike_height,
            spike_duration_points,
            spike_interval_points,
        } => {
            let spike_dur_secs = points_to_duration(*spike_duration_points, rate);
            let spike_int_secs = points_to_duration(*spike_interval_points, rate);
            let params = vec![
                ("baseline".to_string(), ParamValue::Float(*baseline)),
                ("spike_height".to_string(), ParamValue::Float(*spike_height)),
                (
                    "spike_duration".to_string(),
                    ParamValue::String(format_duration(spike_dur_secs)),
                ),
                (
                    "spike_interval".to_string(),
                    ParamValue::String(format_duration(spike_int_secs)),
                ),
            ];
            ("spike_event".to_string(), params)
        }
        Pattern::Climb { baseline, ceiling } => {
            // Use "leak" alias: one-way ramp to ceiling. The leak alias
            // desugaring in sonda-core validates that time_to_ceiling >=
            // scenario duration, so we set it to match the scenario
            // duration exactly (the ramp fills the entire run).
            let params = vec![
                ("baseline".to_string(), ParamValue::Float(*baseline)),
                ("ceiling".to_string(), ParamValue::Float(*ceiling)),
                (
                    "time_to_ceiling".to_string(),
                    ParamValue::String(duration.to_string()),
                ),
            ];
            ("leak".to_string(), params)
        }
        Pattern::Sawtooth {
            min,
            max,
            period_points,
        } => {
            let period_secs = points_to_duration(*period_points, rate);
            let params = vec![
                ("min".to_string(), ParamValue::Float(*min)),
                ("max".to_string(), ParamValue::Float(*max)),
                ("period_secs".to_string(), ParamValue::Float(period_secs)),
            ];
            ("sawtooth".to_string(), params)
        }
        Pattern::Flap {
            up_value,
            down_value,
            up_duration_points,
            down_duration_points,
        } => {
            let up_dur_secs = points_to_duration(*up_duration_points, rate);
            let down_dur_secs = points_to_duration(*down_duration_points, rate);
            let params = vec![
                ("up_value".to_string(), ParamValue::Float(*up_value)),
                ("down_value".to_string(), ParamValue::Float(*down_value)),
                (
                    "up_duration".to_string(),
                    ParamValue::String(format_duration(up_dur_secs)),
                ),
                (
                    "down_duration".to_string(),
                    ParamValue::String(format_duration(down_dur_secs)),
                ),
            ];
            ("flap".to_string(), params)
        }
        Pattern::Step { start, step_size } => {
            let params = vec![
                ("start".to_string(), ParamValue::Float(*start)),
                ("step_size".to_string(), ParamValue::Float(*step_size)),
            ];
            ("step".to_string(), params)
        }
    };

    ScenarioSpec {
        name,
        generator_type,
        generator_params,
        labels,
    }
}

/// Convert data points to a duration in seconds, given the emission rate.
fn points_to_duration(points: usize, rate: f64) -> f64 {
    if rate <= 0.0 {
        return points as f64;
    }
    points as f64 / rate
}

/// Format a duration in seconds as a human-readable string.
///
/// Uses the smallest whole unit that expresses the value without fractions:
/// - "Xs" for whole seconds
/// - "Xm" for values that divide evenly into minutes
/// - Falls back to "X.Ys" for fractional seconds
fn format_duration(secs: f64) -> String {
    if secs <= 0.0 {
        return "1s".to_string();
    }

    let rounded = (secs * 10.0).round() / 10.0;

    if rounded >= 60.0 && (rounded % 60.0).abs() < 0.01 {
        format!("{}m", (rounded / 60.0).round() as u64)
    } else if (rounded - rounded.round()).abs() < 0.01 {
        format!("{}s", rounded.round() as u64)
    } else {
        format!("{rounded:.1}s")
    }
}

/// Render a complete scenario YAML string from a list of scenario specs.
///
/// Always produces a v2 scenario file (`version: 2`) with a `defaults:`
/// block carrying the shared rate/duration/encoder/sink plus a
/// `scenarios:` list where each entry is a `metrics` signal. v1 shapes
/// are never emitted — the unified loader only accepts v2.
pub fn render_yaml(specs: &[ScenarioSpec], rate: f64, duration: &str) -> String {
    if specs.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(specs.len() * 512 + 256);
    out.push_str("version: 2\n");
    out.push('\n');
    out.push_str("defaults:\n");
    out.push_str(&format!("  rate: {}\n", format_rate(rate)));
    out.push_str(&format!("  duration: {duration}\n"));
    out.push_str("  encoder:\n");
    out.push_str("    type: prometheus_text\n");
    out.push_str("  sink:\n");
    out.push_str("    type: stdout\n");
    out.push('\n');
    out.push_str("scenarios:\n");

    for spec in specs {
        out.push_str(&format!("  - id: {}\n", spec.name));
        out.push_str("    signal_type: metrics\n");
        out.push_str(&format!("    name: {}\n", spec.name));

        render_generator(&mut out, spec, 4);

        if !spec.labels.is_empty() {
            render_labels(&mut out, &spec.labels, 4);
        }

        out.push('\n');
    }

    out
}

/// Render the generator block for a scenario spec.
fn render_generator(out: &mut String, spec: &ScenarioSpec, indent: usize) {
    let pad = " ".repeat(indent);
    out.push_str(&format!("{pad}generator:\n"));
    out.push_str(&format!("{pad}  type: {}\n", spec.generator_type));
    for (key, value) in &spec.generator_params {
        match value {
            ParamValue::Float(v) => {
                out.push_str(&format!("{pad}  {key}: {}\n", format_float(*v)));
            }
            ParamValue::String(s) => {
                out.push_str(&format!("{pad}  {key}: \"{s}\"\n"));
            }
        }
    }
}

/// Render the labels block.
fn render_labels(out: &mut String, labels: &HashMap<String, String>, indent: usize) {
    let pad = " ".repeat(indent);
    out.push_str(&format!("{pad}labels:\n"));
    // Sort labels for deterministic output.
    let mut sorted: Vec<_> = labels.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    for (key, value) in sorted {
        // Quote values that might be misinterpreted by YAML parsers.
        if needs_quoting(value) {
            out.push_str(&format!("{pad}  {key}: \"{value}\"\n"));
        } else {
            out.push_str(&format!("{pad}  {key}: {value}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // format_duration
    // -----------------------------------------------------------------------

    #[test]
    fn format_duration_whole_seconds() {
        assert_eq!(format_duration(30.0), "30s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(120.0), "2m");
    }

    #[test]
    fn format_duration_fractional_seconds() {
        assert_eq!(format_duration(1.5), "1.5s");
    }

    #[test]
    fn format_duration_zero_returns_one_second() {
        assert_eq!(format_duration(0.0), "1s");
    }

    // -----------------------------------------------------------------------
    // pattern_to_spec: steady
    // -----------------------------------------------------------------------

    #[test]
    fn steady_pattern_to_spec_uses_alias() {
        let pattern = Pattern::Steady {
            center: 50.0,
            amplitude: 10.0,
        };
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("cpu_usage".to_string()),
            labels: HashMap::new(),
        };
        let spec = pattern_to_spec(&pattern, &meta, 1.0, "60s");
        assert_eq!(spec.name, "cpu_usage");
        assert_eq!(spec.generator_type, "steady");
    }

    #[test]
    fn spike_pattern_to_spec_uses_alias() {
        let pattern = Pattern::Spike {
            baseline: 10.0,
            spike_height: 90.0,
            spike_duration_points: 5,
            spike_interval_points: 30,
        };
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("error_rate".to_string()),
            labels: HashMap::new(),
        };
        let spec = pattern_to_spec(&pattern, &meta, 1.0, "60s");
        assert_eq!(spec.generator_type, "spike_event");
    }

    #[test]
    fn climb_pattern_to_spec_uses_leak_alias() {
        let pattern = Pattern::Climb {
            baseline: 0.0,
            ceiling: 100.0,
        };
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("mem_usage".to_string()),
            labels: HashMap::new(),
        };
        let spec = pattern_to_spec(&pattern, &meta, 1.0, "60s");
        assert_eq!(spec.generator_type, "leak");
    }

    #[test]
    fn climb_pattern_sets_time_to_ceiling_from_duration() {
        let pattern = Pattern::Climb {
            baseline: 10.0,
            ceiling: 90.0,
        };
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("mem_leak".to_string()),
            labels: HashMap::new(),
        };
        let spec = pattern_to_spec(&pattern, &meta, 1.0, "5m");
        let ttc = spec
            .generator_params
            .iter()
            .find(|(k, _)| k == "time_to_ceiling");
        assert!(ttc.is_some(), "must include time_to_ceiling param");
        match &ttc.unwrap().1 {
            ParamValue::String(s) => assert_eq!(s, "5m"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn flap_pattern_to_spec_uses_alias() {
        let pattern = Pattern::Flap {
            up_value: 1.0,
            down_value: 0.0,
            up_duration_points: 10,
            down_duration_points: 5,
        };
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("link_state".to_string()),
            labels: HashMap::new(),
        };
        let spec = pattern_to_spec(&pattern, &meta, 1.0, "60s");
        assert_eq!(spec.generator_type, "flap");
    }

    // -----------------------------------------------------------------------
    // render_yaml: single scenario
    // -----------------------------------------------------------------------

    #[test]
    fn render_single_scenario_produces_valid_v2_yaml() {
        let spec = ScenarioSpec {
            name: "cpu_usage".to_string(),
            generator_type: "steady".to_string(),
            generator_params: vec![
                ("center".to_string(), ParamValue::Float(50.0)),
                ("amplitude".to_string(), ParamValue::Float(10.0)),
                ("period".to_string(), ParamValue::String("60s".to_string())),
            ],
            labels: HashMap::new(),
        };
        let yaml = render_yaml(&[spec], 1.0, "60s");
        assert!(
            yaml.starts_with("version: 2\n"),
            "v2 output must begin with `version: 2`, got: {yaml}"
        );
        assert!(yaml.contains("defaults:"));
        assert!(yaml.contains("scenarios:"));
        assert!(yaml.contains("id: cpu_usage"));
        assert!(yaml.contains("name: cpu_usage"));
        assert!(yaml.contains("rate: 1"));
        assert!(yaml.contains("type: steady"));
        assert!(yaml.contains("center: 50.0"));
        assert!(yaml.contains("type: prometheus_text"));
        assert!(yaml.contains("type: stdout"));
    }

    // -----------------------------------------------------------------------
    // render_yaml: multi scenario
    // -----------------------------------------------------------------------

    #[test]
    fn render_multi_scenario_has_v2_header_and_defaults() {
        let specs = vec![
            ScenarioSpec {
                name: "cpu".to_string(),
                generator_type: "steady".to_string(),
                generator_params: vec![("center".to_string(), ParamValue::Float(50.0))],
                labels: HashMap::new(),
            },
            ScenarioSpec {
                name: "mem".to_string(),
                generator_type: "steady".to_string(),
                generator_params: vec![("center".to_string(), ParamValue::Float(80.0))],
                labels: HashMap::new(),
            },
        ];
        let yaml = render_yaml(&specs, 1.0, "60s");
        assert!(yaml.starts_with("version: 2\n"));
        assert!(yaml.contains("defaults:"));
        assert!(yaml.contains("scenarios:"));
        assert!(yaml.contains("signal_type: metrics"));
        assert!(yaml.contains("id: cpu"));
        assert!(yaml.contains("id: mem"));
        assert!(yaml.contains("name: cpu"));
        assert!(yaml.contains("name: mem"));
    }

    // -----------------------------------------------------------------------
    // render_yaml: labels preserved
    // -----------------------------------------------------------------------

    #[test]
    fn render_yaml_preserves_labels() {
        let mut labels = HashMap::new();
        labels.insert("instance".to_string(), "web-01".to_string());
        labels.insert("job".to_string(), "node_exporter".to_string());
        let spec = ScenarioSpec {
            name: "up".to_string(),
            generator_type: "flap".to_string(),
            generator_params: vec![
                ("up_value".to_string(), ParamValue::Float(1.0)),
                ("down_value".to_string(), ParamValue::Float(0.0)),
            ],
            labels,
        };
        let yaml = render_yaml(&[spec], 1.0, "60s");
        assert!(yaml.contains("instance: web-01"));
        assert!(yaml.contains("job: node_exporter"));
    }

    // -----------------------------------------------------------------------
    // render_yaml: empty specs
    // -----------------------------------------------------------------------

    #[test]
    fn render_yaml_empty_returns_empty() {
        assert_eq!(render_yaml(&[], 1.0, "60s"), "");
    }

    // -----------------------------------------------------------------------
    // points_to_duration
    // -----------------------------------------------------------------------

    #[test]
    fn points_to_duration_basic() {
        assert_eq!(points_to_duration(10, 1.0), 10.0);
        assert_eq!(points_to_duration(10, 2.0), 5.0);
        assert_eq!(points_to_duration(30, 0.5), 60.0);
    }

    // -----------------------------------------------------------------------
    // Determinism: same input produces same output
    // -----------------------------------------------------------------------

    #[test]
    fn render_yaml_is_deterministic() {
        let make_spec = || ScenarioSpec {
            name: "test".to_string(),
            generator_type: "steady".to_string(),
            generator_params: vec![("center".to_string(), ParamValue::Float(50.0))],
            labels: HashMap::new(),
        };
        let a = render_yaml(&[make_spec()], 1.0, "60s");
        let b = render_yaml(&[make_spec()], 1.0, "60s");
        assert_eq!(a, b);
    }
}
