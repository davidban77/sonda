#![cfg(feature = "config")]
//! Integration tests that parse the YAML fixtures in `tests/fixtures/v2-examples/`.
//!
//! Each fixture file serves dual duty: human-readable documentation of what the
//! v2 parser accepts and rejects, and a machine-verified test that the parser
//! actually behaves that way.
//!
//! Normalization fixtures go one step further and compare the resolved entries
//! against [`insta`] JSON snapshots under `tests/snapshots/`. Run
//! `INSTA_UPDATE=always cargo test -p sonda-core --test fixture_examples`
//! (or `cargo insta accept`) to regenerate them after a schema change.

mod common;

use common::{example_fixture, snapshot_settings};
use sonda_core::compiler::normalize::{normalize, NormalizeError};
use sonda_core::compiler::parse::{parse, ParseError};

// ======================================================================
// Valid fixtures — parse only
// ======================================================================

#[test]
fn valid_single_metric_parses() {
    let yaml = example_fixture("valid-single-metric.yaml");
    let file = parse(&yaml).expect("valid-single-metric.yaml must parse");
    assert_eq!(file.version, 2);
    assert_eq!(file.scenarios.len(), 1);

    let entry = &file.scenarios[0];
    assert_eq!(entry.name.as_deref(), Some("cpu_usage"));
    assert_eq!(entry.signal_type, "metrics");
    assert!(entry.generator.is_some());
    assert!(entry.encoder.is_some());
    assert!(entry.sink.is_some());
    assert_eq!(entry.duration.as_deref(), Some("30s"));
}

#[test]
fn valid_multi_scenario_parses() {
    let yaml = example_fixture("valid-multi-scenario.yaml");
    let file = parse(&yaml).expect("valid-multi-scenario.yaml must parse");
    assert_eq!(file.version, 2);
    assert_eq!(file.scenarios.len(), 3);

    // Defaults block is present
    let defaults = file.defaults.as_ref().expect("must have defaults");
    assert!((defaults.rate.unwrap() - 1.0).abs() < f64::EPSILON);
    assert_eq!(defaults.duration.as_deref(), Some("5m"));
    assert!(defaults.encoder.is_some());
    assert!(defaults.sink.is_some());

    // First entry: inline metric with id
    let e0 = &file.scenarios[0];
    assert_eq!(e0.id.as_deref(), Some("link_state"));
    assert_eq!(e0.signal_type, "metrics");
    assert!(e0.generator.is_some());
    assert!(e0.labels.is_some());

    // Second entry: metric with after clause
    let e1 = &file.scenarios[1];
    assert_eq!(e1.id.as_deref(), Some("backup_util"));
    let after = e1.after.as_ref().expect("must have after clause");
    assert_eq!(after.ref_id, "link_state");

    // Third entry: log signal with after clause
    let e2 = &file.scenarios[2];
    assert_eq!(e2.signal_type, "logs");
    assert!(e2.log_generator.is_some());
    let after = e2.after.as_ref().expect("must have after clause");
    assert_eq!(after.ref_id, "backup_util");
}

#[test]
fn valid_pack_shorthand_parses() {
    let yaml = example_fixture("valid-pack-shorthand.yaml");
    let file = parse(&yaml).expect("valid-pack-shorthand.yaml must parse");
    assert_eq!(file.version, 2);
    assert_eq!(file.scenarios.len(), 1);

    let entry = &file.scenarios[0];
    assert_eq!(entry.signal_type, "metrics");
    assert_eq!(entry.pack.as_deref(), Some("telegraf_snmp_interface"));
    let labels = entry.labels.as_ref().expect("must have labels");
    assert_eq!(
        labels.get("device").map(String::as_str),
        Some("rtr-edge-01")
    );
    assert_eq!(labels.get("ifIndex").map(String::as_str), Some("1"));
}

