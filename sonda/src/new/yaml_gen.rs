//! YAML rendering for `sonda new`.

use std::collections::HashMap;
use std::fmt::Write as _;

use sonda_core::analysis::pattern::Pattern;

use super::csv_reader::ColumnMeta;
use super::prompts::{Answers, SignalKind, SinkKind};

pub enum ParamValue {
    Float(f64),
    String(String),
}

pub struct ScenarioSpec {
    pub name: String,
    pub generator_type: String,
    pub generator_params: Vec<(String, ParamValue)>,
    pub labels: HashMap<String, String>,
}

pub fn minimal_template() -> String {
    "version: 2\nkind: runnable\n\
defaults:\n  rate: 1\n  duration: 60s\n  encoder:\n    type: prometheus_text\n  sink:\n    type: stdout\n\n\
scenarios:\n  - id: example\n    signal_type: metrics\n    name: example_metric\n    generator:\n      type: constant\n      value: 1.0\n"
        .to_string()
}

pub fn spec_from_pattern(
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

    let (generator_type, params): (&str, Vec<(&str, ParamValue)>) = match pattern {
        Pattern::Steady { center, amplitude } => (
            "steady",
            vec![
                ("center", ParamValue::Float(*center)),
                ("amplitude", ParamValue::Float(*amplitude)),
                ("period", ParamValue::String("60s".to_string())),
            ],
        ),
        Pattern::Spike {
            baseline,
            spike_height,
            spike_duration_points,
            spike_interval_points,
        } => (
            "spike_event",
            vec![
                ("baseline", ParamValue::Float(*baseline)),
                ("spike_height", ParamValue::Float(*spike_height)),
                (
                    "spike_duration",
                    ParamValue::String(format_duration(points_to_secs(
                        *spike_duration_points,
                        rate,
                    ))),
                ),
                (
                    "spike_interval",
                    ParamValue::String(format_duration(points_to_secs(
                        *spike_interval_points,
                        rate,
                    ))),
                ),
            ],
        ),
        Pattern::Climb { baseline, ceiling } => (
            "leak",
            vec![
                ("baseline", ParamValue::Float(*baseline)),
                ("ceiling", ParamValue::Float(*ceiling)),
                ("time_to_ceiling", ParamValue::String(duration.to_string())),
            ],
        ),
        Pattern::Sawtooth {
            min,
            max,
            period_points,
        } => (
            "sawtooth",
            vec![
                ("min", ParamValue::Float(*min)),
                ("max", ParamValue::Float(*max)),
                (
                    "period_secs",
                    ParamValue::Float(points_to_secs(*period_points, rate)),
                ),
            ],
        ),
        Pattern::Flap {
            up_value,
            down_value,
            up_duration_points,
            down_duration_points,
        } => (
            "flap",
            vec![
                ("up_value", ParamValue::Float(*up_value)),
                ("down_value", ParamValue::Float(*down_value)),
                (
                    "up_duration",
                    ParamValue::String(format_duration(points_to_secs(*up_duration_points, rate))),
                ),
                (
                    "down_duration",
                    ParamValue::String(format_duration(points_to_secs(
                        *down_duration_points,
                        rate,
                    ))),
                ),
            ],
        ),
        Pattern::Step { start, step_size } => (
            "step",
            vec![
                ("start", ParamValue::Float(*start)),
                ("step_size", ParamValue::Float(*step_size)),
            ],
        ),
    };

    ScenarioSpec {
        name,
        generator_type: generator_type.to_string(),
        generator_params: params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        labels,
    }
}

pub fn render_v2(specs: &[ScenarioSpec], rate: f64, duration: &str) -> String {
    let mut out = String::new();
    write_header(&mut out, rate, duration, "prometheus_text", "stdout", None);
    out.push_str("scenarios:\n");
    for spec in specs {
        let _ = writeln!(out, "  - id: {}", spec.name);
        out.push_str("    signal_type: metrics\n");
        let _ = writeln!(out, "    name: {}", spec.name);
        out.push_str("    generator:\n");
        let _ = writeln!(out, "      type: {}", spec.generator_type);
        for (k, v) in &spec.generator_params {
            write_param(&mut out, "      ", k, v);
        }
        if !spec.labels.is_empty() {
            out.push_str("    labels:\n");
            let mut sorted: Vec<_> = spec.labels.iter().collect();
            sorted.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in sorted {
                let _ = writeln!(out, "      {k}: {v}");
            }
        }
        out.push('\n');
    }
    out
}

