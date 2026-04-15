//! Config loading: YAML file deserialization, CLI override merging, and
//! `ScenarioConfig` construction from flags alone.
//!
//! The precedence order (lowest → highest) is:
//! 1. YAML scenario file
//! 2. CLI flags (any non-`None` value overrides the file)
//!
//! No business logic lives here beyond translating user-facing arguments into
//! the `sonda_core` config types.

use std::collections::{BTreeMap, HashMap};
use std::fs;

use anyhow::{bail, Context, Result};
use sonda_core::config::{
    BaseScheduleConfig, BurstConfig, CardinalitySpikeConfig, GapConfig, LogScenarioConfig,
    MultiScenarioConfig, ScenarioConfig, SpikeStrategy,
};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
use sonda_core::sink::retry::RetryConfig;
use sonda_core::sink::SinkConfig;

use crate::cli::{LogsArgs, MetricsArgs, PacksRunArgs, RunArgs, ScenariosRunArgs};

/// Validate CLI flag combinations that are invalid regardless of the scenario
/// file contents.
///
/// Checks:
/// - `--value` is only valid with `--value-mode constant` (or the implicit
///   constant default). Using it with `sine`, `uniform`, or `sawtooth` is an
///   error.
/// - `--offset` is only valid with `--value-mode sine`. Using it with
///   `constant`, `uniform`, or `sawtooth` is an error.
/// - Sink companion flags (`--endpoint`, `--brokers`, `--topic`, etc.) require
///   `--sink` to be present.
/// - `--sink http_push` requires `--endpoint`.
/// - `--sink remote_write` requires `--endpoint`.
/// - `--sink loki` requires `--endpoint`.
/// - `--sink otlp_grpc` requires `--endpoint` and `--signal-type`.
/// - `--sink kafka` requires `--brokers` and `--topic`.
///
/// # Errors
///
/// Returns an error when any of the above constraints are violated.
fn validate_cli_flags(args: &MetricsArgs) -> Result<()> {
    if args.value.is_some() {
        let mode = args.value_mode.as_deref().unwrap_or("constant");
        if mode != "constant" {
            bail!(
                "--value is only valid with --value-mode constant, \
                 but --value-mode is {:?}",
                mode
            );
        }
    }
    if args.offset.is_some() {
        let mode = args.value_mode.as_deref().unwrap_or("constant");
        if mode != "sine" {
            bail!("--offset is only valid with --value-mode sine");
        }
    }

    validate_sink_flags(
        &SinkFlags {
            sink: args.sink.as_deref(),
            endpoint: args.endpoint.as_deref(),
            signal_type: args.signal_type.as_deref(),
            brokers: args.brokers.as_deref(),
            topic: args.topic.as_deref(),
            content_type: args.content_type.as_deref(),
            batch_size: args.batch_size,
        },
        true, // metrics subcommand requires explicit --signal-type for otlp_grpc
    )?;

    // Retry flags: all-or-nothing group validation.
    build_retry_config_from_metrics(args)?;

    // Retry flags require --sink with a network sink.
    if args.retry_max_attempts.is_some() && args.sink.is_none() && args.output.is_none() {
        // Only error if the default sink is stdout (non-network).
        // If a YAML scenario has a network sink, the flags will be applied there.
        // We skip this check when --scenario is provided since the sink comes from YAML.
        if args.scenario.is_none() {
            bail!("--retry-* flags require --sink with a network sink");
        }
    }

    Ok(())
}

/// Validate CLI flag combinations for the logs subcommand.
///
/// Mirrors [`validate_cli_flags`] for metrics but allows `--signal-type` to
/// default to `"logs"` when `--sink otlp_grpc` is used.
fn validate_log_cli_flags(args: &LogsArgs) -> Result<()> {
    validate_sink_flags(
        &SinkFlags {
            sink: args.sink.as_deref(),
            endpoint: args.endpoint.as_deref(),
            signal_type: args.signal_type.as_deref(),
            brokers: args.brokers.as_deref(),
            topic: args.topic.as_deref(),
            content_type: args.content_type.as_deref(),
            batch_size: args.batch_size,
        },
        false, // logs subcommand defaults signal_type to "logs"
    )?;

    // Retry flags: all-or-nothing group validation.
    build_retry_config_from_logs(args)?;

    if args.retry_max_attempts.is_some()
        && args.sink.is_none()
        && args.output.is_none()
        && args.scenario.is_none()
    {
        bail!("--retry-* flags require --sink with a network sink");
    }

    Ok(())
}

/// Collected sink-related CLI flag values for validation and construction.
struct SinkFlags<'a> {
    sink: Option<&'a str>,
    endpoint: Option<&'a str>,
    signal_type: Option<&'a str>,
    brokers: Option<&'a str>,
    topic: Option<&'a str>,
    content_type: Option<&'a str>,
    batch_size: Option<usize>,
}

