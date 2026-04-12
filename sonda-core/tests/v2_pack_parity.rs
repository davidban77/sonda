#![cfg(feature = "config")]
//! Pack expansion parity bridge (validation matrix rows 17.1–17.3).
//!
//! For each built-in pack the tests:
//!
//! 1. Parse, normalize, and expand a v2 YAML fixture that references the
//!    pack inside `scenarios:`.
//! 2. Build an equivalent `PackScenarioConfig` in code and run it through
//!    the existing v1 [`sonda_core::packs::expand_pack`] function.
//! 3. Assert that the two outputs describe the same concrete set of
//!    signals — same metric names, same generators, same composed label
//!    maps, same rate, duration, encoder, and sink.
//!
//! Runtime parity (identical stdout output) is PR 6 work; this file is
//! compile-parity only.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use sonda_core::compiler::compile_after::{compile_after, CompiledEntry};
use sonda_core::compiler::expand::{expand, ExpandedEntry, ExpandedFile, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::compiler::{AfterClause, AfterOp};
use sonda_core::config::ScenarioEntry;
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::packs::{expand_pack, MetricOverride, MetricPackDef, PackScenarioConfig};
use sonda_core::sink::SinkConfig;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has parent")
        .to_path_buf()
}

fn load_pack(file_name: &str) -> MetricPackDef {
    let path = repo_root().join("packs").join(file_name);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
    serde_yaml_ng::from_str::<MetricPackDef>(&yaml)
        .unwrap_or_else(|e| panic!("cannot parse {}: {}", path.display(), e))
}

fn parity_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/v2-parity")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e))
}

/// Run the v2 pipeline (parse → normalize → expand) on a fixture YAML.
fn v2_compile(yaml: &str, resolver: &InMemoryPackResolver) -> ExpandedFile {
    let parsed = parse(yaml).expect("parse");
    let normalized = normalize(parsed).expect("normalize");
    expand(normalized, resolver).expect("expand")
}

/// Normalize a label source into a sorted BTreeMap for comparison.
///
/// The v1 path produces `HashMap<String, String>`, v2 produces
/// `BTreeMap<String, String>`. The equality test converts both to
/// `BTreeMap` so iteration order cannot cause false negatives.
fn into_btree_labels(hm: Option<&HashMap<String, String>>) -> BTreeMap<String, String> {
    hm.cloned().unwrap_or_default().into_iter().collect()
}

/// Extract a sorted (metric-name, labels, generator, rate, duration,
/// encoder, sink) tuple from a v2 expanded entry.
type ComparableSignal = (
    String,
    BTreeMap<String, String>,
    GeneratorConfig,
    f64,
    Option<String>,
    EncoderConfig,
    SinkConfig,
);

fn v2_signal(entry: &ExpandedEntry) -> ComparableSignal {
    (
        entry.name.clone(),
        entry.labels.clone().unwrap_or_default(),
        entry
            .generator
            .clone()
            .expect("pack-expanded entries always have a generator"),
        entry.rate,
        entry.duration.clone(),
        entry.encoder.clone(),
        entry.sink.clone(),
    )
}

fn v1_signal(entry: &ScenarioEntry) -> ComparableSignal {
    let ScenarioEntry::Metrics(c) = entry else {
        panic!("expected Metrics entry");
    };
    (
        c.base.name.clone(),
        into_btree_labels(c.base.labels.as_ref()),
        c.generator.clone(),
        c.base.rate,
        c.base.duration.clone(),
        c.encoder.clone(),
        c.base.sink.clone(),
    )
}

