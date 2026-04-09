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
#[command(name = "sonda", version, about = "Synthetic telemetry generator", styles = clap_styles())]
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

    /// Directory containing metric pack YAML files.
    ///
    /// When provided, this is the **sole** search path for packs — the
    /// `SONDA_PACK_PATH` env var and default directories (`./packs/`,
    /// `~/.sonda/packs/`) are not consulted. Useful for one-off testing
    /// with a custom pack collection.
    #[arg(long, global = true)]
    pub pack_path: Option<PathBuf>,

    /// Directory containing scenario YAML files.
    ///
    /// When provided, this is the **sole** search path for scenarios — the
    /// `SONDA_SCENARIO_PATH` env var and default directories (`./scenarios/`,
    /// `~/.sonda/scenarios/`) are not consulted. Useful for one-off testing
    /// with a custom scenario collection.
    #[arg(long, global = true)]
    pub scenario_path: Option<PathBuf>,

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
    /// Generate synthetic histogram metrics (bucket, count, sum series).
    ///
    /// Produces Prometheus-style histogram data with cumulative bucket counts.
    /// Requires a `--scenario` file with histogram-specific configuration
    /// (distribution model, bucket boundaries, observations per tick).
    Histogram(HistogramArgs),
    /// Generate synthetic summary metrics (quantile, count, sum series).
    ///
    /// Produces Prometheus-style summary data with pre-computed quantile values.
    /// Requires a `--scenario` file with summary-specific configuration
    /// (distribution model, quantile targets, observations per tick).
    Summary(SummaryArgs),
    /// Run multiple scenarios concurrently from a multi-scenario YAML file.
    ///
    /// The scenario file must contain a top-level `scenarios:` list. Each
    /// entry specifies a `signal_type` of either `metrics`, `logs`,
    /// `histogram`, or `summary`, plus the scenario-specific configuration
    /// fields.
    Run(RunArgs),
    /// Browse, inspect, and run pre-built scenario patterns.
    ///
    /// The `scenarios` subcommand provides access to scenario YAML files
    /// discovered from the search path (`--scenario-path`,
    /// `SONDA_SCENARIO_PATH`, `./scenarios/`, `~/.sonda/scenarios/`).
    /// Use `list` to discover available scenarios, `show` to view the raw
    /// YAML, and `run` to execute one directly.
    Scenarios(ScenariosArgs),
    /// Browse, inspect, and run metric packs from the filesystem.
    ///
    /// A metric pack is a reusable bundle of metric names and label schemas
    /// that expands into a multi-metric scenario. Packs are discovered from
    /// the search path (`--pack-path`, `SONDA_PACK_PATH`, `./packs/`,
    /// `~/.sonda/packs/`). Use `list` to discover available packs, `show`
    /// to view the raw YAML, and `run` to execute one with overrides.
    Packs(PacksArgs),
    /// Import a CSV file: detect time-series patterns and generate a scenario.
    ///
    /// Analyzes numeric columns in a CSV file, detects dominant patterns
    /// (steady, spike, climb, flap, sawtooth, step), and generates a portable
    /// scenario YAML that uses sonda generators instead of `csv_replay`.
    ///
    /// Use `--analyze` for read-only pattern analysis, `-o` to write a
    /// scenario file, or `--run` to generate and immediately execute.
    Import(ImportArgs),
    /// Interactively create a new scenario YAML file.
    ///
    /// Walks through a guided prompt flow asking domain-relevant questions
    /// (signal type, situation, rate, etc.) and generates a valid, runnable
    /// scenario YAML. Uses operational language — "What situation?" not
    /// "Which generator type?".
    ///
    /// The generated YAML can be immediately run with `sonda run --scenario`.
    Init(InitArgs),
}

/// Arguments for the `histogram` subcommand.
///
/// Requires a `--scenario` file — histogram scenarios are too complex for
/// inline CLI flags alone.
#[derive(Debug, Args)]
pub struct HistogramArgs {
    /// Path to a YAML histogram scenario file.
    ///
    /// The file must contain a histogram scenario configuration with a
    /// `distribution` field specifying the observation model.
    #[arg(long)]
    pub scenario: PathBuf,
}

/// Arguments for the `summary` subcommand.
///
/// Requires a `--scenario` file — summary scenarios are too complex for
/// inline CLI flags alone.
#[derive(Debug, Args)]
pub struct SummaryArgs {
    /// Path to a YAML summary scenario file.
    ///
    /// The file must contain a summary scenario configuration with a
    /// `distribution` field specifying the observation model.
    #[arg(long)]
    pub scenario: PathBuf,
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
    #[arg(long, help_heading = "Generator")]
    pub value_mode: Option<String>,

    /// Sine wave amplitude (half the peak-to-peak swing).
    ///
    /// Used when `--value-mode sine`. Default: `1.0`.
    #[arg(long, help_heading = "Generator")]
    pub amplitude: Option<f64>,

    /// Sine wave or sawtooth period in seconds.
    ///
    /// Used when `--value-mode sine` or `--value-mode sawtooth`. Default: `60.0`.
    #[arg(long, help_heading = "Generator")]
    pub period_secs: Option<f64>,

    /// Fixed value emitted by the `constant` generator.
    ///
    /// Only valid when `--value-mode` is `constant` (the default).
    #[arg(long, help_heading = "Generator")]
    pub value: Option<f64>,

    /// Sine wave vertical offset.
    ///
    /// Sets the midpoint around which the wave oscillates. Used when
    /// `--value-mode sine`. Default: `0.0`.
    #[arg(long, help_heading = "Generator")]
    pub offset: Option<f64>,

    /// Minimum value for the `uniform` generator.
    ///
    /// Used when `--value-mode uniform`. Default: `0.0`.
    #[arg(long, help_heading = "Generator")]
    pub min: Option<f64>,

    /// Maximum value for the `uniform` generator.
    ///
    /// Used when `--value-mode uniform`. Default: `1.0`.
    #[arg(long, help_heading = "Generator")]
    pub max: Option<f64>,

    /// RNG seed for the `uniform` generator (enables deterministic replay).
    ///
    /// When absent a seed of `0` is used.
    #[arg(long, help_heading = "Generator")]
    pub seed: Option<u64>,

