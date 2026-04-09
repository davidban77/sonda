//! Interactive prompt logic for `sonda init`.
//!
//! Uses `dialoguer` for terminal prompts. Every prompt has a sensible default.
//! Questions use operational language ("What situation?"), not generator
//! internals ("sawtooth period").
//!
//! Prompt groups are visually separated by styled section headers with step
//! indicators so the user knows where they are in the flow.

use std::collections::BTreeMap;
use std::io;

use dialoguer::{theme::ColorfulTheme, Input, Select};
use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use crate::packs::PackCatalog;

use super::yaml_gen::{
    DeliveryAnswers, LogAnswers, MetricAnswers, PackAnswers, ParamValue, ScenarioKind,
};

/// Available operational vocabulary aliases for metric scenarios.
///
/// These match the aliases defined in `sonda-core/src/config/aliases.rs`.
const SITUATIONS: &[&str] = &[
    "steady",
    "spike_event",
    "flap",
    "leak",
    "saturation",
    "degradation",
];

/// Human-readable descriptions for each situation.
const SITUATION_DESCRIPTIONS: &[&str] = &[
    "steady       - stable value with gentle oscillation and noise",
    "spike_event  - baseline with periodic spikes (anomaly testing)",
    "flap         - value toggling between two states (up/down)",
    "leak         - gradual climb to a ceiling (memory leak)",
    "saturation   - repeating fill-and-reset cycles",
    "degradation  - slow ramp with increasing noise",
];

/// Section header width for the styled horizontal rule.
///
/// Shared with `mod.rs` so the welcome banner and section headers use
/// consistent widths.
pub const SECTION_WIDTH: usize = 45;

/// Available metric encoder formats.
const METRIC_ENCODERS: &[&str] = &["prometheus_text", "influx_lp", "json_lines"];

/// Available log encoder formats.
const LOG_ENCODERS: &[&str] = &["json_lines", "syslog"];

/// Available sink types for the scaffolding wizard.
///
/// This list is intentionally limited to the three most common sinks so the
/// `sonda init` wizard stays simple for new users. Advanced sinks like `tcp`,
/// `udp`, `remote_write`, `loki`, `kafka`, and `otlp_grpc` can be configured
/// by editing the generated YAML file directly.
const SINKS: &[&str] = &["stdout", "http_push", "file"];

/// Available domain categories.
const DOMAINS: &[&str] = &["infrastructure", "network", "application", "custom"];

