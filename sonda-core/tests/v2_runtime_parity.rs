#![cfg(feature = "config")]
//! v2 runtime parity for built-in scenarios (validation matrix rows 16.1–16.11).
//!
//! Each row drives the same scenario through the v1 load path
//! (`serde_yaml_ng::from_str::<ScenarioConfig|MultiScenarioConfig|…>` — the
//! exact shape `sonda/src/config.rs::parse_builtin_scenario` constructs) and
//! through the v2 one-shot [`sonda_core::compile_scenario_file`]. Both sides
//! feed their `Vec<ScenarioEntry>` into
//! [`common::run_and_capture_stdout`], which drives the runtime scheduler
//! with an in-memory sink. Byte outputs are compared after timestamp
//! normalization.
//!
//! # Comparison modes
//!
//! - **Byte-equal** for single-signal scenarios: one entry → one runner
//!   thread → one byte stream with deterministic ordering.
//! - **Line-multiset** for multi-signal scenarios: concurrent runner
//!   threads interleave writes at byte granularity, so line order is
//!   nondeterministic even when every generator is seeded.
//!
//! Per the PR 6 plan, only `network-link-failure` and `interface-flap` use
//! line-multiset comparison; every other built-in emits from a single
//! scenario entry and collapses to a byte-equal assertion.

use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::{
    HistogramScenarioConfig, LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};

mod common;

use common::{
    assert_line_multisets_equal, normalize_timestamps, parity_fixture, run_and_capture_stdout,
};

use rstest::rstest;

/// How a parity row compares the v1 and v2 byte streams.
#[derive(Debug, Clone, Copy)]
enum Comparison {
    /// Exact byte-for-byte equality after timestamp normalization.
    ByteEqual,
    /// Multiset-of-lines equality after timestamp normalization.
    LineMultiset,
}

/// v1 signal-type discriminator — matches `BuiltinScenario.signal_type`.
///
/// The `Summary` variant is kept in lockstep with
/// [`sonda::BuiltinScenario`]'s supported set even though no built-in
/// scenario YAML currently uses it — the match in [`load_v1_entries`]
/// would be incomplete otherwise if a future scenario is added.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum V1Kind {
    Metrics,
    Logs,
    Histogram,
    Summary,
    Multi,
}

/// Load the v1 built-in scenario at `scenario_path` into a
/// `Vec<ScenarioEntry>` — mirrors the dispatch logic in
/// `sonda/src/config.rs::parse_builtin_scenario` without any CLI overrides.
fn load_v1_entries(scenario_path: &str, kind: V1Kind) -> Vec<ScenarioEntry> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has parent")
        .to_path_buf();
    let path = repo_root.join(scenario_path);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    match kind {
        V1Kind::Metrics => {
            let c: ScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{} parse failed: {e}", scenario_path));
            vec![ScenarioEntry::Metrics(c)]
        }
        V1Kind::Logs => {
            let c: LogScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{} parse failed: {e}", scenario_path));
            vec![ScenarioEntry::Logs(c)]
        }
        V1Kind::Histogram => {
            let c: HistogramScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{} parse failed: {e}", scenario_path));
            vec![ScenarioEntry::Histogram(c)]
        }
        V1Kind::Summary => {
            let c: SummaryScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{} parse failed: {e}", scenario_path));
            vec![ScenarioEntry::Summary(c)]
        }
        V1Kind::Multi => {
            let c: MultiScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{} parse failed: {e}", scenario_path));
            c.scenarios
        }
    }
}

/// Compile a v2 fixture at `tests/fixtures/v2-parity/{fixture}` into
/// `Vec<ScenarioEntry>` via the new one-shot.
fn load_v2_entries(fixture: &str) -> Vec<ScenarioEntry> {
    let yaml = parity_fixture(fixture);
    let resolver = InMemoryPackResolver::new();
    sonda_core::compile_scenario_file(&yaml, &resolver)
        .unwrap_or_else(|e| panic!("{fixture} v2 compile failed: {e}"))
}