    /// Gap recurrence interval (e.g. `"2m"`).
    ///
    /// Together with `--gap-for`, this defines a recurring silent period:
    /// no events are emitted for `--gap-for` out of every `--gap-every` cycle.
    /// Both `--gap-every` and `--gap-for` must be provided together.
    #[arg(long, help_heading = "Schedule")]
    pub gap_every: Option<String>,

    /// Gap duration within each cycle (e.g. `"20s"`).
    ///
    /// Must be strictly less than `--gap-every`.
    #[arg(long, help_heading = "Schedule")]
    pub gap_for: Option<String>,

    /// Burst recurrence interval (e.g. `"10s"`).
    ///
    /// Together with `--burst-for` and `--burst-multiplier`, this defines a
    /// recurring high-rate period: events are emitted at `rate * multiplier`
    /// for `--burst-for` out of every `--burst-every` cycle.
    /// All three `--burst-*` flags must be provided together.
    #[arg(long, help_heading = "Schedule")]
    pub burst_every: Option<String>,

    /// Burst duration within each cycle (e.g. `"1s"`).
    ///
    /// Must be strictly less than `--burst-every`.
    #[arg(long, help_heading = "Schedule")]
    pub burst_for: Option<String>,

    /// Rate multiplier during each burst (must be strictly positive, e.g. `10.0`).
    ///
    /// Effective rate during burst = base rate × multiplier.
    #[arg(long, help_heading = "Schedule")]
    pub burst_multiplier: Option<f64>,

    /// Label key for a cardinality spike (e.g. `"pod_name"`).
    ///
    /// Together with `--spike-every`, `--spike-for`, and `--spike-cardinality`,
    /// defines a recurring window that injects dynamic label values to simulate
    /// cardinality explosions. All four `--spike-*` flags must be provided together.
    #[arg(long, help_heading = "Schedule")]
    pub spike_label: Option<String>,

    /// Spike recurrence interval (e.g. `"2m"`).
    #[arg(long, help_heading = "Schedule")]
    pub spike_every: Option<String>,

    /// Spike duration within each cycle (e.g. `"30s"`).
    ///
    /// Must be strictly less than `--spike-every`.
    #[arg(long, help_heading = "Schedule")]
    pub spike_for: Option<String>,

    /// Number of unique label values during the spike.
    #[arg(long, help_heading = "Schedule")]
    pub spike_cardinality: Option<u64>,

    /// Spike strategy: `counter` or `random`. Default: `counter`.
    #[arg(long, help_heading = "Schedule")]
    pub spike_strategy: Option<String>,

    /// Prefix for generated spike label values.
    ///
    /// Defaults to `"{spike_label}_"` when not specified.
    #[arg(long, help_heading = "Schedule")]
    pub spike_prefix: Option<String>,

    /// RNG seed for the `random` spike strategy.
    #[arg(long, help_heading = "Schedule")]
    pub spike_seed: Option<u64>,

    /// Optional jitter amplitude. Adds uniform noise in `[-jitter, +jitter]` to
    /// every generated value for more realistic output.
    #[arg(long, help_heading = "Schedule")]
    pub jitter: Option<f64>,

    /// Optional seed for jitter noise. Defaults to `0` when absent.
    #[arg(long, help_heading = "Schedule")]
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
    #[arg(long, help_heading = "Encoder")]
    pub encoder: Option<String>,

    /// Decimal precision for metric values (0--17).
    ///
    /// Limits the number of decimal places in formatted metric values.
    /// When absent, full f64 precision is used. Applies to text-based
    /// encoders (`prometheus_text`, `influx_lp`, `json_lines`).
    #[arg(long, help_heading = "Encoder")]
    pub precision: Option<u8>,

    /// Write output to a file at this path instead of stdout.
    ///
    /// Shorthand for `sink: file` in a YAML scenario. Parent directories are
    /// created automatically if they do not exist. Takes precedence over any
    /// sink configured in the scenario file.
    #[arg(long, conflicts_with = "sink", help_heading = "Sink")]
    pub output: Option<PathBuf>,

    /// Sink type for delivering encoded events.
    ///
    /// Accepted values: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`.
    /// Mutually exclusive with `--output`. For OTLP, Kafka, and remote write sinks,
    /// the corresponding Cargo feature must be compiled in.
    #[arg(long, conflicts_with = "output", help_heading = "Sink")]
    pub sink: Option<String>,

    /// Endpoint URL for the selected sink.
    ///
    /// Required for `--sink http_push`, `--sink remote_write`, `--sink loki`,
    /// and `--sink otlp_grpc`.
    #[arg(long, help_heading = "Sink")]
    pub endpoint: Option<String>,

    /// OTLP signal type: `metrics` or `logs`.
    ///
    /// Required for `--sink otlp_grpc` in the metrics subcommand (where the
    /// signal type is ambiguous). In the logs subcommand this defaults to `logs`.
    #[arg(long, help_heading = "Sink")]
    pub signal_type: Option<String>,

    /// Batch size for batching sinks (number of entries or bytes, depending on sink).
    ///
    /// Optional for `http_push`, `remote_write`, `loki`, and `otlp_grpc`.
    #[arg(long, help_heading = "Sink")]
    pub batch_size: Option<usize>,

    /// Content-Type header for the `http_push` sink.
    ///
    /// Optional; defaults to `application/octet-stream` when not specified.
    #[arg(long, help_heading = "Sink")]
    pub content_type: Option<String>,

    /// Comma-separated Kafka broker addresses (e.g. `127.0.0.1:9092`).
    ///
    /// Required for `--sink kafka`.
    #[arg(long, help_heading = "Sink")]
    pub brokers: Option<String>,

    /// Kafka topic name.
    ///
    /// Required for `--sink kafka`.
    #[arg(long, help_heading = "Sink")]
    pub topic: Option<String>,

    /// Maximum retry attempts after initial failure.
    ///
    /// Together with `--retry-backoff` and `--retry-max-backoff`, configures
    /// exponential backoff retry for network sinks. All three flags must be
    /// provided together.
    #[arg(long, help_heading = "Sink")]
    pub retry_max_attempts: Option<u32>,

