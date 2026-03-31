//! Integration tests for slice 2.5 — CLI Logs subcommand (example YAMLs).
//!
//! Test criteria from the spec:
//! 1. Config from YAML: log-template.yaml → valid `LogScenarioConfig`.
//! 2. The log runner integration test (MemorySink, rate=10, duration=1s) lives in
//!    sonda-core's log_runner tests.
//!
//! This file validates the example YAML scenario files shipped with the project,
//! and exercises the full factory stack (generator + encoder + sink) end-to-end.

use std::path::PathBuf;

use sonda_core::config::LogScenarioConfig;
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{create_log_generator, LogGeneratorConfig};
use sonda_core::sink::{create_sink, SinkConfig};

/// Return an absolute path to a file under the workspace root.
///
/// `CARGO_MANIFEST_DIR` for the `sonda` crate points to `<workspace>/sonda`.
/// One `parent()` step takes us to the workspace root.
fn workspace_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent directory (workspace root)")
        .join(relative)
}

// ---------------------------------------------------------------------------
// examples/log-template.yaml
// ---------------------------------------------------------------------------

#[test]
fn log_template_yaml_deserializes_without_error() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml::from_str::<LogScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("log-template.yaml failed to deserialize: {e}"));
}

#[test]
fn log_template_yaml_has_correct_name() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    assert_eq!(
        config.name, "app_logs_template",
        "name must be app_logs_template"
    );
}

#[test]
fn log_template_yaml_has_correct_rate() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    assert_eq!(config.rate, 10.0, "rate must be 10");
}

#[test]
fn log_template_yaml_has_correct_duration() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    assert_eq!(
        config.duration.as_deref(),
        Some("60s"),
        "duration must be 60s"
    );
}

#[test]
fn log_template_yaml_has_template_generator_with_seed_42() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    match &config.generator {
        LogGeneratorConfig::Template {
            templates,
            severity_weights,
            seed,
        } => {
            assert!(!templates.is_empty(), "templates must not be empty");
            assert!(
                severity_weights.is_some(),
                "severity_weights must be present in log-template.yaml"
            );
            assert_eq!(*seed, Some(42), "seed must be 42 per the example file");
        }
        other => panic!("expected Template generator, got {other:?}"),
    }
}

#[test]
fn log_template_yaml_has_json_lines_encoder() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    assert!(
        matches!(config.encoder, EncoderConfig::JsonLines { .. }),
        "encoder must be json_lines"
    );
}

#[test]
fn log_template_yaml_has_stdout_sink() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

#[test]
fn log_template_yaml_generator_factory_succeeds_and_resolves_placeholders() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    let gen = create_log_generator(&config.generator)
        .expect("log template generator factory must succeed");
    // Seeded generator must produce deterministic events with no unresolved placeholders.
    for tick in 0..5 {
        let event = gen.generate(tick);
        assert!(
            !event.message.contains('{'),
            "tick {tick}: generated message must have no unresolved placeholders: {:?}",
            event.message
        );
    }
}

#[test]
fn log_template_yaml_sink_factory_succeeds() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    let _sink =
        create_sink(&config.sink, None).expect("sink factory must succeed for log-template.yaml");
}

#[test]
fn log_template_yaml_generator_is_deterministic_for_same_tick() {
    let path = workspace_file("examples/log-template.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-template.yaml");
    let gen1 = create_log_generator(&config.generator).expect("factory must succeed");
    let gen2 = create_log_generator(&config.generator).expect("factory must succeed");
    // Same seed, same tick → same message.
    for tick in 0..10 {
        assert_eq!(
            gen1.generate(tick).message,
            gen2.generate(tick).message,
            "generator must be deterministic at tick {tick}"
        );
    }
}

// ---------------------------------------------------------------------------
// examples/log-replay.yaml
// ---------------------------------------------------------------------------

#[test]
fn log_replay_yaml_deserializes_without_error() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_yaml::from_str::<LogScenarioConfig>(&contents)
        .unwrap_or_else(|e| panic!("log-replay.yaml failed to deserialize: {e}"));
}

#[test]
fn log_replay_yaml_has_correct_name() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-replay.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-replay.yaml");
    assert_eq!(
        config.name, "app_logs_replay",
        "name must be app_logs_replay"
    );
}

#[test]
fn log_replay_yaml_has_correct_rate() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-replay.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-replay.yaml");
    assert_eq!(config.rate, 5.0, "rate must be 5");
}

#[test]
fn log_replay_yaml_has_replay_generator() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-replay.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-replay.yaml");
    match &config.generator {
        LogGeneratorConfig::Replay { file } => {
            assert!(!file.is_empty(), "replay file path must not be empty");
        }
        other => panic!("expected Replay generator, got {other:?}"),
    }
}

#[test]
fn log_replay_yaml_has_json_lines_encoder() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-replay.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-replay.yaml");
    assert!(
        matches!(config.encoder, EncoderConfig::JsonLines { .. }),
        "encoder must be json_lines"
    );
}

#[test]
fn log_replay_yaml_has_stdout_sink() {
    let path = workspace_file("examples/log-replay.yaml");
    let contents = std::fs::read_to_string(&path).expect("read log-replay.yaml");
    let config: LogScenarioConfig =
        serde_yaml::from_str(&contents).expect("deserialize log-replay.yaml");
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

// ---------------------------------------------------------------------------
// LogScenarioConfig YAML fixture: sonda/tests/fixtures
// ---------------------------------------------------------------------------

/// A log scenario YAML fixture stored alongside other test fixtures.
#[test]
fn log_scenario_fixture_template_mode_deserializes() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let config: LogScenarioConfig = serde_yaml::from_str(&contents)
        .unwrap_or_else(|e| panic!("log-template fixture failed to deserialize: {e}"));
    assert_eq!(config.rate, 10.0, "fixture rate must be 10");
    assert!(
        matches!(config.generator, LogGeneratorConfig::Template { .. }),
        "fixture generator must be Template"
    );
}
