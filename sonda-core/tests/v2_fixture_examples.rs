#![cfg(feature = "config")]
//! Integration tests that parse the YAML fixtures in `tests/fixtures/v2-examples/`.
//!
//! Each fixture file serves dual duty: human-readable documentation of what the
//! v2 parser accepts and rejects, and a machine-verified test that the parser
//! actually behaves that way.

use sonda_core::compiler::parse::{parse_v2, ParseError};

/// Helper: read a fixture file relative to the crate root.
fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/v2-examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
}

// ======================================================================
// Valid fixtures
// ======================================================================

#[test]
fn valid_single_metric_parses() {
    let yaml = fixture("valid-single-metric.yaml");
    let file = parse_v2(&yaml).expect("valid-single-metric.yaml must parse");
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
    let yaml = fixture("valid-multi-scenario.yaml");
    let file = parse_v2(&yaml).expect("valid-multi-scenario.yaml must parse");
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
    let yaml = fixture("valid-pack-shorthand.yaml");
    let file = parse_v2(&yaml).expect("valid-pack-shorthand.yaml must parse");
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
    let yaml = fixture("valid-pack-in-scenarios.yaml");
    let file = parse_v2(&yaml).expect("valid-pack-in-scenarios.yaml must parse");
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
    let yaml = fixture("valid-histogram.yaml");
    let file = parse_v2(&yaml).expect("valid-histogram.yaml must parse");
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
// Invalid fixtures
// ======================================================================

#[test]
fn invalid_wrong_version_rejected() {
    let yaml = fixture("invalid-wrong-version.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-wrong-version.yaml must fail");
    assert!(
        matches!(err, ParseError::InvalidVersion(1)),
        "expected InvalidVersion(1), got: {err}"
    );
}

#[test]
fn invalid_duplicate_ids_rejected() {
    let yaml = fixture("invalid-duplicate-ids.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-duplicate-ids.yaml must fail");
    assert!(
        matches!(err, ParseError::DuplicateId(ref id) if id == "my_signal"),
        "expected DuplicateId('my_signal'), got: {err}"
    );
}

#[test]
fn invalid_generator_and_pack_rejected() {
    let yaml = fixture("invalid-generator-and-pack.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-generator-and-pack.yaml must fail");
    assert!(
        matches!(err, ParseError::GeneratorAndPack { index: 0 }),
        "expected GeneratorAndPack at index 0, got: {err}"
    );
}

#[test]
fn invalid_pack_with_logs_rejected() {
    let yaml = fixture("invalid-pack-with-logs.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-pack-with-logs.yaml must fail");
    assert!(
        matches!(err, ParseError::PackNotMetrics { index: 0 }),
        "expected PackNotMetrics at index 0, got: {err}"
    );
}

#[test]
fn invalid_missing_name_rejected() {
    let yaml = fixture("invalid-missing-name.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-missing-name.yaml must fail");
    assert!(
        matches!(err, ParseError::MissingName { index: 0 }),
        "expected MissingName at index 0, got: {err}"
    );
}

#[test]
fn invalid_bad_after_op_rejected() {
    let yaml = fixture("invalid-bad-after-op.yaml");
    let err = parse_v2(&yaml).expect_err("invalid-bad-after-op.yaml must fail");
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
