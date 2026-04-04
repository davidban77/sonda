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
    /// Suppress all status output (errors are still printed).
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Show the resolved configuration at startup, then run normally.
    ///
    /// Mutually exclusive with `--quiet`. Prints the full resolved scenario
    /// config to stderr before starting the event loop.
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Parse and validate the scenario config, print it, then exit without
    /// emitting any events.
    ///
    /// Useful for checking that a YAML file is valid and seeing the resolved
    /// configuration. Works with all subcommands. Orthogonal to `--quiet` and
    /// `--verbose` — always prints the resolved config.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// The operation to perform.
    #[command(subcommand)]
    pub command: Commands,
}

/// Verbosity level derived from `--quiet` / `--verbose` flags.
///
/// `--quiet` and `--verbose` are mutually exclusive (enforced by clap's
/// `conflicts_with`). The default is [`Verbosity::Normal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Suppress all banners and status output.
    Quiet,
    /// Default: show start and stop banners.
    Normal,
    /// Show resolved config at startup, then start and stop banners.
    Verbose,
}

impl Verbosity {
    /// Construct a [`Verbosity`] from the `--quiet` and `--verbose` booleans.
    ///
    /// Clap enforces mutual exclusivity, so at most one of `quiet` and
    /// `verbose` is true.
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

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Generate synthetic metrics and write them to the configured sink.
    Metrics(MetricsArgs),
    /// Generate synthetic log events and write them to the configured sink.
    Logs(LogsArgs),
    /// Run multiple scenarios concurrently from a multi-scenario YAML file.
    ///
    /// The scenario file must contain a top-level `scenarios:` list. Each
    /// entry specifies a `signal_type` of either `metrics` or `logs`, plus
    /// the scenario-specific configuration fields.
    Run(RunArgs),
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

    /// Fixed value emitted by the `constant` generator.
    ///
    /// Only valid when `--value-mode` is `constant` (the default).
    #[arg(long)]
    pub value: Option<f64>,

    /// Sine wave vertical offset.
    ///
    /// Sets the midpoint around which the wave oscillates. Used when
    /// `--value-mode sine`. Default: `0.0`.
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

    /// Burst recurrence interval (e.g. `"10s"`).
    ///
    /// Together with `--burst-for` and `--burst-multiplier`, this defines a
    /// recurring high-rate period: events are emitted at `rate * multiplier`
    /// for `--burst-for` out of every `--burst-every` cycle.
    /// All three `--burst-*` flags must be provided together.
    #[arg(long)]
    pub burst_every: Option<String>,

    /// Burst duration within each cycle (e.g. `"1s"`).
    ///
    /// Must be strictly less than `--burst-every`.
    #[arg(long)]
    pub burst_for: Option<String>,

    /// Rate multiplier during each burst (must be strictly positive, e.g. `10.0`).
    ///
    /// Effective rate during burst = base rate × multiplier.
    #[arg(long)]
    pub burst_multiplier: Option<f64>,

    /// Label key for a cardinality spike (e.g. `"pod_name"`).
    ///
    /// Together with `--spike-every`, `--spike-for`, and `--spike-cardinality`,
    /// defines a recurring window that injects dynamic label values to simulate
    /// cardinality explosions. All four `--spike-*` flags must be provided together.
    #[arg(long)]
    pub spike_label: Option<String>,

    /// Spike recurrence interval (e.g. `"2m"`).
    #[arg(long)]
    pub spike_every: Option<String>,

    /// Spike duration within each cycle (e.g. `"30s"`).
    ///
    /// Must be strictly less than `--spike-every`.
    #[arg(long)]
    pub spike_for: Option<String>,

    /// Number of unique label values during the spike.
    #[arg(long)]
    pub spike_cardinality: Option<u64>,

    /// Spike strategy: `counter` or `random`. Default: `counter`.
    #[arg(long)]
    pub spike_strategy: Option<String>,

