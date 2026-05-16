//! YAML generation for the `sonda init` command.
//!
//! Emits v2 scenario YAML (spec §2) for every [`InitScenarioType`] variant.
//! Every generated file starts with `version: 2`, carries a `defaults:`
//! block (shared rate / duration / encoder / sink / labels), and exposes
//! a single entry under `scenarios:`. Run-now path goes through
//! [`sonda_core::compile_scenario_file`] so the emitted YAML is
//! guaranteed to pass the same pipeline a user invoking
//! `sonda run --scenario <file>` would hit.
//!
//! Inline comments explain each section so users can hand-edit the file
//! after generation.

use std::collections::BTreeMap;

pub use crate::yaml_helpers::ParamValue;
use crate::yaml_helpers::{escape_yaml_double_quoted, format_float, format_rate, needs_quoting};

/// Collected answers for a single-metric scenario.
#[derive(Debug, Clone)]
pub struct MetricAnswers {
    /// The metric name (e.g., `"node_cpu_usage_percent"`).
    pub name: String,
    /// Operational vocabulary alias (e.g., `"steady"`, `"spike_event"`).
    pub situation: String,
    /// Situation-specific parameters as key-value pairs.
    pub situation_params: Vec<(String, ParamValue)>,
    /// Static labels attached to every event.
    pub labels: BTreeMap<String, String>,
}

/// Collected answers for a pack-based scenario.
#[derive(Debug, Clone)]
pub struct PackAnswers {
    /// The pack name (e.g., `"telegraf_snmp_interface"`).
    pub pack_name: String,
    /// Static labels applied to all metrics in the pack.
    pub labels: BTreeMap<String, String>,
}

/// Collected answers for a logs scenario.
#[derive(Debug, Clone)]
pub struct LogAnswers {
    /// Scenario name.
    pub name: String,
    /// Log message template.
    pub message_template: String,
    /// Severity weights as (name, weight) pairs.
    pub severity_weights: Vec<(String, f64)>,
    /// Static labels.
    pub labels: BTreeMap<String, String>,
}

/// Collected answers for a histogram scenario.
#[derive(Debug, Clone)]
pub struct HistogramAnswers {
    /// The metric name (e.g., `"http_request_duration_seconds"`).
    pub name: String,
    /// Distribution model type: `"normal"`, `"exponential"`, or `"uniform"`.
    pub distribution_type: String,
    /// Distribution-specific parameters as key-value pairs.
    pub distribution_params: Vec<(String, ParamValue)>,
    /// Observations per tick.
    pub observations_per_tick: u64,
    /// Bucket boundaries (`None` = use Prometheus defaults).
    pub buckets: Option<Vec<f64>>,
    /// RNG seed.
    pub seed: u64,
    /// Static labels.
    pub labels: BTreeMap<String, String>,
}

/// Collected answers for a summary scenario.
#[derive(Debug, Clone)]
pub struct SummaryAnswers {
    /// The metric name (e.g., `"rpc_duration_seconds"`).
    pub name: String,
    /// Distribution model type: `"normal"`, `"exponential"`, or `"uniform"`.
    pub distribution_type: String,
    /// Distribution-specific parameters as key-value pairs.
    pub distribution_params: Vec<(String, ParamValue)>,
    /// Observations per tick.
    pub observations_per_tick: u64,
    /// Quantile targets (`None` = use defaults `[0.5, 0.9, 0.95, 0.99]`).
    pub quantiles: Option<Vec<f64>>,
    /// RNG seed.
    pub seed: u64,
    /// Static labels.
    pub labels: BTreeMap<String, String>,
}

/// Common delivery configuration collected from the user.
#[derive(Debug, Clone)]
pub struct DeliveryAnswers {
    /// The domain/category (e.g., `"infrastructure"`).
    pub domain: String,
    /// Events per second.
    pub rate: f64,
    /// Run duration (e.g., `"60s"`).
    pub duration: String,
    /// Encoder format (e.g., `"prometheus_text"`).
    pub encoder: String,
    /// Sink type (e.g., `"stdout"`).
    pub sink: String,
    /// Sink-specific endpoint (URL, file path, host:port).
    pub endpoint: Option<String>,
    /// Additional sink-specific fields for advanced sinks.
    pub sink_extra: BTreeMap<String, String>,
}