/// Shared validation for sink-related CLI flags.
///
/// When `require_signal_type` is true (metrics subcommand), `--sink otlp_grpc`
/// requires an explicit `--signal-type`. When false (logs subcommand), the
/// signal type defaults to `"logs"`.
fn validate_sink_flags(flags: &SinkFlags<'_>, require_signal_type: bool) -> Result<()> {
    let SinkFlags {
        sink,
        endpoint,
        signal_type,
        brokers,
        topic,
        content_type,
        batch_size,
    } = *flags;
    // Orphaned companion flags without --sink.
    if sink.is_none() {
        let orphans: Vec<&str> = [
            endpoint.map(|_| "--endpoint"),
            signal_type.map(|_| "--signal-type"),
            brokers.map(|_| "--brokers"),
            topic.map(|_| "--topic"),
            content_type.map(|_| "--content-type"),
            batch_size.map(|_| "--batch-size"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !orphans.is_empty() {
            bail!("{} requires --sink to be specified", orphans.join(", "));
        }
        return Ok(());
    }

    let sink_type = sink.expect("checked above");
    match sink_type {
        "http_push" => {
            if endpoint.is_none() {
                bail!("--sink http_push requires --endpoint");
            }
        }
        "remote_write" => {
            if endpoint.is_none() {
                bail!("--sink remote_write requires --endpoint");
            }
        }
        "loki" => {
            if endpoint.is_none() {
                bail!("--sink loki requires --endpoint");
            }
        }
        "otlp_grpc" => {
            if endpoint.is_none() {
                bail!("--sink otlp_grpc requires --endpoint");
            }
            if require_signal_type && signal_type.is_none() {
                bail!("--sink otlp_grpc requires --signal-type (metrics or logs)");
            }
        }
        "kafka" => {
            if brokers.is_none() {
                bail!("--sink kafka requires --brokers");
            }
            if topic.is_none() {
                bail!("--sink kafka requires --topic");
            }
        }
        other => bail!(
            "unknown sink type {:?}: expected one of http_push, remote_write, loki, otlp_grpc, kafka",
            other
        ),
    }

    Ok(())
}

/// Build a [`SinkConfig`] from the `--sink` flag and its companion flags.
///
/// Each sink variant is feature-gated. When a required feature is not compiled
/// in, a clear error message is returned indicating which feature to enable.
///
/// # CLI-only limitations
///
/// For the `http_push` sink, `headers` is always `None` when constructed from
/// CLI flags because there is no `--header` CLI flag. Users who need custom
/// HTTP headers must use a YAML scenario file where the `headers` map can be
/// specified directly.
///
/// # Arguments
///
/// * `sink_type` - The `--sink` flag value (e.g. `"http_push"`).
/// * `endpoint` - The `--endpoint` URL.
/// * `signal_type` - The `--signal-type` for OTLP (e.g. `"metrics"` or `"logs"`).
/// * `batch_size` - Optional `--batch-size`.
/// * `content_type` - Optional `--content-type` for `http_push`.
/// * `brokers` - Kafka `--brokers`.
/// * `topic` - Kafka `--topic`.
fn build_sink_config(
    sink_type: &str,
    endpoint: Option<&str>,
    signal_type: Option<&str>,
    batch_size: Option<usize>,
    content_type: Option<&str>,
    brokers: Option<&str>,
    topic: Option<&str>,
) -> Result<SinkConfig> {
    match sink_type {
        "http_push" => {
            #[cfg(feature = "http")]
            {
                Ok(SinkConfig::HttpPush {
                    url: endpoint
                        .expect("validated: --endpoint required for http_push")
                        .to_string(),
                    content_type: content_type.map(|s| s.to_string()),
                    batch_size,
                    // No --header CLI flag exists; users needing custom headers
                    // must use a YAML scenario file.
                    headers: None,
                    // Retry is not set here; it will be applied via override.
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                // Suppress unused-variable warnings when feature is disabled.
                let _ = (endpoint, content_type, batch_size);
                bail!("--sink http_push requires the http feature: cargo build -F http")
            }
        }
        "remote_write" => {
            #[cfg(feature = "remote-write")]
            {
                Ok(SinkConfig::RemoteWrite {
                    url: endpoint
                        .expect("validated: --endpoint required for remote_write")
                        .to_string(),
                    batch_size,
                    retry: None,
                })
            }
            #[cfg(not(feature = "remote-write"))]
            {
                let _ = (endpoint, batch_size);
                bail!(
                    "--sink remote_write requires the remote-write feature: \
                     cargo build -F remote-write"
                )
            }
        }
        "loki" => {
            #[cfg(feature = "http")]
            {
                Ok(SinkConfig::Loki {
                    url: endpoint
                        .expect("validated: --endpoint required for loki")
                        .to_string(),
                    batch_size,
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = (endpoint, batch_size);
                bail!("--sink loki requires the http feature: cargo build -F http")
            }
        }
        "otlp_grpc" => {
            #[cfg(feature = "otlp")]
            {
                let sig = signal_type.unwrap_or("logs");
                let parsed_signal = match sig {
                    "metrics" => sonda_core::sink::otlp_grpc::OtlpSignalType::Metrics,
                    "logs" => sonda_core::sink::otlp_grpc::OtlpSignalType::Logs,
                    other => bail!(
                        "unknown signal type {:?}: expected one of metrics, logs",
                        other
                    ),
                };
                Ok(SinkConfig::OtlpGrpc {
                    endpoint: endpoint
                        .expect("validated: --endpoint required for otlp_grpc")
                        .to_string(),
                    signal_type: parsed_signal,
                    batch_size,
                    retry: None,
                })
            }
            #[cfg(not(feature = "otlp"))]
            {
                let _ = (endpoint, signal_type, batch_size);
                bail!("--sink otlp_grpc requires the otlp feature: cargo build -F otlp")
            }
        }
        "kafka" => {
            #[cfg(feature = "kafka")]
            {
                Ok(SinkConfig::Kafka {
                    brokers: brokers
                        .expect("validated: --brokers required for kafka")
                        .to_string(),
                    topic: topic
                        .expect("validated: --topic required for kafka")
                        .to_string(),
                    retry: None,
                    tls: None,
                    sasl: None,
                })
            }
            #[cfg(not(feature = "kafka"))]
            {
                let _ = (brokers, topic);
                bail!("--sink kafka requires the kafka feature: cargo build -F kafka")
            }
        }
        other => bail!(
            "unknown sink type {:?}: expected one of http_push, remote_write, loki, otlp_grpc, kafka",
            other
        ),
    }
}

/// Load and return a [`ScenarioConfig`] from the provided [`MetricsArgs`].
///
/// If `--scenario` is given the file is loaded through
/// [`crate::scenario_loader::load_scenario_entries`] — both v1 flat /
/// multi / pack shapes and v2 (`version: 2`) files are accepted, and
/// pack references resolve against the supplied
/// [`PackCatalog`](crate::packs::PackCatalog). The compiled entries
/// must consist of exactly one `metrics` entry; anything else (a
/// multi-entry compilation or a non-metrics signal) is rejected with a
/// pointer to the right subcommand. Any CLI flag that is `Some(...)`
/// then overrides the corresponding field in the resulting config.
///
/// If no `--scenario` file is given the config is built entirely from
/// CLI flags; `--name` and `--rate` are required in this case.
///
/// # Errors
///
/// Returns an error if:
/// - The scenario file cannot be read or is not valid YAML / v2 source.
/// - The file compiles to zero or more than one entry, or produces a
///   non-metrics entry.
/// - `--name` or `--rate` are absent and no scenario file was provided.
/// - An unrecognized `--encoder` value is given.
/// - Both `--gap-every` and `--gap-for` are not provided together.
/// - `--value` is provided with a non-constant mode.
/// - `--offset` is provided with a non-sine mode.
pub fn load_config(
    args: &MetricsArgs,
    scenario_catalog: &crate::scenarios::ScenarioCatalog,
    pack_catalog: &crate::packs::PackCatalog,
) -> Result<ScenarioConfig> {
    validate_cli_flags(args)?;

    let mut config = if let Some(ref path) = args.scenario {
        load_single_entry_from_scenario_file(
            path,
            scenario_catalog,
            pack_catalog,
            SignalKind::Metrics,
            "metrics",
        )?
        .into_metrics()
    } else {
        // No scenario file — build a baseline config from required flags.
        let name = args.name.clone().ok_or_else(|| {
            anyhow::anyhow!("--name is required when no --scenario file is provided\n\n  hint: try `sonda metrics --scenario @cpu-spike` to use a built-in scenario")
        })?;
        let rate = args.rate.ok_or_else(|| {
            anyhow::anyhow!("--rate is required when no --scenario file is provided\n\n  hint: try `sonda metrics --scenario @cpu-spike` to use a built-in scenario")
        })?;

        ScenarioConfig {
            base: BaseScheduleConfig {
                name,
                rate,
                duration: args.duration.clone(),
                gaps: build_gap_config(args)?,
                bursts: build_burst_config(args)?,
                cardinality_spikes: build_spike_config(args)?,
                dynamic_labels: None,
                labels: build_labels(args),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: args.jitter,
                jitter_seed: args.jitter_seed,
            },
            generator: build_generator_config(args)?,
            encoder: parse_encoder_config(
                args.encoder.as_deref().unwrap_or("prometheus_text"),
                args.precision,
            )?,
        }
    };

    // Apply CLI overrides onto the loaded file config (each Some(...) wins).
    apply_overrides(&mut config, args)?;

    // --output overrides the sink to a file sink regardless of YAML.
    if let Some(ref path) = args.output {
        config.sink = SinkConfig::File {
            path: path.display().to_string(),
        };
    }

    // --sink overrides the sink using the build_sink_config factory.
    // (clap enforces --sink and --output are mutually exclusive.)
    if let Some(ref sink_type) = args.sink {
        config.sink = build_sink_config(
            sink_type,
            args.endpoint.as_deref(),
            args.signal_type.as_deref(),
            args.batch_size,
            args.content_type.as_deref(),
            args.brokers.as_deref(),
            args.topic.as_deref(),
        )?;
    }

    // --retry-* overrides the retry config on the current sink.
    if let Some(retry_cfg) = build_retry_config_from_metrics(args)? {
        apply_retry_to_sink(&mut config.sink, retry_cfg)?;
    }

    Ok(config)
}

/// Signal kind expected by a single-signal subcommand
/// (`metrics` / `logs` / `histogram` / `summary`).
///
/// Used by [`load_single_entry_from_scenario_file`] to pick the matching
/// [`ScenarioEntry`] variant and to render precise diagnostics when the
/// scenario file's signal type does not match.
#[derive(Debug, Clone, Copy)]
enum SignalKind {
    /// Expect [`ScenarioEntry::Metrics`].
    Metrics,
    /// Expect [`ScenarioEntry::Logs`].
    Logs,
    /// Expect [`ScenarioEntry::Histogram`].
    Histogram,
    /// Expect [`ScenarioEntry::Summary`].
    Summary,
}

impl SignalKind {
    /// Human-readable label used in error messages.
    fn label(self) -> &'static str {
        match self {
            SignalKind::Metrics => "metrics",
            SignalKind::Logs => "logs",
            SignalKind::Histogram => "histogram",
            SignalKind::Summary => "summary",
        }
    }
}

/// The unwrapped single-signal configuration extracted from a scenario
/// file, one variant per supported subcommand.
///
/// Downstream callers use the `into_*` accessors to reach the concrete
/// config type they need; constructing the wrong variant is impossible
/// because [`load_single_entry_from_scenario_file`] enforces the
/// match up-front.
enum LoadedSingleEntry {
    Metrics(ScenarioConfig),
    Logs(LogScenarioConfig),
    Histogram(sonda_core::config::HistogramScenarioConfig),
    Summary(sonda_core::config::SummaryScenarioConfig),
}

impl LoadedSingleEntry {
    /// Extract the metrics variant. Panics if a different variant was
    /// produced — only callable after [`load_single_entry_from_scenario_file`]
    /// was given [`SignalKind::Metrics`].
    fn into_metrics(self) -> ScenarioConfig {
        match self {
            LoadedSingleEntry::Metrics(cfg) => cfg,
            _ => unreachable!("signal kind mismatch should have been rejected earlier"),
        }
    }

    /// Extract the logs variant. See [`Self::into_metrics`].
    fn into_logs(self) -> LogScenarioConfig {
        match self {
            LoadedSingleEntry::Logs(cfg) => cfg,
            _ => unreachable!("signal kind mismatch should have been rejected earlier"),
        }
    }

    /// Extract the histogram variant. See [`Self::into_metrics`].
    fn into_histogram(self) -> sonda_core::config::HistogramScenarioConfig {
        match self {
            LoadedSingleEntry::Histogram(cfg) => cfg,
            _ => unreachable!("signal kind mismatch should have been rejected earlier"),
        }
    }

    /// Extract the summary variant. See [`Self::into_metrics`].
    fn into_summary(self) -> sonda_core::config::SummaryScenarioConfig {
        match self {
            LoadedSingleEntry::Summary(cfg) => cfg,
            _ => unreachable!("signal kind mismatch should have been rejected earlier"),
        }
    }
}

/// Human-readable label for the signal type carried by a
/// [`sonda_core::ScenarioEntry`]. Used in mismatch diagnostics.
fn scenario_entry_signal_label(entry: &sonda_core::ScenarioEntry) -> &'static str {
    match entry {
        sonda_core::ScenarioEntry::Metrics(_) => "metrics",
        sonda_core::ScenarioEntry::Logs(_) => "logs",
        sonda_core::ScenarioEntry::Histogram(_) => "histogram",
        sonda_core::ScenarioEntry::Summary(_) => "summary",
    }
}

/// Load exactly one scenario entry from a file for a single-signal
/// subcommand (`metrics`, `logs`, `histogram`, `summary`).
///
/// Dispatches on [`detect_version`][sonda_core::compiler::parse::detect_version]:
///
/// - **v2 files** are compiled via
///   [`sonda_core::compile_scenario_file`] with a
///   [`FilesystemPackResolver`](crate::scenario_loader::FilesystemPackResolver)
///   backed by the CLI's [`PackCatalog`](crate::packs::PackCatalog), so
///   `pack: node_exporter_cpu` resolves identically from every entry
///   point (notably `sonda run --scenario`). Compilations producing more
///   than one entry (e.g. a pack-backed v2 file) are rejected with a
///   pointer to `sonda run --scenario`.
/// - **v1 files** are deserialized directly into the expected single-signal
///   config type. This preserves legacy behavior where a flat v1 log
///   scenario file (without an explicit top-level `signal_type:`) parses
///   as the subcommand's signal type without relying on a fallible
///   `signal_type:` probe.
///
/// When the single v2 entry produced is not of the expected [`SignalKind`]
/// — v1 parsing is already type-locked by the deserialization target —
/// the caller gets a signal-type mismatch error naming the correct
/// subcommand.
///
/// The `subcommand` argument is only used for error messages ("expected
/// a metrics entry" etc.).
///
/// # Errors
///
/// - YAML parse / compilation errors are wrapped with the scenario path
///   via [`anyhow::Context`].
/// - v2 compilations producing zero or more than one entry fail with a
///   multi-entry diagnostic.
/// - A v2 single entry with the wrong [`SignalKind`] fails with a
///   signal-type mismatch diagnostic.
fn load_single_entry_from_scenario_file(
    path: &std::path::Path,
    scenario_catalog: &crate::scenarios::ScenarioCatalog,
    pack_catalog: &crate::packs::PackCatalog,
    kind: SignalKind,
    subcommand: &str,
) -> Result<LoadedSingleEntry> {
    let yaml = resolve_scenario_source(path, scenario_catalog)?;
    let version = sonda_core::compiler::parse::detect_version(&yaml);

    if version == Some(2) {
        load_single_entry_from_v2(&yaml, path, pack_catalog, kind, subcommand)
    } else {
        load_single_entry_from_v1(&yaml, path, kind)
    }
}

/// v1 path: deserialize directly into the expected single-signal config
/// type. Preserves the pre-v2-dispatch semantics of `load_log_config` /
/// `load_histogram_config` / `load_summary_config`, which treated every
/// YAML field as belonging to their declared signal type.
fn load_single_entry_from_v1(
    yaml: &str,
    path: &std::path::Path,
    kind: SignalKind,
) -> Result<LoadedSingleEntry> {
    match kind {
        SignalKind::Metrics => {
            let cfg: ScenarioConfig = serde_yaml_ng::from_str(yaml)
                .with_context(|| format!("failed to parse scenario file {}", path.display()))?;
            Ok(LoadedSingleEntry::Metrics(cfg))
        }
        SignalKind::Logs => {
            let cfg: LogScenarioConfig = serde_yaml_ng::from_str(yaml)
                .with_context(|| format!("failed to parse scenario file {}", path.display()))?;
            Ok(LoadedSingleEntry::Logs(cfg))
        }
        SignalKind::Histogram => {
            let cfg: sonda_core::config::HistogramScenarioConfig = serde_yaml_ng::from_str(yaml)
                .with_context(|| {
                    format!("failed to parse histogram scenario file {}", path.display())
                })?;
            Ok(LoadedSingleEntry::Histogram(cfg))
        }
        SignalKind::Summary => {
            let cfg: sonda_core::config::SummaryScenarioConfig = serde_yaml_ng::from_str(yaml)
                .with_context(|| {
                    format!("failed to parse summary scenario file {}", path.display())
                })?;
            Ok(LoadedSingleEntry::Summary(cfg))
        }
    }
}

/// v2 path: compile via the scenario-loader pipeline (with the CLI pack
/// catalog backing pack references), then enforce single-entry +
/// expected-kind invariants.
fn load_single_entry_from_v2(
    yaml: &str,
    path: &std::path::Path,
    pack_catalog: &crate::packs::PackCatalog,
    kind: SignalKind,
    subcommand: &str,
) -> Result<LoadedSingleEntry> {
    use crate::scenario_loader::FilesystemPackResolver;

    let resolver = FilesystemPackResolver::new(pack_catalog);
    let mut entries = sonda_core::compile_scenario_file(yaml, &resolver)
        .with_context(|| format!("failed to compile v2 scenario file {}", path.display()))?;

    if entries.len() != 1 {
        bail!(
            "v2 scenario file {} compiled to {} entries; \
             `sonda {} --scenario` expects a single {} entry. \
             Use `sonda run --scenario` for multi-entry v2 scenarios.",
            path.display(),
            entries.len(),
            subcommand,
            kind.label(),
        );
    }

    let entry = entries.remove(0);
    match (kind, entry) {
        (SignalKind::Metrics, sonda_core::ScenarioEntry::Metrics(cfg)) => {
            Ok(LoadedSingleEntry::Metrics(cfg))
        }
        (SignalKind::Logs, sonda_core::ScenarioEntry::Logs(cfg)) => {
            Ok(LoadedSingleEntry::Logs(cfg))
        }
        (SignalKind::Histogram, sonda_core::ScenarioEntry::Histogram(cfg)) => {
            Ok(LoadedSingleEntry::Histogram(cfg))
        }
        (SignalKind::Summary, sonda_core::ScenarioEntry::Summary(cfg)) => {
            Ok(LoadedSingleEntry::Summary(cfg))
        }
        (_, entry) => {
            let actual = scenario_entry_signal_label(&entry);
            bail!(
                "v2 scenario file {} contains a {} entry; \
                 `sonda {} --scenario` expects a {} entry. \
                 Use `sonda {} --scenario` instead.",
                path.display(),
                actual,
                subcommand,
                kind.label(),
                actual,
            )
        }
    }
}

/// Apply CLI flag overrides onto a config that was loaded from a YAML file.
///
/// Any flag that is `Some(...)` replaces the corresponding config field.
/// Fields that are `None` in the CLI args are left unchanged from the file.
fn apply_overrides(config: &mut ScenarioConfig, args: &MetricsArgs) -> Result<()> {
    if let Some(ref name) = args.name {
        config.name = name.clone();
    }
    if let Some(rate) = args.rate {
        config.rate = rate;
    }
    if args.duration.is_some() {
        config.duration = args.duration.clone();
    }

    // Generator: rebuild from CLI flags if any generator-related flag is set.
    // We check whether any generator flag was provided so we don't accidentally
    // replace a fully-specified file generator with a half-specified CLI one.
    if args.value_mode.is_some()
        || args.value.is_some()
        || args.amplitude.is_some()
        || args.period_secs.is_some()
        || args.offset.is_some()
        || args.min.is_some()
        || args.max.is_some()
        || args.seed.is_some()
    {
        config.generator = build_generator_config(args)?;
    }

    // Gap: override if either gap flag is present.
    if args.gap_every.is_some() || args.gap_for.is_some() {
        config.gaps = build_gap_config(args)?;
    }

    // Burst: override if any burst flag is present.
    if args.burst_every.is_some() || args.burst_for.is_some() || args.burst_multiplier.is_some() {
        config.bursts = build_burst_config(args)?;
    }

    // Spike: override if any spike flag is present.
    if args.spike_label.is_some()
        || args.spike_every.is_some()
        || args.spike_for.is_some()
        || args.spike_cardinality.is_some()
    {
        config.cardinality_spikes = build_spike_config(args)?;
    }

    // Jitter: override if either jitter flag is present.
    if let Some(jitter) = args.jitter {
        config.base.jitter = Some(jitter);
    }
    if let Some(jitter_seed) = args.jitter_seed {
        config.base.jitter_seed = Some(jitter_seed);
    }

    // Labels: CLI labels are merged on top of (not replacing) the file labels.
    // This lets users add labels without listing all file labels again.
    if !args.labels.is_empty() {
        let mut label_map: HashMap<String, String> = config.labels.take().unwrap_or_default();
        for (k, v) in &args.labels {
            label_map.insert(k.clone(), v.clone());
        }
        config.labels = Some(label_map);
    }

    // Encoder: only override when the user explicitly passes --encoder.
    // Because `encoder` is `Option<String>` (no clap default_value), a `None`
    // here means the flag was omitted and the YAML value should be kept as-is.
    if let Some(ref enc) = args.encoder {
        config.encoder = parse_encoder_config(enc, args.precision)?;
    } else if let Some(p) = args.precision {
        // Precision without --encoder: update the existing encoder's precision.
        // This lets users set precision on top of a YAML-specified encoder
        // without having to re-specify the encoder type.
        match &mut config.encoder {
            EncoderConfig::PrometheusText {
                ref mut precision, ..
            } => *precision = Some(p),
            EncoderConfig::InfluxLineProtocol {
                ref mut precision, ..
            } => *precision = Some(p),
            EncoderConfig::JsonLines {
                ref mut precision, ..
            } => *precision = Some(p),
            _ => {} // syslog, remote_write — no precision field
        }
    }

    Ok(())
}

/// Build a [`GeneratorConfig`] from the generator-related CLI flags.
///
/// Defaults when flags are absent:
/// - mode: `constant`
/// - constant value: `0.0` (via `--value`)
/// - sine offset: `0.0`
/// - amplitude: `1.0`
/// - period_secs: `60.0`
/// - min: `0.0`, max: `1.0`
/// - seed: `None`
fn build_generator_config(args: &MetricsArgs) -> Result<GeneratorConfig> {
    let mode = args.value_mode.as_deref().unwrap_or("constant");
    match mode {
        "constant" => Ok(GeneratorConfig::Constant {
            value: args.value.unwrap_or(0.0),
        }),
        "uniform" => Ok(GeneratorConfig::Uniform {
            min: args.min.unwrap_or(0.0),
            max: args.max.unwrap_or(1.0),
            seed: args.seed,
        }),
        "sine" => Ok(GeneratorConfig::Sine {
            amplitude: args.amplitude.unwrap_or(1.0),
            period_secs: args.period_secs.unwrap_or(60.0),
            offset: args.offset.unwrap_or(0.0),
        }),
        "sawtooth" => Ok(GeneratorConfig::Sawtooth {
            min: args.min.unwrap_or(0.0),
            max: args.max.unwrap_or(1.0),
            period_secs: args.period_secs.unwrap_or(60.0),
        }),
        other => bail!(
            "unknown value mode {:?}: expected one of constant, uniform, sine, sawtooth",
            other
        ),
    }
}

/// Build an optional [`GapConfig`] from `--gap-every` and `--gap-for`.
///
/// Both flags must be provided together, or neither. Providing only one is an
/// error.
fn build_gap_config(args: &MetricsArgs) -> Result<Option<GapConfig>> {
    match (&args.gap_every, &args.gap_for) {
        (Some(every), Some(gap_for)) => Ok(Some(GapConfig {
            every: every.clone(),
            r#for: gap_for.clone(),
        })),
        (None, None) => Ok(None),
        (Some(_), None) => bail!("--gap-for is required when --gap-every is provided"),
        (None, Some(_)) => bail!("--gap-every is required when --gap-for is provided"),
    }
}

/// Build an optional [`BurstConfig`] from `--burst-every`, `--burst-for`, and `--burst-multiplier`.
///
/// All three flags must be provided together, or none. Providing a partial set is an error.
fn build_burst_config(args: &MetricsArgs) -> Result<Option<BurstConfig>> {
    match (&args.burst_every, &args.burst_for, args.burst_multiplier) {
        (Some(every), Some(burst_for), Some(multiplier)) => Ok(Some(BurstConfig {
            every: every.clone(),
            r#for: burst_for.clone(),
            multiplier,
        })),
        (None, None, None) => Ok(None),
        _ => bail!(
            "--burst-every, --burst-for, and --burst-multiplier must all be provided together"
        ),
    }
}

/// Build an optional [`RetryConfig`] from `--retry-*` flags.
///
/// All three flags (`--retry-max-attempts`, `--retry-backoff`, `--retry-max-backoff`)
/// must be provided together, or none. Providing a partial set is an error.
fn build_retry_config_from_metrics(args: &MetricsArgs) -> Result<Option<RetryConfig>> {
    match (
        args.retry_max_attempts,
        &args.retry_backoff,
        &args.retry_max_backoff,
    ) {
        (Some(max_attempts), Some(backoff), Some(max_backoff)) => Ok(Some(RetryConfig {
            max_attempts,
            initial_backoff: backoff.clone(),
            max_backoff: max_backoff.clone(),
        })),
        (None, None, None) => Ok(None),
        _ => bail!(
            "--retry-max-attempts, --retry-backoff, and --retry-max-backoff must all be provided together"
        ),
    }
}

/// Build an optional [`RetryConfig`] from `--retry-*` flags for the logs subcommand.
fn build_retry_config_from_logs(args: &LogsArgs) -> Result<Option<RetryConfig>> {
    match (
        args.retry_max_attempts,
        &args.retry_backoff,
        &args.retry_max_backoff,
    ) {
        (Some(max_attempts), Some(backoff), Some(max_backoff)) => Ok(Some(RetryConfig {
            max_attempts,
            initial_backoff: backoff.clone(),
            max_backoff: max_backoff.clone(),
        })),
        (None, None, None) => Ok(None),
        _ => bail!(
            "--retry-max-attempts, --retry-backoff, and --retry-max-backoff must all be provided together"
        ),
    }
}

/// Apply a retry config to a [`SinkConfig`], returning an error if the sink
/// type does not support retry (e.g. `stdout`, `file`, `udp`).
fn apply_retry_to_sink(sink: &mut SinkConfig, retry: RetryConfig) -> Result<()> {
    match sink {
        SinkConfig::Stdout | SinkConfig::File { .. } | SinkConfig::Udp { .. } => {
            bail!(
                "--retry-* flags are not supported for sink type {:?}; \
                 retry is only available for network sinks (http_push, remote_write, loki, otlp_grpc, kafka, tcp)",
                sink_type_name(sink)
            );
        }
        SinkConfig::Tcp {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(feature = "http")]
        SinkConfig::HttpPush {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(feature = "http")]
        SinkConfig::Loki {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc {
            retry: ref mut r, ..
        } => {
            *r = Some(retry);
        }
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled { .. } | SinkConfig::LokiDisabled { .. } => {
            bail!(
                "--retry-* flags cannot be applied: this sink type requires a feature \
                 that was not compiled in"
            );
        }
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled { .. } => {
            bail!(
                "--retry-* flags cannot be applied: this sink type requires a feature \
                 that was not compiled in"
            );
        }
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled { .. } => {
            bail!(
                "--retry-* flags cannot be applied: this sink type requires a feature \
                 that was not compiled in"
            );
        }
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled { .. } => {
            bail!(
                "--retry-* flags cannot be applied: this sink type requires a feature \
                 that was not compiled in"
            );
        }
    }
    Ok(())
}

/// Return a human-readable name for a [`SinkConfig`] variant.
fn sink_type_name(sink: &SinkConfig) -> &'static str {
    match sink {
        SinkConfig::Stdout => "stdout",
        SinkConfig::File { .. } => "file",
        SinkConfig::Tcp { .. } => "tcp",
        SinkConfig::Udp { .. } => "udp",
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { .. } => "http_push",
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { .. } => "remote_write",
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { .. } => "kafka",
        #[cfg(feature = "http")]
        SinkConfig::Loki { .. } => "loki",
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc { .. } => "otlp_grpc",
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled { .. } => "http_push",
        #[cfg(not(feature = "http"))]
        SinkConfig::LokiDisabled { .. } => "loki",
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled { .. } => "remote_write",
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled { .. } => "kafka",
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled { .. } => "otlp_grpc",
    }
}

/// Build an optional [`Vec<CardinalitySpikeConfig>`] from `--spike-*` flags.
///
/// All four required flags (`--spike-label`, `--spike-every`, `--spike-for`,
/// `--spike-cardinality`) must be provided together, or none. Providing a
/// partial set is an error.
fn build_spike_config(args: &MetricsArgs) -> Result<Option<Vec<CardinalitySpikeConfig>>> {
    match (
        &args.spike_label,
        &args.spike_every,
        &args.spike_for,
        args.spike_cardinality,
    ) {
        (Some(label), Some(every), Some(spike_for), Some(cardinality)) => {
            let strategy = match args.spike_strategy.as_deref() {
                Some("counter") | None => SpikeStrategy::Counter,
                Some("random") => SpikeStrategy::Random,
                Some(other) => bail!(
                    "unknown spike strategy {:?}: expected one of counter, random",
                    other
                ),
            };
            Ok(Some(vec![CardinalitySpikeConfig {
                label: label.clone(),
                every: every.clone(),
                r#for: spike_for.clone(),
                cardinality,
                strategy,
                prefix: args.spike_prefix.clone(),
                seed: args.spike_seed,
            }]))
        }
        (None, None, None, None) => Ok(None),
        _ => bail!(
            "--spike-label, --spike-every, --spike-for, and --spike-cardinality must all be provided together"
        ),
    }
}

/// Build a label `HashMap` from the `--label k=v` CLI args.
///
/// Returns `None` when no labels were provided.
fn build_labels(args: &MetricsArgs) -> Option<HashMap<String, String>> {
    if args.labels.is_empty() {
        None
    } else {
        Some(
            args.labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }
}

/// Parse the `--encoder` flag value into an [`EncoderConfig`].
///
/// When `precision` is `Some`, the value is propagated into text-based encoder
/// variants so the encoder can limit decimal places in formatted metric values.
///
/// `remote_write` and `otlp` encoders are available when the corresponding
/// Cargo features are compiled in. When the feature is absent, a clear error
/// message is returned indicating which feature to enable.
fn parse_encoder_config(encoder: &str, precision: Option<u8>) -> Result<EncoderConfig> {
    match encoder {
        "prometheus_text" => Ok(EncoderConfig::PrometheusText { precision }),
        "influx_lp" => Ok(EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision,
        }),
        "json_lines" => Ok(EncoderConfig::JsonLines { precision }),
        "remote_write" => {
            #[cfg(feature = "remote-write")]
            {
                let _ = precision; // remote_write has no precision field
                Ok(EncoderConfig::RemoteWrite)
            }
            #[cfg(not(feature = "remote-write"))]
            {
                let _ = precision;
                bail!(
                    "--encoder remote_write requires the remote-write feature: \
                     cargo build -F remote-write"
                )
            }
        }
        "otlp" => {
            #[cfg(feature = "otlp")]
            {
                let _ = precision; // otlp has no precision field
                Ok(EncoderConfig::Otlp)
            }
            #[cfg(not(feature = "otlp"))]
            {
                let _ = precision;
                bail!("--encoder otlp requires the otlp feature: cargo build -F otlp")
            }
        }
        other => bail!(
            "unknown encoder \"{}\": expected one of prometheus_text, influx_lp, json_lines, \
             remote_write, otlp (syslog is available via YAML scenario files)",
            other
        ),
    }
}

/// Parse the `--encoder` flag value into a log-appropriate [`EncoderConfig`].
///
/// Log encoders are a subset: `json_lines`, `syslog`, and `otlp` (feature-gated).
/// When `precision` is `Some`, the value is propagated into text-based encoder
/// variants so the encoder can limit decimal places in formatted values.
fn parse_log_encoder_config(encoder: &str, precision: Option<u8>) -> Result<EncoderConfig> {
    match encoder {
        "json_lines" => Ok(EncoderConfig::JsonLines { precision }),
        "syslog" => Ok(EncoderConfig::Syslog {
            hostname: None,
            app_name: None,
        }),
        "otlp" => {
            #[cfg(feature = "otlp")]
            {
                let _ = precision;
                Ok(EncoderConfig::Otlp)
            }
            #[cfg(not(feature = "otlp"))]
            {
                let _ = precision;
                bail!("--encoder otlp requires the otlp feature: cargo build -F otlp")
            }
        }
        other => bail!(
            "unknown log encoder {:?}: expected one of json_lines, syslog, otlp",
            other
        ),
    }
}

/// Load and return a [`LogScenarioConfig`] from the provided [`LogsArgs`].
///
/// If `--scenario` is given the file is loaded through
/// [`crate::scenario_loader::load_scenario_entries`] (v1 and v2 accepted).
/// The compiled entries must consist of exactly one `logs` entry; any
/// other shape is rejected with a pointer to the right subcommand. Any
/// CLI flag that is `Some(...)` then overrides the corresponding field
/// in the resulting config.
///
/// If no `--scenario` file is given the config is built entirely from CLI
/// flags; `--mode` is required in this case.
///
/// # Errors
///
/// Returns an error if:
/// - The scenario file cannot be read or is not valid YAML / v2 source.
/// - The file compiles to zero or more than one entry, or produces a
///   non-logs entry.
/// - `--mode` is absent and no scenario file was provided.
/// - `--mode replay` is specified without `--file`.
/// - An unrecognized `--encoder` value is given.
/// - Both `--gap-every` and `--gap-for` are not provided together.
/// - `--burst-every`, `--burst-for`, and `--burst-multiplier` are not all
///   provided together.
pub fn load_log_config(
    args: &LogsArgs,
    scenario_catalog: &crate::scenarios::ScenarioCatalog,
    pack_catalog: &crate::packs::PackCatalog,
) -> Result<LogScenarioConfig> {
    validate_log_cli_flags(args)?;

    let mut config = if let Some(ref path) = args.scenario {
        load_single_entry_from_scenario_file(
            path,
            scenario_catalog,
            pack_catalog,
            SignalKind::Logs,
            "logs",
        )?
        .into_logs()
    } else {
        // No scenario file — build from CLI flags.
        let mode = args.mode.as_deref().ok_or_else(|| {
            anyhow::anyhow!("--mode is required when no --scenario file is provided")
        })?;
        let generator = build_log_generator_config(mode, args)?;
        let rate = args.rate.unwrap_or(10.0);

        LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "logs".to_string(),
                rate,
                duration: args.duration.clone(),
                gaps: build_gap_config_for_logs(args)?,
                bursts: build_log_burst_config(args)?,
                cardinality_spikes: build_log_spike_config(args)?,
                dynamic_labels: None,
                labels: build_log_labels(args),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: args.jitter,
                jitter_seed: args.jitter_seed,
            },
            generator,
            encoder: parse_log_encoder_config(
                args.encoder.as_deref().unwrap_or("json_lines"),
                args.precision,
            )?,
        }
    };

    // Apply CLI overrides onto the loaded file config.
    apply_log_overrides(&mut config, args)?;

    // --output overrides the sink to a file sink regardless of YAML.
    if let Some(ref path) = args.output {
        config.sink = SinkConfig::File {
            path: path.display().to_string(),
        };
    }

    // --sink overrides the sink using the build_sink_config factory.
    // For the logs subcommand, --signal-type defaults to "logs" when --sink
    // otlp_grpc is used so the user doesn't need to specify it explicitly.
    if let Some(ref sink_type) = args.sink {
        let signal = args.signal_type.as_deref().or(if sink_type == "otlp_grpc" {
            Some("logs")
        } else {
            None
        });
        config.sink = build_sink_config(
            sink_type,
            args.endpoint.as_deref(),
            signal,
            args.batch_size,
            args.content_type.as_deref(),
            args.brokers.as_deref(),
            args.topic.as_deref(),
        )?;
    }

    // --retry-* overrides the retry config on the current sink.
    if let Some(retry_cfg) = build_retry_config_from_logs(args)? {
        apply_retry_to_sink(&mut config.sink, retry_cfg)?;
    }

    Ok(config)
}

/// Apply CLI flag overrides onto a log config loaded from a YAML file.
fn apply_log_overrides(config: &mut LogScenarioConfig, args: &LogsArgs) -> Result<()> {
    if let Some(rate) = args.rate {
        config.rate = rate;
    }
    if args.duration.is_some() {
        config.duration = args.duration.clone();
    }

    // Generator: rebuild if any generator-related flag was provided.
    //
    // When --mode is absent but --message / --severity-weights / --seed are
    // present, apply those overrides on top of the existing generator config
    // rather than replacing it wholesale. This lets users tweak a YAML-loaded
    // template generator without re-specifying the mode.
    if let Some(ref mode) = args.mode {
        config.generator = build_log_generator_config(mode, args)?;
    } else if args.message.is_some() || args.severity_weights.is_some() || args.seed.is_some() {
        // Patch the existing template generator config in place if it is a
        // Template variant; ignore for Replay (flags have no meaning there).
        if let LogGeneratorConfig::Template {
            ref mut templates,
            ref mut severity_weights,
            ref mut seed,
        } = config.generator
        {
            if let Some(ref msg) = args.message {
                // Replace all templates with the single CLI-supplied message.
                *templates = vec![TemplateConfig {
                    message: msg.clone(),
                    field_pools: BTreeMap::new(),
                }];
            }
            if let Some(ref sw) = args.severity_weights {
                *severity_weights = Some(parse_severity_weights(sw)?);
            }
            if let Some(s) = args.seed {
                *seed = Some(s);
            }
        }
    }

    // Gap: override if either gap flag is present.
    if args.gap_every.is_some() || args.gap_for.is_some() {
        config.gaps = build_gap_config_for_logs(args)?;
    }

    // Burst: override if any burst flag is present.
    if args.burst_every.is_some() || args.burst_for.is_some() || args.burst_multiplier.is_some() {
        config.bursts = build_log_burst_config(args)?;
    }

    // Spike: override if any spike flag is present.
    if args.spike_label.is_some()
        || args.spike_every.is_some()
        || args.spike_for.is_some()
        || args.spike_cardinality.is_some()
    {
        config.cardinality_spikes = build_log_spike_config(args)?;
    }

    // Jitter: override if either jitter flag is present.
    if let Some(jitter) = args.jitter {
        config.base.jitter = Some(jitter);
    }
    if let Some(jitter_seed) = args.jitter_seed {
        config.base.jitter_seed = Some(jitter_seed);
    }

    // Encoder: override when the user explicitly passes --encoder.
    if let Some(ref enc) = args.encoder {
        config.encoder = parse_log_encoder_config(enc, args.precision)?;
    } else if let Some(p) = args.precision {
        // Precision without --encoder: update the existing encoder's precision.
        match &mut config.encoder {
            EncoderConfig::PrometheusText {
                ref mut precision, ..
            } => *precision = Some(p),
            EncoderConfig::InfluxLineProtocol {
                ref mut precision, ..
            } => *precision = Some(p),
            EncoderConfig::JsonLines {
                ref mut precision, ..
            } => *precision = Some(p),
            _ => {} // syslog, remote_write — no precision field
        }
    }

    // Labels: merge CLI --label flags into existing labels.
    if !args.labels.is_empty() {
        let mut label_map = config.labels.take().unwrap_or_default();
        for (k, v) in &args.labels {
            label_map.insert(k.clone(), v.clone());
        }
        config.labels = Some(label_map);
    }

    Ok(())
}

/// Build a labels map from CLI `--label` flags for a log scenario.
///
/// Returns `None` if no labels were provided (so the config field stays `None`
/// rather than `Some(empty_map)`), matching the metrics pattern.
fn build_log_labels(args: &LogsArgs) -> Option<HashMap<String, String>> {
    if args.labels.is_empty() {
        None
    } else {
        Some(
            args.labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }
}

/// Parse a `--severity-weights` string (e.g. `"info=0.7,warn=0.2,error=0.1"`) into a
/// `HashMap<String, f64>`.
///
/// Each entry must be in `name=weight` format where `weight` is a non-negative float.
fn parse_severity_weights(s: &str) -> Result<HashMap<String, f64>> {
    let mut map = HashMap::new();
    for pair in s.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let eq = pair.find('=').ok_or_else(|| {
            anyhow::anyhow!(
                "severity weight {:?} must be in name=weight format (no '=' found)",
                pair
            )
        })?;
        let name = pair[..eq].trim().to_string();
        let weight_str = pair[eq + 1..].trim();
        let weight: f64 = weight_str.parse().with_context(|| {
            format!(
                "severity weight for {:?}: {:?} is not a valid float",
                name, weight_str
            )
        })?;
        map.insert(name, weight);
    }
    Ok(map)
}

/// Build a [`LogGeneratorConfig`] from CLI flags.
fn build_log_generator_config(mode: &str, args: &LogsArgs) -> Result<LogGeneratorConfig> {
    match mode {
        "template" => {
            let message = args
                .message
                .clone()
                .unwrap_or_else(|| "synthetic log event".to_string());

            let severity_weights = args
                .severity_weights
                .as_deref()
                .map(parse_severity_weights)
                .transpose()?;

            Ok(LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message,
                    field_pools: BTreeMap::new(),
                }],
                severity_weights,
                seed: args.seed,
            })
        }
        "replay" => {
            let file = args.file.clone().ok_or_else(|| {
                anyhow::anyhow!("--file is required when --mode replay is specified")
            })?;
            Ok(LogGeneratorConfig::Replay { file })
        }
        other => bail!(
            "unknown log mode {:?}: expected one of template, replay",
            other
        ),
    }
}