    /// Prefix for generated spike label values.
    ///
    /// Defaults to `"{spike_label}_"` when not specified.
    #[arg(long)]
    pub spike_prefix: Option<String>,

    /// RNG seed for the `random` spike strategy.
    #[arg(long)]
    pub spike_seed: Option<u64>,

    /// Optional jitter amplitude. Adds uniform noise in `[-jitter, +jitter]` to
    /// every generated value for more realistic output.
    #[arg(long)]
    pub jitter: Option<f64>,

    /// Optional seed for jitter noise. Defaults to `0` when absent.
    #[arg(long)]
    pub jitter_seed: Option<u64>,

    /// Static label attached to every emitted event (repeatable).
    ///
    /// Format: `key=value`. Keys must match `[a-zA-Z_][a-zA-Z0-9_]*`.
    /// Example: `--label hostname=t0-a1 --label zone=eu1`
    #[arg(long = "label", value_parser = parse_label)]
    pub labels: Vec<(String, String)>,

    /// Output encoder format.
    ///
    /// Accepted values: `prometheus_text`, `influx_lp`, `json_lines`. Default: `prometheus_text`.
    /// When omitted, the YAML scenario file's `encoder` field is used; when
    /// neither is set, `prometheus_text` is the default.
    #[arg(long)]
    pub encoder: Option<String>,

    /// Decimal precision for metric values (0--17).
    ///
    /// Limits the number of decimal places in formatted metric values.
    /// When absent, full f64 precision is used. Applies to text-based
    /// encoders (`prometheus_text`, `influx_lp`, `json_lines`).
    #[arg(long)]
    pub precision: Option<u8>,

    /// Write output to a file at this path instead of stdout.
    ///
    /// Shorthand for `sink: file` in a YAML scenario. Parent directories are
    /// created automatically if they do not exist. Takes precedence over any
    /// sink configured in the scenario file.
    #[arg(long, conflicts_with = "sink")]
    pub output: Option<PathBuf>,

    /// Sink type for delivering encoded events.
    ///
    /// Accepted values: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`.
    /// Mutually exclusive with `--output`. For OTLP, Kafka, and remote write sinks,
    /// the corresponding Cargo feature must be compiled in.
    #[arg(long, conflicts_with = "output")]
    pub sink: Option<String>,

    /// Endpoint URL for the selected sink.
    ///
    /// Required for `--sink http_push`, `--sink remote_write`, `--sink loki`,
    /// and `--sink otlp_grpc`.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// OTLP signal type: `metrics` or `logs`.
    ///
    /// Required for `--sink otlp_grpc` in the metrics subcommand (where the
    /// signal type is ambiguous). In the logs subcommand this defaults to `logs`.
    #[arg(long)]
    pub signal_type: Option<String>,

    /// Batch size for batching sinks (number of entries or bytes, depending on sink).
    ///
    /// Optional for `http_push`, `remote_write`, `loki`, and `otlp_grpc`.
    #[arg(long)]
    pub batch_size: Option<usize>,

    /// Content-Type header for the `http_push` sink.
    ///
    /// Optional; defaults to `application/octet-stream` when not specified.
    #[arg(long)]
    pub content_type: Option<String>,

    /// Comma-separated Kafka broker addresses (e.g. `127.0.0.1:9092`).
    ///
    /// Required for `--sink kafka`.
    #[arg(long)]
    pub brokers: Option<String>,

    /// Kafka topic name.
    ///
    /// Required for `--sink kafka`.
    #[arg(long)]
    pub topic: Option<String>,
}

