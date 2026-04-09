//! YAML generation for the `sonda init` command.
//!
//! Converts the collected user answers from the interactive prompts into
//! valid, commented scenario YAML that is immediately runnable by
//! `sonda metrics --scenario <file>`, `sonda logs --scenario <file>`,
//! or `sonda run --scenario <file>`.
//!
//! Supports all sink types including advanced sinks (remote_write, loki,
//! otlp_grpc, kafka, tcp, udp) with their protocol-specific YAML fields.

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
}

/// Classifies the generated YAML so the run-now path can dispatch to the
/// correct parser without content sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitScenarioType {
    /// Single metric scenario — parse as `ScenarioConfig`.
    SingleMetric,
    /// Pack-based scenario — expand via `PackCatalog`.
    Pack,
    /// Logs scenario — parse as `LogScenarioConfig`.
    Logs,
}

impl ScenarioKind {
    /// Return the corresponding [`InitScenarioType`] for this scenario kind.
    pub fn scenario_type(&self) -> InitScenarioType {
        match self {
            ScenarioKind::SingleMetric(_) => InitScenarioType::SingleMetric,
            ScenarioKind::Pack(_) => InitScenarioType::Pack,
            ScenarioKind::Logs(_) => InitScenarioType::Logs,
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
/// The output includes inline comments explaining each section and is
/// immediately runnable by `sonda run --scenario <file>` (or the
/// signal-specific subcommand).
pub fn render_scenario_yaml(kind: &ScenarioKind, delivery: &DeliveryAnswers) -> String {
    match kind {
        ScenarioKind::SingleMetric(answers) => render_single_metric(answers, delivery),
        ScenarioKind::Pack(answers) => render_pack_scenario(answers, delivery),
        ScenarioKind::Logs(answers) => render_logs_scenario(answers, delivery),
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
    }
}

/// Render a single-metric scenario YAML with comments.
fn render_single_metric(answers: &MetricAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);

    // Header comment.
    out.push_str(&format!(
        "# {}: {} scenario using the '{}' pattern.\n",
        answers.name, delivery.domain, answers.situation
    ));
    out.push_str("#\n");
    out.push_str("# Generated by `sonda init`. Run with:\n");
    out.push_str("#   sonda metrics --scenario <this-file>\n");
    out.push_str("#   sonda run --scenario <this-file>\n");
    out.push('\n');

    // Scenario metadata.
    out.push_str("# Scenario metadata for `sonda scenarios list`.\n");
    out.push_str(&format!(
        "scenario_name: {}\n",
        answers.name.replace('_', "-")
    ));
    out.push_str(&format!("category: {}\n", delivery.domain));
    out.push_str("signal_type: metrics\n");
    out.push_str(&format!(
        "description: \"{} scenario with {} pattern\"\n",
        escape_yaml_double_quoted(&capitalize_first(&delivery.domain)),
        escape_yaml_double_quoted(&answers.situation)
    ));
    out.push('\n');

    // Core schedule fields.
    out.push_str("# Metric name emitted by this scenario.\n");
    out.push_str(&format!("name: {}\n", answers.name));
    out.push_str("# Events per second.\n");
    out.push_str(&format!("rate: {}\n", format_rate(delivery.rate)));
    out.push_str("# Total run duration. Remove to run indefinitely.\n");
    out.push_str(&format!("duration: {}\n", delivery.duration));
    out.push('\n');

    // Generator.
    out.push_str("# Value generator using the operational vocabulary.\n");
    out.push_str("# See `sonda-core/src/config/aliases.rs` for alias parameters.\n");
    out.push_str("generator:\n");
    out.push_str(&format!("  type: {}\n", answers.situation));
    for (key, value) in &answers.situation_params {
        match value {
            ParamValue::Float(v) => {
                out.push_str(&format!("  {key}: {}\n", format_float(*v)));
            }
            ParamValue::String(s) => {
                out.push_str(&format!("  {key}: \"{}\"\n", escape_yaml_double_quoted(s)));
            }
        }
    }
    out.push('\n');

    // Labels.
    if !answers.labels.is_empty() {
        out.push_str("# Static labels attached to every emitted event.\n");
        out.push_str("labels:\n");
        for (key, value) in &answers.labels {
            if needs_quoting(value) {
                out.push_str(&format!(
                    "  {key}: \"{}\"\n",
                    escape_yaml_double_quoted(value)
                ));
            } else {
                out.push_str(&format!("  {key}: {value}\n"));
            }
        }
        out.push('\n');
    }

    // Encoder.
    out.push_str("# Output encoding format.\n");
    out.push_str("encoder:\n");
    out.push_str(&format!("  type: {}\n", delivery.encoder));
    out.push('\n');

    // Sink.
    out.push_str("# Delivery destination.\n");
    render_sink(&mut out, delivery, 0);

    out
}

/// Render a pack-based scenario YAML.
fn render_pack_scenario(answers: &PackAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!(
        "# Pack-based scenario using '{}' metric pack.\n",
        answers.pack_name
    ));
    out.push_str("#\n");
    out.push_str("# Generated by `sonda init`. Run with:\n");
    out.push_str("#   sonda run --scenario <this-file>\n");
    out.push_str("#   sonda packs run <pack-name> --label key=value ...\n");
    out.push('\n');

    // Pack reference.
    out.push_str("# The pack name, resolved via the pack search path.\n");
    out.push_str(&format!("pack: {}\n", answers.pack_name));
    out.push('\n');

    // Schedule.
    out.push_str("# Events per second (applied to all metrics in the pack).\n");
    out.push_str(&format!("rate: {}\n", format_rate(delivery.rate)));
    out.push_str("# Total run duration. Remove to run indefinitely.\n");
    out.push_str(&format!("duration: {}\n", delivery.duration));
    out.push('\n');

    // Labels.
    if !answers.labels.is_empty() {
        out.push_str("# Labels applied to every metric in the pack.\n");
        out.push_str("labels:\n");
        for (key, value) in &answers.labels {
            if needs_quoting(value) {
                out.push_str(&format!(
                    "  {key}: \"{}\"\n",
                    escape_yaml_double_quoted(value)
                ));
            } else {
                out.push_str(&format!("  {key}: {value}\n"));
            }
        }
        out.push('\n');
    }

    // Encoder.
    out.push_str("# Output encoding format.\n");
    out.push_str("encoder:\n");
    out.push_str(&format!("  type: {}\n", delivery.encoder));
    out.push('\n');

    // Sink.
    out.push_str("# Delivery destination.\n");
    render_sink(&mut out, delivery, 0);

    out
}

/// Render a logs scenario YAML.
fn render_logs_scenario(answers: &LogAnswers, delivery: &DeliveryAnswers) -> String {
    let mut out = String::with_capacity(1024);

    out.push_str(&format!(
        "# Log scenario: {}.\n",
        answers.name.replace('_', " ")
    ));
    out.push_str("#\n");
    out.push_str("# Generated by `sonda init`. Run with:\n");
    out.push_str("#   sonda logs --scenario <this-file>\n");
    out.push_str("#   sonda run --scenario <this-file>\n");
    out.push('\n');

    // Scenario metadata.
    out.push_str("# Scenario metadata for `sonda scenarios list`.\n");
    out.push_str(&format!(
        "scenario_name: {}\n",
        answers.name.replace('_', "-")
    ));
    out.push_str(&format!("category: {}\n", delivery.domain));
    out.push_str("signal_type: logs\n");
    out.push_str(&format!(
        "description: \"Log scenario: {}\"\n",
        escape_yaml_double_quoted(&answers.name.replace('_', " "))
    ));
    out.push('\n');

    // Core fields.
    out.push_str(&format!("name: {}\n", answers.name));
    out.push_str(&format!("rate: {}\n", format_rate(delivery.rate)));
    out.push_str(&format!("duration: {}\n", delivery.duration));
    out.push('\n');

    // Generator.
    out.push_str("# Template-based log generator.\n");
    out.push_str("generator:\n");
    out.push_str("  type: template\n");
    out.push_str("  templates:\n");
    out.push_str(&format!(
        "    - message: \"{}\"\n",
        escape_yaml_double_quoted(&answers.message_template)
    ));
    out.push_str("      field_pools: {}\n");

    // Severity weights.
    if !answers.severity_weights.is_empty() {
        out.push_str("  severity_weights:\n");
        for (sev, weight) in &answers.severity_weights {
            out.push_str(&format!("    {sev}: {weight}\n"));
        }
    }
    out.push_str("  seed: 42\n");
    out.push('\n');

    // Labels.
    if !answers.labels.is_empty() {
        out.push_str("labels:\n");
        for (key, value) in &answers.labels {
            if needs_quoting(value) {
                out.push_str(&format!(
                    "  {key}: \"{}\"\n",
                    escape_yaml_double_quoted(value)
                ));
            } else {
                out.push_str(&format!("  {key}: {value}\n"));
            }
        }
        out.push('\n');
    }

    // Encoder.
    out.push_str("encoder:\n");
    out.push_str(&format!("  type: {}\n", delivery.encoder));
    out.push('\n');

    // Sink.
    render_sink(&mut out, delivery, 0);

    out
}

/// Render the sink block.
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