/// Build an optional [`GapConfig`] from `--gap-every` and `--gap-for` log args.
fn build_gap_config_for_logs(args: &LogsArgs) -> Result<Option<GapConfig>> {
    match (&args.gap_every, &args.gap_for) {
        (Some(every), Some(gap_for)) => Ok(Some(GapConfig {
            every: every.clone(),
            r#for: gap_for.clone(),
        })),
        (None, None) => Ok(None),
        (Some(_), None) => bail!("--gap-for is required when --gap-every is provided"),
        (None, Some(_)) => bail!("--gap-every is required when --gap-for is provided"),
    }
}

/// Build an optional [`BurstConfig`] from `--burst-every`, `--burst-for`, and
/// `--burst-multiplier` log args.
fn build_log_burst_config(args: &LogsArgs) -> Result<Option<BurstConfig>> {
    match (&args.burst_every, &args.burst_for, args.burst_multiplier) {
        (Some(every), Some(burst_for), Some(multiplier)) => Ok(Some(BurstConfig {
            every: every.clone(),
            r#for: burst_for.clone(),
            multiplier,
        })),
        (None, None, None) => Ok(None),
        _ => bail!(
            "--burst-every, --burst-for, and --burst-multiplier must all be provided together"
        ),
    }
}

/// Build an optional [`Vec<CardinalitySpikeConfig>`] from `--spike-*` log flags.
///
/// Mirrors [`build_spike_config`] for the `LogsArgs` struct.
fn build_log_spike_config(args: &LogsArgs) -> Result<Option<Vec<CardinalitySpikeConfig>>> {
    match (
        &args.spike_label,
        &args.spike_every,
        &args.spike_for,
        args.spike_cardinality,
    ) {
        (Some(label), Some(every), Some(spike_for), Some(cardinality)) => {
            let strategy = match args.spike_strategy.as_deref() {
                Some("counter") | None => SpikeStrategy::Counter,
                Some("random") => SpikeStrategy::Random,
                Some(other) => bail!(
                    "unknown spike strategy {:?}: expected one of counter, random",
                    other
                ),
            };
            Ok(Some(vec![CardinalitySpikeConfig {
                label: label.clone(),
                every: every.clone(),
                r#for: spike_for.clone(),
                cardinality,
                strategy,
                prefix: args.spike_prefix.clone(),
                seed: args.spike_seed,
            }]))
        }
        (None, None, None, None) => Ok(None),
        _ => bail!(
            "--spike-label, --spike-every, --spike-for, and --spike-cardinality must all be provided together"
        ),
    }
}

/// Load and return a [`MultiScenarioConfig`] from the provided [`RunArgs`].
///
/// The scenario file is read and deserialized. The YAML must have a top-level
/// `scenarios:` list where each entry carries a `signal_type` field of either
/// `metrics` or `logs`.
///
/// # Errors
///
/// Returns an error if:
/// - The scenario file cannot be read.
/// - The file is not valid YAML.
/// - The YAML does not match the `MultiScenarioConfig` structure.
#[allow(dead_code)] // retained for existing tests; main.rs now uses scenario_loader.
pub fn load_multi_config(
    args: &RunArgs,
    catalog: &crate::scenarios::ScenarioCatalog,
) -> Result<MultiScenarioConfig> {
    let path = &args.scenario;
    let contents = resolve_scenario_source(path, catalog)?;
    serde_yaml_ng::from_str::<MultiScenarioConfig>(&contents)
        .with_context(|| format!("failed to parse multi-scenario file {}", path.display()))
}

/// Load a histogram scenario from a YAML file.
///
/// Routes through [`crate::scenario_loader::load_scenario_entries`] so
/// both v1 flat-file scenarios and v2 compiled files are accepted. Files
/// that compile to more than one entry are rejected with a pointer to
/// `sonda run --scenario`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed, if the file
/// compiles to more than one entry, or if the single entry is not a
/// `histogram` entry.
pub fn load_histogram_config(
    args: &crate::cli::HistogramArgs,
    scenario_catalog: &crate::scenarios::ScenarioCatalog,
    pack_catalog: &crate::packs::PackCatalog,
) -> Result<sonda_core::config::HistogramScenarioConfig> {
    let loaded = load_single_entry_from_scenario_file(
        &args.scenario,
        scenario_catalog,
        pack_catalog,
        SignalKind::Histogram,
        "histogram",
    )?;
    Ok(loaded.into_histogram())
}

/// Load a summary scenario from a YAML file.
///
/// Routes through [`crate::scenario_loader::load_scenario_entries`] so
/// both v1 and v2 files are accepted. See [`load_histogram_config`] for
/// the dispatch contract.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed, if the file
/// compiles to more than one entry (use `sonda run --scenario`), or if
/// the single entry is not a `summary` entry.
pub fn load_summary_config(
    args: &crate::cli::SummaryArgs,
    scenario_catalog: &crate::scenarios::ScenarioCatalog,
    pack_catalog: &crate::packs::PackCatalog,
) -> Result<sonda_core::config::SummaryScenarioConfig> {
    let loaded = load_single_entry_from_scenario_file(
        &args.scenario,
        scenario_catalog,
        pack_catalog,
        SignalKind::Summary,
        "summary",
    )?;
    Ok(loaded.into_summary())
}

/// Parse a cataloged scenario into one or more [`ScenarioEntry`] values.
///
/// Reads the scenario YAML from disk via `source_path`, then dispatches
/// based on `signal_type`:
/// - `"metrics"` -> parse as [`ScenarioConfig`], return one entry.
/// - `"logs"` -> parse as [`LogScenarioConfig`], return one entry.
/// - `"histogram"` -> parse as [`HistogramScenarioConfig`], return one entry.
/// - `"summary"` -> parse as [`SummaryScenarioConfig`], return one entry.
/// - `"multi"` -> parse as [`MultiScenarioConfig`], return all entries.
///
/// Applies CLI overrides from [`ScenariosRunArgs`] (duration, rate, sink,
/// endpoint, encoder) to each entry before returning.
///
/// # Errors
///
/// Returns an error if the file cannot be read, the YAML fails to parse,
/// or if override values are invalid.
pub fn parse_builtin_scenario(
    scenario: &sonda_core::BuiltinScenario,
    args: &ScenariosRunArgs,
) -> Result<Vec<sonda_core::ScenarioEntry>> {
    use sonda_core::config::{
        HistogramScenarioConfig, LogScenarioConfig, MultiScenarioConfig, ScenarioConfig,
        SummaryScenarioConfig,
    };

    let yaml = fs::read_to_string(&scenario.source_path).with_context(|| {
        format!(
            "failed to read scenario file {:?} for {:?}",
            scenario.source_path.display(),
            scenario.name
        )
    })?;

    let mut entries = match scenario.signal_type.as_str() {
        "metrics" => {
            let config = serde_yaml_ng::from_str::<ScenarioConfig>(&yaml).with_context(|| {
                format!(
                    "failed to parse scenario {:?} as metrics config",
                    scenario.name
                )
            })?;
            vec![sonda_core::ScenarioEntry::Metrics(config)]
        }
        "logs" => {
            let config =
                serde_yaml_ng::from_str::<LogScenarioConfig>(&yaml).with_context(|| {
                    format!(
                        "failed to parse scenario {:?} as logs config",
                        scenario.name
                    )
                })?;
            vec![sonda_core::ScenarioEntry::Logs(config)]
        }
        "histogram" => {
            let config =
                serde_yaml_ng::from_str::<HistogramScenarioConfig>(&yaml).with_context(|| {
                    format!(
                        "failed to parse scenario {:?} as histogram config",
                        scenario.name
                    )
                })?;
            vec![sonda_core::ScenarioEntry::Histogram(config)]
        }
        "summary" => {
            let config =
                serde_yaml_ng::from_str::<SummaryScenarioConfig>(&yaml).with_context(|| {
                    format!(
                        "failed to parse scenario {:?} as summary config",
                        scenario.name
                    )
                })?;
            vec![sonda_core::ScenarioEntry::Summary(config)]
        }
        "multi" => {
            let config =
                serde_yaml_ng::from_str::<MultiScenarioConfig>(&yaml).with_context(|| {
                    format!(
                        "failed to parse scenario {:?} as multi config",
                        scenario.name
                    )
                })?;
            config.scenarios
        }
        other => bail!(
            "scenario {:?} has unsupported signal_type {:?}",
            scenario.name,
            other
        ),
    };

    // Apply overrides to each entry.
    for entry in &mut entries {
        apply_builtin_overrides(entry, args)?;
    }

    Ok(entries)
}

/// Apply CLI overrides from the top-level `sonda run --scenario` flags to
/// every entry in the resolved scenario list.
///
/// Used after v1/v2 dispatch has produced a `Vec<ScenarioEntry>` — the
/// overrides are the same regardless of source format. Mirrors the fields
/// exposed on [`crate::cli::RunArgs`]:
///
/// - `--duration` → entry `duration`
/// - `--rate` → entry `rate`
/// - `--sink` / `--endpoint` / `-o` / `--output` → entry sink
/// - `--encoder` → entry encoder
/// - `--label key=value` (repeatable) → merged into entry labels (CLI
///   wins on key conflict)
///
/// When none of the override flags are set, this is a cheap no-op.
///
/// # Errors
///
/// Returns an error if sink parsing fails (missing endpoint for network
/// sinks) or encoder parsing fails (unknown format).
pub fn apply_run_overrides(
    entries: &mut [sonda_core::ScenarioEntry],
    args: &crate::cli::RunArgs,
) -> Result<()> {
    // --output is shorthand for --sink file --endpoint <path>. Resolve it
    // once up-front so every entry sees the same sink.
    let (sink_override, encoder_override) = resolve_run_overrides(args)?;

    for entry in entries.iter_mut() {
        let base = match entry {
            sonda_core::ScenarioEntry::Metrics(ref mut c) => &mut c.base,
            sonda_core::ScenarioEntry::Logs(ref mut c) => &mut c.base,
            sonda_core::ScenarioEntry::Histogram(ref mut c) => &mut c.base,
            sonda_core::ScenarioEntry::Summary(ref mut c) => &mut c.base,
        };

        if let Some(ref dur) = args.duration {
            base.duration = Some(dur.clone());
        }
        if let Some(rate) = args.rate {
            base.rate = rate;
        }
        if let Some(ref sink) = sink_override {
            base.sink = sink.clone();
        }
        if !args.labels.is_empty() {
            let map = base.labels.get_or_insert_with(HashMap::new);
            for (k, v) in &args.labels {
                map.insert(k.clone(), v.clone());
            }
        }

        if let Some(ref enc) = encoder_override {
            match entry {
                sonda_core::ScenarioEntry::Metrics(ref mut c) => c.encoder = enc.clone(),
                sonda_core::ScenarioEntry::Logs(ref mut c) => c.encoder = enc.clone(),
                sonda_core::ScenarioEntry::Histogram(ref mut c) => c.encoder = enc.clone(),
                sonda_core::ScenarioEntry::Summary(ref mut c) => c.encoder = enc.clone(),
            }
        }
    }

    Ok(())
}