/// The kind of scenario the user chose to build.
#[derive(Debug, Clone)]
pub enum ScenarioKind {
    /// A single metric with an operational alias.
    SingleMetric(MetricAnswers),
    /// A metric pack expanded into multiple metrics.
    Pack(PackAnswers),
    /// A logs scenario.
    Logs(LogAnswers),
    /// A histogram scenario with distribution-based observations.
    Histogram(HistogramAnswers),
    /// A summary scenario with quantile-based observations.
    Summary(SummaryAnswers),
}

/// Classifies the generated YAML so the run-now path can dispatch to the
/// correct parser without content sniffing.
///
/// All variants now emit v2 YAML; dispatch keys off this enum rather than
/// probing the file shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitScenarioType {
    /// Single metric scenario.
    SingleMetric,
    /// Pack-based scenario.
    Pack,
    /// Logs scenario.
    Logs,
    /// Histogram scenario.
    Histogram,
    /// Summary scenario.
    Summary,
}

impl ScenarioKind {
    /// Return the corresponding [`InitScenarioType`] for this scenario kind.
    pub fn scenario_type(&self) -> InitScenarioType {
        match self {
            ScenarioKind::SingleMetric(_) => InitScenarioType::SingleMetric,
            ScenarioKind::Pack(_) => InitScenarioType::Pack,
            ScenarioKind::Logs(_) => InitScenarioType::Logs,
            ScenarioKind::Histogram(_) => InitScenarioType::Histogram,
            ScenarioKind::Summary(_) => InitScenarioType::Summary,
        }
    }
}

/// Return the required encoder for a given sink, if the sink mandates a
/// specific encoder.
///
/// - `remote_write` sink requires the `remote_write` encoder.
/// - `otlp_grpc` sink requires the `otlp` encoder.
/// - All other sinks work with any user-chosen encoder.
pub fn required_encoder_for_sink(sink: &str) -> Option<&'static str> {
    match sink {
        "remote_write" => Some("remote_write"),
        "otlp_grpc" => Some("otlp"),
        _ => None,
    }
}

/// Render a complete, commented scenario YAML from the collected answers.
///
/// The output is always v2 (`version: 2` + `defaults:` + `scenarios:`). It
/// is immediately runnable via `sonda run --scenario <file>`, regardless
/// of signal type or pack-vs-inline choice.
pub fn render_scenario_yaml(kind: &ScenarioKind, delivery: &DeliveryAnswers) -> String {
    match kind {
        ScenarioKind::SingleMetric(answers) => render_single_metric(answers, delivery),
        ScenarioKind::Pack(answers) => render_pack_scenario(answers, delivery),
        ScenarioKind::Logs(answers) => render_logs_scenario(answers, delivery),
        ScenarioKind::Histogram(answers) => render_histogram_scenario(answers, delivery),
        ScenarioKind::Summary(answers) => render_summary_scenario(answers, delivery),
    }
}