#[test]
fn valid_pack_in_scenarios_parses() {
    let yaml = example_fixture("valid-pack-in-scenarios.yaml");
    let file = parse(&yaml).expect("valid-pack-in-scenarios.yaml must parse");
    assert_eq!(file.version, 2);
    assert_eq!(file.scenarios.len(), 1);

    let entry = &file.scenarios[0];
    assert_eq!(entry.id.as_deref(), Some("primary_uplink"));
    assert_eq!(entry.pack.as_deref(), Some("telegraf_snmp_interface"));
    let overrides = entry.overrides.as_ref().expect("must have overrides");
    assert!(overrides.contains_key("ifOperStatus"));

    // Defaults block is present
    let defaults = file.defaults.as_ref().expect("must have defaults");
    assert!((defaults.rate.unwrap() - 1.0).abs() < f64::EPSILON);
    assert_eq!(defaults.duration.as_deref(), Some("10m"));
}

#[test]
fn valid_histogram_parses() {
    let yaml = example_fixture("valid-histogram.yaml");
    let file = parse(&yaml).expect("valid-histogram.yaml must parse");
    assert_eq!(file.version, 2);
    assert_eq!(file.scenarios.len(), 1);

    let entry = &file.scenarios[0];
    assert_eq!(entry.signal_type, "histogram");
    assert_eq!(entry.name.as_deref(), Some("http_request_duration_seconds"));
    assert!(entry.distribution.is_some());
    let buckets = entry.buckets.as_ref().expect("must have buckets");
    assert_eq!(buckets.len(), 10);
    assert_eq!(entry.observations_per_tick, Some(100));
    let labels = entry.labels.as_ref().expect("must have labels");
    assert_eq!(
        labels.get("service").map(String::as_str),
        Some("api-gateway")
    );
}

// ======================================================================
// Invalid fixtures — parse-time rejections
// ======================================================================

#[test]
fn invalid_wrong_version_rejected() {
    let yaml = example_fixture("invalid-wrong-version.yaml");
    let err = parse(&yaml).expect_err("invalid-wrong-version.yaml must fail");
    assert!(
        matches!(err, ParseError::InvalidVersion(1)),
        "expected InvalidVersion(1), got: {err}"
    );
}

#[test]
fn invalid_duplicate_ids_rejected() {
    let yaml = example_fixture("invalid-duplicate-ids.yaml");
    let err = parse(&yaml).expect_err("invalid-duplicate-ids.yaml must fail");
    assert!(
        matches!(err, ParseError::DuplicateId(ref id) if id == "my_signal"),
        "expected DuplicateId('my_signal'), got: {err}"
    );
}

#[test]
fn invalid_generator_and_pack_rejected() {
    let yaml = example_fixture("invalid-generator-and-pack.yaml");
    let err = parse(&yaml).expect_err("invalid-generator-and-pack.yaml must fail");
    assert!(
        matches!(err, ParseError::GeneratorAndPack { index: 0 }),
        "expected GeneratorAndPack at index 0, got: {err}"
    );
}

#[test]
fn invalid_pack_with_logs_rejected() {
    let yaml = example_fixture("invalid-pack-with-logs.yaml");
    let err = parse(&yaml).expect_err("invalid-pack-with-logs.yaml must fail");
    assert!(
        matches!(err, ParseError::PackNotMetrics { index: 0 }),
        "expected PackNotMetrics at index 0, got: {err}"
    );
}

#[test]
fn invalid_missing_name_rejected() {
    let yaml = example_fixture("invalid-missing-name.yaml");
    let err = parse(&yaml).expect_err("invalid-missing-name.yaml must fail");
    assert!(
        matches!(err, ParseError::MissingName { index: 0 }),
        "expected MissingName at index 0, got: {err}"
    );
}

#[test]
fn invalid_bad_after_op_rejected() {
    let yaml = example_fixture("invalid-bad-after-op.yaml");
    let err = parse(&yaml).expect_err("invalid-bad-after-op.yaml must fail");
    assert!(
        matches!(err, ParseError::Yaml(_)),
        "expected Yaml error for invalid op, got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("=="),
        "error message should mention the invalid op '==', got: {msg}"
    );
}

// ======================================================================
// Defaults normalization — valid fixtures with insta snapshots
// ======================================================================

