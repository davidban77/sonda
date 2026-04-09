//! `sonda init` — guided scenario scaffolding.
//!
//! This module implements the interactive `sonda init` subcommand. It walks
//! the user through building a scenario by asking domain-relevant questions
//! and generates a valid, runnable YAML file.
//!
//! The flow uses operational language ("What situation?") rather than
//! generator internals ("sawtooth period"), leveraging the operational
//! vocabulary aliases from `sonda-core/src/config/aliases.rs`.
//!
//! After writing the file, the user is offered the option to run the scenario
//! immediately. If accepted, the generated YAML is parsed and executed using
//! the same pipeline as `sonda run --scenario`.
//!
//! ## Non-interactive mode
//!
//! When CLI flags supply values for init prompts, those prompts are skipped.
//! The `--from` flag pre-fills values from a built-in scenario (`@name`) or a
//! CSV file (`path.csv`). Explicit flags override `--from` values.
//!
//! # Module structure
//!
//! - [`prompts`] — interactive prompt logic using `dialoguer`.
//! - [`yaml_gen`] — YAML rendering from collected answers.

pub mod prompts;
pub mod yaml_gen;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use crate::cli::InitArgs;
use crate::import;
use crate::packs::PackCatalog;
use crate::scenarios::ScenarioCatalog;
use prompts::Prefill;

use yaml_gen::{render_scenario_yaml, suggest_filename, InitScenarioType};

/// Width used for horizontal rules in the init flow.
const RULE_WIDTH: usize = 45;

/// Result of a successful `sonda init` interactive flow.
///
/// Contains the generated YAML and a typed scenario indicator so the
/// caller can execute the scenario immediately without content sniffing.
pub struct InitResult {
    /// The generated YAML content.
    pub yaml: String,
    /// Whether the user chose to run the scenario immediately.
    pub run_now: bool,
    /// The type of scenario that was generated, used for dispatch.
    pub scenario_type: InitScenarioType,
}

/// Run the `sonda init` scaffolding flow.
///
/// Builds a [`Prefill`] from CLI flags and `--from` data, then runs the
/// interactive prompts (skipping any that have pre-filled values). Generates
/// the YAML, writes it to the chosen output path, and offers to run the
/// scenario immediately.
///
/// # Errors
///
/// Returns an error if:
/// - `--from @name` references an unknown scenario.
/// - `--from path.csv` cannot be read or analyzed.
/// - Terminal interaction fails (stdin is not a TTY).
/// - The output file cannot be written.
pub fn run_init(
    args: &InitArgs,
    pack_catalog: &PackCatalog,
    scenario_catalog: &ScenarioCatalog,
) -> Result<InitResult> {
    // 1. Build a Prefill from --from and/or CLI flags.
    let prefill = build_prefill(args, scenario_catalog)?;

    print_welcome_banner();

    // Show pre-fill summary if any values were loaded.
    print_prefill_summary(args, &prefill);

    // 2. Run the prompts (with prefill).
    let (kind, delivery) =
        prompts::run_prompts(pack_catalog, &prefill).context("interactive prompt failed")?;

    // Remember the scenario type before rendering.
    let scenario_type = kind.scenario_type();

    // Render the YAML.
    let yaml = render_scenario_yaml(&kind, &delivery);

    // Show a preview of the generated YAML before asking for the output path.
    print_yaml_preview(&yaml);

    // Section 4: Output.
    prompts::print_section(4, 4, "Output");

    // Use --output if provided, otherwise prompt.
    let output_path = if let Some(ref path) = args.output {
        path.clone()
    } else {
        let suggested = suggest_filename(&kind);
        let theme = ColorfulTheme::default();
        prompts::prompt_output_path(&theme, &suggested).context("output path prompt failed")?
    };

    // Ensure parent directories exist.
    let path = Path::new(&output_path);
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }

    // Write the file.
    std::fs::write(path, &yaml)
        .with_context(|| format!("failed to write scenario to {}", path.display()))?;

    // Print styled success summary.
    print_success(&kind, &output_path);

    // Offer to run immediately.
    // --run-now flag: use it directly. Otherwise, prompt if stdin is a TTY;
    // default to false when non-interactive.
    let run_now = if args.run_now {
        true
    } else if std::io::stdin().is_terminal() {
        let theme = ColorfulTheme::default();
        prompts::prompt_run_now(&theme).context("run-now prompt failed")?
    } else {
        false
    };

    Ok(InitResult {
        yaml,
        run_now,
        scenario_type,
    })
}