    /// Initial backoff duration for retries (e.g. `"100ms"`, `"1s"`).
    ///
    /// Must be provided together with `--retry-max-attempts` and
    /// `--retry-max-backoff`.
    #[arg(long, help_heading = "Sink")]
    pub retry_backoff: Option<String>,

    /// Maximum backoff cap for retries (e.g. `"5s"`, `"30s"`).
    ///
    /// Must be >= `--retry-backoff`. Must be provided together with
    /// `--retry-max-attempts` and `--retry-backoff`.
    #[arg(long, help_heading = "Sink")]
    pub retry_max_backoff: Option<String>,
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
    #[arg(long, help_heading = "Generator")]
    pub mode: Option<String>,

    /// Path to a log file for use with `--mode replay`.
    ///
    /// Lines from this file are replayed in order, cycling back to the start
    /// when exhausted. `--replay-file` is accepted as an alias for this flag.
    #[arg(long, alias = "replay-file", help_heading = "Generator")]
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
    #[arg(long, help_heading = "Encoder")]
    pub encoder: Option<String>,

    /// Decimal precision for numeric values in log fields (0--17).
    ///
    /// Limits the number of decimal places when the encoder formats
    /// numeric values. When absent, full f64 precision is used.
    /// Only applies to `json_lines`; ignored for `syslog`.
    #[arg(long, help_heading = "Encoder")]
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
    #[arg(long, help_heading = "Schedule")]
    pub gap_every: Option<String>,

    /// Gap duration within each cycle (e.g. `"20s"`).
    ///
    /// Must be strictly less than `--gap-every`.
    #[arg(long, help_heading = "Schedule")]
    pub gap_for: Option<String>,

    /// Burst recurrence interval (e.g. `"5s"`).
    ///
    /// Together with `--burst-for` and `--burst-multiplier`, this defines a
    /// recurring high-rate period.
    #[arg(long, help_heading = "Schedule")]
    pub burst_every: Option<String>,

    /// Burst duration within each cycle (e.g. `"1s"`).
    ///
    /// Must be strictly less than `--burst-every`.
    #[arg(long, help_heading = "Schedule")]
    pub burst_for: Option<String>,

    /// Rate multiplier during burst periods (e.g. `10.0` for 10× the base rate).
    #[arg(long, help_heading = "Schedule")]
    pub burst_multiplier: Option<f64>,

    /// Label key for a cardinality spike (e.g. `"pod_name"`).
    ///
    /// Together with `--spike-every`, `--spike-for`, and `--spike-cardinality`,
    /// defines a recurring window that injects dynamic label values.
    #[arg(long, help_heading = "Schedule")]
    pub spike_label: Option<String>,

    /// Spike recurrence interval (e.g. `"2m"`).
    #[arg(long, help_heading = "Schedule")]
    pub spike_every: Option<String>,

    /// Spike duration within each cycle (e.g. `"30s"`).
    #[arg(long, help_heading = "Schedule")]
    pub spike_for: Option<String>,

    /// Number of unique label values during the spike.
    #[arg(long, help_heading = "Schedule")]
    pub spike_cardinality: Option<u64>,

    /// Spike strategy: `counter` or `random`. Default: `counter`.
    #[arg(long, help_heading = "Schedule")]
    pub spike_strategy: Option<String>,

    /// Prefix for generated spike label values.
    #[arg(long, help_heading = "Schedule")]
    pub spike_prefix: Option<String>,

    /// RNG seed for the `random` spike strategy.
    #[arg(long, help_heading = "Schedule")]
    pub spike_seed: Option<u64>,

    /// Optional jitter amplitude. Adds uniform noise in `[-jitter, +jitter]` to
    /// every generated value for more realistic output.
    #[arg(long, help_heading = "Schedule")]
    pub jitter: Option<f64>,

    /// Optional seed for jitter noise. Defaults to `0` when absent.
    #[arg(long, help_heading = "Schedule")]
    pub jitter_seed: Option<u64>,

    /// Write output to a file at this path instead of stdout.
    ///
    /// Shorthand for `sink: file` in a YAML scenario. Takes precedence over
    /// any sink configured in the scenario file.
    #[arg(long, conflicts_with = "sink", help_heading = "Sink")]
    pub output: Option<PathBuf>,

    /// Sink type for delivering encoded events.
    ///
    /// Accepted values: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`.
    /// Mutually exclusive with `--output`. For OTLP, Kafka, and remote write sinks,
    /// the corresponding Cargo feature must be compiled in.
    #[arg(long, conflicts_with = "output", help_heading = "Sink")]
    pub sink: Option<String>,

    /// Endpoint URL for the selected sink.
    ///
    /// Required for `--sink http_push`, `--sink remote_write`, `--sink loki`,
    /// and `--sink otlp_grpc`.
    #[arg(long, help_heading = "Sink")]
    pub endpoint: Option<String>,

    /// OTLP signal type: `metrics` or `logs`.
    ///
    /// For the logs subcommand this defaults to `logs` when `--sink otlp_grpc`
    /// is used, so typically you do not need to specify it.
    #[arg(long, help_heading = "Sink")]
    pub signal_type: Option<String>,

    /// Batch size for batching sinks (number of entries or bytes, depending on sink).
    ///
    /// Optional for `http_push`, `remote_write`, `loki`, and `otlp_grpc`.
    #[arg(long, help_heading = "Sink")]
    pub batch_size: Option<usize>,

    /// Content-Type header for the `http_push` sink.
    ///
    /// Optional; defaults to `application/octet-stream` when not specified.
    #[arg(long, help_heading = "Sink")]
    pub content_type: Option<String>,

    /// Comma-separated Kafka broker addresses (e.g. `127.0.0.1:9092`).
    ///
    /// Required for `--sink kafka`.
    #[arg(long, help_heading = "Sink")]
    pub brokers: Option<String>,

    /// Kafka topic name.
    ///
    /// Required for `--sink kafka`.
    #[arg(long, help_heading = "Sink")]
    pub topic: Option<String>,