/// Print a styled section header with a step indicator to stderr.
///
/// Renders a dimmed horizontal rule with a bold section title and a step
/// counter (e.g., `[1/4]`). The total width is [`SECTION_WIDTH`] characters.
///
/// # Example output
///
/// ```text
/// ── [1/4] Signal ─────────────────────────────
/// ```
pub fn print_section(step: usize, total: usize, title: &str) {
    let prefix = "\u{2500}\u{2500}";
    let tag = format!("[{step}/{total}]");
    // Display width: "── " (3) + tag + " " + title + " " + tail.
    let prefix_display = 2; // Two box-drawing chars, each 1 column wide.
    let used = prefix_display + 1 + tag.len() + 1 + title.len() + 1;
    let remaining = if SECTION_WIDTH > used {
        SECTION_WIDTH - used
    } else {
        3
    };
    let tail: String = "\u{2500}".repeat(remaining);

    let rule = format!("{prefix} {tag} {title} {tail}");
    eprintln!("\n{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!();
}

/// Run the full interactive prompt flow and return the collected answers.
///
/// # Errors
///
/// Returns an I/O error if terminal interaction fails (e.g., stdin is not a TTY).
pub fn run_prompts(
    pack_catalog: &PackCatalog,
) -> Result<(ScenarioKind, DeliveryAnswers), io::Error> {
    let theme = ColorfulTheme::default();

    // Section 1: Signal.
    print_section(1, 4, "Signal");

    let signal_type = prompt_signal_type(&theme)?;
    let domain = prompt_domain(&theme)?;

    match signal_type.as_str() {
        "metrics" => run_metrics_prompts(&theme, &domain, pack_catalog),
        "logs" => run_logs_prompts(&theme, &domain),
        _ => unreachable!("signal type is constrained by prompt"),
    }
}

/// Prompt for signal type: metrics or logs.
fn prompt_signal_type(theme: &ColorfulTheme) -> Result<String, io::Error> {
    let items = &["metrics", "logs"];
    let selection = Select::with_theme(theme)
        .with_prompt("What type of signal?")
        .items(items)
        .default(0)
        .interact()?;
    Ok(items[selection].to_string())
}

/// Prompt for domain category.
fn prompt_domain(theme: &ColorfulTheme) -> Result<String, io::Error> {
    let selection = Select::with_theme(theme)
        .with_prompt("What domain?")
        .items(DOMAINS)
        .default(0)
        .interact()?;
    Ok(DOMAINS[selection].to_string())
}

/// Full metrics prompt flow.
fn run_metrics_prompts(
    theme: &ColorfulTheme,
    domain: &str,
    pack_catalog: &PackCatalog,
) -> Result<(ScenarioKind, DeliveryAnswers), io::Error> {
    let available_packs = pack_catalog.list();

    // Section 2: Metric.
    print_section(2, 4, "Metric");

    let kind = if !available_packs.is_empty() {
        let approach_items = &["Single metric", "Use a metric pack"];
        let approach = Select::with_theme(theme)
            .with_prompt("How would you like to define metrics?")
            .items(approach_items)
            .default(0)
            .interact()?;

        match approach {
            0 => prompt_single_metric(theme)?,
            1 => prompt_pack(theme, pack_catalog)?,
            _ => unreachable!(),
        }
    } else {
        prompt_single_metric(theme)?
    };

    // Section 3: Delivery.
    print_section(3, 4, "Delivery");

    let rate = prompt_rate(theme)?;
    let duration = prompt_duration(theme)?;
    let encoder = prompt_encoder(theme, METRIC_ENCODERS)?;
    let (sink, endpoint) = prompt_sink(theme)?;

    let delivery = DeliveryAnswers {
        domain: domain.to_string(),
        rate,
        duration,
        encoder,
        sink,
        endpoint,
    };

    Ok((kind, delivery))
}

/// Full logs prompt flow.
fn run_logs_prompts(
    theme: &ColorfulTheme,
    domain: &str,
) -> Result<(ScenarioKind, DeliveryAnswers), io::Error> {
    // Section 2: Log.
    print_section(2, 4, "Log");

    let name: String = Input::with_theme(theme)
        .with_prompt("Log scenario name")
        .default("app_logs".to_string())
        .interact_text()?;

    // Message template.
    let message_template: String = Input::with_theme(theme)
        .with_prompt("Message template (use {field} for placeholders)")
        .default("Request to {endpoint} completed with status {status}".to_string())
        .interact_text()?;

    // Severity distribution — aligned columns for readability.
    let severity_items = &[
        "Mostly info   info 70%  warn 20%  error 10%",
        "Balanced      info 40%  warn 30%  error 20%  debug 10%",
        "Error-heavy   error 60%  warn 30%  info 10%",
    ];
    let severity_idx = Select::with_theme(theme)
        .with_prompt("Severity distribution")
        .items(severity_items)
        .default(0)
        .interact()?;
    let severity_weights = match severity_idx {
        0 => vec![
            ("info".to_string(), 0.7),
            ("warn".to_string(), 0.2),
            ("error".to_string(), 0.1),
        ],
        1 => vec![
            ("info".to_string(), 0.4),
            ("warn".to_string(), 0.3),
            ("error".to_string(), 0.2),
            ("debug".to_string(), 0.1),
        ],
        2 => vec![
            ("error".to_string(), 0.6),
            ("warn".to_string(), 0.3),
            ("info".to_string(), 0.1),
        ],
        _ => unreachable!(),
    };

    // Labels.
    let labels = prompt_labels(theme)?;

    // Section 3: Delivery.
    print_section(3, 4, "Delivery");

    let rate = prompt_rate(theme)?;
    let duration = prompt_duration(theme)?;
    let encoder = prompt_encoder(theme, LOG_ENCODERS)?;
    let (sink, endpoint) = prompt_sink(theme)?;

    let kind = ScenarioKind::Logs(LogAnswers {
        name,
        message_template,
        severity_weights,
        labels,
    });

    let delivery = DeliveryAnswers {
        domain: domain.to_string(),
        rate,
        duration,
        encoder,
        sink,
        endpoint,
    };

    Ok((kind, delivery))
}

/// Prompt for a single metric: name, situation, parameters, labels.
fn prompt_single_metric(theme: &ColorfulTheme) -> Result<ScenarioKind, io::Error> {
    // Metric name.
    let name: String = Input::with_theme(theme)
        .with_prompt("Metric name")
        .default("my_metric".to_string())
        .interact_text()?;

    // Situation (operational vocabulary).
    let situation_idx = Select::with_theme(theme)
        .with_prompt("What situation should this metric simulate?")
        .items(SITUATION_DESCRIPTIONS)
        .default(0)
        .interact()?;
    let situation = SITUATIONS[situation_idx].to_string();

    // Situation-specific parameters.
    let situation_params = prompt_situation_params(theme, &situation)?;

    // Labels.
    let labels = prompt_labels(theme)?;

    Ok(ScenarioKind::SingleMetric(MetricAnswers {
        name,
        situation,
        situation_params,
        labels,
    }))
}

/// Prompt for pack selection and pack-specific labels.
fn prompt_pack(theme: &ColorfulTheme, catalog: &PackCatalog) -> Result<ScenarioKind, io::Error> {
    let packs = catalog.list();
    let pack_names: Vec<String> = packs
        .iter()
        .map(|p| {
            format!(
                "{} - {} ({} metrics)",
                p.name, p.description, p.metric_count
            )
        })
        .collect();

    let pack_idx = Select::with_theme(theme)
        .with_prompt("Which metric pack?")
        .items(&pack_names)
        .default(0)
        .interact()?;

    let selected_pack = &packs[pack_idx];
    let pack_name = selected_pack.name.clone();

    // Read the pack YAML to find shared_labels with empty values.
    let mut labels = BTreeMap::new();

    if let Some(Ok(yaml_content)) = catalog.read_yaml(&pack_name) {
        // Parse shared_labels to find required values.
        if let Ok(pack_def) =
            serde_yaml_ng::from_str::<sonda_core::packs::MetricPackDef>(&yaml_content)
        {
            if let Some(shared_labels) = &pack_def.shared_labels {
                for (key, value) in shared_labels {
                    if value.is_empty() {
                        // Prompt the user for this label value.
                        let label_value: String = Input::with_theme(theme)
                            .with_prompt(format!("Value for label '{key}'"))
                            .default(format!("my-{key}"))
                            .interact_text()?;
                        labels.insert(key.clone(), label_value);
                    } else {
                        // Carry forward the default value from the pack.
                        labels.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    // Ask for any additional labels.
    let extra_labels = prompt_labels(theme)?;
    for (k, v) in extra_labels {
        labels.insert(k, v);
    }

    Ok(ScenarioKind::Pack(PackAnswers { pack_name, labels }))
}

/// Prompt for situation-specific parameters with sensible defaults.
///
/// Each alias has its own set of parameters matching the fields in
/// `sonda-core/src/config/aliases.rs`.
fn prompt_situation_params(
    theme: &ColorfulTheme,
    situation: &str,
) -> Result<Vec<(String, ParamValue)>, io::Error> {
    let params = match situation {
        "steady" => {
            let center: f64 = Input::with_theme(theme)
                .with_prompt("Center value")
                .default(50.0)
                .interact_text()?;
            let amplitude: f64 = Input::with_theme(theme)
                .with_prompt("Amplitude (oscillation range)")
                .default(10.0)
                .interact_text()?;
            let period: String = Input::with_theme(theme)
                .with_prompt("Oscillation period")
                .default("60s".to_string())
                .interact_text()?;
            vec![
                ("center".to_string(), ParamValue::Float(center)),
                ("amplitude".to_string(), ParamValue::Float(amplitude)),
                ("period".to_string(), ParamValue::String(period)),
            ]
        }
        "spike_event" => {
            let baseline: f64 = Input::with_theme(theme)
                .with_prompt("Baseline value (between spikes)")
                .default(0.0)
                .interact_text()?;
            let spike_height: f64 = Input::with_theme(theme)
                .with_prompt("Spike height (amount added during spike)")
                .default(100.0)
                .interact_text()?;
            let spike_duration: String = Input::with_theme(theme)
                .with_prompt("Spike duration")
                .default("10s".to_string())
                .interact_text()?;
            let spike_interval: String = Input::with_theme(theme)
                .with_prompt("Spike interval (time between spikes)")
                .default("30s".to_string())
                .interact_text()?;
            vec![
                ("baseline".to_string(), ParamValue::Float(baseline)),
                ("spike_height".to_string(), ParamValue::Float(spike_height)),
                (
                    "spike_duration".to_string(),
                    ParamValue::String(spike_duration),
                ),
                (
                    "spike_interval".to_string(),
                    ParamValue::String(spike_interval),
                ),
            ]
        }
        "flap" => {
            let up_value: f64 = Input::with_theme(theme)
                .with_prompt("Up-state value")
                .default(1.0)
                .interact_text()?;
            let down_value: f64 = Input::with_theme(theme)
                .with_prompt("Down-state value")
                .default(0.0)
                .interact_text()?;
            let up_duration: String = Input::with_theme(theme)
                .with_prompt("Up-state duration")
                .default("10s".to_string())
                .interact_text()?;
            let down_duration: String = Input::with_theme(theme)
                .with_prompt("Down-state duration")
                .default("5s".to_string())
                .interact_text()?;
            vec![
                ("up_value".to_string(), ParamValue::Float(up_value)),
                ("down_value".to_string(), ParamValue::Float(down_value)),
                ("up_duration".to_string(), ParamValue::String(up_duration)),
                (
                    "down_duration".to_string(),
                    ParamValue::String(down_duration),
                ),
            ]
        }
        "leak" => {
            let baseline: f64 = Input::with_theme(theme)
                .with_prompt("Starting value")
                .default(0.0)
                .interact_text()?;
            let ceiling: f64 = Input::with_theme(theme)
                .with_prompt("Ceiling value")
                .default(100.0)
                .interact_text()?;
            let time_to_ceiling: String = Input::with_theme(theme)
                .with_prompt("Time to reach ceiling")
                .default("10m".to_string())
                .interact_text()?;
            vec![
                ("baseline".to_string(), ParamValue::Float(baseline)),
                ("ceiling".to_string(), ParamValue::Float(ceiling)),
                (
                    "time_to_ceiling".to_string(),
                    ParamValue::String(time_to_ceiling),
                ),
            ]
        }
        "saturation" => {
            let baseline: f64 = Input::with_theme(theme)
                .with_prompt("Baseline value")
                .default(0.0)
                .interact_text()?;
            let ceiling: f64 = Input::with_theme(theme)
                .with_prompt("Ceiling value")
                .default(100.0)
                .interact_text()?;
            let time_to_saturate: String = Input::with_theme(theme)
                .with_prompt("Time to saturate")
                .default("5m".to_string())
                .interact_text()?;
            vec![
                ("baseline".to_string(), ParamValue::Float(baseline)),
                ("ceiling".to_string(), ParamValue::Float(ceiling)),
                (
                    "time_to_saturate".to_string(),
                    ParamValue::String(time_to_saturate),
                ),
            ]
        }
        "degradation" => {
            let baseline: f64 = Input::with_theme(theme)
                .with_prompt("Starting value")
                .default(0.0)
                .interact_text()?;
            let ceiling: f64 = Input::with_theme(theme)
                .with_prompt("Ceiling value")
                .default(100.0)
                .interact_text()?;
            let time_to_degrade: String = Input::with_theme(theme)
                .with_prompt("Time to degrade")
                .default("5m".to_string())
                .interact_text()?;
            let noise: f64 = Input::with_theme(theme)
                .with_prompt("Noise amplitude")
                .default(1.0)
                .interact_text()?;
            vec![
                ("baseline".to_string(), ParamValue::Float(baseline)),
                ("ceiling".to_string(), ParamValue::Float(ceiling)),
                (
                    "time_to_degrade".to_string(),
                    ParamValue::String(time_to_degrade),
                ),
                ("noise".to_string(), ParamValue::Float(noise)),
            ]
        }
        _ => vec![],
    };

    Ok(params)
}

/// Prompt for key=value labels, one at a time.
///
/// The user enters labels as `key=value` strings. An empty input ends the
/// label collection. After each successful addition, the accumulated labels
/// are shown in dimmed text so the user can see what has been collected.
fn prompt_labels(theme: &ColorfulTheme) -> Result<BTreeMap<String, String>, io::Error> {
    let mut labels = BTreeMap::new();

    loop {
        let input: String = Input::with_theme(theme)
            .with_prompt("Add a label (key=value, empty to finish)")
            .default(String::new())
            .allow_empty(true)
            .interact_text()?;

        if input.is_empty() {
            break;
        }

        if let Some(pos) = input.find('=') {
            let key = input[..pos].trim().to_string();
            let value = input[pos + 1..].trim().to_string();
            if !key.is_empty() {
                labels.insert(key, value);
                // Show accumulated labels as feedback.
                let summary = format_label_summary(&labels);
                eprintln!("  {}", summary.if_supports_color(Stderr, |t| t.dimmed()));
            }
        } else {
            eprintln!("  Labels must be in key=value format. Try again.");
        }
    }

    Ok(labels)
}

/// Format a label map as a compact `key=value, key=value` summary string.
fn format_label_summary(labels: &BTreeMap<String, String>) -> String {
    let pairs: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("Labels: {}", pairs.join(", "))
}

/// Prompt for events-per-second rate.
fn prompt_rate(theme: &ColorfulTheme) -> Result<f64, io::Error> {
    let rate: f64 = Input::with_theme(theme)
        .with_prompt("Events per second (rate)")
        .default(1.0)
        .interact_text()?;
    Ok(rate)
}

/// Prompt for scenario duration.
fn prompt_duration(theme: &ColorfulTheme) -> Result<String, io::Error> {
    let duration: String = Input::with_theme(theme)
        .with_prompt("Duration (e.g., 30s, 5m, 1h)")
        .default("60s".to_string())
        .interact_text()?;
    Ok(duration)
}

/// Prompt for encoder format.
fn prompt_encoder(theme: &ColorfulTheme, options: &[&str]) -> Result<String, io::Error> {
    let selection = Select::with_theme(theme)
        .with_prompt("Output encoding format")
        .items(options)
        .default(0)
        .interact()?;
    Ok(options[selection].to_string())
}

/// Prompt for sink type and any sink-specific fields.
fn prompt_sink(theme: &ColorfulTheme) -> Result<(String, Option<String>), io::Error> {
    let sink_idx = Select::with_theme(theme)
        .with_prompt("Where should output be sent?")
        .items(SINKS)
        .default(0)
        .interact()?;
    let sink = SINKS[sink_idx].to_string();

    let endpoint = match sink.as_str() {
        "http_push" => {
            let url: String = Input::with_theme(theme)
                .with_prompt("Endpoint URL")
                .default("http://localhost:9090/api/v1/write".to_string())
                .interact_text()?;
            Some(url)
        }
        "file" => {
            let path: String = Input::with_theme(theme)
                .with_prompt("Output file path")
                .default("/tmp/sonda-output.txt".to_string())
                .interact_text()?;
            Some(path)
        }
        _ => None,
    };

    Ok((sink, endpoint))
}

/// Prompt for the output file path for the generated YAML.
pub fn prompt_output_path(theme: &ColorfulTheme, suggested: &str) -> Result<String, io::Error> {
    let default_path = format!("./scenarios/{suggested}");
    let path: String = Input::with_theme(theme)
        .with_prompt("Output file path")
        .default(default_path)
        .interact_text()?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Constants: verify situation list matches aliases.rs
    // -----------------------------------------------------------------------

    #[test]
    fn situations_list_has_all_aliases() {
        // These must match the aliases in sonda-core/src/config/aliases.rs.
        assert!(SITUATIONS.contains(&"steady"));
        assert!(SITUATIONS.contains(&"spike_event"));
        assert!(SITUATIONS.contains(&"flap"));
        assert!(SITUATIONS.contains(&"leak"));
        assert!(SITUATIONS.contains(&"saturation"));
        assert!(SITUATIONS.contains(&"degradation"));
    }

    #[test]
    fn situations_and_descriptions_have_same_length() {
        assert_eq!(
            SITUATIONS.len(),
            SITUATION_DESCRIPTIONS.len(),
            "each situation must have a description"
        );
    }

    #[test]
    fn descriptions_contain_their_situation_name() {
        for (i, &situation) in SITUATIONS.iter().enumerate() {
            assert!(
                SITUATION_DESCRIPTIONS[i].contains(situation),
                "description for '{situation}' must contain the situation name"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Constants: encoder and sink options
    // -----------------------------------------------------------------------

    #[test]
    fn metric_encoders_include_prometheus_text() {
        assert!(METRIC_ENCODERS.contains(&"prometheus_text"));
    }

    #[test]
    fn log_encoders_include_json_lines() {
        assert!(LOG_ENCODERS.contains(&"json_lines"));
    }

    #[test]
    fn sinks_include_stdout() {
        assert!(SINKS.contains(&"stdout"));
    }

    #[test]
    fn domains_include_infrastructure() {
        assert!(DOMAINS.contains(&"infrastructure"));
    }

    // -----------------------------------------------------------------------
    // format_label_summary: accumulated label display
    // -----------------------------------------------------------------------

    #[test]
    fn format_label_summary_single_label() {
        let mut labels = BTreeMap::new();
        labels.insert("instance".to_string(), "web-01".to_string());
        assert_eq!(format_label_summary(&labels), "Labels: instance=web-01");
    }

    #[test]
    fn format_label_summary_multiple_labels_sorted() {
        let mut labels = BTreeMap::new();
        labels.insert("job".to_string(), "node_exporter".to_string());
        labels.insert("instance".to_string(), "web-01".to_string());
        // BTreeMap sorts by key, so instance comes before job.
        assert_eq!(
            format_label_summary(&labels),
            "Labels: instance=web-01, job=node_exporter"
        );
    }

    #[test]
    fn format_label_summary_empty() {
        let labels = BTreeMap::new();
        assert_eq!(format_label_summary(&labels), "Labels: ");
    }

    // -----------------------------------------------------------------------
    // Section header width constant
    // -----------------------------------------------------------------------

    #[test]
    fn section_width_is_reasonable() {
        assert!(
            SECTION_WIDTH >= 30,
            "section width must be wide enough for readable headers"
        );
    }
}
