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
            encoder: parse_encoder_config(args.encoder.as_deref().unwrap_or("prometheus_text"))?,
            sink: SinkConfig::Stdout,
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
    // Because `encoder` is `Option<String>` (no clap default_value), a `None`
    // here means the flag was omitted and the YAML value should be kept as-is.
    if let Some(ref enc) = args.encoder {
        config.encoder = parse_encoder_config(enc)?;
    }

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
        "influx_lp" => Ok(EncoderConfig::InfluxLineProtocol { field_key: None }),
        "json_lines" => Ok(EncoderConfig::JsonLines),
        other => bail!(
            "unknown encoder {:?}: expected one of prometheus_text, influx_lp, json_lines",
            other
        ),
    }
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
            amplitude: None,
            period_secs: None,
            offset: None,
            min: None,
            max: None,
            seed: None,
            gap_every: None,
            gap_for: None,
            labels: vec![],
            encoder: None,
            output: None,
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
            offset: Some(1.0),
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
            matches!(config.encoder, EncoderConfig::PrometheusText),
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
        match config.sink {
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
        match config.sink {
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
        match config.sink {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda/nested/dir/test.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
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
        let _gen = create_generator(&config.generator, config.rate);
        let _enc = create_encoder(&config.encoder);
        let _sink = create_sink(&config.sink).expect("sink factory should succeed");
    }
}