/// Arguments for the `logs` subcommand.
///
/// All flags are optional when a `--scenario` file is provided. CLI flags take
/// precedence over any value in the scenario file.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Path to a YAML log scenario file.
    ///
    /// When provided, the file is loaded and deserialized first. Any CLI flag
    /// that is also present overrides the corresponding value in the file.
    #[arg(long)]
    pub scenario: Option<PathBuf>,

    /// Log generator mode.
    ///
    /// Accepted values: `template`, `replay`.
    /// Required when no `--scenario` file is provided.
    #[arg(long)]
    pub mode: Option<String>,

    /// Path to a log file for use with `--mode replay`.
    ///
    /// Lines from this file are replayed in order, cycling back to the start
    /// when exhausted. `--replay-file` is accepted as an alias for this flag.
    #[arg(long, alias = "replay-file")]
    pub file: Option<String>,

    /// Target event rate in events per second.
    ///
    /// Must be strictly positive. Defaults to `10.0` when no scenario file
    /// is provided and this flag is omitted.
    #[arg(long)]
    pub rate: Option<f64>,

    /// Total run duration (e.g. `"30s"`, `"5m"`, `"1h"`, `"100ms"`).
    ///
    /// When absent the scenario runs indefinitely until Ctrl+C.
    #[arg(long)]
    pub duration: Option<String>,

    /// Output encoder format.
    ///
    /// Accepted values: `json_lines`, `syslog`. Default: `json_lines`.
    #[arg(long)]
    pub encoder: Option<String>,

    /// Decimal precision for numeric values in log fields (0--17).
    ///
    /// Limits the number of decimal places when the encoder formats
    /// numeric values. When absent, full f64 precision is used.
    /// Only applies to `json_lines`; ignored for `syslog`.
    #[arg(long)]
    pub precision: Option<u8>,

    /// Static label attached to every emitted event (repeatable).
    ///
    /// Format: `key=value`. Keys must match `[a-zA-Z_][a-zA-Z0-9_]*`.
    /// Example: `--label hostname=t0-a1 --label zone=eu1`
    #[arg(long = "label", value_parser = parse_label)]
    pub labels: Vec<(String, String)>,

    /// Gap recurrence interval (e.g. `"2m"`).
    ///
    /// Together with `--gap-for`, this defines a recurring silent period.
    #[arg(long)]
    pub gap_every: Option<String>,

    /// Gap duration within each cycle (e.g. `"20s"`).
    ///
    /// Must be strictly less than `--gap-every`.
    #[arg(long)]
    pub gap_for: Option<String>,

    /// Burst recurrence interval (e.g. `"5s"`).
    ///
    /// Together with `--burst-for` and `--burst-multiplier`, this defines a
    /// recurring high-rate period.
    #[arg(long)]
    pub burst_every: Option<String>,

    /// Burst duration within each cycle (e.g. `"1s"`).
    ///
    /// Must be strictly less than `--burst-every`.
    #[arg(long)]
    pub burst_for: Option<String>,

    /// Rate multiplier during burst periods (e.g. `10.0` for 10× the base rate).
    #[arg(long)]
    pub burst_multiplier: Option<f64>,

    /// Label key for a cardinality spike (e.g. `"pod_name"`).
    ///
    /// Together with `--spike-every`, `--spike-for`, and `--spike-cardinality`,
    /// defines a recurring window that injects dynamic label values.
    #[arg(long)]
    pub spike_label: Option<String>,

    /// Spike recurrence interval (e.g. `"2m"`).
    #[arg(long)]
    pub spike_every: Option<String>,

    /// Spike duration within each cycle (e.g. `"30s"`).
    #[arg(long)]
    pub spike_for: Option<String>,

    /// Number of unique label values during the spike.
    #[arg(long)]
    pub spike_cardinality: Option<u64>,

    /// Spike strategy: `counter` or `random`. Default: `counter`.
    #[arg(long)]
    pub spike_strategy: Option<String>,

    /// Prefix for generated spike label values.
    #[arg(long)]
    pub spike_prefix: Option<String>,

    /// RNG seed for the `random` spike strategy.
    #[arg(long)]
    pub spike_seed: Option<u64>,

    /// Optional jitter amplitude. Adds uniform noise in `[-jitter, +jitter]` to
    /// every generated value for more realistic output.
    #[arg(long)]
    pub jitter: Option<f64>,

    /// Optional seed for jitter noise. Defaults to `0` when absent.
    #[arg(long)]
    pub jitter_seed: Option<u64>,

    /// Write output to a file at this path instead of stdout.
    ///
    /// Shorthand for `sink: file` in a YAML scenario. Takes precedence over
    /// any sink configured in the scenario file.
    #[arg(long, conflicts_with = "sink")]
    pub output: Option<PathBuf>,

    /// Sink type for delivering encoded events.
    ///
    /// Accepted values: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`.
    /// Mutually exclusive with `--output`. For OTLP, Kafka, and remote write sinks,
    /// the corresponding Cargo feature must be compiled in.
    #[arg(long, conflicts_with = "output")]
    pub sink: Option<String>,

    /// Endpoint URL for the selected sink.
    ///
    /// Required for `--sink http_push`, `--sink remote_write`, `--sink loki`,
    /// and `--sink otlp_grpc`.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// OTLP signal type: `metrics` or `logs`.
    ///
    /// For the logs subcommand this defaults to `logs` when `--sink otlp_grpc`
    /// is used, so typically you do not need to specify it.
    #[arg(long)]
    pub signal_type: Option<String>,

    /// Batch size for batching sinks (number of entries or bytes, depending on sink).
    ///
    /// Optional for `http_push`, `remote_write`, `loki`, and `otlp_grpc`.
    #[arg(long)]
    pub batch_size: Option<usize>,

    /// Content-Type header for the `http_push` sink.
    ///
    /// Optional; defaults to `application/octet-stream` when not specified.
    #[arg(long)]
    pub content_type: Option<String>,

    /// Comma-separated Kafka broker addresses (e.g. `127.0.0.1:9092`).
    ///
    /// Required for `--sink kafka`.
    #[arg(long)]
    pub brokers: Option<String>,

    /// Kafka topic name.
    ///
    /// Required for `--sink kafka`.
    #[arg(long)]
    pub topic: Option<String>,

    /// A single static message template for use with `--mode template`.
    ///
    /// Overrides any templates defined in the scenario file. The message string
    /// may contain `{placeholder}` tokens, but no field pools are configured
    /// from the CLI, so placeholders remain as-is unless a scenario file
    /// supplies them.
    #[arg(long)]
    pub message: Option<String>,

    /// Comma-separated severity weight pairs for `--mode template`.
    ///
    /// Format: `info=0.7,warn=0.2,error=0.1`. Weights are relative — they do
    /// not need to sum to 1.0. Valid severity names: `trace`, `debug`, `info`,
    /// `warn`, `error`, `fatal`.
    #[arg(long = "severity-weights")]
    pub severity_weights: Option<String>,

    /// RNG seed for deterministic template resolution.
    ///
    /// Used with `--mode template`. When absent a seed of `0` is used.
    #[arg(long)]
    pub seed: Option<u64>,
}

