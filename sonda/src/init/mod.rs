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
//! # Module structure
//!
//! - [`prompts`] — interactive prompt logic using `dialoguer`.
//! - [`yaml_gen`] — YAML rendering from collected answers.

pub mod prompts;
pub mod yaml_gen;

use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use crate::packs::PackCatalog;

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

/// Run the `sonda init` interactive scaffolding flow.
///
/// Walks the user through building a scenario, generates the YAML, and
/// writes it to the chosen output path. After writing, offers to run the
/// scenario immediately.
///
/// Returns an [`InitResult`] containing the generated YAML, output path,
/// and whether the user chose immediate execution.
///
/// # Errors
///
/// Returns an error if:
/// - Terminal interaction fails (stdin is not a TTY).
/// - The output file cannot be written.
pub fn run_init(pack_catalog: &PackCatalog) -> Result<InitResult> {
    print_welcome_banner();

    // Run the interactive prompts.
    let (kind, delivery) =
        prompts::run_prompts(pack_catalog).context("interactive prompt failed")?;

    // Remember the scenario type before rendering.
    let scenario_type = kind.scenario_type();

    // Render the YAML.
    let yaml = render_scenario_yaml(&kind, &delivery);

    // Show a preview of the generated YAML before asking for the output path.
    print_yaml_preview(&yaml);

    // Section 4: Output.
    prompts::print_section(4, 4, "Output");

    // Prompt for output path.
    let suggested = suggest_filename(&kind);
    let theme = ColorfulTheme::default();
    let output_path =
        prompts::prompt_output_path(&theme, &suggested).context("output path prompt failed")?;

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
    let run_now = prompts::prompt_run_now(&theme).context("run-now prompt failed")?;

    Ok(InitResult {
        yaml,
        run_now,
        scenario_type,
    })
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
    }

    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
