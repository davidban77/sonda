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

use crate::cli::{LogsArgs, MetricsArgs, RunArgs};

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
/// If `--scenario` is given the file is read and deserialized first. Any CLI
/// flag that is `Some(...)` then overrides the corresponding field in the file.
///
/// If no `--scenario` file is given the config is built entirely from CLI flags;
/// `--name` and `--rate` are required in this case.
///
/// # Errors
///
/// Returns an error if:
/// - The scenario file cannot be read or is not valid YAML.
/// - `--name` or `--rate` are absent and no scenario file was provided.
/// - An unrecognized `--encoder` value is given.
/// - Both `--gap-every` and `--gap-for` are not provided together.
/// - `--value` is provided with a non-constant mode.
/// - `--offset` is provided with a non-sine mode.
pub fn load_config(args: &MetricsArgs) -> Result<ScenarioConfig> {
    validate_cli_flags(args)?;

    let mut config = if let Some(ref path) = args.scenario {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read scenario file {}", path.display()))?;
        serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
            .with_context(|| format!("failed to parse scenario file {}", path.display()))?
    } else {
        // No scenario file — build a baseline config from required flags.
        let name = args.name.clone().ok_or_else(|| {
            anyhow::anyhow!("--name is required when no --scenario file is provided")
        })?;
        let rate = args.rate.ok_or_else(|| {
            anyhow::anyhow!("--rate is required when no --scenario file is provided")
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
/// If `--scenario` is given the file is read and deserialized first. Any CLI
/// flag that is `Some(...)` then overrides the corresponding field in the file.
///
/// If no `--scenario` file is given the config is built entirely from CLI
/// flags; `--mode` is required in this case.
///
/// # Errors
///
/// Returns an error if:
/// - The scenario file cannot be read or is not valid YAML.
/// - `--mode` is absent and no scenario file was provided.
/// - `--mode replay` is specified without `--file`.
/// - An unrecognized `--encoder` value is given.
/// - Both `--gap-every` and `--gap-for` are not provided together.
/// - `--burst-every`, `--burst-for`, and `--burst-multiplier` are not all
///   provided together.
pub fn load_log_config(args: &LogsArgs) -> Result<LogScenarioConfig> {
    validate_log_cli_flags(args)?;

    let mut config = if let Some(ref path) = args.scenario {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read scenario file {}", path.display()))?;
        serde_yaml_ng::from_str::<LogScenarioConfig>(&contents)
            .with_context(|| format!("failed to parse scenario file {}", path.display()))?
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
pub fn load_multi_config(args: &RunArgs) -> Result<MultiScenarioConfig> {
    let path = &args.scenario;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read scenario file {}", path.display()))?;
    serde_yaml_ng::from_str::<MultiScenarioConfig>(&contents)
        .with_context(|| format!("failed to parse multi-scenario file {}", path.display()))
}

/// Load a histogram scenario from a YAML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub fn load_histogram_config(
    args: &crate::cli::HistogramArgs,
) -> Result<sonda_core::config::HistogramScenarioConfig> {
    let path = &args.scenario;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read scenario file {}", path.display()))?;
    serde_yaml_ng::from_str(&contents)
        .with_context(|| format!("failed to parse histogram scenario file {}", path.display()))
}

/// Load a summary scenario from a YAML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub fn load_summary_config(
    args: &crate::cli::SummaryArgs,
) -> Result<sonda_core::config::SummaryScenarioConfig> {
    let path = &args.scenario;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read scenario file {}", path.display()))?;
    serde_yaml_ng::from_str(&contents)
        .with_context(|| format!("failed to parse summary scenario file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sonda_core::config::validate::validate_config;
    use sonda_core::encoder::EncoderConfig;
    use sonda_core::generator::GeneratorConfig;

    use super::*;
    use crate::cli::MetricsArgs;

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

        let config = load_config(&args).expect("should build config from flags");
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

        let config = load_config(&args).expect("should build sine config from flags");
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

        let config = load_config(&args).expect("should build uniform config");
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

        let config = load_config(&args).expect("should build sawtooth config");
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

        let config = load_config(&args).expect("should load YAML scenario");
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

        let config = load_config(&args).expect("should load YAML with labels and gaps");
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
        let err = load_config(&args).expect_err("missing file should fail");
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

        let config = load_config(&args).expect("override should succeed");
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

        let config = load_config(&args).expect("name override should succeed");
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

        let config = load_config(&args).expect("duration override should succeed");
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

        let config = load_config(&args).expect("label merge should succeed");
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

        let config = load_config(&args).expect("label override should succeed");
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
        let err = load_config(&args).expect_err("missing --name should fail");
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
        let err = load_config(&args).expect_err("missing --rate should fail");
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
        let err = load_config(&args).expect_err("unknown value mode should fail");
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
        let err = load_config(&args).expect_err("unknown encoder should fail");
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
        let err = load_config(&args).expect_err("--gap-every alone should fail");
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
        let err = load_config(&args).expect_err("--gap-for alone should fail");
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
        let config = load_config(&args).expect("both gap flags should succeed");
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
        let config = load_config(&args).expect("prometheus_text encoder should parse");
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
        let config = load_config(&args).expect("default config should succeed");
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
        let config = load_config(&args).expect("output flag should produce valid config");
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
        let config_no_output = load_config(&args_no_output).expect("default config should succeed");
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
            load_config(&args_with_output).expect("output flag config should succeed");
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
        let config = load_config(&args).expect("output override on YAML should succeed");
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
        let config = load_config(&args).expect("nested output path should succeed");
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
        let err = load_config(&args).expect_err("--burst-every alone should fail");
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
        let err = load_config(&args).expect_err("--burst-for alone should fail");
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
        let err = load_config(&args).expect_err("--burst-multiplier alone should fail");
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
        let err = load_config(&args)
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
        let config = load_config(&args).expect("all three burst flags should succeed");
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
        let config = load_config(&args).expect("no burst flags should succeed");
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
        let config = load_config(&args).expect("burst flags should override YAML");
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

        let config = load_config(&args).expect("round-trip config should load");
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
        let config = load_config(&args).expect("jitter flags should produce valid config");
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
        let config = load_config(&args).expect("jitter override should succeed");
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
        let config = load_config(&args).expect("config without jitter should succeed");
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

        let config = load_log_config(&args).expect("template mode flags must produce config");
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

        let config = load_log_config(&args).expect("replay mode with file must produce config");
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

        let err = load_log_config(&args).expect_err("replay without --file must fail");
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
        let err = load_log_config(&args).expect_err("missing --mode must fail");
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
        let err = load_log_config(&args).expect_err("unknown mode must fail");
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

        let config = load_log_config(&args).expect("json_lines encoder must be accepted");
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

        let config = load_log_config(&args).expect("syslog encoder must be accepted for logs");
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

        let err = load_log_config(&args).expect_err("prometheus_text is not a valid log encoder");
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

        let config = load_log_config(&args).expect("default rate config must succeed");
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

        let config = load_log_config(&args).expect("default encoder config must succeed");
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

        let err = load_log_config(&args).expect_err("gap-every without gap-for must fail");
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

        let err = load_log_config(&args).expect_err("gap-for without gap-every must fail");
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

        let config = load_log_config(&args).expect("both gap flags must succeed");
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

        let err = load_log_config(&args).expect_err("partial burst flags must fail");
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

        let config = load_log_config(&args).expect("all burst flags must succeed");
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

        let config = load_log_config(&args).expect("output flag must produce valid config");
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

        let config = load_log_config(&args).expect("log-template fixture must load");
        assert_eq!(config.name, "test_log_template");
        assert_eq!(config.rate, 10.0);
    }

    #[test]
    fn load_log_config_from_missing_yaml_file_returns_error() {
        let args = crate::cli::LogsArgs {
            scenario: Some(PathBuf::from("/nonexistent/path/log-scenario.yaml")),
            ..default_logs_args()
        };
        let err = load_log_config(&args).expect_err("missing file must fail");
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

        let config = load_log_config(&args).expect("CLI rate override must succeed");
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

        let config = load_log_config(&args).expect("CLI duration override must succeed");
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

        let config = load_log_config(&args).expect("CLI encoder override must succeed");
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

        let config = load_log_config(&args).expect("config with labels must build");
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

        let config = load_log_config(&args).expect("config without labels must build");
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

        let config = load_log_config(&args).expect("YAML with labels must load");
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

        let config = load_log_config(&args).expect("label merge must succeed");
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

        let config = load_log_config(&args).expect("label override must succeed");
        let labels = config.labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("eth0"),
            "CLI --label must override YAML label with same key"
        );
    }

    // ---- load_multi_config --------------------------------------------------

    fn default_run_args(path: PathBuf) -> crate::cli::RunArgs {
        crate::cli::RunArgs { scenario: path }
    }

    #[test]
    fn load_multi_config_from_example_file_returns_ok() {
        // The example multi-scenario file ships with the repo. Verify it parses.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/multi-scenario.yaml");
        let args = default_run_args(path);
        let config = load_multi_config(&args).expect("example multi-scenario.yaml must load");
        assert_eq!(config.scenarios.len(), 2, "example must have 2 scenarios");
    }

    #[test]
    fn load_multi_config_metrics_entry_has_correct_signal_type() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/multi-scenario.yaml");
        let args = default_run_args(path);
        let config = load_multi_config(&args).unwrap();
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
        let config = load_multi_config(&args).unwrap();
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
        let err = load_multi_config(&args).expect_err("missing file must fail");
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
        let result = load_multi_config(&args);
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
        let config = load_config(&args).expect("precision flag must produce valid config");
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
        let config = load_config(&args).expect("precision override must succeed");
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
        let config = load_config(&args).expect("precision-only override must succeed");
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
        let config = load_config(&args).expect("encoder + precision must produce valid config");
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
        let config = load_config(&args).expect("no precision must succeed");
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
        let config = load_config(&args).expect("precision=0 must be valid");
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
        let config = load_config(&args).expect("json_lines + precision must succeed");
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
        let config = load_log_config(&args).expect("log precision flag must produce valid config");
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
        let config = load_log_config(&args).expect("log precision override must succeed");
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
        let config = load_log_config(&args).expect("log precision-only override must succeed");
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
        let config =
            load_log_config(&args).expect("log encoder + precision must produce valid config");
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
        let config = load_log_config(&args).expect("log no precision must succeed");
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
        let config = load_log_config(&args).expect("syslog + precision must not error");
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
        let config = load_config(&args).expect("all spike flags must succeed");
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
        let config = load_config(&args).expect("no spike flags must succeed");
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
        let err = load_config(&args).expect_err("--spike-label alone must fail");
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
        let err = load_config(&args).expect_err("--spike-every alone must fail");
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
        let err = load_config(&args).expect_err("--spike-for alone must fail");
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
        let err = load_config(&args).expect_err("--spike-cardinality alone must fail");
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
        let err = load_config(&args).expect_err("partial spike flags must fail");
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
        let err = load_config(&args).expect_err("unknown strategy must fail");
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
        let config = load_config(&args).expect("default strategy must succeed");
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
        let config = load_config(&args).expect("explicit counter strategy must succeed");
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
        let config = load_config(&args).expect("random strategy must succeed");
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
        let config = load_config(&args).expect("spike prefix must succeed");
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
        let config = load_config(&args).expect("no prefix must succeed");
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
        let config = load_log_config(&args).expect("all log spike flags must succeed");
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
        let config = load_log_config(&args).expect("no log spike flags must succeed");
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
        let err = load_log_config(&args).expect_err("partial log spike flags must fail");
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
        let err = load_log_config(&args).expect_err("unknown log spike strategy must fail");
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
        let config = load_log_config(&args).expect("log spike default strategy must succeed");
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
        let config = load_log_config(&args).expect("log spike random strategy must succeed");
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
        let config = load_log_config(&args).expect("log spike prefix must succeed");
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
        let config = load_log_config(&args).expect("log jitter flags should produce valid config");
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
        let config = load_log_config(&args).expect("log jitter override should succeed");
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
        let config = load_log_config(&args).expect("log config without jitter should succeed");
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
        let config = load_config(&args).expect("--value must produce valid config");
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
        let config = load_config(&args).expect("--value with --value-mode constant must succeed");
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
        let config = load_config(&args).expect("--value without --offset must succeed");
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
        let err = load_config(&args).expect_err("--value with sine must fail");
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
        let err = load_config(&args).expect_err("--value with uniform must fail");
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
        let err = load_config(&args).expect_err("--value with sawtooth must fail");
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
        let err = load_config(&args).expect_err("--offset with constant must fail");
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
        let config = load_config(&args).expect("--value must override YAML generator");
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
        let err = load_config(&args).expect_err("--offset without --value-mode must fail");
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
        let config =
            load_config(&args).expect("--value must override sine YAML generator to constant");
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
        let config = load_config(&args).expect("--offset with sine must succeed");
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
        let err = load_config(&args).expect_err("--offset with uniform must fail");
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
        let err = load_config(&args).expect_err("--offset with sawtooth must fail");
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
        let config = load_config(&args).expect("http_push sink should produce valid config");
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
        let config = load_config(&args).expect("http_push with batch_size should work");
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
        let config = load_config(&args).expect("http_push with content_type should work");
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
        let err = load_config(&args).expect_err("http_push without endpoint must fail");
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
        let config = load_config(&args).expect("loki sink should produce valid config");
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
        let err = load_config(&args).expect_err("loki without endpoint must fail");
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
        let config = load_config(&args).expect("remote_write sink should produce valid config");
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
        let err = load_config(&args).expect_err("remote_write without endpoint must fail");
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
        let config = load_config(&args).expect("otlp_grpc sink should produce valid config");
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
        let err = load_config(&args).expect_err("otlp_grpc without endpoint must fail");
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
        let err =
            load_config(&args).expect_err("otlp_grpc for metrics without --signal-type must fail");
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
        let config = load_config(&args).expect("kafka sink should produce valid config");
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
        let err = load_config(&args).expect_err("kafka without --brokers must fail");
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
        let err = load_config(&args).expect_err("kafka without --topic must fail");
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
        let err = load_config(&args).expect_err("--endpoint without --sink must fail");
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
        let err = load_config(&args).expect_err("--brokers without --sink must fail");
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
        let err = load_config(&args).expect_err("--topic without --sink must fail");
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
        let err = load_config(&args).expect_err("unknown sink type must fail");
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
        let config = load_config(&args).expect("remote_write encoder should parse");
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
        let config = load_config(&args).expect("otlp encoder should parse");
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
        let config = load_log_config(&args).expect("logs loki sink should work");
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
        let config =
            load_log_config(&args).expect("logs otlp_grpc should default signal_type to logs");
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
        let err = load_log_config(&args).expect_err("logs --endpoint without --sink must fail");
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
        let config = load_log_config(&args).expect("logs http_push sink should work");
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
        let config = load_log_config(&args).expect("logs otlp encoder should parse");
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
        let err = load_config(&args).expect_err("--content-type without --sink must fail");
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
        let err = load_config(&args).expect_err("--signal-type without --sink must fail");
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
        let err = load_config(&args).expect_err("--batch-size without --sink must fail");
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
        let err = load_log_config(&args).expect_err("logs --content-type without --sink must fail");
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
        let err = load_log_config(&args).expect_err("logs --signal-type without --sink must fail");
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
        let err = load_log_config(&args).expect_err("logs --batch-size without --sink must fail");
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
        let config = load_log_config(&args).expect("logs remote_write sink should work");
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
        let config = load_log_config(&args).expect("logs kafka sink should work");
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
        let err = load_log_config(&args).expect_err("logs kafka without --brokers must fail");
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
        let err = load_log_config(&args).expect_err("logs kafka without --topic must fail");
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
        let result = load_config(&args);
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
        let err = load_config(&args).expect_err("partial retry flags must fail");
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
        let err = load_config(&args).expect_err("partial retry flags must fail");
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
        let err = load_config(&args).expect_err("retry without --sink must fail");
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
        let config = load_config(&args).expect("should succeed without retry flags");
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
        let result = load_log_config(&args);
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
        let err = load_log_config(&args).expect_err("partial retry flags must fail");
        assert!(err.to_string().contains("together"));
    }
}