/// Build a [`Prefill`] by merging `--from` data with explicit CLI flags.
///
/// CLI flags always take precedence over values loaded from `--from`.
///
/// # Errors
///
/// Returns an error if `--from @name` references an unknown scenario or
/// `--from path.csv` cannot be read.
pub fn build_prefill(args: &InitArgs, scenario_catalog: &ScenarioCatalog) -> Result<Prefill> {
    // Start with --from data (if any).
    let mut prefill = match args.from.as_deref() {
        Some(from) if from.starts_with('@') => {
            let name = &from[1..];
            prefill_from_scenario(name, scenario_catalog)?
        }
        Some(from) => prefill_from_csv(from)?,
        None => Prefill::default(),
    };

    // Overlay explicit CLI flags (they take precedence over --from).
    if let Some(ref v) = args.signal_type {
        prefill.signal_type = Some(v.clone());
    }
    if let Some(ref v) = args.domain {
        prefill.domain = Some(v.clone());
    }
    if let Some(ref v) = args.situation {
        prefill.situation = Some(v.clone());
    }
    if let Some(ref v) = args.metric {
        prefill.metric = Some(v.clone());
    }
    if let Some(ref v) = args.pack {
        prefill.pack = Some(v.clone());
    }
    if let Some(v) = args.rate {
        prefill.rate = Some(v);
    }
    if let Some(ref v) = args.duration {
        prefill.duration = Some(v.clone());
    }
    if let Some(ref v) = args.encoder {
        prefill.encoder = Some(v.clone());
    }
    if let Some(ref v) = args.sink {
        prefill.sink = Some(v.clone());
    }
    if let Some(ref v) = args.endpoint {
        prefill.endpoint = Some(v.clone());
    }

    // Log-specific fields.
    if let Some(ref v) = args.message_template {
        prefill.message_template = Some(v.clone());
    }
    if let Some(ref v) = args.severity {
        prefill.severity = Some(v.clone());
    }

    // Sink-specific extra fields.
    if let Some(ref v) = args.kafka_brokers {
        prefill.kafka_brokers = Some(v.clone());
    }
    if let Some(ref v) = args.kafka_topic {
        prefill.kafka_topic = Some(v.clone());
    }
    if let Some(ref v) = args.otlp_signal_type {
        prefill.otlp_signal_type = Some(v.clone());
    }

    // Parse --label key=value flags into the labels map.
    for label_str in &args.labels {
        if let Some(pos) = label_str.find('=') {
            let key = label_str[..pos].to_string();
            let value = label_str[pos + 1..].to_string();
            if !key.is_empty() {
                prefill.labels.insert(key, value);
            }
        }
    }

    Ok(prefill)
}

/// Build a [`Prefill`] from a built-in scenario looked up by name.
///
/// Reads the scenario YAML and extracts metadata fields into the prefill.
///
/// # Errors
///
/// Returns an error if the scenario name is not found in the catalog.
fn prefill_from_scenario(name: &str, catalog: &ScenarioCatalog) -> Result<Prefill> {
    let scenario = catalog.find(name).ok_or_else(|| {
        let available = catalog.available_names();
        let list = if available.is_empty() {
            "(no scenarios found in search path)".to_string()
        } else {
            available.join(", ")
        };
        anyhow::anyhow!(
            "scenario '{}' not found. Available scenarios: {}",
            name,
            list
        )
    })?;

    let mut prefill = Prefill {
        signal_type: Some(scenario.signal_type.clone()),
        domain: Some(scenario.category.clone()),
        ..Prefill::default()
    };

    // Try to extract deeper fields from the YAML content.
    if let Some(Ok(yaml_content)) = catalog.read_yaml(name) {
        if let Ok(probe) = serde_yaml_ng::from_str::<ScenarioProbe>(&yaml_content) {
            if let Some(ref n) = probe.name {
                prefill.metric = Some(n.clone());
            }
            if let Some(ref gen) = probe.generator {
                if let Some(ref gtype) = gen.generator_type {
                    prefill.situation = Some(gtype.clone());
                }
            }
            if let Some(v) = probe.rate {
                prefill.rate = Some(v);
            }
            if let Some(ref d) = probe.duration {
                prefill.duration = Some(d.clone());
            }
            if let Some(ref enc) = probe.encoder {
                if let Some(ref etype) = enc.encoder_type {
                    prefill.encoder = Some(etype.clone());
                }
            }
            if let Some(ref s) = probe.sink {
                if let Some(ref stype) = s.sink_type {
                    prefill.sink = Some(stype.clone());
                }
            }
            if let Some(ref p) = probe.pack {
                prefill.pack = Some(p.clone());
            }
            if let Some(ref l) = probe.labels {
                for (k, v) in l {
                    prefill.labels.insert(k.clone(), v.clone());
                }
            }
        }
    }

    Ok(prefill)
}

