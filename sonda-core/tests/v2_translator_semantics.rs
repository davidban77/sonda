#![cfg(feature = "config")]
//! Translator semantic tests for v2 fields not naturally covered by the
//! runtime parity suite.
//!
//! Each test compiles a hand-written v2 YAML through
//! [`sonda_core::compile_scenario_file`] and asserts the resulting
//! [`ScenarioEntry`] shape matches a v1-equivalent reference. These are
//! **compile-time** assertions on the translator — fast and deterministic
//! — for matrix rows where the cost of running the full scheduler is not
//! worth the additional coverage over the translator's output shape.
//!
//! Rows covered:
//!
//! - 1.6 — mixed signal types in one file (metrics + logs + histogram).
//! - 2.9 — csv_replay with auto column discovery (multi-column fan-out).
//! - 2.10 — csv_replay with per-column labels.
//! - 4.1–4.8 — summary signal type (distribution, quantiles, seed, drift).
//!   Distribution coverage is parameterized across Exponential / Normal /
//!   Uniform via rstest cases on the same fixture shape.
//! - 4.4 — histogram entry with custom `buckets:` list (translator-level
//!   assertion that the runtime parity suite's default-bucket path
//!   complements).
//! - 5.2 — influx_lp encoder with explicit `field_key`.
//! - 5.7 — `precision` field on text encoders.
//! - 6.12 — retry/backoff config passes through a TCP sink.
//! - 7.1 — recurring gap window on a metrics scenario.
//! - 7.2 — recurring burst window on a logs scenario.
//! - 7.3 — gap + burst overlapping on the same scenario (gap takes
//!   priority at runtime; this test asserts both fields carry through).

use rstest::rstest;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::{DistributionConfig, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig};
use sonda_core::sink::SinkConfig;

mod common;

use common::parity_fixture;

/// Compile a v2 YAML fixture and return the translated `Vec<ScenarioEntry>`.
fn compile(fixture: &str) -> Vec<ScenarioEntry> {
    let yaml = parity_fixture(fixture);
    let resolver = InMemoryPackResolver::new();
    sonda_core::compile_scenario_file(&yaml, &resolver)
        .unwrap_or_else(|e| panic!("{fixture} v2 compile failed: {e}"))
}

// =============================================================================
// 1.6 — mixed signal types in one file
// =============================================================================

/// A single v2 file can carry metrics, logs, and histogram entries — each
/// flows through the translator into its matching `ScenarioEntry` variant.
#[test]
fn row_1_6_mixed_signal_types_translate_to_matching_variants() {
    let entries = compile("mixed-signal-types.yaml");
    assert_eq!(entries.len(), 3);
    assert!(matches!(entries[0], ScenarioEntry::Metrics(_)));
    assert!(matches!(entries[1], ScenarioEntry::Logs(_)));
    assert!(matches!(entries[2], ScenarioEntry::Histogram(_)));
}

// =============================================================================
// 2.9 — csv_replay auto column discovery (multi-column fan-out)
// =============================================================================

