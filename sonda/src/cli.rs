//! Clap argument definitions for the `sonda` binary.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "sonda", version, about = "Synthetic telemetry generator", styles = clap_styles())]
pub struct Cli {
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Parse and validate the scenario, print it, exit without emitting events.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Directory containing scenario / pack YAML files for `@name` resolution.
    #[arg(long, global = true, value_name = "DIR")]
    pub catalog: Option<PathBuf>,

    /// Output format for `--dry-run` on v2 scenario files: `text` (default) or `json`.
    #[arg(long, global = true, value_name = "FORMAT")]
    pub format: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

impl Verbosity {
    pub fn from_flags(quiet: bool, verbose: bool) -> Self {
        if quiet {
            Verbosity::Quiet
        } else if verbose {
            Verbosity::Verbose
        } else {
            Verbosity::Normal
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a scenario from a YAML file or `@name` catalog reference.
    Run(RunArgs),
    /// List catalog entries.
    List(ListArgs),
    /// Print a catalog entry's YAML.
    Show(ShowArgs),
    /// Scaffold a new scenario YAML.
    New(NewArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to a v2 YAML file, or `@name` for a catalog reference.
    pub scenario: String,

    #[arg(long)]
    pub duration: Option<String>,

    #[arg(long)]
    pub rate: Option<f64>,

    #[arg(long, help_heading = "Sink")]
    pub sink: Option<String>,

    #[arg(long, help_heading = "Sink")]
    pub endpoint: Option<String>,

    #[arg(long, help_heading = "Encoder")]
    pub encoder: Option<String>,

    #[arg(short = 'o', long, conflicts_with = "sink", help_heading = "Sink")]
    pub output: Option<PathBuf>,

    #[arg(long = "label", value_parser = parse_label, help_heading = "Scenario")]
    pub labels: Vec<(String, String)>,

    #[arg(long, value_parser = parse_on_sink_error, help_heading = "Scenario")]
    pub on_sink_error: Option<sonda_core::OnSinkError>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by kind: `runnable` or `composable`.
    #[arg(long)]
    pub kind: Option<String>,

    /// Filter by tag (matches any entry whose `tags:` contains this value).
    #[arg(long)]
    pub tag: Option<String>,

    /// Emit a stable JSON array on stdout instead of the default table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// `@name` of a catalog entry.
    pub name: String,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Print a minimal template YAML to stdout and exit (skip the interactive flow).
    #[arg(long)]
    pub template: bool,

    /// Seed the scaffold from a CSV file (pattern detection picks an operational alias).
    #[arg(long, value_name = "FILE")]
    pub from: Option<PathBuf>,

    /// Output file path. When omitted, the YAML is printed to stdout.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

fn clap_styles() -> clap::builder::styling::Styles {
    use clap::builder::styling::{AnsiColor, Style, Styles};

    Styles::styled()
        .header(Style::new().bold().underline())
        .usage(Style::new().bold())
        .literal(Style::new().fg_color(Some(AnsiColor::Cyan.into())).bold())
        .placeholder(Style::new().fg_color(Some(AnsiColor::Green.into())))
        .valid(Style::new().fg_color(Some(AnsiColor::Green.into())))
        .invalid(Style::new().fg_color(Some(AnsiColor::Red.into())))
}

pub fn parse_on_sink_error(s: &str) -> Result<sonda_core::OnSinkError, String> {
    match s {
        "warn" => Ok(sonda_core::OnSinkError::Warn),
        "fail" => Ok(sonda_core::OnSinkError::Fail),
        other => Err(format!(
            "invalid --on-sink-error {other:?}: expected 'warn' or 'fail'"
        )),
    }
}

pub fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.find('=') {
        Some(pos) => {
            let key = s[..pos].to_string();
            let value = s[pos + 1..].to_string();
            if key.is_empty() {
                return Err(format!("label key must not be empty in {:?}", s));
            }
            Ok((key, value))
        }
        None => Err(format!(
            "label {:?} must be in key=value format (no '=' found)",
            s
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_label_simple_key_value() {
        let result = parse_label("hostname=t0-a1").expect("should parse");
        assert_eq!(result, ("hostname".to_string(), "t0-a1".to_string()));
    }

    #[test]
    fn parse_label_value_with_equals_sign() {
        let result = parse_label("key=a=b").expect("should parse");
        assert_eq!(result, ("key".to_string(), "a=b".to_string()));
    }

    #[test]
    fn parse_label_empty_value_is_allowed() {
        let result = parse_label("key=").expect("should parse empty value");
        assert_eq!(result, ("key".to_string(), String::new()));
    }

    #[test]
    fn parse_label_no_equals_sign_returns_error() {
        let err = parse_label("bad").expect_err("should fail without '='");
        assert!(err.contains("key=value"), "got: {err}");
    }

    #[test]
    fn parse_label_empty_key_returns_error() {
        let err = parse_label("=value").expect_err("empty key should fail");
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn verbosity_default_is_normal() {
        assert_eq!(Verbosity::from_flags(false, false), Verbosity::Normal);
    }

    #[test]
    fn verbosity_quiet_flag() {
        assert_eq!(Verbosity::from_flags(true, false), Verbosity::Quiet);
    }

    #[test]
    fn verbosity_verbose_flag() {
        assert_eq!(Verbosity::from_flags(false, true), Verbosity::Verbose);
    }

    #[test]
    fn cli_dry_run_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "--dry-run", "run", "scenario.yaml"])
            .expect("--dry-run should parse");
        assert!(cli.dry_run);
    }

    #[test]
    fn cli_verbose_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "--verbose", "run", "scenario.yaml"])
            .expect("--verbose should parse");
        assert!(cli.verbose);
    }

    #[test]
    fn cli_quiet_and_verbose_conflict() {
        let result = Cli::try_parse_from(["sonda", "--quiet", "--verbose", "run", "scenario.yaml"]);
        assert!(result.is_err(), "--quiet and --verbose must conflict");
    }

    #[test]
    fn cli_catalog_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "--catalog", "/tmp/cat", "list"])
            .expect("--catalog should parse");
        assert_eq!(
            cli.catalog.as_deref(),
            Some(std::path::Path::new("/tmp/cat"))
        );
        assert!(matches!(cli.command, Commands::List(_)));
    }

    #[test]
    fn cli_scenario_path_flag_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "--scenario-path", "/tmp", "run", "x.yaml"]);
        assert!(result.is_err(), "--scenario-path must be rejected");
    }

    #[test]
    fn cli_pack_path_flag_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "--pack-path", "/tmp", "run", "x.yaml"]);
        assert!(result.is_err(), "--pack-path must be rejected");
    }

    #[test]
    fn cli_metrics_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "metrics", "--name", "x", "--rate", "1"]);
        assert!(result.is_err(), "metrics subcommand must be removed");
    }

    #[test]
    fn cli_logs_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "logs", "--mode", "template"]);
        assert!(result.is_err(), "logs subcommand must be removed");
    }

    #[test]
    fn cli_import_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "import", "x.csv", "--analyze"]);
        assert!(result.is_err(), "import subcommand must be removed");
    }

    #[test]
    fn cli_init_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "init"]);
        assert!(result.is_err(), "init subcommand must be removed");
    }

    #[test]
    fn cli_scenarios_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "scenarios", "list"]);
        assert!(result.is_err(), "scenarios subcommand must be removed");
    }

    #[test]
    fn cli_packs_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "packs", "list"]);
        assert!(result.is_err(), "packs subcommand must be removed");
    }

    #[test]
    fn cli_catalog_subcommand_is_rejected() {
        let result = Cli::try_parse_from(["sonda", "catalog", "list"]);
        assert!(result.is_err(), "catalog subcommand must be removed");
    }

    #[test]
    fn cli_run_at_name_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "run", "@cpu-spike"]).expect("run @name parses");
        match cli.command {
            Commands::Run(args) => assert_eq!(args.scenario, "@cpu-spike"),
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn cli_list_kind_filter_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "list", "--kind", "runnable"])
            .expect("list --kind parses");
        match cli.command {
            Commands::List(args) => assert_eq!(args.kind.as_deref(), Some("runnable")),
            _ => panic!("expected List command"),
        }
    }

    #[test]
    fn cli_new_template_flag_is_parsed() {
        let cli =
            Cli::try_parse_from(["sonda", "new", "--template"]).expect("new --template parses");
        match cli.command {
            Commands::New(args) => assert!(args.template),
            _ => panic!("expected New command"),
        }
    }
}