    /// A single static message template for use with `--mode template`.
    ///
    /// Overrides any templates defined in the scenario file. The message string
    /// may contain `{placeholder}` tokens, but no field pools are configured
    /// from the CLI, so placeholders remain as-is unless a scenario file
    /// supplies them.
    #[arg(long, help_heading = "Generator")]
    pub message: Option<String>,

    /// Comma-separated severity weight pairs for `--mode template`.
    ///
    /// Format: `info=0.7,warn=0.2,error=0.1`. Weights are relative — they do
    /// not need to sum to 1.0. Valid severity names: `trace`, `debug`, `info`,
    /// `warn`, `error`, `fatal`.
    #[arg(long = "severity-weights", help_heading = "Generator")]
    pub severity_weights: Option<String>,

    /// RNG seed for deterministic template resolution.
    ///
    /// Used with `--mode template`. When absent a seed of `0` is used.
    #[arg(long, help_heading = "Generator")]
    pub seed: Option<u64>,

    /// Maximum retry attempts after initial failure.
    ///
    /// Together with `--retry-backoff` and `--retry-max-backoff`, configures
    /// exponential backoff retry for network sinks. All three flags must be
    /// provided together.
    #[arg(long, help_heading = "Sink")]
    pub retry_max_attempts: Option<u32>,

    /// Initial backoff duration for retries (e.g. `"100ms"`, `"1s"`).
    ///
    /// Must be provided together with `--retry-max-attempts` and
    /// `--retry-max-backoff`.
    #[arg(long, help_heading = "Sink")]
    pub retry_backoff: Option<String>,

    /// Maximum backoff cap for retries (e.g. `"5s"`, `"30s"`).
    ///
    /// Must be >= `--retry-backoff`. Must be provided together with
    /// `--retry-max-attempts` and `--retry-backoff`.
    #[arg(long, help_heading = "Sink")]
    pub retry_max_backoff: Option<String>,
}

/// Arguments for the `run` subcommand (multi-scenario).
///
/// Accepts a YAML file that defines multiple concurrent scenarios under a
/// top-level `scenarios:` key. Each entry carries a `signal_type` field
/// (`metrics`, `logs`, `histogram`, or `summary`) along with the full
/// scenario configuration.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to a multi-scenario YAML file.
    ///
    /// The file must have a top-level `scenarios:` list. Each list entry must
    /// include a `signal_type` field (`metrics`, `logs`, `histogram`, or
    /// `summary`), followed by the scenario-specific configuration fields.
    #[arg(long)]
    pub scenario: PathBuf,
}

/// Arguments for the `scenarios` subcommand.
///
/// Provides access to scenario files discovered from the filesystem.
#[derive(Debug, Args)]
pub struct ScenariosArgs {
    /// The scenarios action to perform.
    #[command(subcommand)]
    pub action: ScenariosAction,
}

/// Actions available under `sonda scenarios`.
#[derive(Debug, Subcommand)]
pub enum ScenariosAction {
    /// List all available scenarios.
    ///
    /// Prints a formatted table with NAME, CATEGORY, SIGNAL, and DESCRIPTION
    /// columns. Use `--category` to filter by category.
    List(ScenariosListArgs),
    /// Show the raw YAML for a scenario.
    ///
    /// Prints the full YAML content to stdout, suitable for piping to a file
    /// for customization.
    Show(ScenariosShowArgs),
    /// Run a scenario with optional overrides.
    ///
    /// Executes the scenario directly. Use `--duration`, `--rate`, `--sink`,
    /// `--endpoint`, and `--encoder` to override values in the scenario YAML.
    Run(ScenariosRunArgs),
}

/// Arguments for `sonda scenarios list`.
#[derive(Debug, Args)]
pub struct ScenariosListArgs {
    /// Filter scenarios by category (e.g. `infrastructure`, `network`,
    /// `application`, `observability`).
    #[arg(long)]
    pub category: Option<String>,

    /// Output the scenario list as a JSON array instead of a table.
    ///
    /// Each element contains `name`, `category`, `signal_type`,
    /// `description`, and `source` fields.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `sonda scenarios show`.
#[derive(Debug, Args)]
pub struct ScenariosShowArgs {
    /// The kebab-case name of the scenario (e.g. `cpu-spike`).
    pub name: String,
}

/// Arguments for `sonda scenarios run`.
#[derive(Debug, Args)]
pub struct ScenariosRunArgs {
    /// The kebab-case name of the scenario (e.g. `cpu-spike`).
    pub name: String,

    /// Override the scenario duration (e.g. `"10s"`, `"2m"`).
    #[arg(long)]
    pub duration: Option<String>,

    /// Override the event rate in events per second.
    #[arg(long)]
    pub rate: Option<f64>,

    /// Override the sink type (e.g. `stdout`, `file`).
    #[arg(long, help_heading = "Sink")]
    pub sink: Option<String>,

    /// Override the sink endpoint (required for network sinks).
    #[arg(long, help_heading = "Sink")]
    pub endpoint: Option<String>,

    /// Override the encoder format (e.g. `prometheus_text`, `json_lines`).
    #[arg(long, help_heading = "Encoder")]
    pub encoder: Option<String>,
}

/// Arguments for the `packs` subcommand.
///
/// Provides access to metric packs discovered from the filesystem search path.
#[derive(Debug, Args)]
pub struct PacksArgs {
    /// The packs action to perform.
    #[command(subcommand)]
    pub action: PacksAction,
}

/// Actions available under `sonda packs`.
#[derive(Debug, Subcommand)]
pub enum PacksAction {
    /// List all available metric packs found on the search path.
    ///
    /// Prints a formatted table with NAME, CATEGORY, METRICS, DESCRIPTION,
    /// and SOURCE columns. Use `--category` to filter by category.
    List(PacksListArgs),
    /// Show the raw YAML definition for a metric pack.
    ///
    /// Prints the full YAML content to stdout, suitable for piping to a file
    /// for customization.
    Show(PacksShowArgs),
    /// Run a metric pack with the given schedule and delivery options.
    ///
    /// Expands the pack into one metric scenario per metric in the pack, then
    /// runs them all concurrently.
    Run(PacksRunArgs),
}

/// Arguments for `sonda packs list`.
#[derive(Debug, Args)]
pub struct PacksListArgs {
    /// Filter packs by category (e.g. `infrastructure`, `network`).
    #[arg(long)]
    pub category: Option<String>,

