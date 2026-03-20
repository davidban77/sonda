//! CLI argument definitions for the `sonda` binary.
//!
//! All argument structs use the clap derive API. No business logic lives here —
//! parsing is separated from config loading in [`crate::config`].

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Sonda — synthetic telemetry generator.
///
/// Generate realistic observability signals (metrics, logs, traces) for
/// testing pipelines, validating ingest paths, and simulating failure scenarios.
#[derive(Debug, Parser)]
#[command(name = "sonda", version, about = "Synthetic telemetry generator")]
pub struct Cli {
    /// The operation to perform.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Generate synthetic metrics and write them to the configured sink.
    Metrics(MetricsArgs),
}

/// Arguments for the `metrics` subcommand.
///
/// All flags are optional when a `--scenario` file is provided. CLI flags take
/// precedence over any value in the scenario file.
#[derive(Debug, Args)]
pub struct MetricsArgs {
    /// Path to a YAML scenario file.
    ///
    /// When provided, the file is loaded and deserialized first. Any CLI flag
    /// that is also present overrides the corresponding value in the file.
    #[arg(long)]
    pub scenario: Option<PathBuf>,

    /// Metric name emitted by this scenario.
    ///
    /// Must be a valid Prometheus metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
    /// Required when no `--scenario` file is provided.
    #[arg(long)]
    pub name: Option<String>,

    /// Target event rate in events per second.
    ///
    /// Must be strictly positive. Fractional values are supported for
    /// sub-Hz rates (e.g. `0.5` for one event every two seconds).
    /// Required when no `--scenario` file is provided.
    #[arg(long)]
    pub rate: Option<f64>,

    /// Total run duration (e.g. `"30s"`, `"5m"`, `"1h"`, `"100ms"`).
    ///
    /// When absent the scenario runs indefinitely until Ctrl+C.
    #[arg(long)]
    pub duration: Option<String>,

    /// Value generator mode.
    ///
    /// Accepted values: `constant`, `uniform`, `sine`, `sawtooth`.
    /// Defaults to `constant` when no scenario file is provided and this
    /// flag is omitted.
    #[arg(long)]
    pub value_mode: Option<String>,

    /// Sine wave amplitude (half the peak-to-peak swing).
    ///
    /// Used when `--value-mode sine`. Default: `1.0`.
    #[arg(long)]
    pub amplitude: Option<f64>,

    /// Sine wave or sawtooth period in seconds.
    ///
    /// Used when `--value-mode sine` or `--value-mode sawtooth`. Default: `60.0`.
    #[arg(long)]
    pub period_secs: Option<f64>,

    /// Sine wave vertical offset, or the constant value for `--value-mode constant`.
    ///
    /// For `sine`: sets the midpoint around which the wave oscillates.
    /// For `constant`: this is the emitted value. Default: `0.0`.
    #[arg(long)]
    pub offset: Option<f64>,

    /// Minimum value for the `uniform` generator.
    ///
    /// Used when `--value-mode uniform`. Default: `0.0`.
    #[arg(long)]
    pub min: Option<f64>,

    /// Maximum value for the `uniform` generator.
    ///
    /// Used when `--value-mode uniform`. Default: `1.0`.
    #[arg(long)]
    pub max: Option<f64>,

    /// RNG seed for the `uniform` generator (enables deterministic replay).
    ///
    /// When absent a seed of `0` is used.
    #[arg(long)]
    pub seed: Option<u64>,

    /// Gap recurrence interval (e.g. `"2m"`).
    ///
    /// Together with `--gap-for`, this defines a recurring silent period:
    /// no events are emitted for `--gap-for` out of every `--gap-every` cycle.
    /// Both `--gap-every` and `--gap-for` must be provided together.
    #[arg(long)]
    pub gap_every: Option<String>,

    /// Gap duration within each cycle (e.g. `"20s"`).
    ///
    /// Must be strictly less than `--gap-every`.
    #[arg(long)]
    pub gap_for: Option<String>,

    /// Static label attached to every emitted event (repeatable).
    ///
    /// Format: `key=value`. Keys must match `[a-zA-Z_][a-zA-Z0-9_]*`.
    /// Example: `--label hostname=t0-a1 --label zone=eu1`
    #[arg(long = "label", value_parser = parse_label)]
    pub labels: Vec<(String, String)>,

    /// Output encoder format.
    ///
    /// Accepted values: `prometheus_text`. Default: `prometheus_text`.
    #[arg(long, default_value = "prometheus_text")]
    pub encoder: String,
}

/// Parse a `key=value` label string into a `(String, String)` pair.
///
/// Returns an error if the string does not contain an `=` character.
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
