#![cfg(feature = "config")]
//! Integration tests for Phase 3 pack expansion on YAML fixtures.
//!
//! Mirrors the pattern established by `v2_fixture_examples.rs`: every
//! fixture under `tests/fixtures/v2-examples/` starting with
//! `valid-expand-` is parsed, normalized, and expanded, with the output
//! compared against a golden JSON snapshot in
//! `tests/fixtures/v2-examples/expected/`. Invalid fixtures assert the
//! expected [`ExpandError`] variant.
//!
//! Set `UPDATE_SNAPSHOTS=1` to regenerate golden files after a schema
//! change.

use std::path::{Path, PathBuf};

use sonda_core::compiler::expand::{expand, ExpandError, ExpandedFile, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::packs::MetricPackDef;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Read a scenario fixture relative to the crate root.
fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/v2-examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
}

/// Load a pack YAML from the repo-root `packs/` directory.
fn load_repo_pack(file_name: &str) -> MetricPackDef {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has a parent dir")
        .to_path_buf();
    let path = root.join("packs").join(file_name);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read pack {}: {}", path.display(), e));
    serde_yaml_ng::from_str::<MetricPackDef>(&yaml)
        .unwrap_or_else(|e| panic!("cannot parse pack {}: {}", path.display(), e))
}

/// Build a resolver preloaded with the three built-in packs, keyed by both
/// their canonical pack name (catalog lookup) and the typical file-path
/// reference used in test fixtures (starts with `.`, contains `/`).
fn builtin_pack_resolver() -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    for (file, pack_name) in [
        ("telegraf-snmp-interface.yaml", "telegraf_snmp_interface"),
        ("node-exporter-cpu.yaml", "node_exporter_cpu"),
        ("node-exporter-memory.yaml", "node_exporter_memory"),
    ] {
        let pack = load_repo_pack(file);
        r.insert(pack_name, pack.clone());
        r.insert(format!("./packs/{file}"), pack);
    }
    r
}

/// Serialize an [`ExpandedFile`] as pretty JSON for golden comparison.
fn snapshot_expanded(file: &ExpandedFile) -> String {
    let mut s =
        serde_json::to_string_pretty(file).expect("serializing an ExpandedFile must not fail");
    s.push('\n');
    s
}

fn assert_snapshot(actual: &str, golden_name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/v2-examples/expected")
        .join(golden_name);

    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        let dir = path
            .parent()
            .unwrap_or_else(|| panic!("golden path {} has no parent", path.display()));
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        std::fs::write(&path, actual)
            .unwrap_or_else(|e| panic!("cannot write golden {}: {e}", path.display()));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden {} (run with UPDATE_SNAPSHOTS=1 to create it): {}",
            path.display(),
            e
        )
    });
    assert_eq!(
        actual,
        expected,
        "snapshot mismatch for {}\nRun with UPDATE_SNAPSHOTS=1 to update.",
        path.display()
    );
}

fn compile(yaml: &str, resolver: &InMemoryPackResolver) -> ExpandedFile {
    let parsed = parse(yaml).expect("fixture must parse");
    let normalized = normalize(parsed).expect("fixture must normalize");
    expand(normalized, resolver).expect("fixture must expand")
}

// =====================================================================
// Valid fixtures — golden snapshots
// =====================================================================

#[test]
fn valid_expand_pack_with_overrides() {
    let yaml = fixture("valid-expand-pack-with-overrides.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile(&yaml, &resolver);

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

    let snap = snapshot_expanded(&expanded);
    assert_snapshot(&snap, "valid-expand-pack-with-overrides.json");
}

#[test]
fn valid_expand_pack_file_path() {
    let yaml = fixture("valid-expand-pack-file-path.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile(&yaml, &resolver);

    assert_eq!(expanded.entries.len(), 5);
    assert_eq!(expanded.entries[0].name, "ifOperStatus");
    assert_eq!(
        expanded.entries[0].id.as_deref(),
        Some("uplink.ifOperStatus")
    );

    let snap = snapshot_expanded(&expanded);
    assert_snapshot(&snap, "valid-expand-pack-file-path.json");
}

#[test]
fn valid_expand_multiple_packs() {
    let yaml = fixture("valid-expand-multiple-packs.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile(&yaml, &resolver);

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

    let snap = snapshot_expanded(&expanded);
    assert_snapshot(&snap, "valid-expand-multiple-packs.json");
}

#[test]
fn valid_expand_anonymous_pack() {
    let yaml = fixture("valid-expand-anonymous-pack.yaml");
    let resolver = builtin_pack_resolver();
    let expanded = compile(&yaml, &resolver);

    assert_eq!(expanded.entries.len(), 5);
    assert_eq!(
        expanded.entries[0].id.as_deref(),
        Some("telegraf_snmp_interface_0.ifOperStatus")
    );

    let snap = snapshot_expanded(&expanded);
    assert_snapshot(&snap, "valid-expand-anonymous-pack.json");
}

// =====================================================================
// Invalid fixtures — error cases
// =====================================================================

#[test]
fn invalid_expand_unknown_override_key_rejected() {
    let yaml = fixture("invalid-expand-unknown-override.yaml");
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