pub fn render_from_answers(answers: &Answers) -> String {
    let mut out = String::new();
    let encoder = match answers.signal {
        SignalKind::Logs => "json_lines",
        _ => "prometheus_text",
    };
    let (sink_kind, sink_endpoint) = match &answers.sink {
        SinkKind::Stdout => ("stdout", None),
        SinkKind::File { path } => ("file", Some(("path", path.as_str()))),
        SinkKind::HttpPush { endpoint } => ("http_push", Some(("endpoint", endpoint.as_str()))),
    };
    write_header(
        &mut out,
        answers.rate,
        &answers.duration,
        encoder,
        sink_kind,
        sink_endpoint,
    );
    out.push_str("scenarios:\n");
    let _ = writeln!(out, "  - id: {}", answers.id);
    let _ = writeln!(out, "    signal_type: {}", answers.signal.as_str());
    match answers.signal {
        SignalKind::Metrics => {
            let _ = writeln!(out, "    name: {}", answers.id);
            out.push_str("    generator:\n");
            let _ = writeln!(out, "      type: {}", answers.generator_type);
            if answers.generator_type == "constant" {
                out.push_str("      value: 1.0\n");
            }
        }
        SignalKind::Logs => {
            let _ = writeln!(out, "    name: {}", answers.id);
            out.push_str("    log_generator:\n      type: template\n      templates:\n        - message: \"example log line\"\n          severity: info\n");
        }
        SignalKind::Histogram => {
            let _ = writeln!(out, "    name: {}", answers.id);
            out.push_str(
                "    distribution:\n      type: uniform\n      min: 0.0\n      max: 1.0\n",
            );
            out.push_str("    observations_per_tick: 10\n    buckets: [0.1, 0.5, 1.0]\n");
        }
        SignalKind::Summary => {
            let _ = writeln!(out, "    name: {}", answers.id);
            out.push_str(
                "    distribution:\n      type: uniform\n      min: 0.0\n      max: 1.0\n",
            );
            out.push_str("    observations_per_tick: 10\n    quantiles: [0.5, 0.9, 0.99]\n");
        }
    }
    out
}

fn write_header(
    out: &mut String,
    rate: f64,
    duration: &str,
    encoder: &str,
    sink_kind: &str,
    sink_extra: Option<(&str, &str)>,
) {
    out.push_str("version: 2\nkind: runnable\n\ndefaults:\n");
    let _ = writeln!(out, "  rate: {}", format_rate(rate));
    let _ = writeln!(out, "  duration: {duration}");
    out.push_str("  encoder:\n");
    let _ = writeln!(out, "    type: {encoder}");
    out.push_str("  sink:\n");
    let _ = writeln!(out, "    type: {sink_kind}");
    if let Some((k, v)) = sink_extra {
        let _ = writeln!(out, "    {k}: {v}");
    }
    out.push('\n');
}

fn write_param(out: &mut String, indent: &str, key: &str, value: &ParamValue) {
    match value {
        ParamValue::Float(v) => {
            let _ = writeln!(out, "{indent}{key}: {}", format_float(*v));
        }
        ParamValue::String(s) => {
            let _ = writeln!(out, "{indent}{key}: \"{s}\"");
        }
    }
}

fn points_to_secs(points: usize, rate: f64) -> f64 {
    if rate <= 0.0 {
        points as f64
    } else {
        points as f64 / rate
    }
}

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

fn format_float(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn format_rate(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_template_compiles_via_v2_pipeline() {
        let yaml = minimal_template();
        let resolver = sonda_core::compiler::expand::InMemoryPackResolver::new();
        sonda_core::compile_scenario_file(&yaml, &resolver).expect("must compile");
    }

    #[test]
    fn spec_from_steady_pattern_uses_alias() {
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("cpu".to_string()),
            labels: HashMap::new(),
        };
        let spec = spec_from_pattern(
            &Pattern::Steady {
                center: 50.0,
                amplitude: 10.0,
            },
            &meta,
            1.0,
            "60s",
        );
        assert_eq!(spec.generator_type, "steady");
        assert_eq!(spec.name, "cpu");
    }

    #[test]
    fn spec_from_spike_pattern_uses_alias() {
        let meta = ColumnMeta {
            index: 1,
            metric_name: Some("err".to_string()),
            labels: HashMap::new(),
        };
        let spec = spec_from_pattern(
            &Pattern::Spike {
                baseline: 10.0,
                spike_height: 90.0,
                spike_duration_points: 5,
                spike_interval_points: 30,
            },
            &meta,
            1.0,
            "60s",
        );
        assert_eq!(spec.generator_type, "spike_event");
    }

    #[test]
    fn render_v2_emits_v2_header_and_runnable_kind() {
        let spec = ScenarioSpec {
            name: "cpu".to_string(),
            generator_type: "steady".to_string(),
            generator_params: vec![("center".to_string(), ParamValue::Float(50.0))],
            labels: HashMap::new(),
        };
        let yaml = render_v2(&[spec], 1.0, "60s");
        assert!(yaml.starts_with("version: 2\n"));
        assert!(yaml.contains("kind: runnable"));
        assert!(yaml.contains("id: cpu"));
    }
}
