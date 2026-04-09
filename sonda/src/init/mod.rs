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

use yaml_gen::{render_scenario_yaml, suggest_filename};

/// Run the `sonda init` interactive scaffolding flow.
///
/// Walks the user through building a scenario, generates the YAML, and
/// writes it to the chosen output path.
///
/// # Errors
///
/// Returns an error if:
/// - Terminal interaction fails (stdin is not a TTY).
/// - The output file cannot be written.
pub fn run_init(pack_catalog: &PackCatalog) -> Result<()> {
    let bold = owo_colors::Style::new().bold();

    eprintln!(
        "\n{}\n",
        "sonda init — guided scenario scaffolding".if_supports_color(Stderr, |t| t.style(bold))
    );
    eprintln!(
        "Answer the questions below to generate a runnable scenario YAML.\n\
         Every prompt has a default — press Enter to accept it.\n"
    );

    // Run the interactive prompts.
    let (kind, delivery) =
        prompts::run_prompts(pack_catalog).context("interactive prompt failed")?;

    // Render the YAML.
    let yaml = render_scenario_yaml(&kind, &delivery);

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

    // Print success message.
    let green = owo_colors::Style::new().green().bold();
    eprintln!(
        "\n{} Scenario written to {}",
        "done:".if_supports_color(Stderr, |t| t.style(green)),
        output_path.if_supports_color(Stderr, |t| t.style(bold))
    );
    eprintln!();
    eprintln!("  Run it with:");

    match &kind {
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

    eprintln!();

    Ok(())
}
