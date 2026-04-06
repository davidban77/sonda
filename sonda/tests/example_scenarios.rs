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
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("basic-metrics.yaml failed to deserialize: {e}"));
}

#[test]
fn basic_metrics_yaml_has_correct_metric_name() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
    assert_eq!(config.rate, 1000.0, "rate must be 1000 events/sec");
}

#[test]
fn basic_metrics_yaml_has_correct_duration() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("basic-metrics.yaml failed validation: {e}"));
}

#[test]
fn basic_metrics_yaml_factories_all_succeed() {
    let path = workspace_file("examples/basic-metrics.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize basic-metrics.yaml");

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
    let _sink = create_sink(&config.sink, None)
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
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("simple-constant.yaml failed to deserialize: {e}"));
}

#[test]
fn simple_constant_yaml_has_correct_metric_name() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert_eq!(config.name, "up", "metric name must be 'up'");
}

#[test]
fn simple_constant_yaml_has_correct_rate() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
    assert_eq!(config.rate, 10.0, "rate must be 10 events/sec");
}

#[test]
fn simple_constant_yaml_has_correct_duration() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
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
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("simple-constant.yaml failed validation: {e}"));
}

#[test]
fn simple_constant_yaml_factories_all_succeed() {
    let path = workspace_file("examples/simple-constant.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize simple-constant.yaml");

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
    let _sink = create_sink(&config.sink, None)
        .unwrap_or_else(|e| panic!("sink factory failed for simple-constant.yaml: {e}"));
}

// ---------------------------------------------------------------------------
// examples/cardinality-spike.yaml
// ---------------------------------------------------------------------------