    /// Output the pack list as a JSON array instead of a table.
    ///
    /// Each element contains `name`, `category`, `metric_count`,
    /// `description`, and `source` fields.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `sonda packs show`.
#[derive(Debug, Args)]
pub struct PacksShowArgs {
    /// The snake_case name of the pack (e.g. `telegraf_snmp_interface`).
    pub name: String,
}

/// Arguments for `sonda packs run`.
#[derive(Debug, Args)]
pub struct PacksRunArgs {
    /// The snake_case name of the pack (e.g. `telegraf_snmp_interface`).
    pub name: String,

    /// Override the scenario duration (e.g. `"10s"`, `"2m"`).
    #[arg(long)]
    pub duration: Option<String>,

    /// Override the event rate in events per second.
    #[arg(long)]
    pub rate: Option<f64>,

    /// Override the sink type (e.g. `stdout`, `file`).
    #[arg(long, help_heading = "Sink")]
    pub sink: Option<String>,

    /// Override the sink endpoint (required for network sinks).
    #[arg(long, help_heading = "Sink")]
    pub endpoint: Option<String>,

    /// Override the encoder format (e.g. `prometheus_text`, `json_lines`).
    #[arg(long, help_heading = "Encoder")]
    pub encoder: Option<String>,

    /// Add or override a label (format: `key=value`). Can be specified
    /// multiple times to set multiple labels.
    #[arg(long = "label", value_parser = parse_label)]
    pub labels: Vec<(String, String)>,
}

/// Arguments for the `import` subcommand.
///
/// Analyzes a CSV file, detects time-series patterns, and generates a
/// portable scenario YAML. Exactly one of `--analyze`, `-o`, or `--run`
/// must be specified (enforced at runtime).
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to the CSV file to import.
    ///
    /// Supports Grafana "Series joined by time" CSV exports and plain CSV
    /// files with a header row. Column 0 is treated as the timestamp.
    pub file: PathBuf,

    /// Print a read-only analysis of detected patterns (no file output).
    ///
    /// For each numeric column, shows the metric name, detected pattern,
    /// and key parameters. Does not generate any YAML.
    #[arg(long, conflicts_with_all = &["output", "run"])]
    pub analyze: bool,

    /// Write the generated scenario YAML to this path.
    ///
    /// Produces a valid, runnable scenario YAML using generators instead of
    /// csv_replay. Use `sonda run --scenario <output>` to execute it.
    #[arg(short, long, conflicts_with_all = &["analyze", "run"])]
    pub output: Option<PathBuf>,

    /// Generate the scenario and immediately execute it (no file output).
    ///
    /// Equivalent to generating with `-o` and then running with
    /// `sonda run --scenario`, but without writing a file.
    #[arg(long, conflicts_with_all = &["analyze", "output"])]
    pub run: bool,

    /// Select specific columns by index (e.g., `1,3,5`).
    ///
    /// Column indices are zero-based. Column 0 is typically the timestamp
    /// and is excluded by default. Without this flag, all non-timestamp
    /// columns are processed.
    #[arg(long)]
    pub columns: Option<String>,

    /// Target event rate in events per second for the generated scenario.
    ///
    /// Used when generating YAML (`-o` or `--run`). Defaults to 1.0.
    #[arg(long, default_value = "1.0")]
    pub rate: f64,

    /// Scenario duration for the generated scenario (e.g., `"60s"`, `"5m"`).
    ///
    /// Used when generating YAML (`-o` or `--run`). Defaults to `"60s"`.
    #[arg(long, default_value = "60s")]
    pub duration: String,
}

/// Arguments for the `init` subcommand.
///
/// All flags are optional. When a flag is provided its value is used directly,
/// skipping the corresponding interactive prompt. When ALL required fields are
/// supplied via flags (signal type, domain, metric/pack, situation, rate,
/// duration, encoder, sink, and output path), `sonda init` runs fully
/// non-interactively — no terminal interaction needed.
///
/// The `--from` flag pre-fills values from a built-in scenario (`@name`) or a
/// CSV file (`path.csv`). Explicit flags override `--from` values.
///
/// For advanced sinks in non-interactive mode, supply the sink-specific flags
/// (`--kafka-brokers`, `--kafka-topic`, `--otlp-signal-type`) alongside
/// `--sink`.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Start from a built-in scenario (@name) or CSV file (path.csv).
    #[arg(long)]
    pub from: Option<String>,

    /// Signal type: metrics or logs.
    #[arg(long)]
    pub signal_type: Option<String>,

    /// Domain category (infrastructure, network, application, custom).
    #[arg(long)]
    pub domain: Option<String>,

    /// Operational situation/pattern (steady, spike_event, flap, leak, saturation, degradation).
    #[arg(long)]
    pub situation: Option<String>,

    /// Metric name.
    #[arg(long)]
    pub metric: Option<String>,

    /// Use a metric pack instead of single metric.
    #[arg(long)]
    pub pack: Option<String>,

    /// Events per second.
    #[arg(long)]
    pub rate: Option<f64>,

    /// Duration (e.g., 60s, 5m).
    #[arg(long)]
    pub duration: Option<String>,

    /// Encoder format (prometheus_text, influx_lp, json_lines, syslog).
    #[arg(long)]
    pub encoder: Option<String>,

    /// Sink type (stdout, http_push, file, remote_write, loki, otlp_grpc, kafka, tcp, udp).
    #[arg(long)]
    pub sink: Option<String>,

    /// Sink endpoint (URL, file path, or host:port).
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Output file path for the generated YAML.
    #[arg(short, long)]
    pub output: Option<String>,

    /// Static labels (key=value), can be repeated.
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub labels: Vec<String>,

    /// Run the generated scenario immediately after writing (skip the prompt).
    ///
    /// When absent and stdin is a TTY, prompts the user. When absent and stdin
    /// is not a TTY, defaults to `false`.
    #[arg(long)]
    pub run_now: bool,

