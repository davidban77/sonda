//! Config loading: YAML file deserialization, CLI override merging, and
//! `ScenarioConfig` construction from flags alone.
//!
//! The precedence order (lowest → highest) is:
//! 1. YAML scenario file
//! 2. CLI flags (any non-`None` value overrides the file)
//!
//! No business logic lives here beyond translating user-facing arguments into
//! the `sonda_core` config types.

use std::collections::HashMap;
use std::fs;

use anyhow::{bail, Context, Result};
use sonda_core::config::{GapConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::sink::SinkConfig;

use crate::cli::MetricsArgs;

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
pub fn load_config(args: &MetricsArgs) -> Result<ScenarioConfig> {
    let mut config = if let Some(ref path) = args.scenario {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read scenario file {}", path.display()))?;
        serde_yaml::from_str::<ScenarioConfig>(&contents)
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
            name,
            rate,
            duration: args.duration.clone(),
            generator: build_generator_config(args)?,
            gaps: build_gap_config(args)?,
            labels: build_labels(args),
            encoder: parse_encoder_config(&args.encoder)?,
            sink: SinkConfig::Stdout,
        }
    };

    // Apply CLI overrides onto the loaded file config (each Some(...) wins).
    apply_overrides(&mut config, args)?;

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
    // The default_value on the clap arg means args.encoder is always "prometheus_text"
    // unless the user typed something different, so we always parse it but only
    // override when the parsed result differs from the default or when there is
    // no scenario file (in which case apply_overrides was called from the file
    // path and the encoder has already been set to the clap default).
    // Simplest correct approach: always honour the parsed encoder from the CLI.
    config.encoder = parse_encoder_config(&args.encoder)?;

    Ok(())
}

/// Build a [`GeneratorConfig`] from the generator-related CLI flags.
///
/// Defaults when flags are absent:
/// - mode: `constant`
/// - constant value / sine offset: `0.0`
/// - amplitude: `1.0`
/// - period_secs: `60.0`
/// - min: `0.0`, max: `1.0`
/// - seed: `None`
fn build_generator_config(args: &MetricsArgs) -> Result<GeneratorConfig> {
    let mode = args.value_mode.as_deref().unwrap_or("constant");
    match mode {
        "constant" => Ok(GeneratorConfig::Constant {
            value: args.offset.unwrap_or(0.0),
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
fn parse_encoder_config(encoder: &str) -> Result<EncoderConfig> {
    match encoder {
        "prometheus_text" => Ok(EncoderConfig::PrometheusText),
        other => bail!(
            "unknown encoder {:?}: expected one of prometheus_text",
            other
        ),
    }
}