/// A csv_replay entry with a `columns:` list expands via `prepare_entries`
/// into one `ScenarioEntry` per column. The translator itself carries the
/// single pre-fan-out entry through; the fan-out happens in
/// `prepare_entries` which is out of scope here — this test proves the
/// translator preserves the csv_replay columns shape end-to-end.
#[test]
fn row_2_9_csv_replay_per_column_carries_through() {
    let entries = compile("csv-replay-columns.yaml");
    assert_eq!(
        entries.len(),
        1,
        "csv_replay stays a single entry pre-expand"
    );
    match &entries[0] {
        ScenarioEntry::Metrics(c) => match &c.generator {
            GeneratorConfig::CsvReplay {
                columns: Some(cols),
                ..
            } => {
                let names: Vec<&str> = cols.iter().map(|s| s.name.as_str()).collect();
                assert_eq!(names, vec!["cpu_usage", "mem_usage"]);
            }
            other => panic!("expected CsvReplay with columns, got {other:?}"),
        },
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 2.10 — csv_replay per-column labels carry through
// =============================================================================

/// Per-column `labels:` inside a csv_replay column spec survive the
/// translator unchanged.
#[test]
fn row_2_10_csv_replay_per_column_labels_carry_through() {
    let entries = compile("csv-replay-columns.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => match &c.generator {
            GeneratorConfig::CsvReplay {
                columns: Some(cols),
                ..
            } => {
                let first = &cols[0];
                assert_eq!(first.name, "cpu_usage");
                let labels = first
                    .labels
                    .as_ref()
                    .expect("per-column labels must carry through");
                assert_eq!(labels.get("kind").map(String::as_str), Some("cpu"));
            }
            _ => panic!("expected CsvReplay with columns"),
        },
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 4.1–4.8 — summary signal type
// =============================================================================

/// Inline-build a minimal summary v2 YAML around the supplied
/// `distribution:` block so the rstest cases below can swap the
/// distribution variant without authoring a fixture file per case.
fn compile_summary_with_distribution(distribution_yaml: &str) -> Vec<ScenarioEntry> {
    let yaml = format!(
        "version: 2\n\n\
         scenarios:\n\
         \x20 - signal_type: summary\n\
         \x20   name: rpc_duration_seconds\n\
         \x20   rate: 1\n\
         \x20   duration: 1s\n\
         \x20   distribution:\n{distribution_yaml}\n\
         \x20   quantiles: [0.5, 0.9, 0.95, 0.99]\n\
         \x20   observations_per_tick: 100\n\
         \x20   mean_shift_per_sec: 0.001\n\
         \x20   seed: 42\n\
         \x20   labels:\n\
         \x20     service: rpc\n\
         \x20   encoder:\n\
         \x20     type: prometheus_text\n\
         \x20   sink:\n\
         \x20     type: stdout\n",
    );
    let resolver = InMemoryPackResolver::new();
    sonda_core::compile_scenario_file(&yaml, &resolver)
        .unwrap_or_else(|e| panic!("inline summary v2 compile failed: {e}"))
}

/// A summary entry translates with distribution, quantiles, seed,
/// observations_per_tick, and mean_shift_per_sec all preserved across
/// every supported [`DistributionConfig`] variant — exercising rows 4.1
/// (Exponential), 4.2 (Normal), and 4.3 (Uniform) together.
#[rustfmt::skip]
#[rstest]
#[case::exponential(
    "        type: exponential\n        rate: 10.0",
    DistributionConfig::Exponential { rate: 10.0 },
)]
#[case::normal(
    "        type: normal\n        mean: 0.1\n        stddev: 0.02",
    DistributionConfig::Normal { mean: 0.1, stddev: 0.02 },
)]
#[case::uniform(
    "        type: uniform\n        min: 0.05\n        max: 0.25",
    DistributionConfig::Uniform { min: 0.05, max: 0.25 },
)]
fn row_4_1_to_4_8_summary_signal_fields_carry_through(
    #[case] distribution_yaml: &str,
    #[case] expected: DistributionConfig,
) {
    let entries = compile_summary_with_distribution(distribution_yaml);
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        ScenarioEntry::Summary(c) => {
            assert_eq!(c.base.name, "rpc_duration_seconds");
            assert_eq!(c.base.rate, 1.0);
            assert_eq!(
                c.quantiles.as_deref(),
                Some(&[0.5, 0.9, 0.95, 0.99][..]),
                "quantiles must carry through"
            );
            assert_eq!(c.observations_per_tick, Some(100u64));
            assert_eq!(c.seed, Some(42));
            assert!(c.mean_shift_per_sec.is_some());
            assert_distribution_eq(&c.distribution, &expected);
        }
        other => panic!("expected Summary, got {other:?}"),
    }
}

/// Compare two [`DistributionConfig`] values by variant + parameters.
/// `DistributionConfig` does not derive `PartialEq` (its `f64` fields
/// have NaN semantics), so this helper does the field-level compare each
/// rstest case needs.
fn assert_distribution_eq(actual: &DistributionConfig, expected: &DistributionConfig) {
    match (actual, expected) {
        (
            DistributionConfig::Exponential { rate: a },
            DistributionConfig::Exponential { rate: b },
        ) => assert!((a - b).abs() < f64::EPSILON, "rate {a} != {b}"),
        (
            DistributionConfig::Normal {
                mean: am,
                stddev: as_,
            },
            DistributionConfig::Normal {
                mean: bm,
                stddev: bs,
            },
        ) => {
            assert!((am - bm).abs() < f64::EPSILON, "mean {am} != {bm}");
            assert!((as_ - bs).abs() < f64::EPSILON, "stddev {as_} != {bs}");
        }
        (
            DistributionConfig::Uniform { min: a1, max: a2 },
            DistributionConfig::Uniform { min: b1, max: b2 },
        ) => {
            assert!((a1 - b1).abs() < f64::EPSILON, "min {a1} != {b1}");
            assert!((a2 - b2).abs() < f64::EPSILON, "max {a2} != {b2}");
        }
        _ => panic!("distribution variant mismatch: {actual:?} vs {expected:?}"),
    }
}