/// Build a [`Prefill`] from a CSV file by analyzing time-series patterns.
///
/// Reads the first numeric column from the CSV, detects its dominant pattern,
/// and maps it to an operational vocabulary alias. When the CSV has no numeric
/// columns, returns a minimal prefill with `signal_type: "metrics"` and no
/// situation or metric name.
///
/// # Errors
///
/// Returns an error if the CSV file cannot be opened or parsed.
fn prefill_from_csv(path: &str) -> Result<Prefill> {
    let csv_path = Path::new(path);
    let data = match import::csv_reader::read_csv(csv_path, None) {
        Ok(d) => d,
        Err(e) => {
            // When the CSV has no numeric columns, read_csv returns an error.
            // We still want to produce a valid (albeit minimal) Prefill so the
            // init flow can continue with interactive prompts.
            let msg = e.to_string();
            if msg.contains("no numeric data found") {
                return Ok(Prefill {
                    signal_type: Some("metrics".to_string()),
                    domain: Some("custom".to_string()),
                    ..Prefill::default()
                });
            }
            return Err(e).with_context(|| format!("failed to read CSV file: {path}"));
        }
    };

    let mut prefill = Prefill {
        signal_type: Some("metrics".to_string()),
        domain: Some("custom".to_string()),
        ..Prefill::default()
    };

    // Use the first column for pattern detection and metric name.
    if let Some(col) = data.columns.first() {
        if let Some(ref name) = col.metric_name {
            prefill.metric = Some(name.clone());
        }
    }

    if let Some(values) = data.values.first() {
        let pattern = import::pattern::detect_pattern(values);
        prefill.situation = Some(pattern_to_situation(&pattern));
    }

    Ok(prefill)
}

/// Map an import pattern to an operational vocabulary alias for the prefill.
fn pattern_to_situation(pattern: &import::pattern::Pattern) -> String {
    match pattern {
        import::pattern::Pattern::Steady { .. } => "steady".to_string(),
        import::pattern::Pattern::Spike { .. } => "spike_event".to_string(),
        import::pattern::Pattern::Climb { .. } => "leak".to_string(),
        import::pattern::Pattern::Sawtooth { .. } => "saturation".to_string(),
        import::pattern::Pattern::Flap { .. } => "flap".to_string(),
        import::pattern::Pattern::Step { .. } => "steady".to_string(),
    }
}

/// Lightweight YAML probe for extracting init-relevant fields from a scenario.
///
/// Does not attempt to fully deserialize a `ScenarioConfig` — only picks out
/// the fields that can populate a [`Prefill`].
#[derive(serde::Deserialize)]
struct ScenarioProbe {
    name: Option<String>,
    rate: Option<f64>,
    duration: Option<String>,
    generator: Option<GeneratorProbe>,
    encoder: Option<EncoderProbe>,
    sink: Option<SinkProbe>,
    pack: Option<String>,
    /// Static labels from the scenario YAML.
    labels: Option<std::collections::BTreeMap<String, String>>,
}

/// Generator section of a scenario YAML (just the type field).
#[derive(serde::Deserialize)]
struct GeneratorProbe {
    #[serde(rename = "type")]
    generator_type: Option<String>,
}

/// Encoder section of a scenario YAML (just the type field).
#[derive(serde::Deserialize)]
struct EncoderProbe {
    #[serde(rename = "type")]
    encoder_type: Option<String>,
}

/// Sink section of a scenario YAML (just the type field).
#[derive(serde::Deserialize)]
struct SinkProbe {
    #[serde(rename = "type")]
    sink_type: Option<String>,
}

/// Print a styled summary of pre-filled values before prompts begin.
///
/// Shows either "Starting from: @scenario-name" or "Starting from: path.csv"
/// for `--from` mode, or "Pre-filled from flags:" when only flags are present.
fn print_prefill_summary(args: &InitArgs, prefill: &Prefill) {
    let dimmed = owo_colors::Style::new().dimmed();
    let bold_cyan = owo_colors::Style::new().bold().cyan();

    // We need an owned String for the rate (formatted from f64); all other
    // values are borrowed as &str to avoid unnecessary clones.
    let rate_str;

    // Collect fields that have values — borrows wherever possible.
    let mut fields: Vec<(&str, &str)> = Vec::new();
    if let Some(ref v) = prefill.signal_type {
        fields.push(("signal_type", v.as_str()));
    }
    if let Some(ref v) = prefill.domain {
        fields.push(("domain", v.as_str()));
    }
    if let Some(ref v) = prefill.situation {
        fields.push(("situation", v.as_str()));
    }
    if let Some(ref v) = prefill.metric {
        fields.push(("metric", v.as_str()));
    }
    if let Some(ref v) = prefill.pack {
        fields.push(("pack", v.as_str()));
    }
    if let Some(v) = prefill.rate {
        rate_str = v.to_string();
        fields.push(("rate", &rate_str));
    }
    if let Some(ref v) = prefill.duration {
        fields.push(("duration", v.as_str()));
    }
    if let Some(ref v) = prefill.encoder {
        fields.push(("encoder", v.as_str()));
    }
    if let Some(ref v) = prefill.sink {
        fields.push(("sink", v.as_str()));
    }
    if let Some(ref v) = prefill.endpoint {
        fields.push(("endpoint", v.as_str()));
    }

    if fields.is_empty() {
        return;
    }

    // Header line.
    let header = if let Some(ref from) = args.from {
        format!("Starting from: {from}")
    } else {
        "Pre-filled from flags:".to_string()
    };
    eprintln!(
        "\n  {}",
        header.if_supports_color(Stderr, |t| t.style(dimmed))
    );

    // Individual fields.
    for (label, value) in &fields {
        eprintln!(
            "    {:12} {}",
            format!("{label}:").if_supports_color(Stderr, |t| t.style(dimmed)),
            value.if_supports_color(Stderr, |t| t.style(bold_cyan)),
        );
    }

    // Labels.
    if !prefill.labels.is_empty() {
        let pairs: Vec<String> = prefill
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        eprintln!(
            "    {:12} {}",
            "labels:".if_supports_color(Stderr, |t| t.style(dimmed)),
            pairs
                .join(", ")
                .if_supports_color(Stderr, |t| t.style(bold_cyan)),
        );
    }

    eprintln!();
}

