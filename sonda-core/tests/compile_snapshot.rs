#![cfg(feature = "config")]
//! Insta snapshot tests for the scenario compilation pipeline.
//!
//! Each test parses a semantic YAML fixture under `tests/fixtures/semantic/`,
//! runs it through the compilation pipeline (expand, desugar, validate, resolve
//! phase offsets), and captures the JSON shape as an [`insta`] snapshot under
//! `tests/snapshots/`.
//!
//! To regenerate the snapshots after an intentional change:
//!
//! ```sh
//! INSTA_UPDATE=always cargo test -p sonda-core --test compile_snapshot
//! # or
//! cargo insta accept
//! ```
//!
//! These tests are the safety net for every future refactor PR: if the
//! compiled output changes, the snapshot diff tells you exactly what shifted.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use sonda_core::config::{
    HistogramScenarioConfig, LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};
use sonda_core::schedule::launch::{prepare_entries, PreparedEntry};

/// Return the path to the `tests/fixtures/semantic/` directory.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semantic")
}

/// Read a YAML fixture file from the semantic fixtures directory.
fn read_fixture(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

/// View of a [`PreparedEntry`] that flattens the resolved `start_delay` into
/// a `start_delay_ms: Option<u64>` field next to the scenario entry payload.
///
/// Kept local to this test file because the on-disk shape is only meaningful
/// for snapshot comparison; the production runtime consumes
/// `PreparedEntry::start_delay` as a `Duration` directly.
#[derive(Serialize)]
struct PreparedView<'a> {
    #[serde(flatten)]
    entry: &'a ScenarioEntry,
    start_delay_ms: Option<u64>,
}

impl<'a> From<&'a PreparedEntry> for PreparedView<'a> {
    fn from(p: &'a PreparedEntry) -> Self {
        Self {
            entry: &p.entry,
            start_delay_ms: p.start_delay.map(|d: Duration| d.as_millis() as u64),
        }
    }
}

/// Build the serializable view for a slice of prepared entries.
fn prepared_view(prepared: &[PreparedEntry]) -> Vec<PreparedView<'_>> {
    prepared.iter().map(PreparedView::from).collect()
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

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(entries);
    });
}

#[test]
fn snapshot_single_metric_constant_prepared() {
    let yaml = read_fixture("single-metric-constant.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
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

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(entries);
    });
}

#[test]
fn snapshot_single_metric_sine_prepared() {
    let yaml = read_fixture("single-metric-sine.yaml");
    let config: ScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as ScenarioConfig");
    let entries = vec![ScenarioEntry::Metrics(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
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

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(entries);
    });
}

#[test]
fn snapshot_single_log_template_prepared() {
    let yaml = read_fixture("single-log-template.yaml");
    let config: LogScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as LogScenarioConfig");
    let entries = vec![ScenarioEntry::Logs(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
}

// ---------------------------------------------------------------------------
// Multi-scenario: three metrics with different generators
// ---------------------------------------------------------------------------

#[test]
fn snapshot_multi_scenario() {
    let yaml = read_fixture("multi-scenario.yaml");
    let multi: MultiScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as MultiScenarioConfig");

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(multi.scenarios);
    });
}

#[test]
fn snapshot_multi_scenario_prepared() {
    let yaml = read_fixture("multi-scenario.yaml");
    let multi: MultiScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as MultiScenarioConfig");

    let prepared = prepare_entries(multi.scenarios).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
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

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(entries);
    });
}

#[test]
fn snapshot_histogram_basic_prepared() {
    let yaml = read_fixture("histogram-basic.yaml");
    let config: HistogramScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as HistogramScenarioConfig");
    let entries = vec![ScenarioEntry::Histogram(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
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

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(entries);
    });
}

#[test]
fn snapshot_summary_basic_prepared() {
    let yaml = read_fixture("summary-basic.yaml");
    let config: SummaryScenarioConfig =
        serde_yaml_ng::from_str(&yaml).expect("fixture must parse as SummaryScenarioConfig");
    let entries = vec![ScenarioEntry::Summary(config)];

    let prepared = prepare_entries(entries).expect("preparation must succeed");
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!(prepared_view(&prepared));
    });
}