/// Compare two concrete-signal sets built from the v1 and v2 expansion
/// paths. The comparison is order-insensitive: it treats each list as a
/// multiset of `ComparableSignal` tuples so test failures don't hinge on
/// iteration order of pack metrics.
///
/// For `GeneratorConfig` and `EncoderConfig` (neither implements `Eq`),
/// equality is checked via JSON round-trip serialization — both types
/// are `Serialize` under the `config` feature.
///
/// # Why ids are not in the comparison
///
/// [`ComparableSignal`] deliberately omits `id`: v1's [`ScenarioEntry`]
/// does not carry a stable per-entry id (the v1 catalog uses the pack
/// scenario name as the single launchable unit), while v2 synthesizes
/// sub-signal ids of the form `"{effective_entry_id}.{metric_name}"` with
/// an optional `"#{spec_index}"` suffix for packs that ship duplicate
/// metric names. Comparing ids across the two paths would fail by
/// definition.
///
/// Id uniqueness on the v2 side is a distinct invariant and is asserted
/// separately via [`assert_v2_ids_are_unique`] in each parity test.
fn assert_same_signal_set(label: &str, v1: &[ScenarioEntry], v2: &[ExpandedEntry]) {
    assert_eq!(
        v1.len(),
        v2.len(),
        "{label}: v1 produced {} entries, v2 produced {}",
        v1.len(),
        v2.len()
    );

    // Multiset comparison: sort the JSON keys for each side and compare.
    // BTreeSet would deduplicate, which would mask duplicates like the
    // node_cpu_seconds_total pack where the same metric name appears
    // multiple times differentiated by labels.
    let v1_signals: Vec<ComparableSignal> = v1.iter().map(v1_signal).collect();
    let v2_signals: Vec<ComparableSignal> = v2.iter().map(v2_signal).collect();
    let mut v1_sorted: Vec<String> = v1_signals.iter().map(signal_key).collect();
    let mut v2_sorted: Vec<String> = v2_signals.iter().map(signal_key).collect();
    v1_sorted.sort();
    v2_sorted.sort();
    assert_eq!(
        v1_sorted, v2_sorted,
        "{label}: v1 and v2 signal sets differ\nv1: {v1_sorted:#?}\nv2: {v2_sorted:#?}"
    );
}

/// Assert that every [`ExpandedEntry`] produced by the v2 pipeline carries
/// a distinct, non-empty `id`.
///
/// Complements [`assert_same_signal_set`]: parity covers signal *shape*,
/// this covers signal *identity*. Regression anchor for the
/// `node_exporter_cpu` pack, which ships eight `MetricSpec` entries all
/// named `node_cpu_seconds_total` — each must expand into a unique
/// sub-signal id.
fn assert_v2_ids_are_unique(label: &str, v2: &[ExpandedEntry]) {
    let ids: Vec<&str> = v2
        .iter()
        .map(|e| {
            e.id.as_deref()
                .unwrap_or_else(|| panic!("{label}: pack-expanded entry missing id: {e:?}"))
        })
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "{label}: sub-signal ids must be unique; saw {ids:?}"
    );
}

/// Produce a deterministic comparison key for one signal.
fn signal_key(s: &ComparableSignal) -> String {
    // JSON-serialize the tuple with stable field ordering.
    #[derive(serde::Serialize)]
    struct Key<'a> {
        name: &'a str,
        labels: &'a BTreeMap<String, String>,
        generator: &'a GeneratorConfig,
        rate: f64,
        duration: &'a Option<String>,
        encoder: &'a EncoderConfig,
        sink: &'a SinkConfig,
    }
    let key = Key {
        name: &s.0,
        labels: &s.1,
        generator: &s.2,
        rate: s.3,
        duration: &s.4,
        encoder: &s.5,
        sink: &s.6,
    };
    serde_json::to_string(&key).expect("serialization must succeed")
}

fn resolver_with(name: &str, pack: MetricPackDef) -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    r.insert(name, pack);
    r
}

// =============================================================================
// 17.1 — telegraf_snmp_interface parity
// =============================================================================

