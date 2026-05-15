//! Integration tests for the log-flavoured example YAML files.
//!
//! Post-v1 retirement, every example under `examples/*.yaml` is a v2 scenario
//! file. These tests route each log-flavoured example through
//! `sonda_core::compile_scenario_file` and assert on the single compiled
//! [`ScenarioEntry::Logs`] entry (its rate, duration, generator shape,
//! encoder, sink, and dynamic labels).
//!
//! This file also carries the v2 fixture test for the log-template fixture
//! under `tests/fixtures/`.

use std::path::PathBuf;

use sonda_core::compile_scenario_file;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::{LogScenarioConfig, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{create_log_generator, LogGeneratorConfig};
use sonda_core::sink::SinkConfig;

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

/// Compile a v2 example YAML file into exactly one [`LogScenarioConfig`].
///
/// Every log-flavoured example in `examples/` carries a single `logs` entry,
/// so this helper panics if the compile produces a different count or signal
/// type — that's a test shape mismatch, not a legitimate pass.
fn compile_single_log_example(relative: &str) -> LogScenarioConfig {
    let path = workspace_file(relative);
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let resolver = InMemoryPackResolver::new();
    let entries = compile_scenario_file(&contents, &resolver)
        .unwrap_or_else(|e| panic!("{relative} failed v2 compile: {e}"));
    assert_eq!(
        entries.len(),
        1,
        "{relative} must compile to exactly one entry"
    );
    match entries.into_iter().next().unwrap() {
        ScenarioEntry::Logs(cfg) => cfg,
        other => panic!("{relative} must compile to a Logs entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/log-template.yaml
// ---------------------------------------------------------------------------

#[test]
fn log_template_yaml_compiles_to_template_log_entry() {
    let config = compile_single_log_example("examples/log-template.yaml");
    assert_eq!(
        config.name, "app_logs_template",
        "name must be app_logs_template"
    );
    assert_eq!(config.rate, 10.0, "rate must be 10");
    assert_eq!(
        config.duration.as_deref(),
        Some("60s"),
        "duration must be 60s"
    );

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

    assert!(
        matches!(config.encoder, EncoderConfig::JsonLines { .. }),
        "encoder must be json_lines"
    );
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

#[test]
fn log_template_yaml_generator_resolves_placeholders() {
    let config = compile_single_log_example("examples/log-template.yaml");
    let gen = create_log_generator(&config.generator)
        .expect("log template generator factory must succeed");
    // Seeded generator must produce deterministic events with no unresolved
    // placeholders for at least the first few ticks.
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
fn log_template_yaml_generator_is_deterministic_for_same_tick() {
    let config = compile_single_log_example("examples/log-template.yaml");
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
// examples/log-csv-replay.yaml
// ---------------------------------------------------------------------------

#[test]
fn log_csv_replay_yaml_compiles_to_csv_replay_log_entry() {
    let config = compile_single_log_example("examples/log-csv-replay.yaml");
    assert_eq!(
        config.name, "app_logs_csv_replay",
        "name must be app_logs_csv_replay"
    );

    match &config.generator {
        LogGeneratorConfig::CsvReplay { file, .. } => {
            assert!(!file.is_empty(), "csv_replay file path must not be empty");
        }
        other => panic!("expected CsvReplay generator, got {other:?}"),
    }

    assert!(
        matches!(config.encoder, EncoderConfig::JsonLines { .. }),
        "encoder must be json_lines"
    );
    assert!(
        matches!(config.sink, SinkConfig::Stdout),
        "sink must be stdout"
    );
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-logs.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_logs_yaml_compiles_with_pod_name_label() {
    let config = compile_single_log_example("examples/dynamic-labels-logs.yaml");
    assert_eq!(config.name, "app_logs", "name must be app_logs");

    let dls = config
        .base
        .dynamic_labels
        .as_ref()
        .expect("dynamic_labels must be present");
    assert_eq!(dls.len(), 1, "must have exactly one dynamic label");
    assert_eq!(dls[0].key, "pod_name");

    // Generator factory must produce a working template generator.
    let gen = create_log_generator(&config.generator)
        .expect("log generator factory must succeed for dynamic-labels-logs.yaml");
    let event = gen.generate(0);
    assert!(
        !event.message.is_empty(),
        "generated log message must not be empty"
    );
}

// ---------------------------------------------------------------------------
// LogScenarioConfig YAML fixture: sonda/tests/fixtures
// ---------------------------------------------------------------------------

/// A log scenario YAML fixture stored alongside other test fixtures.
///
/// Post-v1 removal the fixture is a v2 scenario; route it through
/// `compile_scenario_file` and assert on the single compiled entry rather
/// than serde-deserializing it as a v1 `LogScenarioConfig`.
#[test]
fn log_scenario_fixture_template_mode_compiles_via_v2() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-template.yaml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let resolver = InMemoryPackResolver::new();
    let entries = compile_scenario_file(&contents, &resolver)
        .unwrap_or_else(|e| panic!("log-template fixture failed to compile: {e}"));
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        ScenarioEntry::Logs(cfg) => {
            assert_eq!(cfg.rate, 10.0, "fixture rate must be 10");
            assert!(
                matches!(cfg.generator, LogGeneratorConfig::Template { .. }),
                "fixture generator must be Template"
            );
        }
        other => panic!("fixture must compile to a Logs entry, got: {other:?}"),
    }
}