/// Truncate every v1 entry's `duration` to `override_duration`. The v1
/// scenarios on disk are sized for live demos (30s–120s); that is far too
/// long for a parity test. Writing the override here lets us keep the v2
/// fixtures semantically aligned with their v1 counterparts on every field
/// **except** duration, which is adjusted on both sides to match.
///
/// The v2 fixtures already hard-code the short duration, so this override
/// only affects the v1 side.
fn override_duration(entries: &mut [ScenarioEntry], override_duration: &str) {
    for entry in entries {
        let base = match entry {
            ScenarioEntry::Metrics(c) => &mut c.base,
            ScenarioEntry::Logs(c) => &mut c.base,
            ScenarioEntry::Histogram(c) => &mut c.base,
            ScenarioEntry::Summary(c) => &mut c.base,
        };
        base.duration = Some(override_duration.to_string());
    }
}

/// Runtime parity driver: v1 YAML vs v2 fixture, byte-for-byte or
/// line-multiset depending on `comparison`.
///
/// Both sides are forced to a short, matching `duration` so tests stay
/// fast; every other semantic field (rate, generator, labels, seeds)
/// matches between the v1 built-in YAML and the hand-written v2 fixture.
#[rustfmt::skip]
#[rstest]
#[case::cpu_spike("scenarios/cpu-spike.yaml", V1Kind::Metrics, "cpu-spike.yaml", "1s", Comparison::ByteEqual)]
#[case::memory_leak("scenarios/memory-leak.yaml", V1Kind::Metrics, "memory-leak.yaml", "1s", Comparison::ByteEqual)]
#[case::disk_fill("scenarios/disk-fill.yaml", V1Kind::Metrics, "disk-fill.yaml", "1s", Comparison::ByteEqual)]
#[case::latency_degradation("scenarios/latency-degradation.yaml", V1Kind::Metrics, "latency-degradation.yaml", "1s", Comparison::ByteEqual)]
#[case::error_rate_spike("scenarios/error-rate-spike.yaml", V1Kind::Metrics, "error-rate-spike.yaml", "1s", Comparison::ByteEqual)]
#[case::interface_flap("scenarios/interface-flap.yaml", V1Kind::Multi, "interface-flap.yaml", "1s", Comparison::LineMultiset)]
#[case::network_link_failure("scenarios/network-link-failure.yaml", V1Kind::Multi, "network-link-failure.yaml", "1s", Comparison::LineMultiset)]
// `steady-state.yaml` has been migrated to v2 (PR 8a sub-slice 1) — the v1↔v2
// parity oracle no longer applies. See docs/refactor/adr-v2-catalog-metadata.md.
#[case::log_storm("scenarios/log-storm.yaml", V1Kind::Logs, "log-storm.yaml", "500ms", Comparison::ByteEqual)]
#[case::cardinality_explosion("scenarios/cardinality-explosion.yaml", V1Kind::Metrics, "cardinality-explosion.yaml", "500ms", Comparison::ByteEqual)]
#[case::histogram_latency("scenarios/histogram-latency.yaml", V1Kind::Histogram, "histogram-latency.yaml", "1s", Comparison::ByteEqual)]
fn v2_runtime_parity_for_builtin_scenario(
    #[case] v1_path: &str,
    #[case] v1_kind: V1Kind,
    #[case] v2_fixture: &str,
    #[case] duration: &str,
    #[case] comparison: Comparison,
) {
    let mut v1_entries = load_v1_entries(v1_path, v1_kind);
    override_duration(&mut v1_entries, duration);
    let v2_entries = load_v2_entries(v2_fixture);

    let v1_bytes = run_and_capture_stdout(v1_entries);
    let v2_bytes = run_and_capture_stdout(v2_entries);

    let v1 = normalize_timestamps(&v1_bytes);
    let v2 = normalize_timestamps(&v2_bytes);

    match comparison {
        Comparison::ByteEqual => {
            assert_eq!(
                v1, v2,
                "{v2_fixture}: v1 vs v2 byte streams differ\n\
                 --- v1 ---\n{}\n--- v2 ---\n{}",
                String::from_utf8_lossy(&v1),
                String::from_utf8_lossy(&v2),
            );
        }
        Comparison::LineMultiset => {
            assert_line_multisets_equal(v2_fixture, &v1, &v2);
        }
    }
}
