#![cfg(feature = "config")]
//! Integration tests for Phase 3 pack expansion on YAML fixtures.
//!
//! Mirrors the pattern established by `fixture_examples.rs`: every
//! fixture under `tests/fixtures/v2-examples/` starting with
//! `valid-expand-` is parsed, normalized, and expanded, with the output
//! compared against an [`insta`] JSON snapshot under `tests/snapshots/`.
//! Invalid fixtures assert the expected [`ExpandError`] variant.
//!
//! Run `INSTA_UPDATE=always cargo test -p sonda-core --test expand_fixtures`
//! (or `cargo insta accept`) to regenerate snapshots after a schema change.

mod common;

use common::{builtin_pack_resolver, compile_to_expanded, example_fixture, snapshot_settings};
use sonda_core::compiler::expand::{expand, ExpandError};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;

// =====================================================================
// Valid fixtures — insta snapshots
// =====================================================================

#[test]
fn valid_expand_pack_with_overrides() {
    let yaml = example_fixture("valid-expand-pack-with-overrides.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile_to_expanded(&yaml, &resolver);

    // Spot-check the expansion before snapshot comparison.
    assert_eq!(expanded.entries.len(), 5, "5 metrics in telegraf snmp pack");
    assert_eq!(expanded.entries[0].name, "ifOperStatus");
    assert_eq!(
        expanded.entries[0].id.as_deref(),
        Some("primary_uplink.ifOperStatus")
    );

    // Override labels and defaults.labels compose correctly.
    let labels = expanded.entries[0].labels.as_ref().unwrap();
    assert_eq!(labels.get("env").unwrap(), "prod");
    assert_eq!(labels.get("device").unwrap(), "rtr-edge-01");
    assert_eq!(labels.get("probe").unwrap(), "synthetic");

    snapshot_settings().bind(|| insta::assert_json_snapshot!(expanded));
}

#[test]
fn valid_expand_multiple_packs() {
    let yaml = example_fixture("valid-expand-multiple-packs.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile_to_expanded(&yaml, &resolver);

    // Two packs x 5 metrics each = 10 entries.
    assert_eq!(expanded.entries.len(), 10);
    // Auto-IDs discriminate the two anonymous pack entries by position.
    assert_eq!(
        expanded.entries[0].id.as_deref(),
        Some("telegraf_snmp_interface_0.ifOperStatus")
    );
    assert_eq!(
        expanded.entries[5].id.as_deref(),
        Some("telegraf_snmp_interface_1.ifOperStatus")
    );
    // Each carries its own device label.
    assert_eq!(
        expanded.entries[0]
            .labels
            .as_ref()
            .unwrap()
            .get("device")
            .unwrap(),
        "rtr-01"
    );
    assert_eq!(
        expanded.entries[5]
            .labels
            .as_ref()
            .unwrap()
            .get("device")
            .unwrap(),
        "rtr-02"
    );

    snapshot_settings().bind(|| insta::assert_json_snapshot!(expanded));
}

#[test]
fn valid_expand_anonymous_pack() {
    let yaml = example_fixture("valid-expand-anonymous-pack.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile_to_expanded(&yaml, &resolver);

    assert_eq!(expanded.entries.len(), 5);
    assert_eq!(
        expanded.entries[0].id.as_deref(),
        Some("telegraf_snmp_interface_0.ifOperStatus")
    );

    snapshot_settings().bind(|| insta::assert_json_snapshot!(expanded));
}

// =====================================================================
// Invalid fixtures — error cases
// =====================================================================

#[test]
fn invalid_expand_unknown_override_key_rejected() {
    let yaml = example_fixture("invalid-expand-unknown-override.yaml");
    let parsed = parse(&yaml).expect("parse");
    let normalized = normalize(parsed).expect("normalize");
    let resolver = builtin_pack_resolver();
    let err = expand(normalized, &resolver).expect_err("expand must fail");
    match err {
        ExpandError::UnknownOverrideKey {
            key,
            pack_name,
            available,
        } => {
            assert_eq!(key, "not_a_real_metric");
            assert_eq!(pack_name, "telegraf_snmp_interface");
            assert!(available.contains("ifOperStatus"));
            assert!(available.contains("ifHCInOctets"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}