    // Map the sink to its YAML field name for the endpoint value. Sinks
    // that use `url:`, `path:`, `address:`, or `endpoint:` are grouped so
    // each pattern appears once.
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

    // Sink-specific extra fields from the advanced prompts.
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

/// Capitalize the first letter of a string.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // capitalize_first
    // -----------------------------------------------------------------------

    #[test]
    fn capitalize_first_lowercase() {
        assert_eq!(capitalize_first("infrastructure"), "Infrastructure");
    }

    #[test]
    fn capitalize_first_empty() {
        assert_eq!(capitalize_first(""), "");
    }

    // -----------------------------------------------------------------------
    // suggest_filename
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
    // render_scenario_yaml: single metric
    // -----------------------------------------------------------------------

    #[test]
    fn render_single_metric_produces_valid_yaml() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![
                ("center".to_string(), ParamValue::Float(50.0)),
                ("amplitude".to_string(), ParamValue::Float(10.0)),
                ("period".to_string(), ParamValue::String("60s".to_string())),
            ],
            labels: BTreeMap::from([
                ("instance".to_string(), "web-01".to_string()),
                ("job".to_string(), "node_exporter".to_string()),
            ]),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("name: cpu_usage"), "must contain metric name");
        assert!(
            yaml.contains("scenario_name: cpu-usage"),
            "must contain scenario name"
        );
        assert!(
            yaml.contains("category: infrastructure"),
            "must contain category"
        );
        assert!(
            yaml.contains("signal_type: metrics"),
            "must contain signal type"
        );
        assert!(yaml.contains("type: steady"), "must contain generator type");
        assert!(yaml.contains("center: 50.0"), "must contain center param");
        assert!(yaml.contains("rate: 1"), "must contain rate");
        assert!(yaml.contains("duration: 60s"), "must contain duration");
        assert!(
            yaml.contains("instance: web-01"),
            "must contain instance label"
        );
        assert!(
            yaml.contains("type: prometheus_text"),
            "must contain encoder type"
        );
        assert!(yaml.contains("type: stdout"), "must contain sink type");
        // Comments must be present.
        assert!(
            yaml.contains("# Generated by `sonda init`"),
            "must contain generation comment"
        );
    }

    #[test]
    fn render_single_metric_without_labels() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "test_metric".to_string(),
            situation: "spike_event".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 5.0,
            duration: "30s".to_string(),
            encoder: "json_lines".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(!yaml.contains("labels:"), "must not contain labels block");
        assert!(
            yaml.contains("type: spike_event"),
            "must contain spike_event"
        );
        assert!(yaml.contains("rate: 5"), "must contain rate 5");
    }

    // -----------------------------------------------------------------------
    // render_scenario_yaml: pack
    // -----------------------------------------------------------------------

    #[test]
    fn render_pack_scenario_produces_valid_yaml() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "telegraf_snmp_interface".to_string(),
            labels: BTreeMap::from([
                ("device".to_string(), "rtr-01".to_string()),
                ("ifName".to_string(), "eth0".to_string()),
            ]),
        });
        let delivery = DeliveryAnswers {
            domain: "network".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("pack: telegraf_snmp_interface"),
            "must contain pack reference"
        );
        assert!(yaml.contains("rate: 1"), "must contain rate");
        assert!(yaml.contains("duration: 60s"), "must contain duration");
        assert!(yaml.contains("device: rtr-01"), "must contain device label");
    }

    // -----------------------------------------------------------------------
    // render_scenario_yaml: logs
    // -----------------------------------------------------------------------

    #[test]
    fn render_logs_scenario_produces_valid_yaml() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_logs".to_string(),
            message_template: "Connection to {service} failed".to_string(),
            severity_weights: vec![
                ("info".to_string(), 0.7),
                ("warn".to_string(), 0.2),
                ("error".to_string(), 0.1),
            ],
            labels: BTreeMap::from([("app".to_string(), "my-service".to_string())]),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 10.0,
            duration: "60s".to_string(),
            encoder: "json_lines".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("signal_type: logs"),
            "must contain signal type logs"
        );
        assert!(
            yaml.contains("type: template"),
            "must contain template generator"
        );
        assert!(
            yaml.contains("Connection to {service} failed"),
            "must contain template"
        );
        assert!(
            yaml.contains("severity_weights:"),
            "must contain severity weights"
        );
        assert!(yaml.contains("info: 0.7"), "must contain info weight");
    }

    // -----------------------------------------------------------------------
    // render_scenario_yaml: sink with endpoint
    // -----------------------------------------------------------------------

    #[test]
    fn render_http_push_sink_includes_url() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "m".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "http_push".to_string(),
            endpoint: Some("http://localhost:9090/api/v1/write".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: http_push"), "must contain http_push");
        assert!(
            yaml.contains("url: \"http://localhost:9090/api/v1/write\""),
            "must contain url field with quoted value"
        );
    }

    #[test]
    fn render_file_sink_includes_path() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "m".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "file".to_string(),
            endpoint: Some("/tmp/output.txt".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: file"), "must contain file sink");
        assert!(
            yaml.contains(r#"path: "/tmp/output.txt""#),
            "must contain quoted file path"
        );
    }

    #[test]
    fn render_file_sink_with_spaces_produces_valid_yaml() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "m".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "file".to_string(),
            endpoint: Some("/tmp/my output dir/sonda.txt".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains(r#"path: "/tmp/my output dir/sonda.txt""#),
            "file path with spaces must be quoted"
        );
        // The generated YAML must still be parseable.
        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("YAML with spaced file path must parse");
        match &config.base.sink {
            sonda_core::sink::SinkConfig::File { path, .. } => {
                assert_eq!(path, "/tmp/my output dir/sonda.txt");
            }
            other => panic!("expected File sink, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Determinism
    // -----------------------------------------------------------------------

    #[test]
    fn render_yaml_is_deterministic() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "test".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![("center".to_string(), ParamValue::Float(50.0))],
            labels: BTreeMap::from([("a".to_string(), "1".to_string())]),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let a = render_scenario_yaml(&kind, &delivery);
        let b = render_scenario_yaml(&kind, &delivery);
        assert_eq!(a, b, "identical inputs must produce identical output");
    }

    // -----------------------------------------------------------------------
    // Parseability: generated YAML is valid sonda-core config
    // -----------------------------------------------------------------------

    #[test]
    fn rendered_steady_metric_parses_as_scenario_config() {
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
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated metric YAML must parse");
        assert_eq!(config.name, "cpu_usage");
        assert!((config.rate - 1.0).abs() < f64::EPSILON);
        assert!(config.generator.is_alias(), "steady is an alias");
    }

    #[test]
    fn rendered_spike_event_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "error_rate".to_string(),
            situation: "spike_event".to_string(),
            situation_params: vec![
                ("baseline".to_string(), ParamValue::Float(0.0)),
                ("spike_height".to_string(), ParamValue::Float(100.0)),
                (
                    "spike_duration".to_string(),
                    ParamValue::String("10s".to_string()),
                ),
                (
                    "spike_interval".to_string(),
                    ParamValue::String("30s".to_string()),
                ),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated spike_event YAML must parse");
        assert_eq!(config.name, "error_rate");
        assert!(config.generator.is_alias());
    }

    #[test]
    fn rendered_flap_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "link_state".to_string(),
            situation: "flap".to_string(),
            situation_params: vec![
                ("up_value".to_string(), ParamValue::Float(1.0)),
                ("down_value".to_string(), ParamValue::Float(0.0)),
                (
                    "up_duration".to_string(),
                    ParamValue::String("10s".to_string()),
                ),
                (
                    "down_duration".to_string(),
                    ParamValue::String("5s".to_string()),
                ),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "network".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated flap YAML must parse");
        assert_eq!(config.name, "link_state");
    }

    #[test]
    fn rendered_leak_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "mem_usage".to_string(),
            situation: "leak".to_string(),
            situation_params: vec![
                ("baseline".to_string(), ParamValue::Float(0.0)),
                ("ceiling".to_string(), ParamValue::Float(100.0)),
                (
                    "time_to_ceiling".to_string(),
                    ParamValue::String("10m".to_string()),
                ),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated leak YAML must parse");
        assert_eq!(config.name, "mem_usage");
    }

    #[test]
    fn rendered_saturation_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "disk_usage".to_string(),
            situation: "saturation".to_string(),
            situation_params: vec![
                ("baseline".to_string(), ParamValue::Float(0.0)),
                ("ceiling".to_string(), ParamValue::Float(100.0)),
                (
                    "time_to_saturate".to_string(),
                    ParamValue::String("5m".to_string()),
                ),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated saturation YAML must parse");
        assert_eq!(config.name, "disk_usage");
    }

    #[test]
    fn rendered_degradation_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "latency_p99".to_string(),
            situation: "degradation".to_string(),
            situation_params: vec![
                ("baseline".to_string(), ParamValue::Float(0.05)),
                ("ceiling".to_string(), ParamValue::Float(0.5)),
                (
                    "time_to_degrade".to_string(),
                    ParamValue::String("5m".to_string()),
                ),
                ("noise".to_string(), ParamValue::Float(0.02)),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated degradation YAML must parse");
        assert_eq!(config.name, "latency_p99");
    }

    #[test]
    fn rendered_logs_parses_as_log_scenario_config() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_logs".to_string(),
            message_template: "Connection to {service} failed".to_string(),
            severity_weights: vec![
                ("info".to_string(), 0.7),
                ("warn".to_string(), 0.2),
                ("error".to_string(), 0.1),
            ],
            labels: BTreeMap::from([("app".to_string(), "my-service".to_string())]),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 10.0,
            duration: "60s".to_string(),
            encoder: "json_lines".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::LogScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated logs YAML must parse");
        assert_eq!(config.name, "app_logs");
        assert!((config.rate - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rendered_pack_parses_as_pack_scenario_config() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "telegraf_snmp_interface".to_string(),
            labels: BTreeMap::from([
                ("device".to_string(), "rtr-01".to_string()),
                ("ifName".to_string(), "eth0".to_string()),
            ]),
        });
        let delivery = DeliveryAnswers {
            domain: "network".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::packs::PackScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("generated pack YAML must parse");
        assert_eq!(config.pack, "telegraf_snmp_interface");
        assert!((config.rate - 1.0).abs() < f64::EPSILON);
        let labels = config.labels.expect("must have labels");
        assert_eq!(labels.get("device").map(String::as_str), Some("rtr-01"));
    }

    // -----------------------------------------------------------------------
    // Edge case: labels with special characters
    // -----------------------------------------------------------------------

    #[test]
    fn render_labels_with_colon_are_quoted() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "test".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::from([("url".to_string(), "http://example.com".to_string())]),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("url: \"http://example.com\""),
            "URL label must be quoted due to colon"
        );
        // Verify it parses correctly.
        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("YAML with quoted URL must parse");
        let labels = config.base.labels.as_ref().expect("must have labels");
        assert_eq!(
            labels.get("url").map(String::as_str),
            Some("http://example.com")
        );
    }

    // -----------------------------------------------------------------------
    // Blocker 1 regression: http_push sink uses `url:` field
    // -----------------------------------------------------------------------

    #[test]
    fn rendered_http_push_sink_parses_as_scenario_config() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![
                ("center".to_string(), ParamValue::Float(50.0)),
                ("amplitude".to_string(), ParamValue::Float(10.0)),
                ("period".to_string(), ParamValue::String("60s".to_string())),
            ],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "http_push".to_string(),
            endpoint: Some("http://localhost:9090/api/v1/write".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        // Must round-trip through ScenarioConfig deserialization.
        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("http_push scenario YAML must parse");
        assert_eq!(config.name, "cpu_usage");
        match &config.base.sink {
            sonda_core::sink::SinkConfig::HttpPush { url, .. } => {
                assert_eq!(url, "http://localhost:9090/api/v1/write");
            }
            other => panic!("expected HttpPush sink, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Blocker 2 regression: user strings with special chars produce valid YAML
    // -----------------------------------------------------------------------

    #[test]
    fn rendered_log_template_with_double_quotes_parses() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "api_logs".to_string(),
            message_template: r#"Request "GET /api" completed"#.to_string(),
            severity_weights: vec![("info".to_string(), 1.0)],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "application".to_string(),
            rate: 1.0,
            duration: "30s".to_string(),
            encoder: "json_lines".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        // The generated YAML must be syntactically valid.
        let config: sonda_core::config::LogScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("log YAML with embedded quotes must parse");
        assert_eq!(config.name, "api_logs");
    }

    #[test]
    fn rendered_log_template_with_backslash_parses() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "win_logs".to_string(),
            message_template: r"File not found: C:\Users\admin\log.txt".to_string(),
            severity_weights: vec![("error".to_string(), 1.0)],
            labels: BTreeMap::new(),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "30s".to_string(),
            encoder: "json_lines".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::LogScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("log YAML with backslashes must parse");
        assert_eq!(config.name, "win_logs");
    }

    #[test]
    fn rendered_label_value_with_quotes_parses() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "test_metric".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::from([("desc".to_string(), r#"host "primary""#.to_string())]),
        });
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "30s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "stdout".to_string(),
            endpoint: None,
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        let config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("YAML with quoted label value must parse");
        let labels = config.base.labels.as_ref().expect("must have labels");
        assert_eq!(
            labels.get("desc").map(String::as_str),
            Some(r#"host "primary""#)
        );
    }

    // -----------------------------------------------------------------------
    // Advanced sinks: YAML generation
    // -----------------------------------------------------------------------

    /// Helper to build a minimal single-metric kind for sink tests.
    fn sink_test_kind() -> ScenarioKind {
        ScenarioKind::SingleMetric(MetricAnswers {
            name: "m".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        })
    }

    /// Helper to build a delivery with a specific sink configuration.
    fn sink_test_delivery(
        sink: &str,
        endpoint: Option<String>,
        extra: BTreeMap<String, String>,
    ) -> DeliveryAnswers {
        DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: sink.to_string(),
            endpoint,
            sink_extra: extra,
        }
    }

    #[test]
    fn render_remote_write_sink_includes_url() {
        let kind = sink_test_kind();
        let delivery = sink_test_delivery(
            "remote_write",
            Some("http://localhost:8428/api/v1/write".to_string()),
            BTreeMap::new(),
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("type: remote_write"),
            "must contain remote_write sink type"
        );
        assert!(
            yaml.contains("url: \"http://localhost:8428/api/v1/write\""),
            "must contain remote write URL"
        );
    }

    #[test]
    fn render_loki_sink_includes_url() {
        let kind = sink_test_kind();
        let delivery = sink_test_delivery(
            "loki",
            Some("http://localhost:3100".to_string()),
            BTreeMap::new(),
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: loki"), "must contain loki sink type");
        assert!(
            yaml.contains("url: \"http://localhost:3100\""),
            "must contain loki URL"
        );
    }

    #[test]
    fn render_otlp_grpc_sink_includes_endpoint_and_signal_type() {
        let kind = sink_test_kind();
        let mut extra = BTreeMap::new();
        extra.insert("signal_type".to_string(), "metrics".to_string());
        let delivery = sink_test_delivery(
            "otlp_grpc",
            Some("http://localhost:4317".to_string()),
            extra,
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("type: otlp_grpc"),
            "must contain otlp_grpc sink type"
        );
        assert!(
            yaml.contains("endpoint: \"http://localhost:4317\""),
            "must contain OTLP endpoint"
        );
        assert!(
            yaml.contains("signal_type: metrics"),
            "must contain OTLP signal type"
        );
    }

    #[test]
    fn render_otlp_grpc_sink_with_logs_signal_type() {
        let kind = sink_test_kind();
        let mut extra = BTreeMap::new();
        extra.insert("signal_type".to_string(), "logs".to_string());
        let delivery = sink_test_delivery(
            "otlp_grpc",
            Some("http://localhost:4317".to_string()),
            extra,
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("signal_type: logs"),
            "must contain logs signal type"
        );
    }

    #[test]
    fn render_kafka_sink_includes_brokers_and_topic() {
        let kind = sink_test_kind();
        let mut extra = BTreeMap::new();
        extra.insert("brokers".to_string(), "localhost:9092".to_string());
        extra.insert("topic".to_string(), "sonda-events".to_string());
        let delivery = sink_test_delivery("kafka", None, extra);
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: kafka"), "must contain kafka sink type");
        assert!(
            yaml.contains("brokers: \"localhost:9092\""),
            "must contain kafka brokers"
        );
        assert!(
            yaml.contains("topic: \"sonda-events\""),
            "must contain kafka topic"
        );
    }

    #[test]
    fn render_tcp_sink_includes_address() {
        let kind = sink_test_kind();
        let delivery =
            sink_test_delivery("tcp", Some("127.0.0.1:9999".to_string()), BTreeMap::new());
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: tcp"), "must contain tcp sink type");
        assert!(
            yaml.contains("address: \"127.0.0.1:9999\""),
            "must contain tcp address"
        );
    }

    #[test]
    fn render_udp_sink_includes_address() {
        let kind = sink_test_kind();
        let delivery =
            sink_test_delivery("udp", Some("127.0.0.1:9999".to_string()), BTreeMap::new());
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: udp"), "must contain udp sink type");
        assert!(
            yaml.contains("address: \"127.0.0.1:9999\""),
            "must contain udp address"
        );
    }

    #[test]
    fn render_kafka_sink_with_multiple_brokers() {
        let kind = sink_test_kind();
        let mut extra = BTreeMap::new();
        extra.insert(
            "brokers".to_string(),
            "broker1:9092,broker2:9092".to_string(),
        );
        extra.insert("topic".to_string(), "metrics".to_string());
        let delivery = sink_test_delivery("kafka", None, extra);
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("brokers: \"broker1:9092,broker2:9092\""),
            "must contain multiple kafka brokers"
        );
    }

    #[test]
    fn render_advanced_sink_in_pack_scenario() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "telegraf_snmp_interface".to_string(),
            labels: BTreeMap::from([("device".to_string(), "rtr-01".to_string())]),
        });
        let delivery = sink_test_delivery(
            "remote_write",
            Some("http://localhost:8428/api/v1/write".to_string()),
            BTreeMap::new(),
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("type: remote_write"),
            "pack scenario must support remote_write sink"
        );
        assert!(
            yaml.contains("pack: telegraf_snmp_interface"),
            "must still contain pack reference"
        );
    }

    #[test]
    fn render_loki_sink_in_logs_scenario() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "app_logs".to_string(),
            message_template: "test message".to_string(),
            severity_weights: vec![("info".to_string(), 1.0)],
            labels: BTreeMap::new(),
        });
        let delivery = sink_test_delivery(
            "loki",
            Some("http://localhost:3100".to_string()),
            BTreeMap::new(),
        );
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("type: loki"),
            "logs scenario must support loki sink"
        );
        assert!(
            yaml.contains("signal_type: logs"),
            "must contain log signal type"
        );
    }

    // -----------------------------------------------------------------------
    // required_encoder_for_sink
    // -----------------------------------------------------------------------

    #[test]
    fn required_encoder_for_remote_write_sink() {
        assert_eq!(
            required_encoder_for_sink("remote_write"),
            Some("remote_write")
        );
    }

    #[test]
    fn required_encoder_for_otlp_grpc_sink() {
        assert_eq!(required_encoder_for_sink("otlp_grpc"), Some("otlp"));
    }

    #[test]
    fn required_encoder_for_stdout_is_none() {
        assert_eq!(required_encoder_for_sink("stdout"), None);
    }

    #[test]
    fn required_encoder_for_http_push_is_none() {
        assert_eq!(required_encoder_for_sink("http_push"), None);
    }

    #[test]
    fn required_encoder_for_file_is_none() {
        assert_eq!(required_encoder_for_sink("file"), None);
    }

    #[test]
    fn required_encoder_for_loki_is_none() {
        assert_eq!(required_encoder_for_sink("loki"), None);
    }

    #[test]
    fn required_encoder_for_kafka_is_none() {
        assert_eq!(required_encoder_for_sink("kafka"), None);
    }

    #[test]
    fn required_encoder_for_tcp_is_none() {
        assert_eq!(required_encoder_for_sink("tcp"), None);
    }

    #[test]
    fn required_encoder_for_udp_is_none() {
        assert_eq!(required_encoder_for_sink("udp"), None);
    }

    // -----------------------------------------------------------------------
    // InitScenarioType: classification
    // -----------------------------------------------------------------------

    #[test]
    fn scenario_type_for_single_metric() {
        let kind = ScenarioKind::SingleMetric(MetricAnswers {
            name: "test".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        assert_eq!(kind.scenario_type(), InitScenarioType::SingleMetric);
    }

    #[test]
    fn scenario_type_for_pack() {
        let kind = ScenarioKind::Pack(PackAnswers {
            pack_name: "test_pack".to_string(),
            labels: BTreeMap::new(),
        });
        assert_eq!(kind.scenario_type(), InitScenarioType::Pack);
    }

    #[test]
    fn scenario_type_for_logs() {
        let kind = ScenarioKind::Logs(LogAnswers {
            name: "test".to_string(),
            message_template: "msg".to_string(),
            severity_weights: vec![],
            labels: BTreeMap::new(),
        });
        assert_eq!(kind.scenario_type(), InitScenarioType::Logs);
    }

    // -----------------------------------------------------------------------
    // Encoder/sink pairing: remote_write sink uses remote_write encoder
    // -----------------------------------------------------------------------

    #[test]
    fn render_remote_write_sink_with_correct_encoder_produces_valid_yaml() {
        let kind = sink_test_kind();
        // Simulate the enforced pairing: remote_write sink with remote_write encoder.
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "remote_write".to_string(),
            sink: "remote_write".to_string(),
            endpoint: Some("http://localhost:8428/api/v1/write".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(
            yaml.contains("type: remote_write"),
            "encoder must be remote_write"
        );
        // Count occurrences — encoder type and sink type should both be remote_write.
        let rw_count = yaml.matches("type: remote_write").count();
        assert_eq!(rw_count, 2, "both encoder and sink must be remote_write");
    }

    #[test]
    fn render_otlp_grpc_sink_with_correct_encoder_produces_valid_yaml() {
        let kind = sink_test_kind();
        let mut extra = BTreeMap::new();
        extra.insert("signal_type".to_string(), "metrics".to_string());
        // Simulate the enforced pairing: otlp_grpc sink with otlp encoder.
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "otlp".to_string(),
            sink: "otlp_grpc".to_string(),
            endpoint: Some("http://localhost:4317".to_string()),
            sink_extra: extra,
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        assert!(yaml.contains("type: otlp\n"), "encoder must be otlp");
        assert!(yaml.contains("type: otlp_grpc"), "sink must be otlp_grpc");
    }

    // -----------------------------------------------------------------------
    // Round-trip: encoder/sink pairing mismatches caught
    // -----------------------------------------------------------------------

    #[test]
    fn remote_write_sink_with_mismatched_encoder_has_wrong_encoder_type() {
        // This documents why the encoder override is necessary:
        // a prometheus_text encoder paired with remote_write sink produces
        // YAML that cannot work at runtime because the sink expects protobuf
        // data from the remote_write encoder.
        let kind = sink_test_kind();
        let delivery = DeliveryAnswers {
            domain: "infrastructure".to_string(),
            rate: 1.0,
            duration: "60s".to_string(),
            encoder: "prometheus_text".to_string(),
            sink: "remote_write".to_string(),
            endpoint: Some("http://localhost:8428/api/v1/write".to_string()),
            sink_extra: BTreeMap::new(),
        };
        let yaml = render_scenario_yaml(&kind, &delivery);

        // The YAML itself is syntactically valid and parses...
        let _config: sonda_core::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("YAML must parse");
        // ...but the encoder is prometheus_text, not remote_write.
        assert!(
            yaml.contains("type: prometheus_text"),
            "mismatched encoder is prometheus_text, not remote_write"
        );
        // The required_encoder_for_sink function catches this at prompt time.
        assert_eq!(
            required_encoder_for_sink("remote_write"),
            Some("remote_write"),
            "pairing logic must detect the mismatch"
        );
    }
}