/// Suggest a filename based on the scenario kind.
///
/// Returns a kebab-case filename suitable for the `scenarios/` directory.
pub fn suggest_filename(kind: &ScenarioKind) -> String {
    match kind {
        ScenarioKind::SingleMetric(answers) => {
            format!("{}.yaml", answers.name.replace('_', "-"))
        }
        ScenarioKind::Pack(answers) => {
            format!("{}.yaml", answers.pack_name.replace('_', "-"))
        }
        ScenarioKind::Logs(answers) => {
            format!("{}.yaml", answers.name.replace('_', "-"))
        }
        ScenarioKind::Histogram(answers) => {
            format!("{}.yaml", answers.name.replace('_', "-"))
        }
        ScenarioKind::Summary(answers) => {
            format!("{}.yaml", answers.name.replace('_', "-"))
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Emit a standardized v2 header comment block. The `detail` slice is
/// written as extra comment lines under the fixed preamble.
fn write_header(out: &mut String, title: &str, detail: &[&str]) {
    out.push_str(&format!("# {title}\n"));
    out.push_str("#\n");
    out.push_str("# Generated by `sonda init`. Run with:\n");
    out.push_str("#   sonda run --scenario <this-file>\n");
    for line in detail {
        out.push_str(&format!("# {line}\n"));
    }
    out.push('\n');
}

/// Emit the `defaults:` block shared by every v2 renderer.
///
/// The block carries `rate`, `duration`, `encoder`, `sink`. Labels are
/// placed at the entry level so pack and inline signals carry per-entry
/// label merge correctly (spec §2.2 precedence 3 vs 7). When
/// `include_default_labels` is true (no entry-level labels in the wizard
/// answers), labels go into defaults.
fn write_defaults_block(
    out: &mut String,
    delivery: &DeliveryAnswers,
    default_labels: &BTreeMap<String, String>,
) {
    out.push_str("# Defaults inherited by every entry in scenarios: below.\n");
    out.push_str("defaults:\n");
    out.push_str(&format!("  rate: {}\n", format_rate(delivery.rate)));
    out.push_str(&format!("  duration: {}\n", delivery.duration));
    out.push_str("  encoder:\n");
    out.push_str(&format!("    type: {}\n", delivery.encoder));
    render_sink(out, delivery, 2);
    if !default_labels.is_empty() {
        out.push_str("  labels:\n");
        for (key, value) in default_labels {
            write_label_line(out, 4, key, value);
        }
    }
    out.push('\n');
}

fn write_label_line(out: &mut String, indent: usize, key: &str, value: &str) {
    let pad = " ".repeat(indent);
    if needs_quoting(value) {
        out.push_str(&format!(
            "{pad}{key}: \"{}\"\n",
            escape_yaml_double_quoted(value)
        ));
    } else {
        out.push_str(&format!("{pad}{key}: {value}\n"));
    }
}

// ---------------------------------------------------------------------------
// Per-kind renderers
// ---------------------------------------------------------------------------

/// Render a single-metric scenario as a v2 YAML file.
fn render_single_metric(answers: &MetricAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);
    write_header(
        &mut out,
        &format!(
            "{}: {} scenario using the '{}' pattern.",
            answers.name, delivery.domain, answers.situation
        ),
        &[],
    );

    out.push_str("version: 2\nkind: runnable\n\n");
    // Labels on a single-entry file live at the entry level so CLI `--label`
    // merges work consistently; defaults.labels stays empty.
    write_defaults_block(&mut out, delivery, &BTreeMap::new());

    out.push_str("scenarios:\n");
    out.push_str("  - signal_type: metrics\n");
    out.push_str(&format!("    name: {}\n", answers.name));
    out.push_str("    generator:\n");
    out.push_str(&format!("      type: {}\n", answers.situation));
    for (key, value) in &answers.situation_params {
        match value {
            ParamValue::Float(v) => {
                out.push_str(&format!("      {key}: {}\n", format_float(*v)));
            }
            ParamValue::String(s) => {
                out.push_str(&format!(
                    "      {key}: \"{}\"\n",
                    escape_yaml_double_quoted(s)
                ));
            }
        }
    }
    if !answers.labels.is_empty() {
        out.push_str("    labels:\n");
        for (key, value) in &answers.labels {
            write_label_line(&mut out, 6, key, value);
        }
    }

    out
}

/// Render a pack-based scenario as a v2 YAML file.
///
/// Pack entries carry `signal_type: metrics` and `pack: <name>`. Labels
/// are placed at the entry level so pack-label precedence (§2.2 rule 6)
/// applies correctly.
fn render_pack_scenario(answers: &PackAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(512);
    write_header(
        &mut out,
        &format!(
            "Pack-based scenario using '{}' metric pack.",
            answers.pack_name
        ),
        &[],
    );

    out.push_str("version: 2\nkind: runnable\n\n");
    write_defaults_block(&mut out, delivery, &BTreeMap::new());

    out.push_str("scenarios:\n");
    out.push_str("  - signal_type: metrics\n");
    out.push_str(&format!("    pack: {}\n", answers.pack_name));
    if !answers.labels.is_empty() {
        out.push_str("    labels:\n");
        for (key, value) in &answers.labels {
            write_label_line(&mut out, 6, key, value);
        }
    }

    out
}

/// Render a logs scenario as a v2 YAML file.
fn render_logs_scenario(answers: &LogAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);
    write_header(
        &mut out,
        &format!("Log scenario: {}.", answers.name.replace('_', " ")),
        &[],
    );

    out.push_str("version: 2\nkind: runnable\n\n");
    write_defaults_block(&mut out, delivery, &BTreeMap::new());

    out.push_str("scenarios:\n");
    out.push_str("  - signal_type: logs\n");
    out.push_str(&format!("    name: {}\n", answers.name));
    out.push_str("    log_generator:\n");
    out.push_str("      type: template\n");
    out.push_str("      templates:\n");
    out.push_str(&format!(
        "        - message: \"{}\"\n",
        escape_yaml_double_quoted(&answers.message_template)
    ));
    out.push_str("          field_pools: {}\n");

    if !answers.severity_weights.is_empty() {
        out.push_str("      severity_weights:\n");
        for (sev, weight) in &answers.severity_weights {
            out.push_str(&format!("        {sev}: {weight}\n"));
        }
    }
    out.push_str("      seed: 42\n");

    if !answers.labels.is_empty() {
        out.push_str("    labels:\n");
        for (key, value) in &answers.labels {
            write_label_line(&mut out, 6, key, value);
        }
    }

    out
}

/// Default Prometheus histogram bucket boundaries, emitted as a comment
/// when the user accepts defaults.
const PROMETHEUS_DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Default summary quantile targets, emitted as a comment when the user
/// accepts defaults.
const DEFAULT_QUANTILES: &[f64] = &[0.5, 0.9, 0.95, 0.99];

/// Render a histogram scenario as a v2 YAML file.
fn render_histogram_scenario(answers: &HistogramAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);
    write_header(
        &mut out,
        &format!("{}: {} histogram scenario.", answers.name, delivery.domain),
        &[],
    );

    out.push_str("version: 2\nkind: runnable\n\n");
    write_defaults_block(&mut out, delivery, &BTreeMap::new());

    out.push_str("scenarios:\n");
    out.push_str("  - signal_type: histogram\n");
    out.push_str(&format!("    name: {}\n", answers.name));

    out.push_str("    distribution:\n");
    out.push_str(&format!("      type: {}\n", answers.distribution_type));
    for (key, value) in &answers.distribution_params {
        match value {
            ParamValue::Float(v) => {
                out.push_str(&format!("      {key}: {}\n", format_float(*v)));
            }
            ParamValue::String(s) => {
                out.push_str(&format!(
                    "      {key}: \"{}\"\n",
                    escape_yaml_double_quoted(s)
                ));
            }
        }
    }
    out.push_str(&format!(
        "    observations_per_tick: {}\n",
        answers.observations_per_tick
    ));
    out.push_str(&format!("    seed: {}\n", answers.seed));

    match &answers.buckets {
        Some(custom) => {
            let formatted: Vec<String> = custom.iter().map(|v| format_float(*v)).collect();
            out.push_str(&format!("    buckets: [{}]\n", formatted.join(", ")));
        }
        None => {
            let formatted: Vec<String> = PROMETHEUS_DEFAULT_BUCKETS
                .iter()
                .map(|v| format_float(*v))
                .collect();
            out.push_str(&format!(
                "    # buckets: [{}]  # (Prometheus defaults; omit to use built-in)\n",
                formatted.join(", ")
            ));
        }
    }

    if !answers.labels.is_empty() {
        out.push_str("    labels:\n");
        for (key, value) in &answers.labels {
            write_label_line(&mut out, 6, key, value);
        }
    }

    out
}