/// Row 4.4 — a histogram entry's custom `buckets:` list threads through
/// the v2 translator into [`HistogramScenarioConfig::buckets`]
/// unchanged. The runtime parity suite (`v2_runtime_parity::case_11_histogram_latency`)
/// covers the default-bucket path; this is the lightweight translator-
/// level assertion for the explicit-bucket path.
#[test]
fn row_4_4_histogram_custom_buckets_carry_through() {
    let yaml = "version: 2\n\n\
                scenarios:\n\
                \x20 - signal_type: histogram\n\
                \x20   name: http_request_duration_seconds\n\
                \x20   rate: 1\n\
                \x20   duration: 1s\n\
                \x20   buckets: [0.01, 0.1, 1.0, 10.0]\n\
                \x20   distribution:\n\
                \x20     type: normal\n\
                \x20     mean: 0.1\n\
                \x20     stddev: 0.03\n\
                \x20   observations_per_tick: 50\n\
                \x20   seed: 1\n\
                \x20   encoder:\n\
                \x20     type: prometheus_text\n\
                \x20   sink:\n\
                \x20     type: stdout\n";
    let resolver = InMemoryPackResolver::new();
    let entries = sonda_core::compile_scenario_file(yaml, &resolver)
        .unwrap_or_else(|e| panic!("inline histogram v2 compile failed: {e}"));
    match &entries[0] {
        ScenarioEntry::Histogram(c) => {
            let buckets = c
                .buckets
                .as_deref()
                .expect("custom buckets must carry through");
            assert_eq!(buckets, &[0.01, 0.1, 1.0, 10.0][..]);
        }
        other => panic!("expected Histogram, got {other:?}"),
    }
}

// =============================================================================
// 5.2 — influx_lp with explicit field_key
// =============================================================================

