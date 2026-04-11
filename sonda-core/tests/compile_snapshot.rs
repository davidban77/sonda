//! Golden-file snapshot tests for the scenario compilation pipeline.
//!
//! Each test parses a semantic YAML fixture, runs it through the compilation
//! pipeline (expand, desugar, validate, resolve phase offsets), and compares the
//! JSON snapshot against a golden file in `tests/fixtures/semantic/expected/`.
//!
//! To create or update golden files after an intentional change:
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test -p sonda-core --test compile_snapshot
//! ```
//!
//! These tests are the safety net for every future refactor PR: if the compiled
//! output changes, the snapshot diff tells you exactly what shifted.

use std::path::PathBuf;

use sonda_core::config::snapshot::{
    assert_or_update_snapshot, snapshot_entries, snapshot_prepared_entries,
};
use sonda_core::config::{
    HistogramScenarioConfig, LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};
use sonda_core::schedule::launch::prepare_entries;

/// Return the path to the `tests/fixtures/semantic/` directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semantic")
}

/// Return the path to the golden file for a given fixture name.
fn golden_path(name: &str) -> PathBuf {
    fixtures_dir().join("expected").join(format!("{name}.json"))
}

/// Return the golden path for a prepared-entry snapshot.
fn golden_prepared_path(name: &str) -> PathBuf {
    fixtures_dir()
        .join("expected")
        .join(format!("{name}.prepared.json"))
}

/// Read a YAML fixture file from the semantic fixtures directory.
fn read_fixture(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

// ---------------------------------------------------------------------------
// Single metric: constant generator
// ---------------------------------------------------------------------------

#[test]
fn snapshot_single_metric_constant() {
    let yaml = read_fixture("single-metric-constant.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let snap = snapshot_entries(&entries);
    assert_or_update_snapshot(&snap, &golden_path("single-metric-constant"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_single_metric_constant_prepared() {
    let yaml = read_fixture("single-metric-constant.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("single-metric-constant"))
        .expect("prepared snapshot must match golden file");
}

// ---------------------------------------------------------------------------
// Single metric: sine generator with labels
// ---------------------------------------------------------------------------

#[test]
fn snapshot_single_metric_sine() {
    let yaml = read_fixture("single-metric-sine.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let snap = snapshot_entries(&entries);
    assert_or_update_snapshot(&snap, &golden_path("single-metric-sine"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_single_metric_sine_prepared() {
    let yaml = read_fixture("single-metric-sine.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("single-metric-sine"))
        .expect("prepared snapshot must match golden file");
}

// ---------------------------------------------------------------------------
// Single log: template generator
// ---------------------------------------------------------------------------

#[test]
fn snapshot_single_log_template() {
    let yaml = read_fixture("single-log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as LogScenarioConfig");
    let entries = vec![ScenarioEntry::Logs(config)];

    let snap = snapshot_entries(&entries);
    assert_or_update_snapshot(&snap, &golden_path("single-log-template"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_single_log_template_prepared() {
    let yaml = read_fixture("single-log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as LogScenarioConfig");
    let entries = vec![ScenarioEntry::Logs(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("single-log-template"))
        .expect("prepared snapshot must match golden file");
}

// ---------------------------------------------------------------------------
// Multi-scenario: three metrics with different generators
// ---------------------------------------------------------------------------

#[test]
fn snapshot_multi_scenario() {
    let yaml = read_fixture("multi-scenario.yaml");
    let multi: MultiScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as MultiScenarioConfig");

    let snap = snapshot_entries(&multi.scenarios);
    assert_or_update_snapshot(&snap, &golden_path("multi-scenario"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_multi_scenario_prepared() {
    let yaml = read_fixture("multi-scenario.yaml");
    let multi: MultiScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as MultiScenarioConfig");

    let prepared = prepare_entries(multi.scenarios).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("multi-scenario"))
        .expect("prepared snapshot must match golden file");
}

// ---------------------------------------------------------------------------
// Histogram: normal distribution with custom buckets
// ---------------------------------------------------------------------------

#[test]
fn snapshot_histogram_basic() {
    let yaml = read_fixture("histogram-basic.yaml");
    let config: HistogramScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as HistogramScenarioConfig");
    let entries = vec![ScenarioEntry::Histogram(config)];

    let snap = snapshot_entries(&entries);
    assert_or_update_snapshot(&snap, &golden_path("histogram-basic"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_histogram_basic_prepared() {
    let yaml = read_fixture("histogram-basic.yaml");
    let config: HistogramScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as HistogramScenarioConfig");
    let entries = vec![ScenarioEntry::Histogram(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("histogram-basic"))
        .expect("prepared snapshot must match golden file");
}

// ---------------------------------------------------------------------------
// Summary: exponential distribution with custom quantiles
// ---------------------------------------------------------------------------

#[test]
fn snapshot_summary_basic() {
    let yaml = read_fixture("summary-basic.yaml");
    let config: SummaryScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as SummaryScenarioConfig");
    let entries = vec![ScenarioEntry::Summary(config)];

    let snap = snapshot_entries(&entries);
    assert_or_update_snapshot(&snap, &golden_path("summary-basic"))
        .expect("snapshot must match golden file");
}

#[test]
fn snapshot_summary_basic_prepared() {
    let yaml = read_fixture("summary-basic.yaml");
    let config: SummaryScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as SummaryScenarioConfig");
    let entries = vec![ScenarioEntry::Summary(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    let snap = snapshot_prepared_entries(&prepared);
    assert_or_update_snapshot(&snap, &golden_prepared_path("summary-basic"))
        .expect("prepared snapshot must match golden file");
}