    /// Log message template (for `--signal-type logs`).
    ///
    /// Uses `{field}` placeholders. Example:
    /// `"Request to {endpoint} completed with status {status}"`.
    #[arg(long, help_heading = "Logs")]
    pub message_template: Option<String>,

    /// Severity distribution preset (for `--signal-type logs`).
    ///
    /// Accepted values: `mostly_info`, `balanced`, `error_heavy`.
    #[arg(long, help_heading = "Logs")]
    pub severity: Option<String>,

    /// Kafka broker(s) for `--sink kafka` (e.g. `localhost:9092`).
    #[arg(long, help_heading = "Sink")]
    pub kafka_brokers: Option<String>,

    /// Kafka topic for `--sink kafka`.
    #[arg(long, help_heading = "Sink")]
    pub kafka_topic: Option<String>,

    /// OTLP signal type for `--sink otlp_grpc`: `metrics` or `logs`.
    #[arg(long, help_heading = "Sink")]
    pub otlp_signal_type: Option<String>,
}

/// Build clap help styling for the CLI.
///
/// Returns a [`clap::builder::styling::Styles`] with colored headers, usage
/// patterns, flag names, and placeholders that match the conventions of modern
/// Rust CLIs like `cargo`.
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

    // ---- scenarios subcommand parsing -------------------------------------------

    #[test]
    fn cli_scenarios_list_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "scenarios", "list"])
            .expect("scenarios list should parse");
        match cli.command {
            Commands::Scenarios(args) => {
                assert!(matches!(args.action, ScenariosAction::List(_)));
            }
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_list_with_category_filter() {
        let cli =
            Cli::try_parse_from(["sonda", "scenarios", "list", "--category", "infrastructure"])
                .expect("scenarios list --category should parse");
        match cli.command {
            Commands::Scenarios(args) => match args.action {
                ScenariosAction::List(list_args) => {
                    assert_eq!(list_args.category.as_deref(), Some("infrastructure"));
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_show_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "scenarios", "show", "cpu-spike"])
            .expect("scenarios show should parse");
        match cli.command {
            Commands::Scenarios(args) => match args.action {
                ScenariosAction::Show(show_args) => {
                    assert_eq!(show_args.name, "cpu-spike");
                }
                _ => panic!("expected Show action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_run_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "scenarios", "run", "cpu-spike"])
            .expect("scenarios run should parse");
        match cli.command {
            Commands::Scenarios(args) => match args.action {
                ScenariosAction::Run(run_args) => {
                    assert_eq!(run_args.name, "cpu-spike");
                    assert!(run_args.duration.is_none());
                    assert!(run_args.rate.is_none());
                    assert!(run_args.sink.is_none());
                    assert!(run_args.endpoint.is_none());
                    assert!(run_args.encoder.is_none());
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_run_with_overrides() {
        let cli = Cli::try_parse_from([
            "sonda",
            "scenarios",
            "run",
            "cpu-spike",
            "--duration",
            "5s",
            "--rate",
            "2",
            "--sink",
            "file",
            "--endpoint",
            "/tmp/out.txt",
            "--encoder",
            "json_lines",
        ])
        .expect("scenarios run with overrides should parse");
        match cli.command {
            Commands::Scenarios(args) => match args.action {
                ScenariosAction::Run(run_args) => {
                    assert_eq!(run_args.duration.as_deref(), Some("5s"));
                    assert_eq!(run_args.rate, Some(2.0));
                    assert_eq!(run_args.sink.as_deref(), Some("file"));
                    assert_eq!(run_args.endpoint.as_deref(), Some("/tmp/out.txt"));
                    assert_eq!(run_args.encoder.as_deref(), Some("json_lines"));
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_show_requires_name() {
        let result = Cli::try_parse_from(["sonda", "scenarios", "show"]);
        assert!(result.is_err(), "show without name must fail");
    }

    #[test]
    fn cli_scenarios_run_requires_name() {
        let result = Cli::try_parse_from(["sonda", "scenarios", "run"]);
        assert!(result.is_err(), "run without name must fail");
    }

    #[test]
    fn cli_scenarios_requires_action() {
        let result = Cli::try_parse_from(["sonda", "scenarios"]);
        assert!(result.is_err(), "scenarios without action must fail");
    }

    // ---- clap_styles: returns valid styles ------------------------------------

    #[test]
    fn clap_styles_returns_valid_styles() {
        // The function must return without panicking and produce a Styles
        // instance that can be used with clap.
        let _styles = clap_styles();
    }

    // ---- --json flag on scenarios list ----------------------------------------

    #[test]
    fn cli_scenarios_list_json_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "scenarios", "list", "--json"])
            .expect("--json flag should parse");
        match cli.command {
            Commands::Scenarios(ref args) => match args.action {
                ScenariosAction::List(ref list_args) => {
                    assert!(list_args.json, "--json must be true");
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_list_json_flag_defaults_to_false() {
        let cli = Cli::try_parse_from(["sonda", "scenarios", "list"])
            .expect("list without --json should parse");
        match cli.command {
            Commands::Scenarios(ref args) => match args.action {
                ScenariosAction::List(ref list_args) => {
                    assert!(!list_args.json, "--json must default to false");
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    #[test]
    fn cli_scenarios_list_json_and_category_combined() {
        let cli = Cli::try_parse_from([
            "sonda",
            "scenarios",
            "list",
            "--json",
            "--category",
            "infrastructure",
        ])
        .expect("--json + --category should parse together");
        match cli.command {
            Commands::Scenarios(ref args) => match args.action {
                ScenariosAction::List(ref list_args) => {
                    assert!(list_args.json);
                    assert_eq!(list_args.category.as_deref(), Some("infrastructure"));
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Scenarios command"),
        }
    }

    // ---- Packs subcommand parsing -----------------------------------------------

    #[test]
    fn cli_packs_list_parses() {
        let cli = Cli::try_parse_from(["sonda", "packs", "list"]).expect("packs list must parse");
        assert!(matches!(cli.command, Commands::Packs(_)));
        match cli.command {
            Commands::Packs(ref args) => {
                assert!(matches!(args.action, PacksAction::List(_)));
            }
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_list_with_category() {
        let cli = Cli::try_parse_from(["sonda", "packs", "list", "--category", "network"])
            .expect("packs list --category must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::List(ref list_args) => {
                    assert_eq!(list_args.category.as_deref(), Some("network"));
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_list_with_json() {
        let cli = Cli::try_parse_from(["sonda", "packs", "list", "--json"])
            .expect("packs list --json must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::List(ref list_args) => {
                    assert!(list_args.json);
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_show_parses() {
        let cli = Cli::try_parse_from(["sonda", "packs", "show", "telegraf_snmp_interface"])
            .expect("packs show must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::Show(ref show_args) => {
                    assert_eq!(show_args.name, "telegraf_snmp_interface");
                }
                _ => panic!("expected Show action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_run_parses() {
        let cli = Cli::try_parse_from([
            "sonda",
            "packs",
            "run",
            "telegraf_snmp_interface",
            "--rate",
            "2",
            "--duration",
            "10s",
        ])
        .expect("packs run must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::Run(ref run_args) => {
                    assert_eq!(run_args.name, "telegraf_snmp_interface");
                    assert_eq!(run_args.rate, Some(2.0));
                    assert_eq!(run_args.duration.as_deref(), Some("10s"));
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_run_with_label() {
        let cli = Cli::try_parse_from([
            "sonda",
            "packs",
            "run",
            "telegraf_snmp_interface",
            "--label",
            "device=rtr-01",
            "--label",
            "ifName=eth0",
        ])
        .expect("packs run --label must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::Run(ref run_args) => {
                    assert_eq!(run_args.labels.len(), 2);
                    assert_eq!(
                        run_args.labels[0],
                        ("device".to_string(), "rtr-01".to_string())
                    );
                    assert_eq!(
                        run_args.labels[1],
                        ("ifName".to_string(), "eth0".to_string())
                    );
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_run_with_sink_and_encoder() {
        let cli = Cli::try_parse_from([
            "sonda",
            "packs",
            "run",
            "node_exporter_cpu",
            "--sink",
            "file",
            "--endpoint",
            "/tmp/out.txt",
            "--encoder",
            "json_lines",
        ])
        .expect("packs run --sink --encoder must parse");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::Run(ref run_args) => {
                    assert_eq!(run_args.sink.as_deref(), Some("file"));
                    assert_eq!(run_args.endpoint.as_deref(), Some("/tmp/out.txt"));
                    assert_eq!(run_args.encoder.as_deref(), Some("json_lines"));
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    #[test]
    fn cli_packs_list_json_and_category_combined() {
        let cli = Cli::try_parse_from([
            "sonda",
            "packs",
            "list",
            "--json",
            "--category",
            "infrastructure",
        ])
        .expect("--json + --category should parse together");
        match cli.command {
            Commands::Packs(ref args) => match args.action {
                PacksAction::List(ref list_args) => {
                    assert!(list_args.json);
                    assert_eq!(list_args.category.as_deref(), Some("infrastructure"));
                }
                _ => panic!("expected List action"),
            },
            _ => panic!("expected Packs command"),
        }
    }

    // ---- Import subcommand parsing -----------------------------------------------

    #[test]
    fn cli_import_analyze_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "import", "foo.csv", "--analyze"])
            .expect("import --analyze should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert_eq!(args.file, PathBuf::from("foo.csv"));
                assert!(args.analyze);
                assert!(args.output.is_none());
                assert!(!args.run);
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_default_rate_and_duration() {
        let cli = Cli::try_parse_from(["sonda", "import", "data.csv", "--analyze"])
            .expect("import with defaults should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert_eq!(args.rate, 1.0, "default rate must be 1.0");
                assert_eq!(args.duration, "60s", "default duration must be 60s");
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_columns_flag_is_parsed() {
        let cli = Cli::try_parse_from([
            "sonda",
            "import",
            "data.csv",
            "--analyze",
            "--columns",
            "1,3,5",
        ])
        .expect("import --columns should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert_eq!(args.columns.as_deref(), Some("1,3,5"));
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_output_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "import", "data.csv", "-o", "out.yaml"])
            .expect("import -o should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert_eq!(args.output, Some(PathBuf::from("out.yaml")));
                assert!(!args.analyze);
                assert!(!args.run);
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_run_flag_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "import", "data.csv", "--run"])
            .expect("import --run should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert!(args.run);
                assert!(!args.analyze);
                assert!(args.output.is_none());
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_rate_and_duration_overrides() {
        let cli = Cli::try_parse_from([
            "sonda",
            "import",
            "data.csv",
            "--run",
            "--rate",
            "5",
            "--duration",
            "2m",
        ])
        .expect("import with rate and duration overrides should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert_eq!(args.rate, 5.0);
                assert_eq!(args.duration, "2m");
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_analyze_conflicts_with_output() {
        let result =
            Cli::try_parse_from(["sonda", "import", "data.csv", "--analyze", "-o", "out.yaml"]);
        assert!(result.is_err(), "--analyze and -o must conflict");
    }

    #[test]
    fn cli_import_analyze_conflicts_with_run() {
        let result = Cli::try_parse_from(["sonda", "import", "data.csv", "--analyze", "--run"]);
        assert!(result.is_err(), "--analyze and --run must conflict");
    }

    #[test]
    fn cli_import_output_conflicts_with_run() {
        let result =
            Cli::try_parse_from(["sonda", "import", "data.csv", "-o", "out.yaml", "--run"]);
        assert!(result.is_err(), "-o and --run must conflict");
    }

    #[test]
    fn cli_import_requires_file_argument() {
        let result = Cli::try_parse_from(["sonda", "import", "--analyze"]);
        assert!(result.is_err(), "import without file must fail");
    }

    #[test]
    fn cli_import_run_with_columns() {
        let cli = Cli::try_parse_from([
            "sonda",
            "import",
            "data.csv",
            "--run",
            "--columns",
            "2,4",
            "--rate",
            "10",
            "--duration",
            "5m",
        ])
        .expect("import --run with all options should parse");
        match cli.command {
            Commands::Import(ref args) => {
                assert!(args.run);
                assert_eq!(args.columns.as_deref(), Some("2,4"));
                assert_eq!(args.rate, 10.0);
                assert_eq!(args.duration, "5m");
            }
            _ => panic!("expected Import command"),
        }
    }

    #[test]
    fn cli_import_verbose_flag_with_run() {
        let cli = Cli::try_parse_from(["sonda", "--verbose", "import", "data.csv", "--run"])
            .expect("import with --verbose should parse");
        assert!(cli.verbose);
        assert!(matches!(cli.command, Commands::Import(_)));
    }

    // ---- Init subcommand parsing -----------------------------------------------

    #[test]
    fn cli_init_is_parsed() {
        let cli = Cli::try_parse_from(["sonda", "init"]).expect("init should parse");
        assert!(matches!(cli.command, Commands::Init(_)));
    }

    #[test]
    fn cli_init_with_quiet_flag() {
        let cli = Cli::try_parse_from(["sonda", "--quiet", "init"])
            .expect("init with --quiet should parse");
        assert!(cli.quiet);
        assert!(matches!(cli.command, Commands::Init(_)));
    }

    #[test]
    fn cli_init_with_pack_path() {
        let cli = Cli::try_parse_from(["sonda", "--pack-path", "/custom/packs", "init"])
            .expect("init with --pack-path should parse");
        assert_eq!(
            cli.pack_path,
            Some(std::path::PathBuf::from("/custom/packs"))
        );
        assert!(matches!(cli.command, Commands::Init(_)));
    }

    #[test]
    fn cli_init_from_builtin_scenario() {
        let cli =
            Cli::try_parse_from(["sonda", "init", "--from", "@cpu-spike"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.from.as_deref(), Some("@cpu-spike"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_from_csv_file() {
        let cli =
            Cli::try_parse_from(["sonda", "init", "--from", "data.csv"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.from.as_deref(), Some("data.csv"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_all_flags() {
        let cli = Cli::try_parse_from([
            "sonda",
            "init",
            "--signal-type",
            "metrics",
            "--domain",
            "network",
            "--situation",
            "flap",
            "--metric",
            "bgp_state",
            "--rate",
            "2.5",
            "--duration",
            "5m",
            "--encoder",
            "prometheus_text",
            "--sink",
            "stdout",
            "--endpoint",
            "http://localhost:9090",
            "-o",
            "output.yaml",
            "--label",
            "env=prod",
            "--label",
            "dc=us-east",
        ])
        .expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.signal_type.as_deref(), Some("metrics"));
            assert_eq!(args.domain.as_deref(), Some("network"));
            assert_eq!(args.situation.as_deref(), Some("flap"));
            assert_eq!(args.metric.as_deref(), Some("bgp_state"));
            assert_eq!(args.rate, Some(2.5));
            assert_eq!(args.duration.as_deref(), Some("5m"));
            assert_eq!(args.encoder.as_deref(), Some("prometheus_text"));
            assert_eq!(args.sink.as_deref(), Some("stdout"));
            assert_eq!(args.endpoint.as_deref(), Some("http://localhost:9090"));
            assert_eq!(args.output.as_deref(), Some("output.yaml"));
            assert_eq!(args.labels, vec!["env=prod", "dc=us-east"]);
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_pack_flag() {
        let cli = Cli::try_parse_from(["sonda", "init", "--pack", "telegraf_snmp"])
            .expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.pack.as_deref(), Some("telegraf_snmp"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_no_flags_defaults_to_none() {
        let cli = Cli::try_parse_from(["sonda", "init"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert!(args.from.is_none());
            assert!(args.signal_type.is_none());
            assert!(args.domain.is_none());
            assert!(args.situation.is_none());
            assert!(args.metric.is_none());
            assert!(args.pack.is_none());
            assert!(args.rate.is_none());
            assert!(args.duration.is_none());
            assert!(args.encoder.is_none());
            assert!(args.sink.is_none());
            assert!(args.endpoint.is_none());
            assert!(args.output.is_none());
            assert!(args.labels.is_empty());
            assert!(!args.run_now);
            assert!(args.message_template.is_none());
            assert!(args.severity.is_none());
            assert!(args.kafka_brokers.is_none());
            assert!(args.kafka_topic.is_none());
            assert!(args.otlp_signal_type.is_none());
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_output_short_flag() {
        let cli =
            Cli::try_parse_from(["sonda", "init", "-o", "my-scenario.yaml"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.output.as_deref(), Some("my-scenario.yaml"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_run_now_flag() {
        let cli = Cli::try_parse_from(["sonda", "init", "--run-now"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert!(args.run_now);
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_message_template_flag() {
        let cli = Cli::try_parse_from([
            "sonda",
            "init",
            "--message-template",
            "Connection from {ip} failed",
        ])
        .expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(
                args.message_template.as_deref(),
                Some("Connection from {ip} failed")
            );
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_severity_flag() {
        let cli =
            Cli::try_parse_from(["sonda", "init", "--severity", "balanced"]).expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.severity.as_deref(), Some("balanced"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_kafka_flags() {
        let cli = Cli::try_parse_from([
            "sonda",
            "init",
            "--sink",
            "kafka",
            "--kafka-brokers",
            "broker1:9092,broker2:9092",
            "--kafka-topic",
            "my-topic",
        ])
        .expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.sink.as_deref(), Some("kafka"));
            assert_eq!(
                args.kafka_brokers.as_deref(),
                Some("broker1:9092,broker2:9092")
            );
            assert_eq!(args.kafka_topic.as_deref(), Some("my-topic"));
        } else {
            panic!("expected Init command");
        }
    }

    #[test]
    fn cli_init_otlp_signal_type_flag() {
        let cli = Cli::try_parse_from([
            "sonda",
            "init",
            "--sink",
            "otlp_grpc",
            "--otlp-signal-type",
            "metrics",
        ])
        .expect("should parse");
        if let Commands::Init(ref args) = cli.command {
            assert_eq!(args.sink.as_deref(), Some("otlp_grpc"));
            assert_eq!(args.otlp_signal_type.as_deref(), Some("metrics"));
        } else {
            panic!("expected Init command");
        }
    }
}