/// Resolve the optional sink and encoder overrides implied by
/// [`crate::cli::RunArgs`]. Extracted so [`apply_run_overrides`] touches
/// each input string exactly once.
fn resolve_run_overrides(
    args: &crate::cli::RunArgs,
) -> Result<(Option<SinkConfig>, Option<EncoderConfig>)> {
    let sink = if let Some(ref path) = args.output {
        Some(SinkConfig::File {
            path: path.display().to_string(),
        })
    } else if let Some(ref s) = args.sink {
        Some(parse_sink_override(s, args.endpoint.as_deref())?)
    } else {
        None
    };

    let encoder = match args.encoder {
        Some(ref name) => Some(parse_encoder_config(name, None)?),
        None => None,
    };

    Ok((sink, encoder))
}

/// Apply CLI overrides from `sonda scenarios run` flags to a scenario entry.
fn apply_builtin_overrides(
    entry: &mut sonda_core::ScenarioEntry,
    args: &ScenariosRunArgs,
) -> Result<()> {
    let base = match entry {
        sonda_core::ScenarioEntry::Metrics(ref mut c) => &mut c.base,
        sonda_core::ScenarioEntry::Logs(ref mut c) => &mut c.base,
        sonda_core::ScenarioEntry::Histogram(ref mut c) => &mut c.base,
        sonda_core::ScenarioEntry::Summary(ref mut c) => &mut c.base,
    };

    if let Some(ref dur) = args.duration {
        base.duration = Some(dur.clone());
    }
    if let Some(rate) = args.rate {
        base.rate = rate;
    }

    // Sink override: interpret string name and optional endpoint.
    if let Some(ref sink_name) = args.sink {
        base.sink = parse_sink_override(sink_name, args.endpoint.as_deref())?;
    }

    // Encoder override: apply to the appropriate config field.
    if let Some(ref enc_name) = args.encoder {
        let encoder = parse_encoder_config(enc_name, None)?;
        match entry {
            sonda_core::ScenarioEntry::Metrics(ref mut c) => c.encoder = encoder,
            sonda_core::ScenarioEntry::Logs(ref mut c) => c.encoder = encoder,
            sonda_core::ScenarioEntry::Histogram(ref mut c) => c.encoder = encoder,
            sonda_core::ScenarioEntry::Summary(ref mut c) => c.encoder = encoder,
        }
    }

    Ok(())
}