#[test]
fn valid_defaults_label_merge_normalizes() {
    let yaml = example_fixture("valid-defaults-label-merge.yaml");
    let parsed = parse(&yaml).expect("must parse");
    let normalized = normalize(parsed).expect("must normalize");

    // Spot-check the merged output before comparing against the snapshot.
    assert_eq!(normalized.entries.len(), 2);

    let e0 = &normalized.entries[0];
    assert!((e0.rate - 1.0).abs() < f64::EPSILON);
    assert_eq!(e0.duration.as_deref(), Some("5m"));
    let labels0 = e0.labels.as_ref().expect("labels must exist");
    assert_eq!(
        labels0.get("device").map(String::as_str),
        Some("rtr-edge-01")
    );
    assert_eq!(labels0.get("region").map(String::as_str), Some("us-east-1"));
    assert_eq!(
        labels0.get("interface").map(String::as_str),
        Some("Gi0/0/0")
    );

    let e1 = &normalized.entries[1];
    assert!((e1.rate - 10.0).abs() < f64::EPSILON);
    let labels1 = e1.labels.as_ref().expect("labels must exist");
    assert_eq!(labels1.get("region").map(String::as_str), Some("us-west-2"));

    snapshot_settings().bind(|| insta::assert_json_snapshot!(normalized));
}

#[test]
fn valid_defaults_logs_default_encoder_normalizes() {
    let yaml = example_fixture("valid-defaults-logs-default-encoder.yaml");
    let parsed = parse(&yaml).expect("must parse");
    let normalized = normalize(parsed).expect("must normalize");

    assert_eq!(normalized.entries.len(), 1);
    let e0 = &normalized.entries[0];
    assert!((e0.rate - 5.0).abs() < f64::EPSILON);
    // Logs signals default to json_lines when no encoder is set anywhere.
    assert!(matches!(
        e0.encoder,
        sonda_core::encoder::EncoderConfig::JsonLines { .. }
    ));

    snapshot_settings().bind(|| insta::assert_json_snapshot!(normalized));
}

#[test]
fn valid_defaults_pack_entry_normalizes() {
    let yaml = example_fixture("valid-defaults-pack-entry.yaml");
    let parsed = parse(&yaml).expect("must parse");
    let normalized = normalize(parsed).expect("must normalize");

    assert_eq!(normalized.entries.len(), 1);
    let e0 = &normalized.entries[0];
    assert_eq!(e0.pack.as_deref(), Some("telegraf_snmp_interface"));
    assert!(e0.overrides.is_some(), "overrides must survive");
    assert!((e0.rate - 1.0).abs() < f64::EPSILON);

    // Pack entry labels are NOT merged with defaults.labels — Phase 3 pack
    // expansion is responsible for composing levels 2–6 (see spec §2.2).
    let labels = e0.labels.as_ref().expect("entry labels must exist");
    assert_eq!(labels.len(), 1, "only entry labels, defaults not merged");
    assert_eq!(labels.get("device").map(String::as_str), Some("rtr-01"));
    assert!(!labels.contains_key("job"));
    assert!(!labels.contains_key("env"));

    // defaults.labels is surfaced at the file level for Phase 3 to apply.
    let d = normalized
        .defaults_labels
        .as_ref()
        .expect("defaults_labels must be carried forward");
    assert_eq!(d.get("job").map(String::as_str), Some("web"));
    assert_eq!(d.get("env").map(String::as_str), Some("prod"));

    snapshot_settings().bind(|| insta::assert_json_snapshot!(normalized));
}

// ======================================================================
// Defaults normalization — missing rate is rejected
// ======================================================================

#[test]
fn invalid_missing_rate_rejected() {
    let yaml = example_fixture("invalid-missing-rate.yaml");
    let parsed = parse(&yaml).expect("parse must succeed (rate is not required at parse time)");
    let err = normalize(parsed).expect_err("normalize must fail on missing rate");
    match err {
        NormalizeError::MissingRate { index, label } => {
            assert_eq!(index, 0);
            assert_eq!(label, "cpu");
        }
        // `NormalizeError` is `#[non_exhaustive]` across the crate boundary
        // (integration tests are a separate crate); any other variant is an
        // unexpected regression for this fixture.
        other => panic!("expected MissingRate, got {other:?}"),
    }
}