/// Print a styled welcome banner for the init flow.
///
/// Displays the command name and a brief description of what will happen,
/// styled consistently with sonda's existing CLI output (bold title, dimmed
/// subtitle).
fn print_welcome_banner() {
    let rule: String = "\u{2500}".repeat(RULE_WIDTH);
    let title_style = owo_colors::Style::new().bold().cyan();

    eprintln!("\n{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!(
        "  {}  {}",
        "sonda init".if_supports_color(Stderr, |t| t.style(title_style)),
        "\u{2014} guided scenario scaffolding".if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!(
        "  {}",
        "Answer the prompts to generate a runnable scenario YAML."
            .if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!(
        "  {}",
        "Every prompt has a default \u{2014} press Enter to accept it."
            .if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
}

/// Print a truncated YAML preview before asking for the output path.
///
/// Shows up to [`PREVIEW_LINES`] lines of the generated YAML in dimmed text
/// to let the user verify the output before writing.
fn print_yaml_preview(yaml: &str) {
    const PREVIEW_LINES: usize = 15;

    let rule: String = "\u{2500}".repeat(RULE_WIDTH);
    // Build the preview header: "── Preview ───────..."
    // "── Preview " uses 2 box-drawing chars + space + "Preview" + space = 11 display chars.
    let tail_chars = RULE_WIDTH.saturating_sub(11);
    let tail: String = "\u{2500}".repeat(tail_chars);
    let header = format!("\u{2500}\u{2500} Preview {tail}");

    eprintln!("\n{}", header.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!();

    let lines: Vec<&str> = yaml.lines().collect();
    let shown = lines.len().min(PREVIEW_LINES);
    for line in &lines[..shown] {
        eprintln!("  {}", line.if_supports_color(Stderr, |t| t.dimmed()));
    }
    if lines.len() > PREVIEW_LINES {
        eprintln!("  {}", "...".if_supports_color(Stderr, |t| t.dimmed()));
    }

    eprintln!();
    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
}

/// Print a styled success summary after writing the scenario file.
///
/// Displays the scenario name, signal type, file path, and the command(s)
/// to run the scenario. Styled consistently with sonda's stop banners.
fn print_success(kind: &yaml_gen::ScenarioKind, output_path: &str) {
    let bold = owo_colors::Style::new().bold();
    let green_bold = owo_colors::Style::new().green().bold();
    let dimmed = owo_colors::Style::new().dimmed();
    let rule: String = "\u{2500}".repeat(RULE_WIDTH);

    let (scenario_name, signal_type) = match kind {
        yaml_gen::ScenarioKind::SingleMetric(a) => (a.name.as_str(), "metrics"),
        yaml_gen::ScenarioKind::Pack(a) => (a.pack_name.as_str(), "metrics (pack)"),
        yaml_gen::ScenarioKind::Logs(a) => (a.name.as_str(), "logs"),
        yaml_gen::ScenarioKind::Histogram(a) => (a.name.as_str(), "histogram"),
        yaml_gen::ScenarioKind::Summary(a) => (a.name.as_str(), "summary"),
    };

    eprintln!("\n{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!(
        "  {} {}",
        "\u{2714}".if_supports_color(Stderr, |t| t.style(green_bold)),
        "Scenario created".if_supports_color(Stderr, |t| t.style(bold)),
    );
    eprintln!();

    // Scenario details.
    let name_label = "name:".if_supports_color(Stderr, |t| t.style(dimmed));
    let type_label = "type:".if_supports_color(Stderr, |t| t.style(dimmed));
    let file_label = "file:".if_supports_color(Stderr, |t| t.style(dimmed));

    let name_value = scenario_name.if_supports_color(Stderr, |t| t.style(bold));
    let type_value = signal_type.if_supports_color(Stderr, |t| t.cyan());
    let file_value = output_path.if_supports_color(Stderr, |t| t.style(bold));

    eprintln!("  {name_label}  {name_value}");
    eprintln!("  {type_label}  {type_value}");
    eprintln!("  {file_label}  {file_value}");
    eprintln!();

    // Run commands.
    eprintln!(
        "  {}",
        "Run it with:".if_supports_color(Stderr, |t| t.style(dimmed)),
    );

    match kind {
        yaml_gen::ScenarioKind::SingleMetric(_) => {
            eprintln!("    sonda metrics --scenario {output_path}");
            eprintln!("    sonda run --scenario {output_path}");
        }
        yaml_gen::ScenarioKind::Pack(_) => {
            eprintln!("    sonda run --scenario {output_path}");
        }
        yaml_gen::ScenarioKind::Logs(_) => {
            eprintln!("    sonda logs --scenario {output_path}");
            eprintln!("    sonda run --scenario {output_path}");
        }
        yaml_gen::ScenarioKind::Histogram(_) => {
            eprintln!("    sonda histogram --scenario {output_path}");
            eprintln!("    sonda run --scenario {output_path}");
        }
        yaml_gen::ScenarioKind::Summary(_) => {
            eprintln!("    sonda summary --scenario {output_path}");
            eprintln!("    sonda run --scenario {output_path}");
        }
    }

    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a default [`InitArgs`] with all fields set to their zero/None values.
    ///
    /// Tests override individual fields as needed.
    fn default_init_args() -> InitArgs {
        InitArgs {
            from: None,
            signal_type: None,
            domain: None,
            situation: None,
            metric: None,
            pack: None,
            rate: None,
            duration: None,
            encoder: None,
            sink: None,
            endpoint: None,
            output: None,
            labels: vec![],
            run_now: false,
            message_template: None,
            severity: None,
            kafka_brokers: None,
            kafka_topic: None,
            otlp_signal_type: None,
        }
    }

    // -----------------------------------------------------------------------
    // YAML preview: truncation behavior
    // -----------------------------------------------------------------------

    #[test]
    fn yaml_preview_does_not_panic_on_empty_input() {
        // Calling with empty string should not panic.
        print_yaml_preview("");
    }

    #[test]
    fn yaml_preview_does_not_panic_on_short_input() {
        print_yaml_preview("name: test\nrate: 1\n");
    }

    #[test]
    fn yaml_preview_does_not_panic_on_long_input() {
        let lines: Vec<String> = (0..50).map(|i| format!("line_{i}: value")).collect();
        let yaml = lines.join("\n");
        print_yaml_preview(&yaml);
    }

    // -----------------------------------------------------------------------
    // Rule width constant
    // -----------------------------------------------------------------------

    #[test]
    fn rule_width_is_consistent_with_prompts_section_width() {
        // Both modules should use a comparable width for visual consistency.
        assert_eq!(
            RULE_WIDTH,
            prompts::SECTION_WIDTH,
            "mod.rs RULE_WIDTH and prompts SECTION_WIDTH should match"
        );
    }

    // -----------------------------------------------------------------------
    // Welcome banner: smoke test
    // -----------------------------------------------------------------------

    #[test]
    fn welcome_banner_does_not_panic() {
        print_welcome_banner();
    }

    // -----------------------------------------------------------------------
    // Success message: smoke test
    // -----------------------------------------------------------------------

    #[test]
    fn success_message_single_metric_does_not_panic() {
        use std::collections::BTreeMap;
        let kind = yaml_gen::ScenarioKind::SingleMetric(yaml_gen::MetricAnswers {
            name: "cpu_usage".to_string(),
            situation: "steady".to_string(),
            situation_params: vec![],
            labels: BTreeMap::new(),
        });
        print_success(&kind, "./scenarios/cpu-usage.yaml");
    }

    #[test]
    fn success_message_pack_does_not_panic() {
        use std::collections::BTreeMap;
        let kind = yaml_gen::ScenarioKind::Pack(yaml_gen::PackAnswers {
            pack_name: "telegraf_snmp".to_string(),
            labels: BTreeMap::new(),
        });
        print_success(&kind, "./scenarios/telegraf-snmp.yaml");
    }

    #[test]
    fn success_message_logs_does_not_panic() {
        use std::collections::BTreeMap;
        let kind = yaml_gen::ScenarioKind::Logs(yaml_gen::LogAnswers {
            name: "app_logs".to_string(),
            message_template: "test".to_string(),
            severity_weights: vec![],
            labels: BTreeMap::new(),
        });
        print_success(&kind, "./scenarios/app-logs.yaml");
    }

    #[test]
    fn success_message_histogram_does_not_panic() {
        use std::collections::BTreeMap;
        let kind = yaml_gen::ScenarioKind::Histogram(yaml_gen::HistogramAnswers {
            name: "http_request_duration_seconds".to_string(),
            distribution_type: "normal".to_string(),
            distribution_params: vec![],
            observations_per_tick: 100,
            buckets: None,
            seed: 42,
            labels: BTreeMap::new(),
        });
        print_success(&kind, "./scenarios/http-request-duration-seconds.yaml");
    }

    #[test]
    fn success_message_summary_does_not_panic() {
        use std::collections::BTreeMap;
        let kind = yaml_gen::ScenarioKind::Summary(yaml_gen::SummaryAnswers {
            name: "rpc_duration_seconds".to_string(),
            distribution_type: "normal".to_string(),
            distribution_params: vec![],
            observations_per_tick: 100,
            quantiles: None,
            seed: 42,
            labels: BTreeMap::new(),
        });
        print_success(&kind, "./scenarios/rpc-duration-seconds.yaml");
    }

    // -----------------------------------------------------------------------
    // pattern_to_situation: pattern → alias mapping
    // -----------------------------------------------------------------------

    #[test]
    fn pattern_to_situation_steady() {
        let pattern = import::pattern::Pattern::Steady {
            center: 50.0,
            amplitude: 5.0,
        };
        assert_eq!(pattern_to_situation(&pattern), "steady");
    }

    #[test]
    fn pattern_to_situation_spike() {
        let pattern = import::pattern::Pattern::Spike {
            baseline: 0.0,
            spike_height: 100.0,
            spike_duration_points: 5,
            spike_interval_points: 30,
        };
        assert_eq!(pattern_to_situation(&pattern), "spike_event");
    }

    #[test]
    fn pattern_to_situation_climb() {
        let pattern = import::pattern::Pattern::Climb {
            baseline: 0.0,
            ceiling: 100.0,
        };
        assert_eq!(pattern_to_situation(&pattern), "leak");
    }

    #[test]
    fn pattern_to_situation_sawtooth() {
        let pattern = import::pattern::Pattern::Sawtooth {
            min: 0.0,
            max: 100.0,
            period_points: 60,
        };
        assert_eq!(pattern_to_situation(&pattern), "saturation");
    }

    #[test]
    fn pattern_to_situation_flap() {
        let pattern = import::pattern::Pattern::Flap {
            up_value: 1.0,
            down_value: 0.0,
            up_duration_points: 10,
            down_duration_points: 5,
        };
        assert_eq!(pattern_to_situation(&pattern), "flap");
    }

    #[test]
    fn pattern_to_situation_step() {
        let pattern = import::pattern::Pattern::Step {
            start: 0.0,
            step_size: 10.0,
        };
        assert_eq!(pattern_to_situation(&pattern), "steady");
    }

    // -----------------------------------------------------------------------
    // build_prefill: CLI flag overlay
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_no_args_produces_default() {
        let args = default_init_args();
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert!(prefill.signal_type.is_none());
        assert!(prefill.domain.is_none());
        assert!(prefill.labels.is_empty());
    }

    #[test]
    fn build_prefill_cli_flags_populate_fields() {
        let args = InitArgs {
            signal_type: Some("metrics".to_string()),
            domain: Some("network".to_string()),
            situation: Some("flap".to_string()),
            metric: Some("bgp_state".to_string()),
            rate: Some(2.0),
            duration: Some("5m".to_string()),
            encoder: Some("prometheus_text".to_string()),
            sink: Some("stdout".to_string()),
            labels: vec!["env=prod".to_string(), "region=us-east".to_string()],
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.signal_type.as_deref(), Some("metrics"));
        assert_eq!(prefill.domain.as_deref(), Some("network"));
        assert_eq!(prefill.situation.as_deref(), Some("flap"));
        assert_eq!(prefill.metric.as_deref(), Some("bgp_state"));
        assert_eq!(prefill.rate, Some(2.0));
        assert_eq!(prefill.duration.as_deref(), Some("5m"));
        assert_eq!(prefill.encoder.as_deref(), Some("prometheus_text"));
        assert_eq!(prefill.sink.as_deref(), Some("stdout"));
        assert_eq!(prefill.labels.get("env").map(String::as_str), Some("prod"));
        assert_eq!(
            prefill.labels.get("region").map(String::as_str),
            Some("us-east")
        );
    }

    #[test]
    fn build_prefill_labels_parse_key_value() {
        let args = InitArgs {
            labels: vec!["host=web-01".to_string(), "dc=us-west-2".to_string()],
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.labels.len(), 2);
        assert_eq!(
            prefill.labels.get("host").map(String::as_str),
            Some("web-01")
        );
        assert_eq!(
            prefill.labels.get("dc").map(String::as_str),
            Some("us-west-2")
        );
    }

    #[test]
    fn build_prefill_labels_skip_malformed() {
        let args = InitArgs {
            labels: vec!["no_equals_sign".to_string(), "good=value".to_string()],
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        // "no_equals_sign" is skipped (no '=' found).
        assert_eq!(prefill.labels.len(), 1);
        assert_eq!(
            prefill.labels.get("good").map(String::as_str),
            Some("value")
        );
    }

    // -----------------------------------------------------------------------
    // build_prefill: --from @builtin
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_from_scenario_populates_fields() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("sonda-init-from-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        let scenario_yaml = r#"scenario_name: cpu-spike
category: infrastructure
signal_type: metrics
description: "CPU spike scenario"

name: cpu_usage
rate: 2
duration: 5m

generator:
  type: spike_event
  baseline: 0.0
  spike_height: 100.0
  spike_duration: "10s"
  spike_interval: "30s"

encoder:
  type: prometheus_text

sink:
  type: stdout
"#;
        fs::write(dir.join("cpu-spike.yaml"), scenario_yaml).expect("write scenario");

        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        let args = InitArgs {
            from: Some("@cpu-spike".to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.signal_type.as_deref(), Some("metrics"));
        assert_eq!(prefill.domain.as_deref(), Some("infrastructure"));
        assert_eq!(prefill.metric.as_deref(), Some("cpu_usage"));
        assert_eq!(prefill.situation.as_deref(), Some("spike_event"));
        assert_eq!(prefill.rate, Some(2.0));
        assert_eq!(prefill.duration.as_deref(), Some("5m"));
        assert_eq!(prefill.encoder.as_deref(), Some("prometheus_text"));
        assert_eq!(prefill.sink.as_deref(), Some("stdout"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_prefill_from_unknown_scenario_returns_error() {
        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some("@nonexistent-scenario".to_string()),
            ..default_init_args()
        };

        let result = build_prefill(&args, &catalog);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention 'not found': {err}"
        );
    }

    #[test]
    fn build_prefill_cli_flags_override_from() {
        use std::fs;

        let dir =
            std::env::temp_dir().join(format!("sonda-init-override-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        let scenario_yaml = r#"scenario_name: cpu-spike
category: infrastructure
signal_type: metrics
description: "CPU spike scenario"

name: cpu_usage
rate: 2
duration: 5m

generator:
  type: spike_event

encoder:
  type: prometheus_text

sink:
  type: stdout
"#;
        fs::write(dir.join("cpu-spike.yaml"), scenario_yaml).expect("write scenario");

        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        let args = InitArgs {
            from: Some("@cpu-spike".to_string()),
            domain: Some("network".to_string()),
            situation: Some("flap".to_string()),
            rate: Some(10.0),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        // CLI flags override --from values.
        assert_eq!(prefill.domain.as_deref(), Some("network"));
        assert_eq!(prefill.situation.as_deref(), Some("flap"));
        assert_eq!(prefill.rate, Some(10.0));
        // --from values are preserved where no CLI override.
        assert_eq!(prefill.signal_type.as_deref(), Some("metrics"));
        assert_eq!(prefill.metric.as_deref(), Some("cpu_usage"));
        assert_eq!(prefill.duration.as_deref(), Some("5m"));

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // build_prefill: --from path.csv
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_from_csv_populates_fields() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("sonda-init-csv-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        // CSV with steady-ish oscillation (enough points for pattern detection).
        let csv_content = "timestamp,cpu_usage\n\
            1000,50.1\n\
            1001,49.9\n\
            1002,50.3\n\
            1003,49.7\n\
            1004,50.2\n\
            1005,49.8\n\
            1006,50.1\n\
            1007,49.9\n\
            1008,50.3\n\
            1009,49.7\n\
            1010,50.2\n\
            1011,49.8\n\
            1012,50.0\n\
            1013,50.1\n\
            1014,49.9\n\
            1015,50.2\n\
            1016,49.8\n\
            1017,50.1\n\
            1018,49.9\n\
            1019,50.0\n";
        let csv_path = dir.join("test-data.csv");
        fs::write(&csv_path, csv_content).expect("write CSV");

        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some(csv_path.to_str().unwrap().to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.signal_type.as_deref(), Some("metrics"));
        assert_eq!(prefill.metric.as_deref(), Some("cpu_usage"));
        // Pattern detection produces a valid situation alias.
        let situation = prefill
            .situation
            .as_deref()
            .expect("should have a situation");
        let valid_situations = ["steady", "spike_event", "leak", "saturation", "flap"];
        assert!(
            valid_situations.contains(&situation),
            "detected situation '{situation}' must be a valid alias"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_prefill_from_nonexistent_csv_returns_error() {
        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some("/nonexistent/path/data.csv".to_string()),
            ..default_init_args()
        };

        let result = build_prefill(&args, &catalog);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Prefill summary: smoke test
    // -----------------------------------------------------------------------

    #[test]
    fn print_prefill_summary_does_not_panic_with_empty_prefill() {
        let args = default_init_args();
        let prefill = Prefill::default();
        print_prefill_summary(&args, &prefill);
    }

    #[test]
    fn print_prefill_summary_does_not_panic_with_from() {
        let args = InitArgs {
            from: Some("@cpu-spike".to_string()),
            ..default_init_args()
        };
        let prefill = Prefill {
            signal_type: Some("metrics".to_string()),
            domain: Some("infrastructure".to_string()),
            ..Prefill::default()
        };
        print_prefill_summary(&args, &prefill);
    }

    #[test]
    fn print_prefill_summary_does_not_panic_with_flags() {
        let args = InitArgs {
            signal_type: Some("metrics".to_string()),
            domain: Some("network".to_string()),
            rate: Some(5.0),
            ..default_init_args()
        };
        let prefill = Prefill {
            signal_type: Some("metrics".to_string()),
            domain: Some("network".to_string()),
            rate: Some(5.0),
            ..Prefill::default()
        };
        print_prefill_summary(&args, &prefill);
    }

    // -----------------------------------------------------------------------
    // build_prefill: CSV with no numeric columns
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_from_csv_no_numeric_columns() {
        use std::fs;

        let dir =
            std::env::temp_dir().join(format!("sonda-init-csv-no-numeric-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        // CSV with only a timestamp column and a non-numeric column.
        let csv_content = "timestamp,status\n\
            1000,ok\n\
            1001,ok\n\
            1002,error\n";
        let csv_path = dir.join("no-numeric.csv");
        fs::write(&csv_path, csv_content).expect("write CSV");

        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some(csv_path.to_str().unwrap().to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        // Signal type is always set to "metrics" for CSV-based prefill.
        assert_eq!(prefill.signal_type.as_deref(), Some("metrics"));
        // No numeric columns means no pattern detection and no metric name.
        assert!(
            prefill.situation.is_none(),
            "situation should be None when no numeric columns exist"
        );
        assert!(
            prefill.metric.is_none(),
            "metric should be None when no numeric columns exist"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // build_prefill: new fields from CLI flags
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_log_fields_from_flags() {
        let args = InitArgs {
            message_template: Some("Error at {line}".to_string()),
            severity: Some("balanced".to_string()),
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.message_template.as_deref(), Some("Error at {line}"));
        assert_eq!(prefill.severity.as_deref(), Some("balanced"));
    }

    #[test]
    fn build_prefill_kafka_fields_from_flags() {
        let args = InitArgs {
            sink: Some("kafka".to_string()),
            kafka_brokers: Some("broker:9092".to_string()),
            kafka_topic: Some("events".to_string()),
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.sink.as_deref(), Some("kafka"));
        assert_eq!(prefill.kafka_brokers.as_deref(), Some("broker:9092"));
        assert_eq!(prefill.kafka_topic.as_deref(), Some("events"));
    }

    #[test]
    fn build_prefill_otlp_signal_type_from_flags() {
        let args = InitArgs {
            sink: Some("otlp_grpc".to_string()),
            otlp_signal_type: Some("logs".to_string()),
            ..default_init_args()
        };
        let catalog = ScenarioCatalog::discover(&[]);
        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(prefill.otlp_signal_type.as_deref(), Some("logs"));
    }

    // -----------------------------------------------------------------------
    // build_prefill: scenario with labels
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_from_scenario_extracts_labels() {
        use std::fs;

        let dir =
            std::env::temp_dir().join(format!("sonda-init-labels-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        let scenario_yaml = r#"scenario_name: labeled-scenario
category: network
signal_type: metrics
description: "Scenario with labels"

name: if_traffic
rate: 1
duration: 30s

labels:
  device: rtr-edge-01
  region: us-west

generator:
  type: steady
  center: 50.0
  amplitude: 10.0
  period: "60s"

encoder:
  type: prometheus_text

sink:
  type: stdout
"#;
        fs::write(dir.join("labeled-scenario.yaml"), scenario_yaml).expect("write scenario");

        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        let args = InitArgs {
            from: Some("@labeled-scenario".to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(
            prefill.labels.get("device").map(String::as_str),
            Some("rtr-edge-01")
        );
        assert_eq!(
            prefill.labels.get("region").map(String::as_str),
            Some("us-west")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // build_prefill: CSV sets domain to "custom"
    // -----------------------------------------------------------------------

    #[test]
    fn build_prefill_from_csv_sets_domain_to_custom() {
        use std::fs;

        let dir =
            std::env::temp_dir().join(format!("sonda-init-csv-domain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        let csv_content = "timestamp,cpu_usage\n\
            1000,50.1\n\
            1001,49.9\n\
            1002,50.3\n\
            1003,49.7\n\
            1004,50.2\n\
            1005,49.8\n\
            1006,50.1\n\
            1007,49.9\n\
            1008,50.3\n\
            1009,49.7\n\
            1010,50.2\n\
            1011,49.8\n\
            1012,50.0\n\
            1013,50.1\n\
            1014,49.9\n\
            1015,50.2\n\
            1016,49.8\n\
            1017,50.1\n\
            1018,49.9\n\
            1019,50.0\n";
        let csv_path = dir.join("domain-test.csv");
        fs::write(&csv_path, csv_content).expect("write CSV");

        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some(csv_path.to_str().unwrap().to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(
            prefill.domain.as_deref(),
            Some("custom"),
            "CSV-based prefill must set domain to 'custom'"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_prefill_from_csv_no_numeric_columns_sets_domain_to_custom() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!(
            "sonda-init-csv-no-num-domain-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");

        let csv_content = "timestamp,status\n\
            1000,ok\n\
            1001,ok\n\
            1002,error\n";
        let csv_path = dir.join("no-numeric-domain.csv");
        fs::write(&csv_path, csv_content).expect("write CSV");

        let catalog = ScenarioCatalog::discover(&[]);
        let args = InitArgs {
            from: Some(csv_path.to_str().unwrap().to_string()),
            ..default_init_args()
        };

        let prefill = build_prefill(&args, &catalog).expect("should succeed");
        assert_eq!(
            prefill.domain.as_deref(),
            Some("custom"),
            "CSV-based prefill with no numeric columns must set domain to 'custom'"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