#[test]
fn parity_telegraf_snmp_interface() {
    let pack = load_pack("telegraf-snmp-interface.yaml");
    let resolver = resolver_with("telegraf_snmp_interface", pack.clone());
    let yaml = parity_fixture("telegraf-snmp-interface.yaml");

    // v2 pipeline.
    let v2_expanded = v2_compile(&yaml, &resolver);

    // Equivalent v1 config.
    let mut user_labels = HashMap::new();
    user_labels.insert("device".to_string(), "rtr-edge-01".to_string());
    user_labels.insert("ifName".to_string(), "GigabitEthernet0/0/0".to_string());
    user_labels.insert("ifIndex".to_string(), "1".to_string());

    let mut overrides = HashMap::new();
    overrides.insert(
        "ifOperStatus".to_string(),
        MetricOverride {
            generator: Some(GeneratorConfig::Flap {
                up_duration: Some("60s".to_string()),
                down_duration: Some("30s".to_string()),
                up_value: None,
                down_value: None,
            }),
            labels: None,
            after: None,
        },
    );

    let v1_config = PackScenarioConfig {
        pack: "telegraf_snmp_interface".to_string(),
        rate: 1.0,
        duration: Some("60s".to_string()),
        labels: Some(user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: Some(overrides),
    };

    let v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");

    assert_same_signal_set("telegraf_snmp_interface", &v1_entries, &v2_expanded.entries);
    assert_v2_ids_are_unique("telegraf_snmp_interface", &v2_expanded.entries);
}

// =============================================================================
// 17.2 — node_exporter_cpu parity
// =============================================================================

#[test]
fn parity_node_exporter_cpu() {
    let pack = load_pack("node-exporter-cpu.yaml");
    let resolver = resolver_with("node_exporter_cpu", pack.clone());
    let yaml = parity_fixture("node-exporter-cpu.yaml");

    let v2_expanded = v2_compile(&yaml, &resolver);

    let mut user_labels = HashMap::new();
    user_labels.insert("instance".to_string(), "web-01:9100".to_string());

    let v1_config = PackScenarioConfig {
        pack: "node_exporter_cpu".to_string(),
        rate: 1.0,
        duration: Some("30s".to_string()),
        labels: Some(user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: None,
    };

    let v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");

    assert_same_signal_set("node_exporter_cpu", &v1_entries, &v2_expanded.entries);
    assert_v2_ids_are_unique("node_exporter_cpu", &v2_expanded.entries);
}

// =============================================================================
// 17.3 — node_exporter_memory parity
// =============================================================================

#[test]
fn parity_node_exporter_memory() {
    let pack = load_pack("node-exporter-memory.yaml");
    let resolver = resolver_with("node_exporter_memory", pack.clone());
    let yaml = parity_fixture("node-exporter-memory.yaml");

    let v2_expanded = v2_compile(&yaml, &resolver);

    let mut user_labels = HashMap::new();
    user_labels.insert("instance".to_string(), "web-01:9100".to_string());

    let mut override_labels = BTreeMap::new();
    override_labels.insert("owner".to_string(), "platform".to_string());
    let mut overrides = HashMap::new();
    overrides.insert(
        "node_memory_MemFree_bytes".to_string(),
        MetricOverride {
            generator: None,
            labels: Some(override_labels),
            after: None,
        },
    );

    let v1_config = PackScenarioConfig {
        pack: "node_exporter_memory".to_string(),
        rate: 1.0,
        duration: Some("30s".to_string()),
        labels: Some(user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: Some(overrides),
    };

    let v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");

    assert_same_signal_set("node_exporter_memory", &v1_entries, &v2_expanded.entries);
    assert_v2_ids_are_unique("node_exporter_memory", &v2_expanded.entries);
}

// =============================================================================
// 11.12 — after on pack override (per-metric dependency)
// 11.13 — pack entry-level after propagation (applies to all expanded signals)
// =============================================================================

fn find_compiled_by_id<'a>(entries: &'a [CompiledEntry], id: &str) -> &'a CompiledEntry {
    entries
        .iter()
        .find(|e| e.id.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("expected entry with id '{id}' in compiled output"))
}

/// Assert that an override-level `after` on a pack sub-signal resolves to
/// exactly that metric's `phase_offset`, leaving its sibling metrics with
/// no `after`-derived offset (matrix row 11.12).
#[test]
fn compile_after_on_pack_override_applies_per_metric() {
    let pack = load_pack("telegraf-snmp-interface.yaml");
    let resolver = resolver_with("telegraf_snmp_interface", pack);

    let yaml = r#"
version: 2

defaults:
  rate: 1
  duration: 5m

scenarios:
  - id: source_link
    signal_type: metrics
    name: primary_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: uplink
    signal_type: metrics
    pack: telegraf_snmp_interface
    overrides:
      ifOperStatus:
        after:
          ref: source_link
          op: "<"
          value: 1
"#;

    let parsed = parse(yaml).expect("parse");
    let normalized = normalize(parsed).expect("normalize");
    let expanded = expand(normalized, &resolver).expect("expand");
    let compiled = compile_after(expanded).expect("compile_after");

    let ifoper = find_compiled_by_id(&compiled.entries, "uplink.ifOperStatus");
    assert_eq!(
        ifoper.phase_offset.as_deref(),
        Some("1m"),
        "override-level after should land on ifOperStatus specifically"
    );

    // Sibling sub-signals in the same pack must NOT inherit the offset.
    let sibling = find_compiled_by_id(&compiled.entries, "uplink.ifHCInOctets");
    assert!(
        sibling.phase_offset.is_none(),
        "sibling pack metrics should not inherit an override-level after"
    );
}

/// Assert that entry-level `after` on a pack entry propagates to every
/// expanded sub-signal (matrix row 11.13).
#[test]
fn compile_after_pack_entry_level_propagates_to_all_sub_signals() {
    let pack = load_pack("telegraf-snmp-interface.yaml");
    let resolver = resolver_with("telegraf_snmp_interface", pack);

    let yaml = r#"
version: 2

defaults:
  rate: 1
  duration: 5m

scenarios:
  - id: source_link
    signal_type: metrics
    name: primary_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: uplink
    signal_type: metrics
    pack: telegraf_snmp_interface
    after:
      ref: source_link
      op: "<"
      value: 1
"#;

    let parsed = parse(yaml).expect("parse");
    let normalized = normalize(parsed).expect("normalize");
    let expanded = expand(normalized, &resolver).expect("expand");

    // Entry-level `after` must have been propagated to every pack metric
    // in the expanded representation.
    let expected_clause = AfterClause {
        ref_id: "source_link".to_string(),
        op: AfterOp::LessThan,
        value: 1.0,
        delay: None,
    };
    for entry in &expanded.entries {
        if entry
            .id
            .as_deref()
            .is_some_and(|id| id.starts_with("uplink."))
        {
            let clause = entry
                .after
                .as_ref()
                .unwrap_or_else(|| panic!("pack sub-signal {:?} missing after", entry.id));
            assert_eq!(clause.ref_id, expected_clause.ref_id);
            assert_eq!(clause.op, expected_clause.op);
            assert!((clause.value - expected_clause.value).abs() < f64::EPSILON);
        }
    }

    let compiled = compile_after(expanded).expect("compile_after");

    // After compilation every sub-signal shares the same resolved offset.
    let pack_ids = [
        "uplink.ifOperStatus",
        "uplink.ifHCInOctets",
        "uplink.ifHCOutOctets",
        "uplink.ifInErrors",
        "uplink.ifOutErrors",
    ];
    for id in pack_ids {
        let entry = find_compiled_by_id(&compiled.entries, id);
        assert_eq!(
            entry.phase_offset.as_deref(),
            Some("1m"),
            "{id} should inherit propagated after offset"
        );
    }

    // The whole chain (source + pack sub-signals) is a single connected
    // component, so they share the auto-assigned clock group.
    let source = find_compiled_by_id(&compiled.entries, "source_link");
    let ifoper = find_compiled_by_id(&compiled.entries, "uplink.ifOperStatus");
    assert!(source.clock_group.is_some());
    assert_eq!(source.clock_group, ifoper.clock_group);
}