/// Render a summary scenario as a v2 YAML file.
fn render_summary_scenario(answers: &SummaryAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);
    write_header(
        &mut out,
        &format!("{}: {} summary scenario.", answers.name, delivery.domain),
        &[],
    );

    out.push_str("version: 2\nkind: runnable\n\n");
    write_defaults_block(&mut out, delivery, &BTreeMap::new());

    out.push_str("scenarios:\n");
    out.push_str("  - signal_type: summary\n");
    out.push_str(&format!("    name: {}\n", answers.name));

    out.push_str("    distribution:\n");
    out.push_str(&format!("      type: {}\n", answers.distribution_type));
    for (key, value) in &answers.distribution_params {
        match value {
            ParamValue::Float(v) => {
                out.push_str(&format!("      {key}: {}\n", format_float(*v)));
            }
            ParamValue::String(s) => {
                out.push_str(&format!(
                    "      {key}: \"{}\"\n",
                    escape_yaml_double_quoted(s)
                ));
            }
        }
    }
    out.push_str(&format!(
        "    observations_per_tick: {}\n",
        answers.observations_per_tick
    ));
    out.push_str(&format!("    seed: {}\n", answers.seed));

    match &answers.quantiles {
        Some(custom) => {
            let formatted: Vec<String> = custom.iter().map(|v| format_float(*v)).collect();
            out.push_str(&format!("    quantiles: [{}]\n", formatted.join(", ")));
        }
        None => {
            let formatted: Vec<String> =
                DEFAULT_QUANTILES.iter().map(|v| format_float(*v)).collect();
            out.push_str(&format!(
                "    # quantiles: [{}]  # (standard defaults; omit to use built-in)\n",
                formatted.join(", ")
            ));
        }
    }

    if !answers.labels.is_empty() {
        out.push_str("    labels:\n");
        for (key, value) in &answers.labels {
            write_label_line(&mut out, 6, key, value);
        }
    }

    out
}