/// Arguments for the `run` subcommand (multi-scenario).
///
/// Accepts a YAML file that defines multiple concurrent scenarios under a
/// top-level `scenarios:` key. Each entry carries a `signal_type` field
/// (`metrics` or `logs`) along with the full scenario configuration.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to a multi-scenario YAML file.
    ///
    /// The file must have a top-level `scenarios:` list. Each list entry must
    /// include a `signal_type: metrics` or `signal_type: logs` field, followed
    /// by the scenario-specific configuration fields.
    #[arg(long)]
    pub scenario: PathBuf,
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_label happy path -----------------------------------------------

    #[test]
    fn parse_label_simple_key_value() {
        let result = parse_label("hostname=t0-a1").expect("should parse");
        assert_eq!(result, ("hostname".to_string(), "t0-a1".to_string()));
    }

    #[test]
    fn parse_label_value_with_equals_sign() {
        // Only the first '=' splits the key — remainder goes into the value.
        let result = parse_label("key=a=b").expect("should parse");
        assert_eq!(result, ("key".to_string(), "a=b".to_string()));
    }

    #[test]
    fn parse_label_empty_value_is_allowed() {
        let result = parse_label("key=").expect("should parse empty value");
        assert_eq!(result, ("key".to_string(), String::new()));
    }

    #[test]
    fn parse_label_zone_label() {
        let result = parse_label("zone=eu1").expect("should parse zone label");
        assert_eq!(result, ("zone".to_string(), "eu1".to_string()));
    }

    // ---- parse_label error cases ----------------------------------------------

    #[test]
    fn parse_label_no_equals_sign_returns_error() {
        let err = parse_label("bad").expect_err("should fail without '='");
        assert!(
            err.contains("key=value"),
            "error should mention key=value format, got: {err}"
        );
    }

    #[test]
    fn parse_label_empty_string_returns_error() {
        let err = parse_label("").expect_err("empty string should fail");
        // No '=' present — should get the no-equals error.
        assert!(
            err.contains("key=value") || err.contains("'='"),
            "error should mention format, got: {err}"
        );
    }

    #[test]
    fn parse_label_empty_key_returns_error() {
        let err = parse_label("=value").expect_err("empty key should fail");
        assert!(
            err.contains("empty"),
            "error should mention empty key, got: {err}"
        );
    }

    // ---- Verbosity::from_flags ---------------------------------------------------

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

    // ---- CLI parsing: --dry-run and --verbose flags ----------------------------

    #[test]
    fn cli_dry_run_flag_is_parsed() {
        let cli = Cli::try_parse_from([
            "sonda",
            "--dry-run",
            "metrics",
            "--name",
            "test",
            "--rate",
            "1",
        ])
        .expect("--dry-run should parse");
        assert!(cli.dry_run);
    }

    #[test]
    fn cli_verbose_flag_is_parsed() {
        let cli = Cli::try_parse_from([
            "sonda",
            "--verbose",
            "metrics",
            "--name",
            "test",
            "--rate",
            "1",
        ])
        .expect("--verbose should parse");
        assert!(cli.verbose);
    }

    #[test]
    fn cli_quiet_and_verbose_conflict() {
        let result = Cli::try_parse_from([
            "sonda",
            "--quiet",
            "--verbose",
            "metrics",
            "--name",
            "test",
            "--rate",
            "1",
        ]);
        assert!(result.is_err(), "--quiet and --verbose must conflict");
    }

    #[test]
    fn cli_dry_run_orthogonal_to_quiet() {
        let cli = Cli::try_parse_from([
            "sonda",
            "--dry-run",
            "--quiet",
            "metrics",
            "--name",
            "test",
            "--rate",
            "1",
        ])
        .expect("--dry-run + --quiet should parse");
        assert!(cli.dry_run);
        assert!(cli.quiet);
    }

    #[test]
    fn cli_dry_run_orthogonal_to_verbose() {
        let cli = Cli::try_parse_from([
            "sonda",
            "--dry-run",
            "--verbose",
            "metrics",
            "--name",
            "test",
            "--rate",
            "1",
        ])
        .expect("--dry-run + --verbose should parse");
        assert!(cli.dry_run);
        assert!(cli.verbose);
    }

    // ---- --value flag: parsing and validation --------------------------------

    #[test]
    fn cli_value_flag_is_parsed() {
        let cli = Cli::try_parse_from([
            "sonda", "metrics", "--name", "up", "--rate", "1", "--value", "42",
        ])
        .expect("--value should parse");
        match cli.command {
            Commands::Metrics(args) => {
                assert_eq!(args.value, Some(42.0));
            }
            _ => panic!("expected Metrics command"),
        }
    }

    #[test]
    fn cli_value_flag_without_value_mode_is_accepted() {
        let cli = Cli::try_parse_from([
            "sonda", "metrics", "--name", "up", "--rate", "1", "--value", "1",
        ])
        .expect("--value without --value-mode should be accepted (defaults to constant)");
        match cli.command {
            Commands::Metrics(args) => {
                assert_eq!(args.value, Some(1.0));
                assert!(args.value_mode.is_none());
            }
            _ => panic!("expected Metrics command"),
        }
    }
}
