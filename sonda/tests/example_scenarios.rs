//! Integration tests for the example scenario YAML files shipped with the project.
//!
//! Slice 0.8 test criteria:
//! - Both `examples/basic-metrics.yaml` and `examples/simple-constant.yaml` must
//!   deserialize into a valid `ScenarioConfig` without error.
//! - Both configs must pass `validate_config`.
//! - Both configs must produce working generator, encoder, and sink instances via
//!   the factory functions.

use std::path::PathBuf;

use sonda_core::config::validate::validate_config;
use sonda_core::config::ScenarioConfig;
use sonda_core::encoder::{create_encoder, EncoderConfig};
use sonda_core::generator::{create_generator, GeneratorConfig};
use sonda_core::sink::{create_sink, SinkConfig};

/// Return an absolute path to a file under the workspace root, regardless of
/// where `cargo test` is invoked from.
fn workspace_file(relative: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR for the `sonda` crate is `<workspace>/sonda`.
    // One parent step takes us to the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent directory (workspace root)")
        .join(relative)
}

// ---------------------------------------------------------------------------
// examples/basic-metrics.yaml
// ---------------------------------------------------------------------------

#[test]
fn basic_metrics_yaml_deserializes_without_error() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("basic-metrics.yaml failed to deserialize: {e}"));
}

#[test]
fn basic_metrics_yaml_has_correct_metric_name() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert_eq!(
        config.name, "interface_oper_state",
        "metric name must match the spec"
    );
}

#[test]
fn basic_metrics_yaml_has_correct_rate() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert_eq!(config.rate, 1000.0, "rate must be 1000 events/sec");
}

#[test]
fn basic_metrics_yaml_has_correct_duration() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert_eq!(
        config.duration.as_deref(),
        Some("30s"),
        "duration must be 30s"
    );
}

#[test]
fn basic_metrics_yaml_has_sine_generator() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    match config.generator {
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => {
            assert_eq!(amplitude, 5.0, "sine amplitude must be 5.0");
            assert_eq!(period_secs, 30.0, "sine period_secs must be 30");
            assert_eq!(offset, 10.0, "sine offset must be 10.0");
        }
        other => panic!("expected Sine generator, got {other:?}"),
    }
}

#[test]
fn basic_metrics_yaml_has_gap_config() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    let gaps = config
        .gaps
        .as_ref()
        .expect("basic-metrics.yaml must have a gaps section");
    assert_eq!(gaps.every, "2m", "gap.every must be 2m");
    assert_eq!(gaps.r#for, "20s", "gap.for must be 20s");
}

#[test]
fn basic_metrics_yaml_has_labels() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    let labels = config
        .labels
        .as_ref()
        .expect("basic-metrics.yaml must have labels");
    assert_eq!(
        labels.get("hostname").map(String::as_str),
        Some("t0-a1"),
        "hostname label must be t0-a1"
    );
    assert_eq!(
        labels.get("zone").map(String::as_str),
        Some("eu1"),
        "zone label must be eu1"
    );
}

#[test]
fn basic_metrics_yaml_uses_prometheus_text_encoder() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert!(
        matches!(config.encoder, EncoderConfig::PrometheusText { .. }),
        "encoder must be prometheus_text"
    );
}

#[test]
fn basic_metrics_yaml_uses_stdout_sink() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

#[test]
fn basic_metrics_yaml_passes_validate_config() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("basic-metrics.yaml failed validation: {e}"));
}

#[test]
fn basic_metrics_yaml_factories_all_succeed() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize basic-metrics.yaml");

    // Generator factory must succeed and produce a working generator.
    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    let value = gen.value(0);
    // Sine at tick 0 should equal offset (10.0) since sin(0) == 0.
    assert!(
        (value - 10.0).abs() < 1e-9,
        "sine at tick 0 must equal offset 10.0, got {value}"
    );

    // Encoder factory must succeed.
    let _enc = create_encoder(&config.encoder);

    // Sink factory must succeed.
    let _sink = create_sink(&config.sink)
        .unwrap_or_else(|e| panic!("sink factory failed for basic-metrics.yaml: {e}"));
}

// ---------------------------------------------------------------------------
// examples/simple-constant.yaml
// ---------------------------------------------------------------------------

#[test]
fn simple_constant_yaml_deserializes_without_error() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("simple-constant.yaml failed to deserialize: {e}"));
}

#[test]
fn simple_constant_yaml_has_correct_metric_name() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert_eq!(config.name, "up", "metric name must be 'up'");
}

#[test]
fn simple_constant_yaml_has_correct_rate() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert_eq!(config.rate, 10.0, "rate must be 10 events/sec");
}

#[test]
fn simple_constant_yaml_has_correct_duration() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert_eq!(
        config.duration.as_deref(),
        Some("10s"),
        "duration must be 10s"
    );
}

#[test]
fn simple_constant_yaml_has_constant_generator_with_value_one() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    match config.generator {
        GeneratorConfig::Constant { value } => {
            assert_eq!(value, 1.0, "constant value must be 1.0");
        }
        other => panic!("expected Constant generator, got {other:?}"),
    }
}

#[test]
fn simple_constant_yaml_has_no_gaps() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert!(
        config.gaps.is_none(),
        "simple-constant.yaml must not define gaps"
    );
}

#[test]
fn simple_constant_yaml_uses_prometheus_text_encoder() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert!(
        matches!(config.encoder, EncoderConfig::PrometheusText { .. }),
        "encoder must be prometheus_text"
    );
}

#[test]
fn simple_constant_yaml_uses_stdout_sink() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

#[test]
fn simple_constant_yaml_passes_validate_config() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("simple-constant.yaml failed validation: {e}"));
}

#[test]
fn simple_constant_yaml_factories_all_succeed() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize simple-constant.yaml");

    // Generator factory must succeed and produce a constant value of 1.0.
    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    assert_eq!(
        gen.value(0),
        1.0,
        "constant generator must return 1.0 at tick 0"
    );
    assert_eq!(
        gen.value(1_000_000),
        1.0,
        "constant generator must return 1.0 at large tick"
    );

    // Encoder factory must succeed.
    let _enc = create_encoder(&config.encoder);

    // Sink factory must succeed.
    let _sink = create_sink(&config.sink)
        .unwrap_or_else(|e| panic!("sink factory failed for simple-constant.yaml: {e}"));
}

// ---------------------------------------------------------------------------
// Cross-file sanity: both examples produce valid configs that pass validation
// ---------------------------------------------------------------------------

#[test]
fn both_example_yamls_pass_full_round_trip() {
    for filename in &[
        "examples/basic-metrics.yaml",
        "examples/simple-constant.yaml",
    ] {
        let path = workspace_file(filename);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let config: ScenarioConfig = serde_yaml::from_str(&contents)
            .unwrap_or_else(|e| panic!("{filename} failed to deserialize: {e}"));
        validate_config(&config).unwrap_or_else(|e| panic!("{filename} failed validation: {e}"));
        let gen = create_generator(&config.generator, config.rate).expect("generator factory");
        // Generator must produce a finite value at tick 0.
        let v = gen.value(0);
        assert!(
            v.is_finite(),
            "{filename}: generator.value(0) must be finite, got {v}"
        );
        let _enc = create_encoder(&config.encoder);
        let _sink = create_sink(&config.sink)
            .unwrap_or_else(|e| panic!("{filename}: sink factory failed: {e}"));
    }
}