/// Render the sink block at the given indent level.
///
/// Handles all supported sink types including advanced sinks. The `extra`
/// map carries additional fields for sinks that need more than one
/// endpoint-style parameter (e.g., kafka brokers + topic).
fn render_sink(out: &mut String, delivery: &DeliveryAnswers, indent: usize) {
    let pad = " ".repeat(indent);
    let sink = &delivery.sink;
    let endpoint = &delivery.endpoint;
    let extra = &delivery.sink_extra;

    out.push_str(&format!("{pad}sink:\n"));
    out.push_str(&format!("{pad}  type: {sink}\n"));

    let endpoint_field = match sink.as_str() {
        "http_push" | "remote_write" | "loki" => Some("url"),
        "file" => Some("path"),
        "otlp_grpc" => Some("endpoint"),
        "tcp" | "udp" => Some("address"),
        _ => None,
    };

    if let (Some(field), Some(ref ep)) = (endpoint_field, endpoint) {
        out.push_str(&format!(
            "{pad}  {field}: \"{}\"\n",
            escape_yaml_double_quoted(ep)
        ));
    }

    match sink.as_str() {
        "otlp_grpc" => {
            if let Some(signal_type) = extra.get("signal_type") {
                out.push_str(&format!("{pad}  signal_type: {signal_type}\n"));
            }
        }
        "kafka" => {
            if let Some(brokers) = extra.get("brokers") {
                out.push_str(&format!(
                    "{pad}  brokers: \"{}\"\n",
                    escape_yaml_double_quoted(brokers)
                ));
            }
            if let Some(topic) = extra.get("topic") {
                out.push_str(&format!(
                    "{pad}  topic: \"{}\"\n",
                    escape_yaml_double_quoted(topic)
                ));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout_delivery() -> DeliveryAnswers {
        DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // suggest_filename — unchanged shape
    // -----------------------------------------------------------------------

    #[test]
    fn suggest_filename_for_single_metric() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "node_cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        assert_eq!(suggest_filename(&kind), "node-cpu-usage.yaml");
    }

    #[test]
    fn suggest_filename_for_pack() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "telegraf_snmp_interface".to_string(),
            labels: BTreeMap::new(),
        });
        assert_eq!(suggest_filename(&kind), "telegraf-snmp-interface.yaml");
    }

    #[test]
    fn suggest_filename_for_logs() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_error_logs".to_string(),
            message_template: "test".to_string(),
            severity_weights: vec![],
            labels: BTreeMap::new(),
        });
        assert_eq!(suggest_filename(&kind), "app-error-logs.yaml");
    }

    // -----------------------------------------------------------------------
    // required_encoder_for_sink — unchanged from v1 codepath
    // -----------------------------------------------------------------------

    #[test]
    fn required_encoder_for_remote_write_sink() {
        assert_eq!(
            required_encoder_for_sink("remote_write"),
            Some("remote_write")
        );
    }

    #[test]
    fn required_encoder_for_otlp_sink() {
        assert_eq!(required_encoder_for_sink("otlp_grpc"), Some("otlp"));
    }

    #[test]
    fn required_encoder_for_stdout_is_none() {
        assert_eq!(required_encoder_for_sink("stdout"), None);
    }

    // -----------------------------------------------------------------------
    // v2 header invariants — every renderer emits `version: 2` + `defaults:`
    // + `scenarios:` in that order.
    // -----------------------------------------------------------------------

    fn assert_v2_shape(yaml: &str) {
        assert!(
            yaml.contains("version: 2"),
            "missing `version: 2`, got:\n{yaml}"
        );
        assert!(
            yaml.contains("kind: runnable"),
            "missing `kind: runnable`, got:\n{yaml}"
        );
        assert!(
            yaml.contains("defaults:"),
            "missing `defaults:`, got:\n{yaml}"
        );
        assert!(
            yaml.contains("scenarios:"),
            "missing `scenarios:`, got:\n{yaml}"
        );
        // Compare on the first non-comment occurrence of each marker; the
        // header comment block contains tokens like `scenarios:` as plain
        // English and must not interfere with structural ordering checks.
        let stripped: String = yaml
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let version_pos = stripped.find("version: 2").expect("has version");
        let kind_pos = stripped.find("kind: runnable").expect("has kind");
        let defaults_pos = stripped.find("defaults:").expect("has defaults");
        let scenarios_pos = stripped.find("scenarios:").expect("has scenarios");
        assert!(
            version_pos < kind_pos && kind_pos < defaults_pos && defaults_pos < scenarios_pos,
            "ordering violated: version/kind/defaults/scenarios in:\n{stripped}"
        );
    }

    #[test]
    fn single_metric_emits_v2_shape() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![
                ("center".to_string(), ParamValue::Float(50.0)),
                ("amplitude".to_string(), ParamValue::Float(10.0)),
            ],
            labels: BTreeMap::from([("instance".to_string(), "web-01".to_string())]),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        assert_v2_shape(&yaml);
        assert!(yaml.contains("signal_type: metrics"));
        assert!(yaml.contains("name: cpu_usage"));
        assert!(yaml.contains("type: steady"));
        assert!(yaml.contains("center: 50.0"));
        assert!(yaml.contains("instance: web-01"));
    }

    #[test]
    fn pack_emits_v2_shape() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "telegraf_snmp_interface".to_string(),
            labels: BTreeMap::from([("device".to_string(), "rtr-01".to_string())]),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        assert_v2_shape(&yaml);
        assert!(yaml.contains("pack: telegraf_snmp_interface"));
        assert!(yaml.contains("device: rtr-01"));
    }

    #[test]
    fn logs_emits_v2_shape() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_logs".to_string(),
            message_template: "event happened".to_string(),
            severity_weights: vec![("info".to_string(), 1.0)],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            encoder: "json_lines".to_string(),
            ..stdout_delivery()
        };
        let yaml = render_scenario_yaml(&kind, &delivery);
        assert_v2_shape(&yaml);
        assert!(yaml.contains("signal_type: logs"));
        assert!(yaml.contains("log_generator:"));
        assert!(yaml.contains("type: template"));
        assert!(yaml.contains("event happened"));
    }

    #[test]
    fn histogram_emits_v2_shape_with_buckets_comment() {
        let kind = ScenarioKind::Histogram(HistogramAnswers {
            name: "latency".to_string(),
            distribution_type: "normal".to_string(),
            distribution_params: vec![
                ("mean".to_string(), ParamValue::Float(0.2)),
                ("stddev".to_string(), ParamValue::Float(0.05)),
            ],
            observations_per_tick: 100,
            buckets: None,
            seed: 0,
            labels: BTreeMap::new(),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        assert_v2_shape(&yaml);
        assert!(yaml.contains("signal_type: histogram"));
        assert!(yaml.contains("distribution:"));
        // Prometheus defaults are shown commented.
        assert!(yaml.contains("# buckets:"));
    }

    #[test]
    fn summary_emits_v2_shape_with_quantiles_comment() {
        let kind = ScenarioKind::Summary(SummaryAnswers {
            name: "rpc_latency".to_string(),
            distribution_type: "exponential".to_string(),
            distribution_params: vec![("rate".to_string(), ParamValue::Float(5.0))],
            observations_per_tick: 50,
            quantiles: None,
            seed: 7,
            labels: BTreeMap::new(),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        assert_v2_shape(&yaml);
        assert!(yaml.contains("signal_type: summary"));
        assert!(yaml.contains("# quantiles:"));
        assert!(yaml.contains("seed: 7"));
    }

    // -----------------------------------------------------------------------
    // Round-trip: the emitted YAML must compile via compile_scenario_file
    // -----------------------------------------------------------------------

    #[test]
    fn single_metric_output_compiles_via_compile_scenario_file() {
        use sonda_core::compile_scenario_file;
        use sonda_core::compiler::expand::InMemoryPackResolver;

        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![
                ("center".to_string(), ParamValue::Float(50.0)),
                ("amplitude".to_string(), ParamValue::Float(10.0)),
                ("period".to_string(), ParamValue::String("60s".to_string())),
            ],
            labels: BTreeMap::from([("instance".to_string(), "web-01".to_string())]),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        let entries = compile_scenario_file(&yaml, &InMemoryPackResolver::new())
            .unwrap_or_else(|e| panic!("emitted YAML must compile, got: {e}\n---\n{yaml}"));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn logs_output_compiles_via_compile_scenario_file() {
        use sonda_core::compile_scenario_file;
        use sonda_core::compiler::expand::InMemoryPackResolver;

        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_logs".to_string(),
            message_template: "event {id}".to_string(),
            severity_weights: vec![("info".to_string(), 0.8), ("error".to_string(), 0.2)],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            encoder: "json_lines".to_string(),
            ..stdout_delivery()
        };
        let yaml = render_scenario_yaml(&kind, &delivery);
        compile_scenario_file(&yaml, &InMemoryPackResolver::new())
            .unwrap_or_else(|e| panic!("emitted logs YAML must compile, got: {e}\n---\n{yaml}"));
    }

    #[test]
    fn histogram_output_compiles_via_compile_scenario_file() {
        use sonda_core::compile_scenario_file;
        use sonda_core::compiler::expand::InMemoryPackResolver;

        let kind = ScenarioKind::Histogram(HistogramAnswers {
            name: "latency".to_string(),
            distribution_type: "normal".to_string(),
            distribution_params: vec![
                ("mean".to_string(), ParamValue::Float(0.2)),
                ("stddev".to_string(), ParamValue::Float(0.05)),
            ],
            observations_per_tick: 100,
            buckets: Some(vec![0.1, 0.5, 1.0]),
            seed: 0,
            labels: BTreeMap::new(),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        compile_scenario_file(&yaml, &InMemoryPackResolver::new()).unwrap_or_else(|e| {
            panic!("emitted histogram YAML must compile, got: {e}\n---\n{yaml}")
        });
    }

    #[test]
    fn summary_output_compiles_via_compile_scenario_file() {
        use sonda_core::compile_scenario_file;
        use sonda_core::compiler::expand::InMemoryPackResolver;

        let kind = ScenarioKind::Summary(SummaryAnswers {
            name: "rpc_latency".to_string(),
            distribution_type: "exponential".to_string(),
            distribution_params: vec![("rate".to_string(), ParamValue::Float(5.0))],
            observations_per_tick: 50,
            quantiles: Some(vec![0.5, 0.9, 0.99]),
            seed: 7,
            labels: BTreeMap::new(),
        });
        let yaml = render_scenario_yaml(&kind, &stdout_delivery());
        compile_scenario_file(&yaml, &InMemoryPackResolver::new())
            .unwrap_or_else(|e| panic!("emitted summary YAML must compile, got: {e}\n---\n{yaml}"));
    }
}
