#![cfg(feature = "config")]
//! Integration tests for Phase 4 `after` clause compilation on YAML fixtures.
//!
//! Mirrors the pattern established by `v2_expand_fixtures.rs`: every fixture
//! under `tests/fixtures/v2-examples/` starting with `valid-compile-` is
//! parsed, normalized, expanded, and compiled, with the output compared
//! against a golden JSON snapshot in `tests/fixtures/v2-examples/expected/`.
//! Invalid fixtures assert the expected [`CompileAfterError`] variant.
//!
//! Set `UPDATE_SNAPSHOTS=1` to regenerate golden files after a schema
//! change.

use std::path::{Path, PathBuf};

use sonda_core::compiler::compile_after::{compile_after, CompileAfterError, CompiledFile};
use sonda_core::compiler::expand::{expand, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::packs::MetricPackDef;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/v2-examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
}

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

fn snapshot_compiled(file: &CompiledFile) -> String {
    let mut s =
        serde_json::to_string_pretty(file).expect("serializing a CompiledFile must not fail");
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

fn compile(yaml: &str, resolver: &InMemoryPackResolver) -> CompiledFile {
    let parsed = parse(yaml).expect("fixture must parse");
    let normalized = normalize(parsed).expect("fixture must normalize");
    let expanded = expand(normalized, resolver).expect("fixture must expand");
    compile_after(expanded).expect("fixture must compile after")
}

fn compile_err(yaml: &str, resolver: &InMemoryPackResolver) -> CompileAfterError {
    let parsed = parse(yaml).expect("fixture must parse");
    let normalized = normalize(parsed).expect("fixture must normalize");
    let expanded = expand(normalized, resolver).expect("fixture must expand");
    compile_after(expanded).expect_err("fixture must fail to compile")
}

// =====================================================================
// Valid fixtures — golden snapshots
// =====================================================================

#[test]
fn valid_compile_simple_chain() {
    let yaml = fixture("valid-compile-simple-chain.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    assert_eq!(compiled.entries.len(), 2);
    let util = &compiled.entries[1];
    assert_eq!(util.phase_offset.as_deref(), Some("1m"));
    // Both entries share the auto-assigned chain_{lowest_id} clock group.
    assert_eq!(
        compiled.entries[0].clock_group.as_deref(),
        Some("chain_link")
    );
    assert_eq!(util.clock_group.as_deref(), Some("chain_link"));

    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-simple-chain.json");
}

#[test]
fn valid_compile_transitive_chain() {
    let yaml = fixture("valid-compile-transitive-chain.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    assert_eq!(compiled.entries.len(), 3);
    // util offset = 60s (flap up_duration).
    assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("1m"));
    // latency offset = 60s + saturation crossing of 70 from (20→85) over 120s.
    // (70-20)/(85-20)*120 = 92.307..., + 60 = 152.307s, rounded to the
    // nearest ms for display.
    let latency_offset = compiled.entries[2].phase_offset.as_deref().unwrap();
    assert!(
        latency_offset.starts_with("152."),
        "expected ~152s, got {latency_offset}"
    );

    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-transitive-chain.json");
}

#[test]
fn valid_compile_step_target() {
    let yaml = fixture("valid-compile-step-target.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    // ceil((55-0)/10) = 6 ticks, rate=2 -> 3.0s.
    assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("3s"));
    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-step-target.json");
}

#[test]
fn valid_compile_sequence_target() {
    let yaml = fixture("valid-compile-sequence-target.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    // values[2] = 2 < 3 at index 2, rate=2 -> 1.0s.
    assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("1s"));
    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-sequence-target.json");
}

#[test]
fn valid_compile_cross_signal_type() {
    let yaml = fixture("valid-compile-cross-signal-type.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    // Saturation crossing of 10 from (1→30) over 90s -> (10-1)/(30-1)*90 = 27.931...s.
    let offset = compiled.entries[1].phase_offset.as_deref().unwrap();
    assert!(offset.starts_with("27."), "expected ~27.9s, got {offset}");
    assert_eq!(compiled.entries[1].signal_type, "logs");

    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-cross-signal-type.json");
}

#[test]
fn valid_compile_phase_offset_and_delay() {
    let yaml = fixture("valid-compile-phase-offset-and-delay.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    // 10s phase_offset + 60s flap crossing + 15s delay = 85s.
    assert_eq!(compiled.entries[1].phase_offset.as_deref(), Some("85s"));

    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-phase-offset-and-delay.json");
}

#[test]
fn valid_compile_pack_dotted_ref() {
    let yaml = fixture("valid-compile-pack-dotted-ref.yaml");
    let resolver = builtin_pack_resolver();
    let compiled = compile(&yaml, &resolver);

    // Find the backup_signal entry — every pack sub-signal came first.
    let backup = compiled
        .entries
        .iter()
        .find(|e| e.id.as_deref() == Some("backup_signal"))
        .expect("backup_signal entry present");
    assert_eq!(backup.phase_offset.as_deref(), Some("1m"));

    // primary_uplink.ifOperStatus carries the flap override and should be in
    // the same chain as backup_signal.
    let ifoper = compiled
        .entries
        .iter()
        .find(|e| e.id.as_deref() == Some("primary_uplink.ifOperStatus"))
        .expect("ifOperStatus sub-signal present");
    assert_eq!(ifoper.clock_group, backup.clock_group);

    let snap = snapshot_compiled(&compiled);
    assert_snapshot(&snap, "valid-compile-pack-dotted-ref.json");
}

// =====================================================================
// Invalid fixtures — error cases
// =====================================================================

#[test]
fn invalid_compile_unknown_ref_rejected() {
    let yaml = fixture("invalid-compile-unknown-ref.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::UnknownRef {
            ref_id, available, ..
        } => {
            assert_eq!(ref_id, "nonexistent");
            assert!(available.contains("link"));
            assert!(available.contains("follower"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_cycle_rejected() {
    let yaml = fixture("invalid-compile-cycle.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::CircularDependency { cycle } => {
            assert!(cycle.len() >= 2);
            assert_eq!(cycle.first(), cycle.last());
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_self_reference_rejected() {
    let yaml = fixture("invalid-compile-self-reference.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::SelfReference { source_id } => {
            assert_eq!(source_id, "loop_entry");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_unsupported_sine_rejected() {
    let yaml = fixture("invalid-compile-unsupported-sine.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::UnsupportedGenerator {
            generator, ref_id, ..
        } => {
            assert_eq!(generator, "sine");
            assert_eq!(ref_id, "wave");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_out_of_range_rejected() {
    let yaml = fixture("invalid-compile-out-of-range.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::OutOfRangeThreshold { value, ref_id, .. } => {
            assert!((value - 150.0).abs() < f64::EPSILON);
            assert_eq!(ref_id, "util");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_ambiguous_at_t0_rejected() {
    let yaml = fixture("invalid-compile-ambiguous-at-t0.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::AmbiguousAtT0 { ref_id, .. } => {
            assert_eq!(ref_id, "spiker");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_conflicting_clock_group_rejected() {
    let yaml = fixture("invalid-compile-conflicting-clock-group.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::ConflictingClockGroup {
            first_group,
            second_group,
            ..
        } => {
            assert!(first_group == "group_alpha" || second_group == "group_alpha");
            assert!(first_group == "group_bravo" || second_group == "group_bravo");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_ambiguous_pack_ref_rejected() {
    let yaml = fixture("invalid-compile-ambiguous-pack-ref.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::AmbiguousSubSignalRef {
            ref_id,
            pack_entry_id,
            candidates,
        } => {
            assert_eq!(ref_id, "node.node_cpu_seconds_total");
            assert_eq!(pack_entry_id, "node");
            assert!(candidates.contains("#0"));
            assert!(candidates.contains("#7"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn invalid_compile_non_metrics_target_rejected() {
    let yaml = fixture("invalid-compile-non-metrics-target.yaml");
    let resolver = builtin_pack_resolver();
    match compile_err(&yaml, &resolver) {
        CompileAfterError::NonMetricsTarget {
            signal_type,
            ref_id,
            ..
        } => {
            assert_eq!(signal_type, "logs");
            assert_eq!(ref_id, "log_src");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}
