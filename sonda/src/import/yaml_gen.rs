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

/// A parameter value that formats appropriately in YAML.
#[derive(Debug, Clone)]
pub enum ParamValue {
    /// A floating-point number.
    Float(f64),
    /// A quoted string (e.g., duration like "10s").
    String(String),
}

/// Convert a pattern and column metadata into a scenario specification.
///
/// The `rate` parameter is needed to convert point-based durations into
/// time-based durations for generator parameters.
pub fn pattern_to_spec(pattern: &Pattern, meta: &ColumnMeta, rate: f64) -> ScenarioSpec {
    let name = meta
        .metric_name
        .clone()
        .unwrap_or_else(|| format!("column_{}", meta.index));

    let labels = meta.labels.clone();

    let (generator_type, generator_params) = match pattern {
        Pattern::Steady { center, amplitude } => {
            let period = format!("{}s", 60); // default 60s period
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
            // Use "leak" alias: one-way ramp, default 10m to ceiling.
            let params = vec![
                ("baseline".to_string(), ParamValue::Float(*baseline)),
                ("ceiling".to_string(), ParamValue::Float(*ceiling)),
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
/// When `specs` has a single entry, produces a flat scenario YAML (no
/// `scenarios:` wrapper). When multiple entries exist, produces a
/// `scenarios:` list suitable for `sonda run --scenario`.
pub fn render_yaml(specs: &[ScenarioSpec], rate: f64, duration: &str) -> String {
    if specs.is_empty() {
        return String::new();
    }

    if specs.len() == 1 {
        render_single_scenario(&specs[0], rate, duration)
    } else {
        render_multi_scenario(specs, rate, duration)
    }
}

/// Render a single flat scenario YAML.
fn render_single_scenario(spec: &ScenarioSpec, rate: f64, duration: &str) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!("name: {}\n", spec.name));
    out.push_str(&format!("rate: {}\n", format_rate(rate)));
    out.push_str(&format!("duration: {duration}\n"));
    out.push('\n');

    render_generator(&mut out, spec, 0);
    out.push('\n');

    if !spec.labels.is_empty() {
        render_labels(&mut out, &spec.labels, 0);
        out.push('\n');
    }

    out.push_str("encoder:\n");
    out.push_str("  type: prometheus_text\n");
    out.push('\n');
    out.push_str("sink:\n");
    out.push_str("  type: stdout\n");

    out
}

/// Render a multi-scenario YAML with `scenarios:` wrapper.
fn render_multi_scenario(specs: &[ScenarioSpec], rate: f64, duration: &str) -> String {
    let mut out = String::with_capacity(specs.len() * 512);
    out.push_str("scenarios:\n");

    for spec in specs {
        out.push_str("  - signal_type: metrics\n");
        out.push_str(&format!("    name: {}\n", spec.name));
        out.push_str(&format!("    rate: {}\n", format_rate(rate)));
        out.push_str(&format!("    duration: {duration}\n"));
        out.push('\n');

        render_generator(&mut out, spec, 4);
        out.push('\n');

        if !spec.labels.is_empty() {
            render_labels(&mut out, &spec.labels, 4);
            out.push('\n');
        }

        out.push_str("    encoder:\n");
        out.push_str("      type: prometheus_text\n");
        out.push('\n');
        out.push_str("    sink:\n");
        out.push_str("      type: stdout\n");
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

/// Check if a YAML value needs quoting to be parsed correctly.
fn needs_quoting(value: &str) -> bool {
    // Quote if: empty, looks numeric, contains special chars, or is a YAML keyword.
    if value.is_empty() {
        return true;
    }
    if value.parse::<f64>().is_ok() {
        return true;
    }
    let lower = value.to_lowercase();
    if lower == "true" || lower == "false" || lower == "null" || lower == "yes" || lower == "no" {
        return true;
    }
    if value.contains(':') || value.contains('#') || value.contains('{') || value.contains('}') {
        return true;
    }
    false
}

/// Format a float nicely: avoid unnecessary trailing zeros.
fn format_float(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{:.1}", v) // e.g., 50.0
    } else {
        format!("{}", v) // full precision
    }
}

/// Format a rate value, using integer form for whole numbers.
fn format_rate(rate: f64) -> String {
    if rate == rate.trunc() && rate >= 1.0 {
        format!("{}", rate as u64)
    } else {
        format!("{}", rate)
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
    // format_float
    // -----------------------------------------------------------------------

    #[test]
    fn format_float_integer() {
        assert_eq!(format_float(50.0), "50.0");
    }

    #[test]
    fn format_float_fractional() {
        assert_eq!(format_float(3.14159), "3.14159");
    }

    // -----------------------------------------------------------------------
    // needs_quoting
    // -----------------------------------------------------------------------

    #[test]
    fn needs_quoting_for_numbers() {
        assert!(needs_quoting("42"));
        assert!(needs_quoting("3.14"));
    }

    #[test]
    fn needs_quoting_for_booleans() {
        assert!(needs_quoting("true"));
        assert!(needs_quoting("false"));
    }

    #[test]
    fn no_quoting_for_plain_strings() {
        assert!(!needs_quoting("web-01"));
        assert!(!needs_quoting("node_exporter"));
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
        let spec = pattern_to_spec(&pattern, &meta, 1.0);
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
        let spec = pattern_to_spec(&pattern, &meta, 1.0);
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
        let spec = pattern_to_spec(&pattern, &meta, 1.0);
        assert_eq!(spec.generator_type, "leak");
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
        let spec = pattern_to_spec(&pattern, &meta, 1.0);
        assert_eq!(spec.generator_type, "flap");
    }

    // -----------------------------------------------------------------------
    // render_yaml: single scenario
    // -----------------------------------------------------------------------

    #[test]
    fn render_single_scenario_produces_valid_yaml() {
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
        assert!(yaml.contains("name: cpu_usage"));
        assert!(yaml.contains("rate: 1"));
        assert!(yaml.contains("type: steady"));
        assert!(yaml.contains("center: 50.0"));
        assert!(yaml.contains("type: prometheus_text"));
        assert!(yaml.contains("type: stdout"));
        // Single scenario should NOT have a scenarios: wrapper.
        assert!(!yaml.contains("scenarios:"));
    }

    // -----------------------------------------------------------------------
    // render_yaml: multi scenario
    // -----------------------------------------------------------------------

    #[test]
    fn render_multi_scenario_has_wrapper() {
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
        assert!(yaml.contains("scenarios:"));
        assert!(yaml.contains("signal_type: metrics"));
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