#[test]
fn cardinality_spike_yaml_deserializes_without_error() {
    let path = workspace_file("examples/cardinality-spike.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("cardinality-spike.yaml failed to deserialize: {e}"));
}

#[test]
fn cardinality_spike_yaml_has_correct_metric_name() {
    let path = workspace_file("examples/cardinality-spike.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize cardinality-spike.yaml");
    assert_eq!(
        config.name, "cardinality_spike_demo",
        "metric name must match"
    );
}

#[test]
fn cardinality_spike_yaml_has_spike_config() {
    let path = workspace_file("examples/cardinality-spike.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize cardinality-spike.yaml");
    let spikes = config
        .cardinality_spikes
        .as_ref()
        .expect("cardinality_spikes must be present");
    assert_eq!(spikes.len(), 1, "must have exactly one spike entry");
    assert_eq!(spikes[0].label, "pod_name");
    assert_eq!(spikes[0].cardinality, 100);
}

#[test]
fn cardinality_spike_yaml_passes_validate_config() {
    let path = workspace_file("examples/cardinality-spike.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize cardinality-spike.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("cardinality-spike.yaml failed validation: {e}"));
}

#[test]
fn cardinality_spike_yaml_factories_all_succeed() {
    let path = workspace_file("examples/cardinality-spike.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize cardinality-spike.yaml");

    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    let value = gen.value(0);
    assert!(
        value.is_finite(),
        "generator.value(0) must be finite, got {value}"
    );

    let _enc = create_encoder(&config.encoder);
    let _sink = create_sink(&config.sink, None)
        .unwrap_or_else(|e| panic!("sink factory failed for cardinality-spike.yaml: {e}"));
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-fleet.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_fleet_yaml_deserializes_without_error() {
    let path = workspace_file("examples/dynamic-labels-fleet.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("dynamic-labels-fleet.yaml failed to deserialize: {e}"));
}

#[test]
fn dynamic_labels_fleet_yaml_passes_validate_config() {
    let path = workspace_file("examples/dynamic-labels-fleet.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-fleet.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("dynamic-labels-fleet.yaml failed validation: {e}"));
}

#[test]
fn dynamic_labels_fleet_yaml_factories_all_succeed() {
    let path = workspace_file("examples/dynamic-labels-fleet.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-fleet.yaml");

    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    let v = gen.value(0);
    assert!(v.is_finite(), "generator.value(0) must be finite, got {v}");

    let _enc = create_encoder(&config.encoder);
    let _sink = create_sink(&config.sink, None)
        .unwrap_or_else(|e| panic!("sink factory failed for dynamic-labels-fleet.yaml: {e}"));
}

#[test]
fn dynamic_labels_fleet_yaml_has_dynamic_labels() {
    let path = workspace_file("examples/dynamic-labels-fleet.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-fleet.yaml");
    let dls = config
        .dynamic_labels
        .as_ref()
        .expect("dynamic_labels must be present");
    assert_eq!(dls.len(), 1, "must have exactly one dynamic label");
    assert_eq!(dls[0].key, "hostname");
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-regions.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_regions_yaml_deserializes_without_error() {
    let path = workspace_file("examples/dynamic-labels-regions.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("dynamic-labels-regions.yaml failed to deserialize: {e}"));
}

#[test]
fn dynamic_labels_regions_yaml_passes_validate_config() {
    let path = workspace_file("examples/dynamic-labels-regions.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-regions.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("dynamic-labels-regions.yaml failed validation: {e}"));
}

#[test]
fn dynamic_labels_regions_yaml_factories_all_succeed() {
    let path = workspace_file("examples/dynamic-labels-regions.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-regions.yaml");

    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    let v = gen.value(0);
    assert!(v.is_finite(), "generator.value(0) must be finite, got {v}");

    let _enc = create_encoder(&config.encoder);
    let _sink = create_sink(&config.sink, None)
        .unwrap_or_else(|e| panic!("sink factory failed for dynamic-labels-regions.yaml: {e}"));
}

#[test]
fn dynamic_labels_regions_yaml_has_values_list_strategy() {
    use sonda_core::config::DynamicLabelStrategy;
    let path = workspace_file("examples/dynamic-labels-regions.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-regions.yaml");
    let dls = config
        .dynamic_labels
        .as_ref()
        .expect("dynamic_labels must be present");
    assert_eq!(dls.len(), 1, "must have exactly one dynamic label");
    assert_eq!(dls[0].key, "region");
    match &dls[0].strategy {
        DynamicLabelStrategy::ValuesList { values } => {
            assert_eq!(values.len(), 3, "must have 3 region values");
        }
        other => panic!("expected ValuesList strategy, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-multi.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_multi_yaml_deserializes_without_error() {
    let path = workspace_file("examples/dynamic-labels-multi.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("dynamic-labels-multi.yaml failed to deserialize: {e}"));
}

#[test]
fn dynamic_labels_multi_yaml_passes_validate_config() {
    let path = workspace_file("examples/dynamic-labels-multi.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-multi.yaml");
    validate_config(&config)
        .unwrap_or_else(|e| panic!("dynamic-labels-multi.yaml failed validation: {e}"));
}

#[test]
fn dynamic_labels_multi_yaml_factories_all_succeed() {
    let path = workspace_file("examples/dynamic-labels-multi.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-multi.yaml");

    let gen = create_generator(&config.generator, config.rate).expect("generator factory");
    let v = gen.value(0);
    assert!(v.is_finite(), "generator.value(0) must be finite, got {v}");

    let _enc = create_encoder(&config.encoder);
    let _sink = create_sink(&config.sink, None)
        .unwrap_or_else(|e| panic!("sink factory failed for dynamic-labels-multi.yaml: {e}"));
}

#[test]
fn dynamic_labels_multi_yaml_has_two_dynamic_labels() {
    let path = workspace_file("examples/dynamic-labels-multi.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize dynamic-labels-multi.yaml");
    let dls = config
        .dynamic_labels
        .as_ref()
        .expect("dynamic_labels must be present");
    assert_eq!(dls.len(), 2, "must have two dynamic labels");
    assert_eq!(dls[0].key, "hostname");
    assert_eq!(dls[1].key, "region");
}

// ---------------------------------------------------------------------------
// Cross-file sanity: all examples produce valid configs that pass validation
// ---------------------------------------------------------------------------

#[test]
fn all_example_yamls_pass_full_round_trip() {
    for filename in &[
        "examples/basic-metrics.yaml",
        "examples/simple-constant.yaml",
        "examples/cardinality-spike.yaml",
        "examples/dynamic-labels-fleet.yaml",
        "examples/dynamic-labels-regions.yaml",
        "examples/dynamic-labels-multi.yaml",
    ] {
        let path = workspace_file(filename);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let config: ScenarioConfig = serde_yaml_ng::from_str(&contents)
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
        let _sink = create_sink(&config.sink, None)
            .unwrap_or_else(|e| panic!("{filename}: sink factory failed: {e}"));
    }
}

// ---------------------------------------------------------------------------
// examples/csv-replay-grafana-auto.yaml
// ---------------------------------------------------------------------------

#[test]
fn csv_replay_grafana_auto_yaml_deserializes_without_error() {
    let path = workspace_file("examples/csv-replay-grafana-auto.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let config: ScenarioConfig = serde_yaml_ng::from_str(&contents)
        .unwrap_or_else(|e| panic!("csv-replay-grafana-auto.yaml failed to deserialize: {e}"));
    match &config.generator {
        GeneratorConfig::CsvReplay { columns, .. } => {
            assert!(
                columns.is_none(),
                "columns should be None for auto-discovery"
            );
        }
        other => panic!("expected CsvReplay variant, got {other:?}"),
    }
}

#[test]
fn csv_replay_grafana_auto_yaml_expands_to_two_scenarios() {
    use sonda_core::expand_scenario;

    let path = workspace_file("examples/csv-replay-grafana-auto.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let mut config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize csv-replay-grafana-auto.yaml");
    // Patch the relative CSV file path to an absolute path so the test works
    // regardless of the working directory.
    if let GeneratorConfig::CsvReplay { ref mut file, .. } = config.generator {
        *file = workspace_file("examples/grafana-export.csv")
            .to_string_lossy()
            .into_owned();
    }
    let expanded = expand_scenario(config).expect("expand must succeed");

    assert_eq!(expanded.len(), 2, "Grafana export has 2 data columns");

    // Both columns should have metric name "up".
    assert_eq!(expanded[0].name, "up");
    assert_eq!(expanded[1].name, "up");

    // First column should have instance=localhost:9090, job=prometheus.
    let labels0 = expanded[0].labels.as_ref().expect("labels must exist");
    assert_eq!(
        labels0.get("instance").map(|s| s.as_str()),
        Some("localhost:9090")
    );
    assert_eq!(labels0.get("job").map(|s| s.as_str()), Some("prometheus"));
    // Plus scenario-level env=production.
    assert_eq!(labels0.get("env").map(|s| s.as_str()), Some("production"));

    // Second column should have instance=localhost:9100, job=node.
    let labels1 = expanded[1].labels.as_ref().expect("labels must exist");
    assert_eq!(
        labels1.get("instance").map(|s| s.as_str()),
        Some("localhost:9100")
    );
    assert_eq!(labels1.get("job").map(|s| s.as_str()), Some("node"));
    assert_eq!(labels1.get("env").map(|s| s.as_str()), Some("production"));

    // Expanded configs should produce working generators.
    for (i, child) in expanded.iter().enumerate() {
        let gen = create_generator(&child.generator, child.rate)
            .unwrap_or_else(|e| panic!("generator factory failed for expanded[{i}]: {e}"));
        let v = gen.value(0);
        assert!(v.is_finite(), "expanded[{i}].value(0) must be finite");
    }
}

// ---------------------------------------------------------------------------
// examples/csv-replay-explicit-labels.yaml
// ---------------------------------------------------------------------------

#[test]
fn csv_replay_explicit_labels_yaml_deserializes_without_error() {
    let path = workspace_file("examples/csv-replay-explicit-labels.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml_ng::from_str::<ScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("csv-replay-explicit-labels.yaml failed to deserialize: {e}"));
}

#[test]
fn csv_replay_explicit_labels_yaml_expands_with_per_column_labels() {
    use sonda_core::expand_scenario;

    let path = workspace_file("examples/csv-replay-explicit-labels.yaml");
    let contents = std::fs::read_to_string(&path).expect("read file");
    let mut config: ScenarioConfig =
        serde_yaml_ng::from_str(&contents).expect("deserialize csv-replay-explicit-labels.yaml");
    // Patch the relative CSV file path to an absolute path.
    if let GeneratorConfig::CsvReplay { ref mut file, .. } = config.generator {
        *file = workspace_file("examples/sample-multi-column.csv")
            .to_string_lossy()
            .into_owned();
    }
    let expanded = expand_scenario(config).expect("expand must succeed");

    assert_eq!(expanded.len(), 3, "should expand to 3 columns");
    assert_eq!(expanded[0].name, "cpu_percent");
    assert_eq!(expanded[1].name, "mem_percent");
    assert_eq!(expanded[2].name, "disk_io_mbps");

    // Column 0 (cpu_percent) should have core=0 plus instance and job.
    let labels0 = expanded[0].labels.as_ref().expect("labels must exist");
    assert_eq!(labels0.get("core").map(|s| s.as_str()), Some("0"));
    assert_eq!(
        labels0.get("instance").map(|s| s.as_str()),
        Some("prod-server-42")
    );

    // Column 1 (mem_percent) should have type=physical plus instance and job.
    let labels1 = expanded[1].labels.as_ref().expect("labels must exist");
    assert_eq!(labels1.get("type").map(|s| s.as_str()), Some("physical"));

    // Column 2 (disk_io_mbps) should have only scenario-level labels.
    let labels2 = expanded[2].labels.as_ref().expect("labels must exist");
    assert!(labels2.get("core").is_none());
    assert!(labels2.get("type").is_none());
    assert_eq!(
        labels2.get("instance").map(|s| s.as_str()),
        Some("prod-server-42")
    );

    // Expanded configs should produce working generators.
    for (i, child) in expanded.iter().enumerate() {
        let gen = create_generator(&child.generator, child.rate)
            .unwrap_or_else(|e| panic!("generator factory failed for expanded[{i}]: {e}"));
        let v = gen.value(0);
        assert!(v.is_finite(), "expanded[{i}].value(0) must be finite");
    }
}