/// Parse a sink name string (from CLI override) into a [`SinkConfig`].
///
/// Supports all sink types: `stdout`, `file`, `tcp`, `udp`, and the
/// feature-gated sinks `http_push`, `loki`, `remote_write`, `otlp_grpc`,
/// and `kafka`. Feature-gated sinks produce a clear error message when
/// the required Cargo feature is not enabled.
fn parse_sink_override(name: &str, endpoint: Option<&str>) -> Result<SinkConfig> {
    match name {
        "stdout" => Ok(SinkConfig::Stdout),
        "file" => {
            let path = endpoint
                .ok_or_else(|| anyhow::anyhow!("--sink file requires --endpoint <path>"))?;
            Ok(SinkConfig::File {
                path: path.to_string(),
            })
        }
        "tcp" => {
            let addr = endpoint
                .ok_or_else(|| anyhow::anyhow!("--sink tcp requires --endpoint <address>"))?;
            Ok(SinkConfig::Tcp {
                address: addr.to_string(),
                retry: None,
            })
        }
        "udp" => {
            let addr = endpoint
                .ok_or_else(|| anyhow::anyhow!("--sink udp requires --endpoint <address>"))?;
            Ok(SinkConfig::Udp {
                address: addr.to_string(),
            })
        }
        "http_push" => {
            #[cfg(feature = "http")]
            {
                let url = endpoint
                    .ok_or_else(|| anyhow::anyhow!("--sink http_push requires --endpoint <url>"))?;
                Ok(SinkConfig::HttpPush {
                    url: url.to_string(),
                    content_type: None,
                    batch_size: None,
                    headers: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = endpoint;
                bail!("--sink http_push requires the http feature: cargo build -F http")
            }
        }
        "loki" => {
            #[cfg(feature = "http")]
            {
                let url = endpoint
                    .ok_or_else(|| anyhow::anyhow!("--sink loki requires --endpoint <url>"))?;
                Ok(SinkConfig::Loki {
                    url: url.to_string(),
                    batch_size: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = endpoint;
                bail!("--sink loki requires the http feature: cargo build -F http")
            }
        }
        "remote_write" => {
            #[cfg(feature = "remote-write")]
            {
                let url = endpoint.ok_or_else(|| {
                    anyhow::anyhow!("--sink remote_write requires --endpoint <url>")
                })?;
                Ok(SinkConfig::RemoteWrite {
                    url: url.to_string(),
                    batch_size: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "remote-write"))]
            {
                let _ = endpoint;
                bail!(
                    "--sink remote_write requires the remote-write feature: \
                     cargo build -F remote-write"
                )
            }
        }
        "otlp_grpc" => {
            #[cfg(feature = "otlp")]
            {
                let ep = endpoint
                    .ok_or_else(|| anyhow::anyhow!("--sink otlp_grpc requires --endpoint <url>"))?;
                Ok(SinkConfig::OtlpGrpc {
                    endpoint: ep.to_string(),
                    signal_type: sonda_core::sink::otlp_grpc::OtlpSignalType::Metrics,
                    batch_size: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "otlp"))]
            {
                let _ = endpoint;
                bail!("--sink otlp_grpc requires the otlp feature: cargo build -F otlp")
            }
        }
        "kafka" => {
            #[cfg(feature = "kafka")]
            {
                let _ = endpoint;
                bail!(
                    "--sink kafka requires --brokers and --topic flags which are not \
                     available on the scenarios run subcommand; use a YAML scenario file \
                     or the metrics/logs subcommand instead"
                )
            }
            #[cfg(not(feature = "kafka"))]
            {
                let _ = endpoint;
                bail!("--sink kafka requires the kafka feature: cargo build -F kafka")
            }
        }
        other => bail!(
            "unknown sink {:?}; expected one of: stdout, file, tcp, udp, \
             http_push, loki, remote_write, otlp_grpc, kafka",
            other
        ),
    }
}

/// Resolve a scenario path that may be a `@name` shorthand for a cataloged scenario.
///
/// If the path starts with `@`, the remainder is treated as a scenario name
/// and looked up in the [`ScenarioCatalog`]. The YAML is read from the
/// discovered file on disk. Otherwise, the file path is read directly.
///
/// # Errors
///
/// Returns an error if:
/// - The path starts with `@` but no scenario matches the name in the catalog.
/// - The file path cannot be read from disk.
///
/// [`ScenarioCatalog`]: crate::scenarios::ScenarioCatalog
pub fn resolve_scenario_source(
    path: &std::path::Path,
    catalog: &crate::scenarios::ScenarioCatalog,
) -> Result<String> {
    let path_str = path.to_string_lossy();
    if let Some(name) = path_str.strip_prefix('@') {
        let yaml = catalog
            .read_yaml(name)
            .ok_or_else(|| {
                let names = catalog.available_names();
                anyhow::anyhow!(
                    "unknown scenario {:?}; available scenarios: {}",
                    name,
                    names.join(", ")
                )
            })?
            .map_err(|e| anyhow::anyhow!("failed to read scenario file for {:?}: {}", name, e))?;
        Ok(yaml)
    } else {
        fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("failed to read scenario file {}: {}\n\n  hint: use `@name` for built-in scenarios, e.g. `--scenario @cpu-spike`", path.display(), e)
            } else {
                anyhow::anyhow!("failed to read scenario file {}: {}", path.display(), e)
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Pack loading
// ---------------------------------------------------------------------------

/// Load and expand a metric pack from the [`PackCatalog`], applying CLI
/// overrides from [`PacksRunArgs`].
///
/// Looks up the pack by name in the catalog, reads its YAML from disk,
/// parses it into a [`MetricPackDef`], builds a [`PackScenarioConfig`]
/// from the CLI flags, and returns the expanded scenario entries.
///
/// # Errors
///
/// Returns an error if:
/// - The pack name is not found in the catalog.
/// - The pack YAML cannot be read or fails to parse.
/// - The expansion fails (e.g., empty metrics list).
///
/// [`PackCatalog`]: crate::packs::PackCatalog
pub fn load_pack_from_catalog(
    args: &PacksRunArgs,
    catalog: &crate::packs::PackCatalog,
) -> Result<Vec<sonda_core::ScenarioEntry>> {
    use sonda_core::packs::{MetricPackDef, PackScenarioConfig};

    let pack_yaml = catalog
        .read_yaml(&args.name)
        .ok_or_else(|| {
            let names = catalog.available_names();
            anyhow::anyhow!(
                "unknown pack {:?}; available packs: {}",
                args.name,
                names.join(", ")
            )
        })?
        .with_context(|| format!("failed to read pack {:?} from disk", args.name))?;

    let def: MetricPackDef = serde_yaml_ng::from_str(&pack_yaml)
        .with_context(|| format!("failed to parse pack {:?}", args.name))?;

    let mut labels: Option<HashMap<String, String>> = None;
    if !args.labels.is_empty() {
        let mut map = HashMap::new();
        for (k, v) in &args.labels {
            map.insert(k.clone(), v.clone());
        }
        labels = Some(map);
    }

    // `--output` takes precedence over `--sink`/`--endpoint` because clap
    // marks the two flags mutually exclusive on `PacksRunArgs`. Resolving
    // the file-sink shorthand here keeps the pack entrypoint symmetric
    // with the `sonda run` path (`resolve_run_overrides`).
    let sink = if let Some(ref path) = args.output {
        sonda_core::sink::SinkConfig::File {
            path: path.display().to_string(),
        }
    } else if let Some(ref sink_name) = args.sink {
        parse_sink_override(sink_name, args.endpoint.as_deref())?
    } else {
        sonda_core::sink::SinkConfig::Stdout
    };

    let encoder = match args.encoder {
        Some(ref enc_name) => parse_encoder_config(enc_name, None)?,
        None => sonda_core::encoder::EncoderConfig::PrometheusText { precision: None },
    };

    let pack_config = PackScenarioConfig {
        pack: args.name.clone(),
        rate: args.rate.unwrap_or(1.0),
        duration: args.duration.clone(),
        labels,
        sink,
        encoder,
        overrides: None,
    };

    let entries =
        sonda_core::packs::expand_pack(&def, &pack_config).map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(entries)
}

/// Resolve a pack reference that may be a catalog name or a file path.
///
/// If the pack string contains `/` or starts with `.`, or ends with
/// `.yaml`/`.yml`, it is treated as a file path and the YAML is read from
/// disk. Otherwise, the string is looked up in the [`PackCatalog`].
///
/// # Errors
///
/// Returns an error if:
/// - The string is a catalog name that does not exist.
/// - The file path cannot be read from disk.
///
/// [`PackCatalog`]: crate::packs::PackCatalog
pub fn resolve_pack_source(pack_ref: &str, catalog: &crate::packs::PackCatalog) -> Result<String> {
    let looks_like_file = pack_ref.contains('/')
        || pack_ref.starts_with('.')
        || pack_ref.ends_with(".yaml")
        || pack_ref.ends_with(".yml");
    if looks_like_file {
        // File path.
        fs::read_to_string(pack_ref).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "failed to read pack file {:?}: {}\n\n  hint: use a pack name on the search path (e.g. `telegraf_snmp_interface`) or a valid file path",
                    pack_ref, e
                )
            } else {
                anyhow::anyhow!("failed to read pack file {:?}: {}", pack_ref, e)
            }
        })
    } else {
        // Catalog name.
        catalog
            .read_yaml(pack_ref)
            .ok_or_else(|| {
                let names = catalog.available_names();
                anyhow::anyhow!(
                    "unknown pack {:?}; available packs: {}",
                    pack_ref,
                    names.join(", ")
                )
            })?
            .map_err(|e| anyhow::anyhow!("failed to read pack {:?} from disk: {}", pack_ref, e))
    }
}

/// Detect whether a YAML string contains a `pack:` field, indicating it
/// should be loaded as a [`PackScenarioConfig`] rather than a standard
/// scenario config.
///
/// Uses a lightweight YAML pre-parse to check for the presence of a `pack`
/// key at the top level.
pub fn is_pack_config(yaml: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct PackProbe {
        #[allow(dead_code)]
        pack: Option<String>,
    }

    serde_yaml_ng::from_str::<PackProbe>(yaml)
        .ok()
        .and_then(|p| p.pack)
        .is_some()
}

/// Load a pack scenario from a YAML string (from a file or inline).
///
/// Parses the YAML as [`PackScenarioConfig`], resolves the pack definition
/// (catalog name or file path), and expands it into scenario entries.
///
/// # Errors
///
/// Returns an error if:
/// - The YAML fails to parse as a pack config.
/// - The pack reference cannot be resolved.
/// - The pack definition fails to parse.
/// - The expansion fails.
pub fn load_pack_from_yaml(
    yaml: &str,
    catalog: &crate::packs::PackCatalog,
) -> Result<Vec<sonda_core::ScenarioEntry>> {
    use sonda_core::packs::{MetricPackDef, PackScenarioConfig};

    let config: PackScenarioConfig =
        serde_yaml_ng::from_str(yaml).context("failed to parse YAML as pack scenario config")?;

    let pack_yaml = resolve_pack_source(&config.pack, catalog)?;

    let def: MetricPackDef = serde_yaml_ng::from_str(&pack_yaml)
        .with_context(|| format!("failed to parse pack definition for {:?}", config.pack))?;

    let entries =
        sonda_core::packs::expand_pack(&def, &config).map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sonda_core::config::validate::validate_config;
    use sonda_core::encoder::EncoderConfig;
    use sonda_core::generator::GeneratorConfig;

    use super::*;
    use crate::cli::MetricsArgs;

    /// Build an empty scenario catalog for tests that don't need scenario
    /// discovery (i.e. all tests that construct configs from CLI flags).
    fn empty_catalog() -> crate::scenarios::ScenarioCatalog {
        crate::scenarios::ScenarioCatalog::discover(&[])
    }

    /// Build an empty pack catalog for tests that don't need pack discovery.
    ///
    /// The single-signal loaders now thread a [`PackCatalog`] through so the
    /// `FilesystemPackResolver` can resolve v2 `pack: <name>` references;
    /// tests that don't exercise pack resolution pass this empty catalog.
    fn empty_pack_catalog() -> crate::packs::PackCatalog {
        crate::packs::PackCatalog::discover(&[])
    }

    /// Construct a minimal `MetricsArgs` with no flags set, suitable for
    /// customising field-by-field in individual tests.
    fn default_args() -> MetricsArgs {
        MetricsArgs {
            scenario: None,
            name: None,
            rate: None,
            duration: None,
            value_mode: None,
            value: None,
            amplitude: None,
            period_secs: None,
            offset: None,
            min: None,
            max: None,
            seed: None,
            gap_every: None,
            gap_for: None,
            burst_every: None,
            burst_for: None,
            burst_multiplier: None,
            spike_label: None,
            spike_every: None,
            spike_for: None,
            spike_cardinality: None,
            spike_strategy: None,
            spike_prefix: None,
            spike_seed: None,
            jitter: None,
            jitter_seed: None,
            labels: vec![],
            encoder: None,
            precision: None,
            output: None,
            sink: None,
            endpoint: None,
            signal_type: None,
            batch_size: None,
            content_type: None,
            brokers: None,
            topic: None,
            retry_max_attempts: None,
            retry_backoff: None,
            retry_max_backoff: None,
        }
    }

    // ---- Config from flags only ----------------------------------------------

    #[test]
    fn config_from_flags_only_constant_mode() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(10.0),
            duration: Some("5s".to_string()),
            value_mode: Some("constant".to_string()),
            value: Some(1.0),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should build config from flags");
        assert_eq!(config.name, "up");
        assert_eq!(config.rate, 10.0);
        assert_eq!(config.duration.as_deref(), Some("5s"));
        match config.generator {
            GeneratorConfig::Constant { value } => assert_eq!(value, 1.0),
            other => panic!("expected Constant generator, got {other:?}"),
        }
    }

    #[test]
    fn config_from_flags_only_sine_mode_maps_all_fields() {
        let args = MetricsArgs {
            name: Some("cpu".to_string()),
            rate: Some(100.0),
            value_mode: Some("sine".to_string()),
            amplitude: Some(5.0),
            period_secs: Some(30.0),
            offset: Some(10.0),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should build sine config from flags");
        match config.generator {
            GeneratorConfig::Sine {
                amplitude,
                period_secs,
                offset,
            } => {
                assert_eq!(amplitude, 5.0);
                assert_eq!(period_secs, 30.0);
                assert_eq!(offset, 10.0);
            }
            other => panic!("expected Sine generator, got {other:?}"),
        }
    }

    #[test]
    fn config_from_flags_only_uniform_mode_maps_fields() {
        let args = MetricsArgs {
            name: Some("rng_metric".to_string()),
            rate: Some(1.0),
            value_mode: Some("uniform".to_string()),
            min: Some(2.0),
            max: Some(8.0),
            seed: Some(42),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should build uniform config");
        match config.generator {
            GeneratorConfig::Uniform { min, max, seed } => {
                assert_eq!(min, 2.0);
                assert_eq!(max, 8.0);
                assert_eq!(seed, Some(42));
            }
            other => panic!("expected Uniform generator, got {other:?}"),
        }
    }

    #[test]
    fn config_from_flags_only_sawtooth_mode_maps_fields() {
        let args = MetricsArgs {
            name: Some("ramp".to_string()),
            rate: Some(1.0),
            value_mode: Some("sawtooth".to_string()),
            min: Some(0.0),
            max: Some(100.0),
            period_secs: Some(60.0),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should build sawtooth config");
        match config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 100.0);
                assert_eq!(period_secs, 60.0);
            }
            other => panic!("expected Sawtooth generator, got {other:?}"),
        }
    }

    // ---- Config from YAML file -----------------------------------------------

    #[test]
    fn config_from_yaml_file_basic() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should load YAML scenario");
        assert_eq!(config.name, "test_metric");
        assert_eq!(config.rate, 100.0);
        assert_eq!(config.duration.as_deref(), Some("10s"));
        validate_config(&config).expect("loaded config should be valid");
    }

    #[test]
    fn config_from_yaml_file_with_labels_and_gaps() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/with-labels.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should load YAML with labels and gaps");
        assert_eq!(config.name, "interface_oper_state");
        let labels = config.labels.as_ref().expect("labels should be present");
        assert_eq!(labels.get("hostname").map(|s| s.as_str()), Some("t0-a1"));
        assert_eq!(labels.get("zone").map(|s| s.as_str()), Some("eu1"));
        assert!(config.gaps.is_some(), "gaps should be present");
    }

    #[test]
    fn config_from_yaml_missing_file_returns_error() {
        let args = MetricsArgs {
            scenario: Some(PathBuf::from("/nonexistent/path/scenario.yaml")),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("missing file should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("scenario") || msg.contains("nonexistent"),
            "error should mention file path, got: {msg}"
        );
    }

    // ---- Config merge: CLI overrides YAML ------------------------------------

    #[test]
    fn cli_rate_overrides_yaml_rate() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        // YAML has rate: 100; CLI provides --rate 500.
        let args = MetricsArgs {
            scenario: Some(path),
            rate: Some(500.0),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("override should succeed");
        assert_eq!(config.rate, 500.0, "CLI rate must override YAML rate");
    }

    #[test]
    fn cli_name_overrides_yaml_name() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            name: Some("overridden".to_string()),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("name override should succeed");
        assert_eq!(config.name, "overridden");
    }

    #[test]
    fn cli_duration_overrides_yaml_duration() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            duration: Some("99s".to_string()),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("duration override should succeed");
        assert_eq!(config.duration.as_deref(), Some("99s"));
    }

    #[test]
    fn cli_labels_are_merged_onto_yaml_labels() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/with-labels.yaml");
        // YAML has hostname and zone; add a new label from CLI.
        let args = MetricsArgs {
            scenario: Some(path),
            labels: vec![("env".to_string(), "prod".to_string())],
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("label merge should succeed");
        let labels = config.labels.as_ref().expect("labels should exist");
        // Both the original YAML labels and the CLI label must be present.
        assert_eq!(labels.get("hostname").map(|s| s.as_str()), Some("t0-a1"));
        assert_eq!(labels.get("zone").map(|s| s.as_str()), Some("eu1"));
        assert_eq!(labels.get("env").map(|s| s.as_str()), Some("prod"));
    }

    #[test]
    fn cli_label_overrides_same_key_in_yaml() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/with-labels.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            labels: vec![("hostname".to_string(), "new-host".to_string())],
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("label override should succeed");
        let labels = config.labels.as_ref().expect("labels should exist");
        assert_eq!(
            labels.get("hostname").map(|s| s.as_str()),
            Some("new-host"),
            "CLI label must override YAML label with same key"
        );
    }

    // ---- Missing required fields --------------------------------------------

    #[test]
    fn missing_name_without_scenario_returns_error() {
        let args = MetricsArgs {
            rate: Some(10.0),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("missing --name should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("name") || msg.contains("required"),
            "error should mention 'name' or 'required', got: {msg}"
        );
    }

    #[test]
    fn missing_rate_without_scenario_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("missing --rate should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("rate") || msg.contains("required"),
            "error should mention 'rate' or 'required', got: {msg}"
        );
    }

    // ---- Unknown values return errors ----------------------------------------

    #[test]
    fn unknown_value_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value_mode: Some("bogus_mode".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown value mode should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("bogus_mode"),
            "error should mention the bad mode, got: {msg}"
        );
    }

    #[test]
    fn unknown_encoder_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("nope_encoder".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown encoder should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("nope_encoder"),
            "error should mention the bad encoder, got: {msg}"
        );
    }

    // ---- Gap config: both flags required together ----------------------------

    #[test]
    fn gap_every_without_gap_for_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            gap_every: Some("2m".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--gap-every alone should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("gap-for") || msg.contains("gap_for"),
            "error should mention gap-for, got: {msg}"
        );
    }

    #[test]
    fn gap_for_without_gap_every_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            gap_for: Some("20s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--gap-for alone should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("gap-every") || msg.contains("gap_every"),
            "error should mention gap-every, got: {msg}"
        );
    }

    #[test]
    fn both_gap_flags_together_succeeds() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            gap_every: Some("2m".to_string()),
            gap_for: Some("20s".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("both gap flags should succeed");
        let gaps = config.gaps.as_ref().expect("gaps should be set");
        assert_eq!(gaps.every, "2m");
        assert_eq!(gaps.r#for, "20s");
    }

    // ---- Encoder config parsing -----------------------------------------------

    #[test]
    fn prometheus_text_encoder_parsed_correctly() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("prometheus_text".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("prometheus_text encoder should parse");
        assert!(
            matches!(config.encoder, EncoderConfig::PrometheusText { .. }),
            "encoder should be PrometheusText"
        );
    }

    // ---- Default generator when no value-mode given --------------------------

    #[test]
    fn default_value_mode_is_constant_at_zero() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("default config should succeed");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 0.0, "default constant value should be 0.0");
            }
            other => panic!("expected Constant generator by default, got {other:?}"),
        }
    }

    // ---- --output flag: overrides sink to File { path } ----------------------

    #[test]
    fn output_flag_sets_sink_to_file_with_correct_path() {
        use sonda_core::sink::SinkConfig;

        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            output: Some(PathBuf::from("/tmp/sonda-output-test.txt")),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("output flag should produce valid config");
        match &config.sink {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda-output-test.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
    }

    #[test]
    fn output_flag_overrides_stdout_default_sink() {
        use sonda_core::sink::SinkConfig;

        // Without --output the sink defaults to Stdout.
        let args_no_output = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config_no_output =
            load_config(&args_no_output, &empty_catalog(), &empty_pack_catalog())
                .expect("default config should succeed");
        assert!(
            matches!(config_no_output.sink, SinkConfig::Stdout),
            "default sink should be Stdout"
        );

        // With --output the sink must be File.
        let args_with_output = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            output: Some(PathBuf::from("/tmp/sonda-override.txt")),
            ..default_args()
        };
        let config_with_output =
            load_config(&args_with_output, &empty_catalog(), &empty_pack_catalog())
                .expect("output flag config should succeed");
        assert!(
            matches!(config_with_output.sink, SinkConfig::File { .. }),
            "sink should be File when --output is given"
        );
    }

    #[test]
    fn output_flag_overrides_yaml_file_sink_config() {
        use sonda_core::sink::SinkConfig;

        // Load a YAML scenario (uses stdout sink by default), then apply --output.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            output: Some(PathBuf::from("/tmp/sonda-yaml-override.txt")),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("output override on YAML should succeed");
        match &config.sink {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda-yaml-override.txt");
            }
            other => panic!("expected SinkConfig::File after --output override, got {other:?}"),
        }
    }

    #[test]
    fn output_flag_with_nested_path_preserves_full_path() {
        use sonda_core::sink::SinkConfig;

        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            output: Some(PathBuf::from("/tmp/sonda/nested/dir/test.txt")),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("nested output path should succeed");
        match &config.sink {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda/nested/dir/test.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
    }

    // ---- Burst config: all three flags required together --------------------

    #[test]
    fn burst_every_without_burst_for_and_multiplier_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            burst_every: Some("10s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--burst-every alone should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("burst"),
            "error should mention burst flags, got: {msg}"
        );
    }

    #[test]
    fn burst_for_without_burst_every_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            burst_for: Some("2s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--burst-for alone should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("burst"),
            "error should mention burst flags, got: {msg}"
        );
    }

    #[test]
    fn burst_multiplier_without_other_burst_flags_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            burst_multiplier: Some(5.0),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--burst-multiplier alone should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("burst"),
            "error should mention burst flags, got: {msg}"
        );
    }

    #[test]
    fn burst_every_and_for_without_multiplier_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            burst_every: Some("10s".to_string()),
            burst_for: Some("2s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--burst-every + --burst-for without --burst-multiplier should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("burst"),
            "error should mention burst flags, got: {msg}"
        );
    }

    #[test]
    fn all_three_burst_flags_together_succeeds() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            burst_every: Some("10s".to_string()),
            burst_for: Some("2s".to_string()),
            burst_multiplier: Some(5.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("all three burst flags should succeed");
        let bursts = config.bursts.as_ref().expect("bursts must be set");
        assert_eq!(bursts.every, "10s");
        assert_eq!(bursts.r#for, "2s");
        assert_eq!(bursts.multiplier, 5.0);
    }

    #[test]
    fn no_burst_flags_produces_none_burst_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("no burst flags should succeed");
        assert!(
            config.bursts.is_none(),
            "bursts must be None when no burst flags are provided"
        );
    }

    #[test]
    fn burst_flags_override_yaml_burst_config() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            burst_every: Some("5s".to_string()),
            burst_for: Some("1s".to_string()),
            burst_multiplier: Some(10.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("burst flags should override YAML");
        let bursts = config.bursts.as_ref().expect("bursts must be set");
        assert_eq!(bursts.every, "5s");
        assert_eq!(bursts.r#for, "1s");
        assert_eq!(bursts.multiplier, 10.0);
    }

    // ---- Round-trip: deserialize → validate → factories succeed ---------------

    #[test]
    fn round_trip_flags_to_valid_runnable_config() {
        use sonda_core::encoder::create_encoder;
        use sonda_core::generator::create_generator;
        use sonda_core::sink::create_sink;

        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(100.0),
            duration: Some("1s".to_string()),
            value_mode: Some("sine".to_string()),
            amplitude: Some(5.0),
            period_secs: Some(30.0),
            offset: Some(10.0),
            ..default_args()
        };

        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("round-trip config should load");
        validate_config(&config).expect("round-trip config should validate");
        let _gen = create_generator(&config.generator, config.rate).expect("generator factory");
        let _enc = create_encoder(&config.encoder).expect("encoder factory");
        let _sink = create_sink(&config.sink, None).expect("sink factory should succeed");
    }

    // ---- Jitter CLI flags: metrics -----------------------------------------

    #[test]
    fn jitter_flag_sets_config_jitter_from_flags_only() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            jitter: Some(3.5),
            jitter_seed: Some(42),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("jitter flags should produce valid config");
        assert_eq!(config.base.jitter, Some(3.5));
        assert_eq!(config.base.jitter_seed, Some(42));
    }

    #[test]
    fn jitter_flag_overrides_yaml_jitter() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            jitter: Some(7.0),
            jitter_seed: Some(99),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("jitter override should succeed");
        assert_eq!(config.base.jitter, Some(7.0));
        assert_eq!(config.base.jitter_seed, Some(99));
    }

    #[test]
    fn no_jitter_flags_leaves_jitter_none() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("config without jitter should succeed");
        assert_eq!(config.base.jitter, None);
        assert_eq!(config.base.jitter_seed, None);
    }

    // =========================================================================
    // Slice 2.5 — load_log_config tests
    // =========================================================================

    /// Helper to build a minimal `LogsArgs` with no flags set.
    fn default_logs_args() -> crate::cli::LogsArgs {
        crate::cli::LogsArgs {
            scenario: None,
            mode: None,
            file: None,
            rate: None,
            duration: None,
            encoder: None,
            precision: None,
            labels: vec![],
            gap_every: None,
            gap_for: None,
            burst_every: None,
            burst_for: None,
            burst_multiplier: None,
            spike_label: None,
            spike_every: None,
            spike_for: None,
            spike_cardinality: None,
            spike_strategy: None,
            spike_prefix: None,
            spike_seed: None,
            jitter: None,
            jitter_seed: None,
            output: None,
            sink: None,
            endpoint: None,
            signal_type: None,
            batch_size: None,
            content_type: None,
            brokers: None,
            topic: None,
            message: None,
            severity_weights: None,
            seed: None,
            retry_max_attempts: None,
            retry_backoff: None,
            retry_max_backoff: None,
        }
    }

    // ---- Config from flags only (log subcommand) -----------------------------

    #[test]
    fn load_log_config_mode_template_produces_template_generator() {
        use sonda_core::generator::LogGeneratorConfig;

        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            duration: Some("5s".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("template mode flags must produce config");
        assert_eq!(config.rate, 10.0);
        assert_eq!(config.duration.as_deref(), Some("5s"));
        assert!(
            matches!(config.generator, LogGeneratorConfig::Template { .. }),
            "generator must be Template when --mode template"
        );
    }

    #[test]
    fn load_log_config_mode_replay_with_file_produces_replay_generator() {
        use std::io::Write;

        use sonda_core::generator::LogGeneratorConfig;
        use tempfile::NamedTempFile;

        let mut tmp = NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "line one").expect("write line");
        writeln!(tmp, "line two").expect("write line");

        let args = crate::cli::LogsArgs {
            mode: Some("replay".to_string()),
            file: Some(tmp.path().to_string_lossy().into_owned()),
            rate: Some(5.0),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("replay mode with file must produce config");
        match config.generator {
            LogGeneratorConfig::Replay { file } => {
                assert!(!file.is_empty(), "replay file path must be set");
            }
            other => panic!("expected Replay generator, got {other:?}"),
        }
    }

    #[test]
    fn load_log_config_mode_replay_without_file_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("replay".to_string()),
            file: None,
            ..default_logs_args()
        };

        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("replay without --file must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("file") || msg.contains("--file"),
            "error must mention --file, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_without_mode_or_scenario_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: None,
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("missing --mode must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("mode") || msg.contains("required"),
            "error must mention --mode or 'required', got: {msg}"
        );
    }

    #[test]
    fn load_log_config_unknown_mode_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("livestream".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown mode must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("livestream"),
            "error must mention the unknown mode, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_encoder_json_lines_is_accepted() {
        use sonda_core::encoder::EncoderConfig;

        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            encoder: Some("json_lines".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("json_lines encoder must be accepted");
        assert!(
            matches!(config.encoder, EncoderConfig::JsonLines { .. }),
            "encoder must be JsonLines"
        );
    }

    #[test]
    fn load_log_config_encoder_syslog_is_accepted() {
        use sonda_core::encoder::EncoderConfig;

        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            encoder: Some("syslog".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("syslog encoder must be accepted for logs");
        assert!(
            matches!(config.encoder, EncoderConfig::Syslog { .. }),
            "encoder must be Syslog, got {:?}",
            config.encoder
        );
    }

    #[test]
    fn load_log_config_encoder_prometheus_text_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            encoder: Some("prometheus_text".to_string()),
            ..default_logs_args()
        };

        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("prometheus_text is not a valid log encoder");
        let msg = err.to_string();
        assert!(
            msg.contains("prometheus_text") || msg.contains("json_lines"),
            "error must mention the bad encoder, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_default_rate_is_10() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: None,
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("default rate config must succeed");
        assert_eq!(
            config.rate, 10.0,
            "default rate must be 10.0 when --rate is omitted"
        );
    }

    #[test]
    fn load_log_config_default_encoder_is_json_lines() {
        use sonda_core::encoder::EncoderConfig;

        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            encoder: None,
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("default encoder config must succeed");
        assert!(
            matches!(config.encoder, EncoderConfig::JsonLines { .. }),
            "default encoder for logs must be json_lines, got {:?}",
            config.encoder
        );
    }

    // ---- Gap config validation for logs --------------------------------------

    #[test]
    fn load_log_config_gap_every_without_gap_for_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            gap_every: Some("2m".to_string()),
            gap_for: None,
            ..default_logs_args()
        };

        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("gap-every without gap-for must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("gap-for") || msg.contains("gap_for"),
            "error must mention gap-for, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_gap_for_without_gap_every_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            gap_every: None,
            gap_for: Some("20s".to_string()),
            ..default_logs_args()
        };

        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("gap-for without gap-every must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("gap-every") || msg.contains("gap_every"),
            "error must mention gap-every, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_both_gap_flags_together_succeeds() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            gap_every: Some("2m".to_string()),
            gap_for: Some("20s".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("both gap flags must succeed");
        let gaps = config.gaps.as_ref().expect("gaps must be set");
        assert_eq!(gaps.every, "2m");
        assert_eq!(gaps.r#for, "20s");
    }

    // ---- Burst config validation for logs ------------------------------------

    #[test]
    fn load_log_config_partial_burst_flags_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            burst_every: Some("5s".to_string()),
            burst_for: Some("1s".to_string()),
            burst_multiplier: None, // missing
            ..default_logs_args()
        };

        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial burst flags must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("burst") || msg.contains("multiplier"),
            "error must mention burst flags, got: {msg}"
        );
    }

    #[test]
    fn load_log_config_all_burst_flags_together_succeeds() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            burst_every: Some("5s".to_string()),
            burst_for: Some("1s".to_string()),
            burst_multiplier: Some(10.0),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("all burst flags must succeed");
        let bursts = config.bursts.as_ref().expect("bursts must be set");
        assert_eq!(bursts.every, "5s");
        assert_eq!(bursts.r#for, "1s");
        assert_eq!(bursts.multiplier, 10.0);
    }

    // ---- --output flag for logs ----------------------------------------------

    #[test]
    fn load_log_config_output_flag_sets_file_sink() {
        use sonda_core::sink::SinkConfig;

        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            output: Some(PathBuf::from("/tmp/sonda-logs-test.json")),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("output flag must produce valid config");
        match &config.sink {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda-logs-test.json");
            }
            other => panic!("expected SinkConfig::File after --output, got {other:?}"),
        }
    }

    // ---- Config from YAML file -----------------------------------------------

    #[test]
    fn load_log_config_from_yaml_file_log_template() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log-template fixture must load");
        assert_eq!(config.name, "test_log_template");
        assert_eq!(config.rate, 10.0);
    }

    #[test]
    fn load_log_config_from_missing_yaml_file_returns_error() {
        let args = crate::cli::LogsArgs {
            scenario: Some(PathBuf::from("/nonexistent/path/log-scenario.yaml")),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("missing file must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("scenario") || msg.contains("nonexistent"),
            "error must mention the file path, got: {msg}"
        );
    }

    // ---- CLI overrides on YAML -----------------------------------------------

    #[test]
    fn load_log_config_cli_rate_overrides_yaml_rate() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        // The fixture has rate: 10. CLI overrides to 999.
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            rate: Some(999.0),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("CLI rate override must succeed");
        assert_eq!(config.rate, 999.0, "CLI --rate must override YAML rate");
    }

    #[test]
    fn load_log_config_cli_duration_overrides_yaml_duration() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            duration: Some("42s".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("CLI duration override must succeed");
        assert_eq!(
            config.duration.as_deref(),
            Some("42s"),
            "CLI --duration must override YAML duration"
        );
    }

    #[test]
    fn load_log_config_cli_encoder_overrides_yaml_encoder() {
        use sonda_core::encoder::EncoderConfig;

        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        // The fixture uses json_lines; override to syslog.
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            encoder: Some("syslog".to_string()),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("CLI encoder override must succeed");
        assert!(
            matches!(config.encoder, EncoderConfig::Syslog { .. }),
            "CLI --encoder must override YAML encoder to syslog"
        );
    }

    // ---- CLI --label flags for logs -----------------------------------------

    #[test]
    fn load_log_config_from_flags_includes_labels() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            duration: Some("1s".to_string()),
            labels: vec![
                ("device".to_string(), "wlan0".to_string()),
                ("hostname".to_string(), "router_01".to_string()),
            ],
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("config with labels must build");
        let labels = config.labels.as_ref().expect("labels must be Some");
        assert_eq!(labels.get("device").map(String::as_str), Some("wlan0"));
        assert_eq!(
            labels.get("hostname").map(String::as_str),
            Some("router_01")
        );
        assert_eq!(labels.len(), 2);
    }

    #[test]
    fn load_log_config_from_flags_no_labels_produces_none() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            labels: vec![],
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("config without labels must build");
        assert!(
            config.labels.is_none(),
            "labels must be None when no --label flags are provided"
        );
    }

    #[test]
    fn load_log_config_yaml_with_labels_deserializes() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/log-template-with-labels.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("YAML with labels must load");
        assert_eq!(config.name, "test_log_template_labels");
        let labels = config
            .labels
            .as_ref()
            .expect("labels must be present from YAML");
        assert_eq!(labels.get("device").map(String::as_str), Some("wlan0"));
        assert_eq!(
            labels.get("hostname").map(String::as_str),
            Some("router-01")
        );
    }

    #[test]
    fn load_log_config_cli_labels_merge_onto_yaml_labels() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/log-template-with-labels.yaml");
        // YAML has device and hostname; CLI adds a new label.
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            labels: vec![("env".to_string(), "prod".to_string())],
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("label merge must succeed");
        let labels = config.labels.as_ref().expect("labels must exist");
        // Original YAML labels must be preserved
        assert_eq!(labels.get("device").map(String::as_str), Some("wlan0"));
        assert_eq!(
            labels.get("hostname").map(String::as_str),
            Some("router-01")
        );
        // CLI label must be added
        assert_eq!(labels.get("env").map(String::as_str), Some("prod"));
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn load_log_config_cli_label_overrides_same_key_in_yaml() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/log-template-with-labels.yaml");
        // Override the "device" label from YAML
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            labels: vec![("device".to_string(), "eth0".to_string())],
            ..default_logs_args()
        };

        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("label override must succeed");
        let labels = config.labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("eth0"),
            "CLI --label must override YAML label with same key"
        );
    }

    // ---- load_multi_config --------------------------------------------------

    fn default_run_args(path: PathBuf) -> crate::cli::RunArgs {
        crate::cli::RunArgs {
            scenario: path,
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
            output: None,
            labels: vec![],
        }
    }

    #[test]
    fn load_multi_config_from_example_file_returns_ok() {
        // The example multi-scenario file ships with the repo. Verify it parses.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/multi-scenario.yaml");
        let args = default_run_args(path);
        let config = load_multi_config(&args, &empty_catalog())
            .expect("example multi-scenario.yaml must load");
        assert_eq!(config.scenarios.len(), 2, "example must have 2 scenarios");
    }

    #[test]
    fn load_multi_config_metrics_entry_has_correct_signal_type() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/multi-scenario.yaml");
        let args = default_run_args(path);
        let config = load_multi_config(&args, &empty_catalog()).unwrap();
        assert!(
            matches!(
                config.scenarios[0],
                sonda_core::config::ScenarioEntry::Metrics(_)
            ),
            "first entry should be Metrics"
        );
    }

    #[test]
    fn load_multi_config_logs_entry_has_correct_signal_type() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/multi-scenario.yaml");
        let args = default_run_args(path);
        let config = load_multi_config(&args, &empty_catalog()).unwrap();
        assert!(
            matches!(
                config.scenarios[1],
                sonda_core::config::ScenarioEntry::Logs(_)
            ),
            "second entry should be Logs"
        );
    }

    #[test]
    fn load_multi_config_from_missing_file_returns_error() {
        let args = default_run_args(PathBuf::from("/nonexistent/multi.yaml"));
        let err = load_multi_config(&args, &empty_catalog()).expect_err("missing file must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("scenario") || msg.contains("nonexistent"),
            "error must mention the missing file, got: {msg}"
        );
    }

    #[test]
    fn load_multi_config_from_invalid_yaml_returns_error() {
        use std::io::Write;
        // Write a temp file with invalid YAML for a MultiScenarioConfig.
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile must be created");
        writeln!(tmp, "not_scenarios_key: true").unwrap();
        let args = default_run_args(tmp.path().to_path_buf());
        let result = load_multi_config(&args, &empty_catalog());
        assert!(
            result.is_err(),
            "invalid multi-scenario YAML should return error"
        );
    }

    // =========================================================================
    // --precision flag tests (metrics)
    // =========================================================================

    #[test]
    fn cli_precision_flag_sets_encoder_precision() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            precision: Some(2),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("precision flag must produce valid config");
        match config.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(precision, Some(2), "precision must be Some(2)");
            }
            other => panic!("expected PrometheusText encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_precision_overrides_yaml_encoder_precision() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        // YAML has no precision; CLI passes --precision 3.
        let args = MetricsArgs {
            scenario: Some(path),
            precision: Some(3),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("precision override must succeed");
        match config.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(
                    precision,
                    Some(3),
                    "CLI --precision must override YAML encoder precision"
                );
            }
            other => panic!("expected PrometheusText encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_precision_without_encoder_flag_updates_existing_encoder() {
        // YAML sets encoder: prometheus_text; CLI passes only --precision 2 (no --encoder).
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            precision: Some(2),
            // no --encoder flag
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("precision-only override must succeed");
        match config.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(
                    precision,
                    Some(2),
                    "precision must be applied to YAML-specified encoder"
                );
            }
            other => panic!("expected PrometheusText encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_precision_with_encoder_flag() {
        // --encoder influx_lp --precision 1
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("influx_lp".to_string()),
            precision: Some(1),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("encoder + precision must produce valid config");
        match config.encoder {
            EncoderConfig::InfluxLineProtocol {
                precision,
                field_key,
            } => {
                assert_eq!(precision, Some(1), "precision must be Some(1)");
                assert_eq!(field_key, None, "field_key defaults to None from CLI");
            }
            other => panic!("expected InfluxLineProtocol encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_no_precision_flag_leaves_precision_none() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("no precision must succeed");
        match config.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(precision, None, "precision must be None when not specified");
            }
            other => panic!("expected PrometheusText encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_precision_zero_is_valid() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            precision: Some(0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("precision=0 must be valid");
        match config.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(precision, Some(0), "precision=0 must be accepted");
            }
            other => panic!("expected PrometheusText encoder, got {other:?}"),
        }
    }

    #[test]
    fn cli_precision_with_json_lines_encoder() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("json_lines".to_string()),
            precision: Some(5),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("json_lines + precision must succeed");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(precision, Some(5), "precision must be Some(5)");
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    // =========================================================================
    // --precision flag tests (logs)
    // =========================================================================

    #[test]
    fn log_cli_precision_flag_sets_encoder_precision() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            precision: Some(2),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log precision flag must produce valid config");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(precision, Some(2), "precision must be Some(2)");
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    #[test]
    fn log_cli_precision_overrides_yaml_encoder_precision() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            precision: Some(4),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log precision override must succeed");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(
                    precision,
                    Some(4),
                    "CLI --precision must override YAML encoder precision"
                );
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    #[test]
    fn log_cli_precision_without_encoder_flag_updates_existing_encoder() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            precision: Some(3),
            // no --encoder flag
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log precision-only override must succeed");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(
                    precision,
                    Some(3),
                    "precision must be applied to YAML-specified encoder"
                );
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    #[test]
    fn log_cli_precision_with_encoder_flag() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            encoder: Some("json_lines".to_string()),
            precision: Some(1),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log encoder + precision must produce valid config");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(precision, Some(1), "precision must be Some(1)");
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    #[test]
    fn log_cli_no_precision_flag_leaves_precision_none() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log no precision must succeed");
        match config.encoder {
            EncoderConfig::JsonLines { precision } => {
                assert_eq!(precision, None, "precision must be None when not specified");
            }
            other => panic!("expected JsonLines encoder, got {other:?}"),
        }
    }

    #[test]
    fn log_cli_precision_with_syslog_encoder_is_ignored() {
        // syslog has no precision field; --precision should be silently ignored
        // (the apply_log_overrides match arm falls through to the _ => {} case).
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(10.0),
            encoder: Some("syslog".to_string()),
            precision: Some(5),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("syslog + precision must not error");
        assert!(
            matches!(config.encoder, EncoderConfig::Syslog { .. }),
            "encoder must still be Syslog, got {:?}",
            config.encoder
        );
    }

    // =========================================================================
    // Spike config builder tests (metrics: build_spike_config)
    // =========================================================================

    #[test]
    fn spike_all_required_flags_succeeds() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("all spike flags must succeed");
        let spikes = config
            .cardinality_spikes
            .as_ref()
            .expect("cardinality_spikes must be set");
        assert_eq!(spikes.len(), 1, "must produce exactly one spike entry");
        assert_eq!(spikes[0].label, "pod_name");
        assert_eq!(spikes[0].every, "2m");
        assert_eq!(spikes[0].r#for, "30s");
        assert_eq!(spikes[0].cardinality, 500);
    }

    #[test]
    fn spike_no_flags_produces_none() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("no spike flags must succeed");
        assert!(
            config.cardinality_spikes.is_none(),
            "cardinality_spikes must be None when no spike flags are provided"
        );
    }

    #[test]
    fn spike_label_without_spike_every_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--spike-label alone must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn spike_every_without_spike_label_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_every: Some("2m".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--spike-every alone must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn spike_for_without_other_spike_flags_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_for: Some("30s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--spike-for alone must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn spike_cardinality_without_other_spike_flags_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_cardinality: Some(100),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--spike-cardinality alone must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn spike_partial_flags_label_and_every_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial spike flags must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn spike_unknown_strategy_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_strategy: Some("fibonacci".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown strategy must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("fibonacci"),
            "error must mention the unknown strategy, got: {msg}"
        );
    }

    #[test]
    fn spike_strategy_defaults_to_counter() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            // spike_strategy: None -> defaults to counter
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("default strategy must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(
            spikes[0].strategy,
            SpikeStrategy::Counter,
            "strategy must default to Counter when omitted"
        );
    }

    #[test]
    fn spike_explicit_counter_strategy_works() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_strategy: Some("counter".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("explicit counter strategy must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(spikes[0].strategy, SpikeStrategy::Counter);
    }

    #[test]
    fn spike_random_strategy_works() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_strategy: Some("random".to_string()),
            spike_seed: Some(42),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("random strategy must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(spikes[0].strategy, SpikeStrategy::Random);
        assert_eq!(spikes[0].seed, Some(42));
    }

    #[test]
    fn spike_prefix_is_passed_through() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_prefix: Some("node-".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("spike prefix must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(
            spikes[0].prefix.as_deref(),
            Some("node-"),
            "prefix must be passed through"
        );
    }

    #[test]
    fn spike_prefix_defaults_to_none_when_omitted() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("no prefix must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert!(
            spikes[0].prefix.is_none(),
            "prefix must be None when not specified"
        );
    }

    // =========================================================================
    // Spike config builder tests (logs: build_log_spike_config)
    // =========================================================================

    #[test]
    fn log_spike_all_required_flags_succeeds() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("all log spike flags must succeed");
        let spikes = config
            .cardinality_spikes
            .as_ref()
            .expect("cardinality_spikes must be set");
        assert_eq!(spikes.len(), 1);
        assert_eq!(spikes[0].label, "pod_name");
        assert_eq!(spikes[0].every, "2m");
        assert_eq!(spikes[0].r#for, "30s");
        assert_eq!(spikes[0].cardinality, 500);
    }

    #[test]
    fn log_spike_no_flags_produces_none() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("no log spike flags must succeed");
        assert!(
            config.cardinality_spikes.is_none(),
            "cardinality_spikes must be None when no spike flags are provided"
        );
    }

    #[test]
    fn log_spike_partial_flags_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            // missing spike_for and spike_cardinality
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial log spike flags must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("spike"),
            "error must mention spike flags, got: {msg}"
        );
    }

    #[test]
    fn log_spike_unknown_strategy_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_strategy: Some("unknown_strat".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown log spike strategy must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown_strat"),
            "error must mention the unknown strategy, got: {msg}"
        );
    }

    #[test]
    fn log_spike_strategy_defaults_to_counter() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log spike default strategy must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(
            spikes[0].strategy,
            SpikeStrategy::Counter,
            "log spike strategy must default to Counter"
        );
    }

    #[test]
    fn log_spike_random_strategy_with_seed_works() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("error_msg".to_string()),
            spike_every: Some("5m".to_string()),
            spike_for: Some("1m".to_string()),
            spike_cardinality: Some(1000),
            spike_strategy: Some("random".to_string()),
            spike_seed: Some(99),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log spike random strategy must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(spikes[0].strategy, SpikeStrategy::Random);
        assert_eq!(spikes[0].seed, Some(99));
        assert_eq!(spikes[0].label, "error_msg");
    }

    #[test]
    fn log_spike_prefix_is_passed_through() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            spike_label: Some("pod_name".to_string()),
            spike_every: Some("2m".to_string()),
            spike_for: Some("30s".to_string()),
            spike_cardinality: Some(500),
            spike_prefix: Some("node-".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log spike prefix must succeed");
        let spikes = config.cardinality_spikes.as_ref().unwrap();
        assert_eq!(
            spikes[0].prefix.as_deref(),
            Some("node-"),
            "log spike prefix must be passed through"
        );
    }

    // ---- Jitter CLI flags: logs ---------------------------------------------

    #[test]
    fn log_jitter_flag_sets_config_jitter_from_flags_only() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            jitter: Some(2.5),
            jitter_seed: Some(77),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log jitter flags should produce valid config");
        assert_eq!(config.base.jitter, Some(2.5));
        assert_eq!(config.base.jitter_seed, Some(77));
    }

    #[test]
    fn log_jitter_flag_overrides_yaml_jitter() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            jitter: Some(4.0),
            jitter_seed: Some(123),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log jitter override should succeed");
        assert_eq!(config.base.jitter, Some(4.0));
        assert_eq!(config.base.jitter_seed, Some(123));
    }

    #[test]
    fn log_no_jitter_flags_leaves_jitter_none() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("log config without jitter should succeed");
        assert_eq!(config.base.jitter, None);
        assert_eq!(config.base.jitter_seed, None);
    }

    // =========================================================================
    // --value flag tests (constant generator)
    // =========================================================================

    #[test]
    fn value_flag_sets_constant_generator_value() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(42.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--value must produce valid config");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 42.0, "--value must set constant generator value");
            }
            other => panic!("expected Constant generator, got {other:?}"),
        }
    }

    #[test]
    fn value_flag_with_explicit_constant_mode_succeeds() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(99.0),
            value_mode: Some("constant".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--value with --value-mode constant must succeed");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 99.0);
            }
            other => panic!("expected Constant generator, got {other:?}"),
        }
    }

    #[test]
    fn value_flag_alone_produces_constant_generator() {
        // --value alone (no --value-mode) defaults to constant mode.
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(7.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--value without --offset must succeed");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 7.0);
            }
            other => panic!("expected Constant generator, got {other:?}"),
        }
    }

    #[test]
    fn value_flag_with_sine_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(5.0),
            value_mode: Some("sine".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--value with sine must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--value") && msg.contains("constant"),
            "error must mention --value and constant, got: {msg}"
        );
    }

    #[test]
    fn value_flag_with_uniform_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(5.0),
            value_mode: Some("uniform".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--value with uniform must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--value") && msg.contains("constant"),
            "error must mention --value and constant, got: {msg}"
        );
    }

    #[test]
    fn value_flag_with_sawtooth_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            value: Some(5.0),
            value_mode: Some("sawtooth".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--value with sawtooth must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--value") && msg.contains("constant"),
            "error must mention --value and constant, got: {msg}"
        );
    }

    #[test]
    fn offset_with_constant_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            offset: Some(3.14),
            value_mode: Some("constant".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--offset with constant must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--offset") && msg.contains("sine"),
            "error must mention --offset and sine, got: {msg}"
        );
    }

    #[test]
    fn value_flag_triggers_generator_override_on_yaml_config() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            value: Some(55.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--value must override YAML generator");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(
                    value, 55.0,
                    "--value must override YAML generator to Constant"
                );
            }
            other => panic!("expected Constant generator after --value override, got {other:?}"),
        }
    }

    #[test]
    fn offset_without_value_mode_returns_error() {
        // When --offset is provided with no --value-mode, the implicit default
        // is "constant". Since --offset is only valid with sine, this must fail.
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            offset: Some(3.14),
            // value_mode: None — implicit constant default
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--offset without --value-mode must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--offset") && msg.contains("sine"),
            "error must mention --offset and sine, got: {msg}"
        );
    }

    #[test]
    fn value_flag_overrides_non_constant_yaml_generator() {
        // The with-labels.yaml fixture has a sine generator. Using --value
        // without --value-mode should override it to a constant generator.
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/with-labels.yaml");
        let args = MetricsArgs {
            scenario: Some(path),
            value: Some(5.0),
            // value_mode: None — not explicitly set; --value triggers the override
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--value must override sine YAML generator to constant");
        match config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(
                    value, 5.0,
                    "--value must override sine generator to Constant with value 5.0"
                );
            }
            other => {
                panic!("expected Constant generator after --value override of sine, got {other:?}")
            }
        }
    }

    #[test]
    fn offset_with_sine_mode_builds_sine_generator() {
        let args = MetricsArgs {
            name: Some("cpu".to_string()),
            rate: Some(1.0),
            offset: Some(10.0),
            value_mode: Some("sine".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("--offset with sine must succeed");
        match config.generator {
            GeneratorConfig::Sine { offset, .. } => {
                assert!(
                    (offset - 10.0).abs() < f64::EPSILON,
                    "--offset must set sine midpoint, got {offset}"
                );
            }
            other => panic!("expected Sine generator, got {other:?}"),
        }
    }

    #[test]
    fn offset_with_uniform_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            offset: Some(10.0),
            value_mode: Some("uniform".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--offset with uniform must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--offset") && msg.contains("sine"),
            "error must mention --offset and sine, got: {msg}"
        );
    }

    #[test]
    fn offset_with_sawtooth_mode_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            offset: Some(10.0),
            value_mode: Some("sawtooth".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--offset with sawtooth must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--offset") && msg.contains("sine"),
            "error must mention --offset and sine, got: {msg}"
        );
    }

    // =========================================================================
    // --sink and --encoder CLI flags for complex sinks
    // =========================================================================

    // ---- --sink http_push ---------------------------------------------------

    #[cfg(feature = "http")]
    #[test]
    fn sink_http_push_with_endpoint_produces_http_push_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/api/v1/write".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("http_push sink should produce valid config");
        match &config.sink {
            SinkConfig::HttpPush { url, .. } => {
                assert_eq!(url, "http://localhost:9090/api/v1/write");
            }
            other => panic!("expected SinkConfig::HttpPush, got {other:?}"),
        }
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_http_push_with_batch_size_sets_batch_size() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090".to_string()),
            batch_size: Some(200),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("http_push with batch_size should work");
        match &config.sink {
            SinkConfig::HttpPush { batch_size, .. } => {
                assert_eq!(*batch_size, Some(200));
            }
            other => panic!("expected SinkConfig::HttpPush, got {other:?}"),
        }
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_http_push_with_content_type_sets_content_type() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090".to_string()),
            content_type: Some("application/json".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("http_push with content_type should work");
        match &config.sink {
            SinkConfig::HttpPush { content_type, .. } => {
                assert_eq!(content_type.as_deref(), Some("application/json"));
            }
            other => panic!("expected SinkConfig::HttpPush, got {other:?}"),
        }
    }

    #[test]
    fn sink_http_push_without_endpoint_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("http_push without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint"),
            "error must mention --endpoint, got: {msg}"
        );
    }

    // ---- --sink loki --------------------------------------------------------

    #[cfg(feature = "http")]
    #[test]
    fn sink_loki_with_endpoint_produces_loki_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("loki".to_string()),
            endpoint: Some("http://localhost:3100".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("loki sink should produce valid config");
        match &config.sink {
            SinkConfig::Loki { url, .. } => {
                assert_eq!(url, "http://localhost:3100");
            }
            other => panic!("expected SinkConfig::Loki, got {other:?}"),
        }
    }

    #[test]
    fn sink_loki_without_endpoint_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("loki".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("loki without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint"),
            "error must mention --endpoint, got: {msg}"
        );
    }

    // ---- --sink remote_write ------------------------------------------------

    #[cfg(feature = "remote-write")]
    #[test]
    fn sink_remote_write_with_endpoint_produces_remote_write_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("remote_write".to_string()),
            endpoint: Some("http://localhost:8428/api/v1/write".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("remote_write sink should produce valid config");
        match &config.sink {
            SinkConfig::RemoteWrite { url, .. } => {
                assert_eq!(url, "http://localhost:8428/api/v1/write");
            }
            other => panic!("expected SinkConfig::RemoteWrite, got {other:?}"),
        }
    }

    #[test]
    fn sink_remote_write_without_endpoint_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("remote_write".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("remote_write without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint"),
            "error must mention --endpoint, got: {msg}"
        );
    }

    // ---- --sink otlp_grpc ---------------------------------------------------

    #[cfg(feature = "otlp")]
    #[test]
    fn sink_otlp_grpc_with_endpoint_and_signal_type_produces_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("otlp_grpc".to_string()),
            endpoint: Some("http://localhost:4317".to_string()),
            signal_type: Some("metrics".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("otlp_grpc sink should produce valid config");
        match &config.sink {
            SinkConfig::OtlpGrpc {
                endpoint,
                signal_type,
                ..
            } => {
                assert_eq!(endpoint, "http://localhost:4317");
                assert_eq!(
                    *signal_type,
                    sonda_core::sink::otlp_grpc::OtlpSignalType::Metrics
                );
            }
            other => panic!("expected SinkConfig::OtlpGrpc, got {other:?}"),
        }
    }

    #[test]
    fn sink_otlp_grpc_without_endpoint_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("otlp_grpc".to_string()),
            signal_type: Some("metrics".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("otlp_grpc without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint"),
            "error must mention --endpoint, got: {msg}"
        );
    }

    #[test]
    fn sink_otlp_grpc_metrics_without_signal_type_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("otlp_grpc".to_string()),
            endpoint: Some("http://localhost:4317".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("otlp_grpc for metrics without --signal-type must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--signal-type"),
            "error must mention --signal-type, got: {msg}"
        );
    }

    // ---- --sink kafka -------------------------------------------------------

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_kafka_with_brokers_and_topic_produces_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("kafka".to_string()),
            brokers: Some("127.0.0.1:9092".to_string()),
            topic: Some("telemetry".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("kafka sink should produce valid config");
        match &config.sink {
            SinkConfig::Kafka { brokers, topic, .. } => {
                assert_eq!(brokers, "127.0.0.1:9092");
                assert_eq!(topic, "telemetry");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    #[test]
    fn sink_kafka_without_brokers_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("kafka".to_string()),
            topic: Some("telemetry".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("kafka without --brokers must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--brokers"),
            "error must mention --brokers, got: {msg}"
        );
    }

    #[test]
    fn sink_kafka_without_topic_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("kafka".to_string()),
            brokers: Some("127.0.0.1:9092".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("kafka without --topic must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--topic"),
            "error must mention --topic, got: {msg}"
        );
    }

    // ---- Orphaned companion flags (--endpoint without --sink) ----------------

    #[test]
    fn endpoint_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            endpoint: Some("http://localhost:9090".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--endpoint without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn brokers_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            brokers: Some("127.0.0.1:9092".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--brokers without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn topic_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            topic: Some("telemetry".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--topic without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    // ---- Unknown sink type --------------------------------------------------

    #[test]
    fn unknown_sink_type_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("mystical_sink".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("unknown sink type must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("mystical_sink"),
            "error must mention the unknown type, got: {msg}"
        );
    }

    // ---- --encoder remote_write and otlp ------------------------------------

    #[cfg(feature = "remote-write")]
    #[test]
    fn encoder_remote_write_produces_remote_write_encoder_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("remote_write".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("remote_write encoder should parse");
        assert!(
            matches!(config.encoder, EncoderConfig::RemoteWrite),
            "encoder should be RemoteWrite, got {:?}",
            config.encoder
        );
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn encoder_otlp_produces_otlp_encoder_config() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            encoder: Some("otlp".to_string()),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("otlp encoder should parse");
        assert!(
            matches!(config.encoder, EncoderConfig::Otlp),
            "encoder should be Otlp, got {:?}",
            config.encoder
        );
    }

    // ---- Logs subcommand: --sink flags --------------------------------------

    #[cfg(feature = "http")]
    #[test]
    fn logs_sink_loki_with_endpoint_produces_loki_config() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("loki".to_string()),
            endpoint: Some("http://localhost:3100".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs loki sink should work");
        match &config.sink {
            SinkConfig::Loki { url, .. } => {
                assert_eq!(url, "http://localhost:3100");
            }
            other => panic!("expected SinkConfig::Loki, got {other:?}"),
        }
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn logs_sink_otlp_grpc_defaults_signal_type_to_logs() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("otlp_grpc".to_string()),
            endpoint: Some("http://localhost:4317".to_string()),
            // signal_type intentionally omitted — should default to "logs"
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs otlp_grpc should default signal_type to logs");
        match &config.sink {
            SinkConfig::OtlpGrpc { signal_type, .. } => {
                assert_eq!(
                    *signal_type,
                    sonda_core::sink::otlp_grpc::OtlpSignalType::Logs,
                    "signal_type should default to Logs for the logs subcommand"
                );
            }
            other => panic!("expected SinkConfig::OtlpGrpc, got {other:?}"),
        }
    }

    #[test]
    fn logs_endpoint_without_sink_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            endpoint: Some("http://localhost:9090".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs --endpoint without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn logs_sink_http_push_with_endpoint_works() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs http_push sink should work");
        match &config.sink {
            SinkConfig::HttpPush { url, .. } => {
                assert_eq!(url, "http://localhost:9090/push");
            }
            other => panic!("expected SinkConfig::HttpPush, got {other:?}"),
        }
    }

    // ---- Logs subcommand: --encoder otlp ------------------------------------

    #[cfg(feature = "otlp")]
    #[test]
    fn logs_encoder_otlp_produces_otlp_config() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            encoder: Some("otlp".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs otlp encoder should parse");
        assert!(
            matches!(config.encoder, EncoderConfig::Otlp),
            "encoder should be Otlp, got {:?}",
            config.encoder
        );
    }

    // =========================================================================
    // Orphan sink-companion flags without --sink (metrics path)
    // =========================================================================

    #[test]
    fn metrics_content_type_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            content_type: Some("application/json".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--content-type without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn metrics_signal_type_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            signal_type: Some("metrics".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--signal-type without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn metrics_batch_size_without_sink_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            batch_size: Some(100),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("--batch-size without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    // =========================================================================
    // Orphan sink-companion flags without --sink (logs path)
    // =========================================================================

    #[test]
    fn logs_content_type_without_sink_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            content_type: Some("application/json".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs --content-type without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn logs_signal_type_without_sink_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            signal_type: Some("logs".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs --signal-type without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    #[test]
    fn logs_batch_size_without_sink_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            batch_size: Some(100),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs --batch-size without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--sink"),
            "error must mention --sink, got: {msg}"
        );
    }

    // =========================================================================
    // Logs subcommand: --sink remote_write and --sink kafka happy paths
    // =========================================================================

    #[cfg(feature = "remote-write")]
    #[test]
    fn logs_sink_remote_write_with_endpoint_produces_config() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("remote_write".to_string()),
            endpoint: Some("http://localhost:8428/api/v1/write".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs remote_write sink should work");
        match &config.sink {
            SinkConfig::RemoteWrite { url, .. } => {
                assert_eq!(url, "http://localhost:8428/api/v1/write");
            }
            other => panic!("expected SinkConfig::RemoteWrite, got {other:?}"),
        }
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn logs_sink_kafka_with_brokers_and_topic_produces_config() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("kafka".to_string()),
            brokers: Some("127.0.0.1:9092".to_string()),
            topic: Some("test".to_string()),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("logs kafka sink should work");
        match &config.sink {
            SinkConfig::Kafka { brokers, topic, .. } => {
                assert_eq!(brokers, "127.0.0.1:9092");
                assert_eq!(topic, "test");
            }
            other => panic!("expected SinkConfig::Kafka, got {other:?}"),
        }
    }

    // =========================================================================
    // Logs subcommand: --sink kafka error paths
    // =========================================================================

    #[test]
    fn logs_sink_kafka_without_brokers_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("kafka".to_string()),
            topic: Some("test".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs kafka without --brokers must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--brokers"),
            "error must mention --brokers, got: {msg}"
        );
    }

    #[test]
    fn logs_sink_kafka_without_topic_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(5.0),
            sink: Some("kafka".to_string()),
            brokers: Some("127.0.0.1:9092".to_string()),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs kafka without --topic must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--topic"),
            "error must mention --topic, got: {msg}"
        );
    }

    // =========================================================================
    // Retry flags: all-or-nothing group validation
    // =========================================================================

    #[test]
    fn all_three_retry_flags_together_succeeds() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            retry_max_attempts: Some(3),
            retry_backoff: Some("100ms".to_string()),
            retry_max_backoff: Some("5s".to_string()),
            ..default_args()
        };
        let result = load_config(&args, &empty_catalog(), &empty_pack_catalog());
        assert!(
            result.is_ok(),
            "all three retry flags together should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn retry_max_attempts_alone_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            retry_max_attempts: Some(3),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial retry flags must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--retry-max-attempts") && msg.contains("together"),
            "error must mention all retry flags, got: {msg}"
        );
    }

    #[test]
    fn retry_backoff_alone_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            retry_backoff: Some("100ms".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial retry flags must fail");
        assert!(err.to_string().contains("together"));
    }

    #[test]
    fn retry_without_sink_or_scenario_returns_error() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            retry_max_attempts: Some(3),
            retry_backoff: Some("100ms".to_string()),
            retry_max_backoff: Some("5s".to_string()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("retry without --sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--retry-") && msg.contains("--sink"),
            "error should mention retry and sink, got: {msg}"
        );
    }

    #[test]
    fn no_retry_flags_preserves_default_behavior() {
        let args = MetricsArgs {
            name: Some("up".to_string()),
            rate: Some(1.0),
            ..default_args()
        };
        let config = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("should succeed without retry flags");
        // Default sink is stdout, which has no retry field — just verify it compiled.
        assert!(matches!(config.sink, SinkConfig::Stdout));
    }

    #[test]
    fn retry_on_non_network_sink_returns_error() {
        use sonda_core::sink::retry::RetryConfig;

        let retry = RetryConfig {
            max_attempts: 3,
            initial_backoff: "100ms".to_string(),
            max_backoff: "5s".to_string(),
        };

        // Stdout: non-network sink, retry must be rejected.
        let mut sink = SinkConfig::Stdout;
        let err =
            apply_retry_to_sink(&mut sink, retry.clone()).expect_err("retry on stdout must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("not supported") && msg.contains("stdout"),
            "error should mention unsupported sink type, got: {msg}"
        );

        // File: non-network sink, retry must be rejected.
        let mut sink = SinkConfig::File {
            path: "/tmp/test.txt".to_string(),
        };
        let err =
            apply_retry_to_sink(&mut sink, retry.clone()).expect_err("retry on file must fail");
        assert!(
            err.to_string().contains("not supported"),
            "error should mention unsupported sink type"
        );

        // Udp: non-network sink, retry must be rejected.
        let mut sink = SinkConfig::Udp {
            address: "127.0.0.1:9999".to_string(),
        };
        let err = apply_retry_to_sink(&mut sink, retry).expect_err("retry on udp must fail");
        assert!(
            err.to_string().contains("not supported"),
            "error should mention unsupported sink type"
        );
    }

    // =========================================================================
    // Retry flags: logs subcommand
    // =========================================================================

    #[test]
    fn logs_all_three_retry_flags_together_succeeds() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            retry_max_attempts: Some(3),
            retry_backoff: Some("100ms".to_string()),
            retry_max_backoff: Some("5s".to_string()),
            ..default_logs_args()
        };
        let result = load_log_config(&args, &empty_catalog(), &empty_pack_catalog());
        assert!(
            result.is_ok(),
            "all three retry flags together should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn logs_retry_partial_flags_returns_error() {
        let args = crate::cli::LogsArgs {
            mode: Some("template".to_string()),
            rate: Some(1.0),
            sink: Some("http_push".to_string()),
            endpoint: Some("http://localhost:9090/push".to_string()),
            retry_max_attempts: Some(3),
            ..default_logs_args()
        };
        let err = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("partial retry flags must fail");
        assert!(err.to_string().contains("together"));
    }

    // ---- resolve_scenario_source tests ------------------------------------------

    /// Build a scenario catalog from the repo's `scenarios/` directory.
    fn repo_scenario_catalog() -> crate::scenarios::ScenarioCatalog {
        let scenarios_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scenarios");
        crate::scenarios::ScenarioCatalog::discover(&[scenarios_dir])
    }

    #[test]
    fn resolve_at_name_returns_yaml_from_catalog() {
        let catalog = repo_scenario_catalog();
        let path = PathBuf::from("@cpu-spike");
        let yaml = resolve_scenario_source(&path, &catalog).expect("@cpu-spike must resolve");
        assert!(
            yaml.contains("node_cpu_usage_percent"),
            "resolved YAML must contain the metric name from cpu-spike"
        );
    }

    #[test]
    fn resolve_at_unknown_name_returns_error_with_hint() {
        let catalog = repo_scenario_catalog();
        let path = PathBuf::from("@nonexistent");
        let err = resolve_scenario_source(&path, &catalog).expect_err("@nonexistent must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown scenario"),
            "error must mention 'unknown scenario', got: {msg}"
        );
        assert!(
            msg.contains("cpu-spike"),
            "error must list available names including cpu-spike, got: {msg}"
        );
    }

    #[test]
    fn resolve_file_path_reads_from_disk() {
        // Use an existing example file to confirm disk path still works.
        let catalog = empty_catalog();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("examples")
            .join("basic-metrics.yaml");
        let yaml = resolve_scenario_source(&path, &catalog).expect("example file must be readable");
        assert!(
            yaml.contains("interface_oper_state"),
            "example YAML must contain the metric name"
        );
    }

    #[test]
    fn resolve_missing_file_path_returns_io_error() {
        let catalog = empty_catalog();
        let path = PathBuf::from("/nonexistent/path/scenario.yaml");
        let err = resolve_scenario_source(&path, &catalog).expect_err("missing file must fail");
        assert!(
            err.to_string().contains("failed to read"),
            "error must mention file reading failure"
        );
    }

    #[test]
    fn load_config_with_at_name_shorthand() {
        let catalog = repo_scenario_catalog();
        let args = MetricsArgs {
            scenario: Some(PathBuf::from("@cpu-spike")),
            ..default_args()
        };
        let config = load_config(&args, &catalog, &empty_pack_catalog())
            .expect("@cpu-spike must load as metrics config");
        assert_eq!(config.name, "node_cpu_usage_percent");
    }

    #[test]
    fn load_config_with_at_unknown_returns_error() {
        let catalog = repo_scenario_catalog();
        let args = MetricsArgs {
            scenario: Some(PathBuf::from("@does-not-exist")),
            ..default_args()
        };
        let err = load_config(&args, &catalog, &empty_pack_catalog())
            .expect_err("@does-not-exist must fail");
        assert!(err.to_string().contains("unknown scenario"));
    }

    #[test]
    fn load_log_config_with_at_name_shorthand() {
        let catalog = repo_scenario_catalog();
        let args = crate::cli::LogsArgs {
            scenario: Some(PathBuf::from("@log-storm")),
            ..default_logs_args()
        };
        let config = load_log_config(&args, &catalog, &empty_pack_catalog())
            .expect("@log-storm must load as logs config");
        assert_eq!(config.name, "app_error_storm");
    }

    #[test]
    fn load_multi_config_with_at_name_shorthand() {
        let catalog = repo_scenario_catalog();
        let args = crate::cli::RunArgs {
            scenario: PathBuf::from("@interface-flap"),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
            output: None,
            labels: vec![],
        };
        let config =
            load_multi_config(&args, &catalog).expect("@interface-flap must load as multi config");
        assert!(
            !config.scenarios.is_empty(),
            "interface-flap must have at least one scenario entry"
        );
    }

    #[test]
    fn load_histogram_config_with_at_name_shorthand() {
        let catalog = repo_scenario_catalog();
        let args = crate::cli::HistogramArgs {
            scenario: PathBuf::from("@histogram-latency"),
        };
        let config = load_histogram_config(&args, &catalog, &empty_pack_catalog())
            .expect("@histogram-latency must load as histogram config");
        assert_eq!(config.name, "http_request_duration_seconds");
    }

    // ---- parse_builtin_scenario tests -------------------------------------------

    #[test]
    fn parse_builtin_metrics_scenario() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("cpu-spike").expect("must exist");
        let args = ScenariosRunArgs {
            name: "cpu-spike".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], sonda_core::ScenarioEntry::Metrics(_)));
    }

    #[test]
    fn parse_builtin_logs_scenario() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("log-storm").expect("must exist");
        let args = ScenariosRunArgs {
            name: "log-storm".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], sonda_core::ScenarioEntry::Logs(_)));
    }

    #[test]
    fn parse_builtin_multi_scenario() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("interface-flap").expect("must exist");
        let args = ScenariosRunArgs {
            name: "interface-flap".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        assert!(
            entries.len() > 1,
            "interface-flap is multi-scenario and must have multiple entries"
        );
    }

    #[test]
    fn parse_builtin_histogram_scenario() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("histogram-latency").expect("must exist");
        let args = ScenariosRunArgs {
            name: "histogram-latency".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0],
            sonda_core::ScenarioEntry::Histogram(_)
        ));
    }

    #[test]
    fn parse_builtin_with_duration_override() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("cpu-spike").expect("must exist");
        let args = ScenariosRunArgs {
            name: "cpu-spike".to_string(),
            duration: Some("5s".to_string()),
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        let base = entries[0].base();
        assert_eq!(base.duration.as_deref(), Some("5s"));
    }

    #[test]
    fn parse_builtin_with_rate_override() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("cpu-spike").expect("must exist");
        let args = ScenariosRunArgs {
            name: "cpu-spike".to_string(),
            duration: None,
            rate: Some(5.0),
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        assert_eq!(entries[0].base().rate, 5.0);
    }

    #[test]
    fn parse_builtin_with_sink_override() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("cpu-spike").expect("must exist");
        let args = ScenariosRunArgs {
            name: "cpu-spike".to_string(),
            duration: None,
            rate: None,
            sink: Some("file".to_string()),
            endpoint: Some("/tmp/test-output.txt".to_string()),
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        let base = entries[0].base();
        assert!(
            matches!(&base.sink, SinkConfig::File { path } if path == "/tmp/test-output.txt"),
            "sink must be overridden to file"
        );
    }

    #[test]
    fn parse_builtin_with_encoder_override() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("cpu-spike").expect("must exist");
        let args = ScenariosRunArgs {
            name: "cpu-spike".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: Some("json_lines".to_string()),
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        match &entries[0] {
            sonda_core::ScenarioEntry::Metrics(c) => {
                assert!(matches!(c.encoder, EncoderConfig::JsonLines { .. }));
            }
            other => panic!("expected Metrics entry, got {:?}", other),
        }
    }

    #[test]
    fn parse_builtin_multi_applies_overrides_to_all_entries() {
        let catalog = repo_scenario_catalog();
        let scenario = catalog.find("interface-flap").expect("must exist");
        let args = ScenariosRunArgs {
            name: "interface-flap".to_string(),
            duration: Some("10s".to_string()),
            rate: Some(2.0),
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(scenario, &args).expect("must parse");
        for entry in &entries {
            assert_eq!(entry.base().duration.as_deref(), Some("10s"));
            assert_eq!(entry.base().rate, 2.0);
        }
    }

    // ---- parse_sink_override tests ----------------------------------------------

    #[test]
    fn parse_sink_override_stdout() {
        let sink = parse_sink_override("stdout", None).expect("stdout must succeed");
        assert!(matches!(sink, SinkConfig::Stdout));
    }

    #[test]
    fn parse_sink_override_file_with_path() {
        let sink =
            parse_sink_override("file", Some("/tmp/out.txt")).expect("file with path must succeed");
        assert!(matches!(sink, SinkConfig::File { ref path } if path == "/tmp/out.txt"));
    }

    #[test]
    fn parse_sink_override_file_without_path_returns_error() {
        let err = parse_sink_override("file", None).expect_err("file without path must fail");
        assert!(err.to_string().contains("--endpoint"));
    }

    #[test]
    fn parse_sink_override_unknown_returns_error() {
        let err = parse_sink_override("nonexistent", None).expect_err("unknown sink must fail");
        assert!(err.to_string().contains("unknown sink"));
    }

    #[test]
    fn parse_sink_override_http_push_with_endpoint() {
        // http feature is enabled by default in the sonda crate.
        let sink = parse_sink_override("http_push", Some("http://localhost:9090/write"))
            .expect("http_push with endpoint must succeed");
        #[cfg(feature = "http")]
        assert!(
            matches!(sink, SinkConfig::HttpPush { ref url, .. } if url == "http://localhost:9090/write")
        );
        let _ = sink;
    }

    #[test]
    fn parse_sink_override_http_push_without_endpoint_returns_error() {
        // When the http feature is enabled, missing endpoint is an error.
        // When disabled, the feature-gate error fires first.
        let err = parse_sink_override("http_push", None)
            .expect_err("http_push without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint") || msg.contains("http feature"),
            "error must mention --endpoint or feature: {msg}"
        );
    }

    #[test]
    fn parse_sink_override_loki_with_endpoint() {
        let sink = parse_sink_override("loki", Some("http://localhost:3100"))
            .expect("loki with endpoint must succeed");
        #[cfg(feature = "http")]
        assert!(matches!(sink, SinkConfig::Loki { ref url, .. } if url == "http://localhost:3100"));
        let _ = sink;
    }

    #[test]
    fn parse_sink_override_loki_without_endpoint_returns_error() {
        let err = parse_sink_override("loki", None).expect_err("loki without endpoint must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("--endpoint") || msg.contains("http feature"),
            "error must mention --endpoint or feature: {msg}"
        );
    }

    #[test]
    fn parse_sink_override_remote_write_requires_feature_or_endpoint() {
        // remote-write is not a default feature, so this tests the disabled path
        // unless compiled with -F remote-write.
        let result = parse_sink_override("remote_write", None);
        assert!(result.is_err(), "remote_write without endpoint must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--endpoint") || msg.contains("remote-write feature"),
            "error must mention --endpoint or feature: {msg}"
        );
    }

    #[test]
    fn parse_sink_override_otlp_grpc_requires_feature_or_endpoint() {
        let result = parse_sink_override("otlp_grpc", None);
        assert!(result.is_err(), "otlp_grpc without endpoint must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--endpoint") || msg.contains("otlp feature"),
            "error must mention --endpoint or feature: {msg}"
        );
    }

    #[test]
    fn parse_sink_override_kafka_returns_error() {
        // kafka always fails in parse_sink_override: either because the feature
        // is disabled, or because --brokers/--topic are not available.
        let err = parse_sink_override("kafka", None).expect_err("kafka must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("kafka") || msg.contains("--brokers"),
            "error must mention kafka: {msg}"
        );
    }

    #[test]
    fn parse_sink_override_error_lists_all_sink_types() {
        let err = parse_sink_override("nonexistent", None).expect_err("unknown sink must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("http_push"),
            "error must list http_push: {msg}"
        );
        assert!(
            msg.contains("remote_write"),
            "error must list remote_write: {msg}"
        );
        assert!(msg.contains("loki"), "error must list loki: {msg}");
        assert!(
            msg.contains("otlp_grpc"),
            "error must list otlp_grpc: {msg}"
        );
        assert!(msg.contains("kafka"), "error must list kafka: {msg}");
    }

    // ---- parse_builtin_scenario: summary signal type --------------------------------

    /// Write a summary scenario YAML to a temp file and return a BuiltinScenario
    /// pointing at it. Each call gets a unique directory keyed by `suffix`.
    fn temp_summary_scenario(suffix: &str) -> (sonda_core::BuiltinScenario, std::path::PathBuf) {
        let yaml = r#"scenario_name: test-summary
category: test
signal_type: summary
description: Test summary scenario

name: rpc_duration_seconds
rate: 1
duration: 10s
generator:
  type: uniform
  min: 0.01
  max: 2.0
quantiles: [0.5, 0.9, 0.99]
observations_per_tick: 50
seed: 42
distribution:
  type: uniform
  min: 0.01
  max: 2.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#;
        let dir = std::env::temp_dir().join(format!(
            "sonda-summary-test-{suffix}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test-summary.yaml");
        std::fs::write(&path, yaml).expect("must write temp file");
        let scenario = sonda_core::BuiltinScenario {
            name: "test-summary".to_string(),
            category: "test".to_string(),
            signal_type: "summary".to_string(),
            description: "Test summary scenario".to_string(),
            source_path: path.clone(),
        };
        (scenario, dir)
    }

    #[test]
    fn parse_builtin_summary_scenario() {
        let (scenario, dir) = temp_summary_scenario("parse");
        let args = ScenariosRunArgs {
            name: "test-summary".to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(&scenario, &args).expect("must parse");
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(entries[0], sonda_core::ScenarioEntry::Summary(_)),
            "expected Summary entry, got {:?}",
            std::mem::discriminant(&entries[0])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_builtin_summary_applies_overrides() {
        let (scenario, dir) = temp_summary_scenario("overrides");
        let args = ScenariosRunArgs {
            name: "test-summary".to_string(),
            duration: Some("30s".to_string()),
            rate: Some(5.0),
            sink: None,
            endpoint: None,
            encoder: None,
        };
        let entries = parse_builtin_scenario(&scenario, &args).expect("must parse");
        let base = entries[0].base();
        assert_eq!(base.duration.as_deref(), Some("30s"));
        assert_eq!(base.rate, 5.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Pack loading tests -----------------------------------------------------

    /// Build a PackCatalog pointing to the repo-root `packs/` directory.
    fn test_pack_catalog() -> crate::packs::PackCatalog {
        let packs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("packs");
        crate::packs::PackCatalog::discover(&[packs_dir])
    }

    fn default_packs_run_args(name: &str) -> PacksRunArgs {
        PacksRunArgs {
            name: name.to_string(),
            duration: None,
            rate: None,
            sink: None,
            endpoint: None,
            encoder: None,
            output: None,
            labels: vec![],
        }
    }

    #[test]
    fn load_pack_telegraf_snmp_produces_five_entries() {
        let catalog = test_pack_catalog();
        let args = default_packs_run_args("telegraf_snmp_interface");
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn load_pack_node_cpu_produces_eight_entries() {
        let catalog = test_pack_catalog();
        let args = default_packs_run_args("node_exporter_cpu");
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        assert_eq!(entries.len(), 8);
    }

    #[test]
    fn load_pack_node_memory_produces_five_entries() {
        let catalog = test_pack_catalog();
        let args = default_packs_run_args("node_exporter_memory");
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn load_pack_unknown_name_returns_error() {
        let catalog = test_pack_catalog();
        let args = default_packs_run_args("nonexistent_pack");
        let err = load_pack_from_catalog(&args, &catalog).expect_err("unknown pack must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown pack"),
            "error must mention unknown pack, got: {msg}"
        );
    }

    #[test]
    fn load_pack_applies_rate_override() {
        let catalog = test_pack_catalog();
        let mut args = default_packs_run_args("node_exporter_memory");
        args.rate = Some(5.0);
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        for entry in &entries {
            assert!(
                (entry.base().rate - 5.0).abs() < f64::EPSILON,
                "rate override must be applied"
            );
        }
    }

    #[test]
    fn load_pack_applies_duration_override() {
        let catalog = test_pack_catalog();
        let mut args = default_packs_run_args("node_exporter_memory");
        args.duration = Some("30s".to_string());
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        for entry in &entries {
            assert_eq!(entry.base().duration.as_deref(), Some("30s"));
        }
    }

    #[test]
    fn load_pack_applies_labels() {
        let catalog = test_pack_catalog();
        let mut args = default_packs_run_args("telegraf_snmp_interface");
        args.labels = vec![
            ("device".to_string(), "rtr-01".to_string()),
            ("ifName".to_string(), "eth0".to_string()),
        ];
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        for entry in &entries {
            let labels = entry.base().labels.as_ref().expect("must have labels");
            assert_eq!(labels.get("device").map(String::as_str), Some("rtr-01"));
            assert_eq!(labels.get("ifName").map(String::as_str), Some("eth0"));
        }
    }

    #[test]
    fn load_pack_default_rate_is_one() {
        let catalog = test_pack_catalog();
        let args = default_packs_run_args("node_exporter_memory");
        let entries = load_pack_from_catalog(&args, &catalog).expect("must succeed");
        for entry in &entries {
            assert!(
                (entry.base().rate - 1.0).abs() < f64::EPSILON,
                "default rate must be 1.0"
            );
        }
    }

    // ---- resolve_pack_source tests ----------------------------------------------

    #[test]
    fn resolve_pack_source_catalog_name_returns_yaml() {
        let catalog = test_pack_catalog();
        let yaml = resolve_pack_source("telegraf_snmp_interface", &catalog).expect("must succeed");
        assert!(yaml.contains("name:"));
    }

    #[test]
    fn resolve_pack_source_unknown_name_returns_error() {
        let catalog = test_pack_catalog();
        let err = resolve_pack_source("nonexistent", &catalog).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown pack"),
            "error must mention unknown pack, got: {msg}"
        );
    }

    #[test]
    fn resolve_pack_source_file_path_not_found() {
        let catalog = test_pack_catalog();
        let err = resolve_pack_source("./nonexistent/pack.yaml", &catalog).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("failed to read"),
            "error must mention failed read, got: {msg}"
        );
    }

    #[test]
    fn resolve_pack_source_bare_yaml_filename_treated_as_file() {
        let catalog = test_pack_catalog();
        let err = resolve_pack_source("my_pack.yaml", &catalog).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("failed to read"),
            "bare .yaml filename must be treated as file path, got: {msg}"
        );
    }

    // ---- is_pack_config tests ---------------------------------------------------

    #[test]
    fn is_pack_config_detects_pack_field() {
        let yaml = "pack: telegraf_snmp_interface\nrate: 1\n";
        assert!(is_pack_config(yaml));
    }

    #[test]
    fn is_pack_config_rejects_normal_scenario() {
        let yaml = "name: cpu_usage\nrate: 1\ngenerator:\n  type: constant\n  value: 1.0\n";
        assert!(!is_pack_config(yaml));
    }

    #[test]
    fn is_pack_config_rejects_multi_scenario() {
        let yaml = "scenarios:\n  - signal_type: metrics\n    name: test\n    rate: 1\n";
        assert!(!is_pack_config(yaml));
    }

    // ---- load_pack_from_yaml tests ----------------------------------------------

    #[test]
    fn load_pack_from_yaml_expands_pack() {
        let catalog = test_pack_catalog();
        let yaml = r#"
pack: telegraf_snmp_interface
rate: 1
duration: 10s
labels:
  device: rtr-01
sink:
  type: stdout
encoder:
  type: prometheus_text
"#;
        let entries = load_pack_from_yaml(yaml, &catalog).expect("must succeed");
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn load_pack_from_yaml_with_overrides() {
        let catalog = test_pack_catalog();
        let yaml = r#"
pack: telegraf_snmp_interface
rate: 1
duration: 10s
labels:
  device: rtr-01
overrides:
  ifOperStatus:
    generator:
      type: constant
      value: 0.0
sink:
  type: stdout
encoder:
  type: prometheus_text
"#;
        let entries = load_pack_from_yaml(yaml, &catalog).expect("must succeed");
        assert_eq!(entries.len(), 5);

        // ifOperStatus should have the overridden generator.
        let if_oper = entries
            .iter()
            .find(|e| e.base().name == "ifOperStatus")
            .expect("must find ifOperStatus");
        match if_oper {
            sonda_core::ScenarioEntry::Metrics(c) => {
                assert!(
                    matches!(c.generator, GeneratorConfig::Constant { value } if value.abs() < f64::EPSILON),
                    "override generator must be constant(0.0), got {:?}",
                    c.generator
                );
            }
            _ => panic!("expected Metrics"),
        }
    }

    #[test]
    fn load_pack_from_yaml_example_file() {
        let catalog = test_pack_catalog();
        let example_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/pack-scenario.yaml");
        let yaml = std::fs::read_to_string(&example_path)
            .expect("example pack-scenario.yaml must be readable");
        let entries = load_pack_from_yaml(&yaml, &catalog).expect("example file must expand");
        assert_eq!(entries.len(), 5, "telegraf_snmp_interface has 5 metrics");
    }

    #[test]
    fn load_pack_from_yaml_example_with_overrides() {
        let catalog = test_pack_catalog();
        let example_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/pack-with-overrides.yaml");
        let yaml = std::fs::read_to_string(&example_path)
            .expect("example pack-with-overrides.yaml must be readable");
        let entries = load_pack_from_yaml(&yaml, &catalog).expect("example file must expand");
        assert_eq!(entries.len(), 5, "telegraf_snmp_interface has 5 metrics");
    }

    #[test]
    fn load_pack_from_yaml_invalid_yaml_returns_error() {
        let catalog = test_pack_catalog();
        let yaml = "not: valid: pack: yaml: :::";
        let result = load_pack_from_yaml(yaml, &catalog);
        assert!(result.is_err(), "invalid YAML must return error");
    }

    // =========================================================================
    // load_single_entry_from_scenario_file: v1/v2 dispatch for single-signal
    // subcommands (metrics, logs, histogram, summary).
    //
    // These tests exercise the shared helper that replaced the per-subcommand
    // v2-dispatch branches. The behaviors covered:
    //
    // - v2 files route through `FilesystemPackResolver` so pack references
    //   resolve against the CLI's pack catalog (BLOCKER 1 regression guard).
    // - v1 flat files deserialize directly into the expected config type
    //   (preserves the legacy log-scenario fixture behavior — no
    //   signal_type="metrics" default misroute).
    // - Multi-entry v2 compilations are rejected with a pointer to
    //   `sonda run --scenario`.
    // - Signal-type mismatches surface actionable diagnostics.
    // =========================================================================

    /// Build a temp dir that survives long enough for a test to read back
    /// files written into it.
    fn scoped_temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonda-single-entry-{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("must create temp dir");
        dir
    }

    /// A v1 flat log scenario file (no top-level `signal_type:`) loads
    /// through the logs subcommand without being misrouted as metrics.
    ///
    /// Regression guard: an earlier iteration of the consolidation
    /// routed every v1 flat file through a shared probe that defaulted
    /// missing `signal_type:` to `"metrics"`, breaking log fixtures that
    /// use `generator.type: template` (not a valid metrics generator).
    #[test]
    fn v1_flat_log_file_without_signal_type_loads_as_logs() {
        let dir = scoped_temp_dir("v1-flat-log");
        let path = dir.join("log.yaml");
        std::fs::write(
            &path,
            r#"name: app_log
rate: 2
duration: 200ms
generator:
  type: template
  templates:
    - message: "hello"
encoder:
  type: json_lines
"#,
        )
        .expect("write fixture");

        let args = crate::cli::LogsArgs {
            scenario: Some(path),
            ..default_logs_args()
        };
        let cfg = load_log_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect("v1 flat log file must load through logs subcommand");
        assert_eq!(cfg.name, "app_log");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A v2 scenario file that compiles to multiple entries (e.g. a pack-
    /// backed entry) is rejected on `sonda metrics --scenario` with a
    /// diagnostic that points the user to `sonda run --scenario`.
    ///
    /// Regression guard for BLOCKER 1 — before the fix, this path either
    /// errored out with a misleading "pack not found" (empty resolver) or
    /// succeeded silently despite multi-entry expansion.
    #[test]
    fn v2_multi_entry_file_is_rejected_with_run_pointer() {
        let pack_dir = scoped_temp_dir("v2-multi-pack");
        std::fs::write(
            pack_dir.join("tiny_pack.yaml"),
            r#"name: tiny_pack
description: test
category: test
metrics:
  - name: metric_a
    generator:
      type: constant
      value: 1.0
  - name: metric_b
    generator:
      type: constant
      value: 2.0
"#,
        )
        .expect("write pack");
        let pack_catalog = crate::packs::PackCatalog::discover(&[pack_dir.clone()]);

        let scenario_dir = scoped_temp_dir("v2-multi-scn");
        let scenario_path = scenario_dir.join("v2-multi.yaml");
        std::fs::write(
            &scenario_path,
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: primary
    signal_type: metrics
    pack: tiny_pack
"#,
        )
        .expect("write scenario");

        let args = MetricsArgs {
            scenario: Some(scenario_path.clone()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &pack_catalog)
            .expect_err("multi-entry v2 must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("compiled to 2 entries"),
            "error must report entry count, got: {msg}"
        );
        assert!(
            msg.contains("sonda run --scenario"),
            "error must point at run subcommand, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&pack_dir);
        let _ = std::fs::remove_dir_all(&scenario_dir);
    }

    /// A v2 scenario whose `pack:` reference matches a catalog entry
    /// resolves via the CLI's [`FilesystemPackResolver`] and compiles
    /// without the "not found in resolver" error that the earlier
    /// `InMemoryPackResolver::new()` path produced.
    #[test]
    fn v2_pack_by_name_resolves_through_filesystem_resolver() {
        let pack_dir = scoped_temp_dir("v2-single-pack");
        std::fs::write(
            pack_dir.join("single_pack.yaml"),
            r#"name: single_pack
description: test
category: test
metrics:
  - name: the_only_metric
    generator:
      type: constant
      value: 42.0
"#,
        )
        .expect("write pack");
        let pack_catalog = crate::packs::PackCatalog::discover(&[pack_dir.clone()]);

        let scenario_dir = scoped_temp_dir("v2-single-scn");
        let scenario_path = scenario_dir.join("v2-single.yaml");
        std::fs::write(
            &scenario_path,
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: primary
    signal_type: metrics
    pack: single_pack
"#,
        )
        .expect("write scenario");

        let args = MetricsArgs {
            scenario: Some(scenario_path),
            ..default_args()
        };
        let cfg = load_config(&args, &empty_catalog(), &pack_catalog)
            .expect("single-metric pack must resolve + compile");
        assert_eq!(cfg.name, "the_only_metric");

        let _ = std::fs::remove_dir_all(&pack_dir);
        let _ = std::fs::remove_dir_all(&scenario_dir);
    }

    /// A v2 scenario whose single entry is the wrong signal type for the
    /// invoking subcommand (e.g. a logs entry loaded via
    /// `sonda metrics --scenario`) fails with a diagnostic naming the
    /// correct subcommand.
    #[test]
    fn v2_signal_type_mismatch_names_right_subcommand() {
        let scenario_dir = scoped_temp_dir("v2-mismatch");
        let scenario_path = scenario_dir.join("v2-logs.yaml");
        std::fs::write(
            &scenario_path,
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: app
    signal_type: logs
    name: app_log
    log_generator:
      type: template
      templates:
        - message: "hi"
"#,
        )
        .expect("write scenario");

        let args = MetricsArgs {
            scenario: Some(scenario_path.clone()),
            ..default_args()
        };
        let err = load_config(&args, &empty_catalog(), &empty_pack_catalog())
            .expect_err("logs entry on metrics subcommand must fail");
        // Use {:#} to flatten anyhow context chain so the inner bail! shows.
        let msg = format!("{err:#}");
        assert!(
            msg.contains("contains a logs entry"),
            "error must name actual signal type, got: {msg}"
        );
        assert!(
            msg.contains("sonda logs --scenario"),
            "error must point at logs subcommand, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&scenario_dir);
    }
}