/// The `field_key` field on the influx_lp encoder survives the translator
/// unchanged.
#[test]
fn row_5_2_influx_lp_field_key_carries_through() {
    let entries = compile("influx-field-key.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => match &c.encoder {
            EncoderConfig::InfluxLineProtocol {
                field_key,
                precision: _,
            } => {
                assert_eq!(
                    field_key.as_deref(),
                    Some("v"),
                    "field_key must carry through verbatim"
                );
            }
            other => panic!("expected InfluxLineProtocol encoder, got {other:?}"),
        },
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 5.7 — `precision` on text encoders
// =============================================================================

/// `precision: 3` on a prometheus_text encoder survives the translator.
#[test]
fn row_5_7_encoder_precision_carries_through() {
    let entries = compile("encoder-precision.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => match &c.encoder {
            EncoderConfig::PrometheusText { precision } => {
                assert_eq!(*precision, Some(3));
            }
            other => panic!("expected PrometheusText, got {other:?}"),
        },
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 6.12 — retry/backoff config on a TCP sink
// =============================================================================

/// A TCP sink's `retry` block (max_attempts, initial_backoff, max_backoff)
/// survives the translator unchanged.
#[test]
fn row_6_12_retry_backoff_on_tcp_sink_carries_through() {
    let entries = compile("tcp-retry.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => match &c.base.sink {
            SinkConfig::Tcp {
                address,
                retry: Some(retry),
            } => {
                assert_eq!(address, "127.0.0.1:9999");
                assert_eq!(retry.max_attempts, 5);
                assert_eq!(retry.initial_backoff, "100ms");
                assert_eq!(retry.max_backoff, "5s");
            }
            other => panic!("expected TCP sink with retry, got {other:?}"),
        },
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 7.1 — recurring gap window on a metrics scenario
// =============================================================================

/// A scenario-level `gaps:` window (every, for) survives the translator.
#[test]
fn row_7_1_gap_window_carries_through() {
    let entries = compile("gap-only.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => {
            let gaps = c.base.gaps.as_ref().expect("gaps must carry through");
            assert_eq!(gaps.every, "30s");
            assert_eq!(gaps.r#for, "5s");
        }
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// 7.2 — recurring burst window on a logs scenario
// =============================================================================

/// A scenario-level `bursts:` window (every, for, multiplier) survives
/// the translator.
#[test]
fn row_7_2_burst_window_carries_through() {
    let entries = compile("burst-only.yaml");
    match &entries[0] {
        ScenarioEntry::Logs(c) => {
            let bursts = c.base.bursts.as_ref().expect("bursts must carry through");
            assert_eq!(bursts.every, "20s");
            assert_eq!(bursts.r#for, "5s");
            assert!((bursts.multiplier - 10.0).abs() < f64::EPSILON);
            assert!(matches!(c.generator, LogGeneratorConfig::Template { .. }));
        }
        _ => panic!("expected logs entry"),
    }
}

// =============================================================================
// 7.3 — gap + burst both present (runtime prioritizes gap; translator
//       preserves both fields)
// =============================================================================

/// When `gaps:` and `bursts:` are both present on a single entry, both
/// configurations carry through unchanged. Runtime behavior (gap wins on
/// overlap) is out of scope for a translator test.
#[test]
fn row_7_3_gap_and_burst_both_carry_through() {
    let entries = compile("gap-and-burst.yaml");
    match &entries[0] {
        ScenarioEntry::Metrics(c) => {
            assert!(c.base.gaps.is_some(), "gaps must survive translator");
            assert!(c.base.bursts.is_some(), "bursts must survive translator");
        }
        _ => panic!("expected metrics entry"),
    }
}

// =============================================================================
// clock_group_is_auto provenance (BLOCKER 3 fix-pass for PR 7)
// =============================================================================

/// Compile an inline v2 YAML string and return the translated entries.
///
/// Avoids depending on a fixture file because the cases below are
/// purpose-built for a single test and would clutter the fixtures
/// directory.
fn compile_inline(yaml: &str) -> Vec<ScenarioEntry> {
    let resolver = InMemoryPackResolver::new();
    sonda_core::compile_scenario_file(yaml, &resolver)
        .unwrap_or_else(|e| panic!("inline v2 compile failed: {e}"))
}

/// An auto-named chain renders `clock_group_is_auto = Some(true)` on
/// every member of the connected component. The compiler synthesizes
/// `chain_{lowest_lex_id}` here because the user did not supply a
/// `clock_group:` value on either entry. Display code keys on this
/// flag (rather than the `chain_` prefix) to decide whether to suffix
/// with `(auto)`.
#[test]
fn clock_group_is_auto_true_for_synthesized_chain_name() {
    let entries = compile_inline(
        r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: backup_util
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    after:
      ref: primary_link
      op: "<"
      value: 1
"#,
    );

    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(
            entry.clock_group(),
            Some("chain_backup_util"),
            "auto-name uses lowest lex id of component members",
        );
        assert_eq!(
            entry.clock_group_is_auto(),
            Some(true),
            "synthesized chain name must report is_auto = Some(true)",
        );
    }
}

/// When a user writes `clock_group: <name>` explicitly on at least one
/// member of an `after:` component — even a name that starts with
/// `chain_` — every member of that component reports
/// `clock_group_is_auto = Some(false)`. This is the regression that
/// motivated the fix: the previous heuristic (`starts_with("chain_")`)
/// flagged explicit chain-style names as auto.
#[test]
fn clock_group_is_auto_false_for_explicit_chain_prefix_value() {
    let entries = compile_inline(
        r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    clock_group: chain_user_assigned
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: backup_util
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    after:
      ref: primary_link
      op: "<"
      value: 1
"#,
    );

    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(
            entry.clock_group(),
            Some("chain_user_assigned"),
            "explicit name propagates across the component",
        );
        assert_eq!(
            entry.clock_group_is_auto(),
            Some(false),
            "explicit user assignment must report is_auto = Some(false), \
             even when the value starts with `chain_`",
        );
    }
}

/// A standalone v2 entry with no `after:` and no `clock_group:` reports
/// `clock_group: None` and `clock_group_is_auto: None`. The display
/// code must render nothing for the `clock_group:` line in this case.
#[test]
fn clock_group_is_auto_none_for_standalone_entry_with_no_group() {
    let entries = compile_inline(
        r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: solo
    signal_type: metrics
    name: solo_metric
    generator:
      type: constant
      value: 1.0
"#,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].clock_group(), None);
    assert_eq!(entries[0].clock_group_is_auto(), None);
}

/// A standalone v2 entry with an explicit `clock_group:` carries the
/// user value through and reports `is_auto = Some(false)`. The
/// single-node case must not be overlooked by the auto/explicit
/// dispatcher.
#[test]
fn clock_group_is_auto_false_for_standalone_entry_with_explicit_group() {
    let entries = compile_inline(
        r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: solo
    signal_type: metrics
    name: solo_metric
    clock_group: my_group
    generator:
      type: constant
      value: 1.0
"#,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].clock_group(), Some("my_group"));
    assert_eq!(entries[0].clock_group_is_auto(), Some(false));
}
