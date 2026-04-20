#![cfg(feature = "config")]
//! Pack expansion and runtime integration tests for built-in packs
//! (validation matrix rows 17.1–17.3).
//!
//! Each built-in pack gets exercised twice:
//!
//! 1. **Compile**: parse + normalize + expand the v2 YAML parity fixture and
//!    assert the shape of the resulting [`ExpandedFile`] — entry count,
//!    sub-signal ids, composed labels, and override passthrough.
//! 2. **Runtime**: drive every expanded entry through the in-process
//!    [`common::run_and_capture_stdout`] harness (same path production
//!    runners take) and assert the encoded byte stream contains one metric
//!    name per pack metric at least once.
//!
//! This file replaces the former `v2_pack_parity.rs` + `v2_pack_runtime_parity.rs`
//! split. The v1 parity oracle (`sonda_core::packs::expand_pack` invoked
//! from a hand-built `PackScenarioConfig`) was retired alongside v1 —
//! every test here is strictly a v2-path assertion.
//!
//! The two override/after-propagation tests (matrix rows 11.12 and 11.13)
//! sit at the bottom: they are v2-only semantics and do not depend on any
//! oracle at all.

use std::collections::BTreeSet;

use sonda_core::compiler::compile_after::{compile_after, CompiledEntry};
use sonda_core::compiler::expand::ExpandedEntry;
use sonda_core::compiler::{AfterClause, AfterOp};
use sonda_core::config::ScenarioEntry;

mod common;

use common::{
    builtin_pack_resolver, compile_to_expanded, load_repo_pack, normalize_timestamps,
    parity_fixture, resolver_with, run_and_capture_stdout,
};

// =============================================================================
// Shared helpers
// =============================================================================

/// Collect the set of metric names present in an expanded pack output.
fn metric_names(entries: &[ExpandedEntry]) -> BTreeSet<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

/// Override every entry's `duration` so the runtime-parity harness stays
/// fast. `ScenarioEntry` does not expose a `base_mut` accessor, so match
/// once per variant here.
fn shorten_duration(entries: &mut [ScenarioEntry], duration: &str) {
    for entry in entries {
        let base = match entry {
            ScenarioEntry::Metrics(c) => &mut c.base,
            ScenarioEntry::Logs(c) => &mut c.base,
            ScenarioEntry::Histogram(c) => &mut c.base,
            ScenarioEntry::Summary(c) => &mut c.base,
        };
        base.duration = Some(duration.to_string());
    }
}

/// Assert every entry in `entries` carries a non-empty, distinct `id`.
///
/// Regression anchor for the `node_exporter_cpu` pack, which ships eight
/// `MetricSpec` entries all named `node_cpu_seconds_total` — each must
/// expand into a unique sub-signal id.
fn assert_ids_are_unique(label: &str, entries: &[ExpandedEntry]) {
    let ids: Vec<&str> = entries
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

/// Collect the set of metric names observed in a Prometheus text byte
/// stream. The runtime harness writes one sample per tick per metric, so
/// the first token on each non-empty line is the metric name.
fn metric_names_in_prometheus_output(bytes: &[u8]) -> BTreeSet<String> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let end = line
                .iter()
                .position(|&b| b == b' ' || b == b'{')
                .unwrap_or(line.len());
            String::from_utf8_lossy(&line[..end]).into_owned()
        })
        .collect()
}

// =============================================================================
// 17.1 — telegraf_snmp_interface
// =============================================================================

/// Compile parity: the v2 fixture expands into 5 sub-signals (one per pack
/// metric), each carrying the user-supplied labels and the
/// `ifOperStatus` override replacing the default constant with a flap
/// generator.
#[test]
fn compile_telegraf_snmp_interface() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("telegraf-snmp-interface.yaml");
    let expanded = compile_to_expanded(&yaml, &resolver);

    assert_eq!(
        expanded.entries.len(),
        5,
        "telegraf_snmp_interface ships 5 metrics"
    );
    assert_ids_are_unique("telegraf_snmp_interface", &expanded.entries);

    // Every pack metric is present exactly once by name.
    let names = metric_names(&expanded.entries);
    for expected in [
        "ifOperStatus",
        "ifHCInOctets",
        "ifHCOutOctets",
        "ifInErrors",
        "ifOutErrors",
    ] {
        assert!(names.contains(expected), "missing metric {expected}");
    }

    // User labels are composed onto every sub-signal via the precedence
    // chain (defaults → shared → per-metric → entry → override).
    for entry in &expanded.entries {
        let labels = entry
            .labels
            .as_ref()
            .unwrap_or_else(|| panic!("entry {:?} has no labels", entry.id));
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("rtr-edge-01")
        );
        assert_eq!(
            labels.get("ifName").map(String::as_str),
            Some("GigabitEthernet0/0/0")
        );
        assert_eq!(labels.get("ifIndex").map(String::as_str), Some("1"));
    }

    // The `ifOperStatus` override replaces the pack's default generator
    // with `flap`. Every other metric keeps the pack-defined generator.
    let ifoper = expanded
        .entries
        .iter()
        .find(|e| e.name == "ifOperStatus")
        .expect("ifOperStatus present");
    let ifoper_gen = ifoper.generator.as_ref().expect("generator present");
    let ifoper_json = serde_json::to_string(ifoper_gen).expect("serialize generator");
    assert!(
        ifoper_json.contains("\"flap\""),
        "ifOperStatus override must set flap generator, got {ifoper_json}"
    );
}

/// Runtime parity: shortening the scenario to `500ms` and running every
/// entry end-to-end yields a byte stream that carries every pack metric
/// name at least once.
#[test]
fn runtime_telegraf_snmp_interface() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("telegraf-snmp-interface.yaml");
    let mut entries =
        sonda_core::compile_scenario_file(&yaml, &resolver).expect("compile must succeed");
    // Shorten every entry to keep the test fast.
    shorten_duration(&mut entries, "500ms");

    let bytes = run_and_capture_stdout(entries);
    let bytes = normalize_timestamps(&bytes);
    assert!(!bytes.is_empty(), "runtime produced no output");

    let observed = metric_names_in_prometheus_output(&bytes);
    for expected in [
        "ifOperStatus",
        "ifHCInOctets",
        "ifHCOutOctets",
        "ifInErrors",
        "ifOutErrors",
    ] {
        assert!(
            observed.contains(expected),
            "runtime output missing metric {expected}; saw {observed:?}"
        );
    }
}

// =============================================================================
// 17.2 — node_exporter_cpu
// =============================================================================

/// Compile parity: the node_exporter_cpu pack ships eight `MetricSpec`
/// entries all named `node_cpu_seconds_total`. They must expand into
/// eight sub-signals with distinct ids (the pack-index suffix mechanism).
#[test]
fn compile_node_exporter_cpu() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("node-exporter-cpu.yaml");
    let expanded = compile_to_expanded(&yaml, &resolver);

    assert_eq!(
        expanded.entries.len(),
        8,
        "node_exporter_cpu ships 8 per-mode metrics"
    );
    assert_ids_are_unique("node_exporter_cpu", &expanded.entries);

    // Every expanded entry has the same metric name — the pack
    // differentiates modes via labels.
    for entry in &expanded.entries {
        assert_eq!(entry.name, "node_cpu_seconds_total");
        let labels = entry
            .labels
            .as_ref()
            .unwrap_or_else(|| panic!("entry {:?} has no labels", entry.id));
        assert_eq!(
            labels.get("instance").map(String::as_str),
            Some("web-01:9100"),
            "user-supplied instance label must propagate"
        );
    }
}

/// Runtime parity: the eight sub-signals produce output containing the
/// metric name `node_cpu_seconds_total`.
#[test]
fn runtime_node_exporter_cpu() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("node-exporter-cpu.yaml");
    let mut entries =
        sonda_core::compile_scenario_file(&yaml, &resolver).expect("compile must succeed");
    shorten_duration(&mut entries, "500ms");

    let bytes = run_and_capture_stdout(entries);
    let bytes = normalize_timestamps(&bytes);
    assert!(!bytes.is_empty(), "runtime produced no output");

    let observed = metric_names_in_prometheus_output(&bytes);
    assert!(
        observed.contains("node_cpu_seconds_total"),
        "runtime output missing node_cpu_seconds_total; saw {observed:?}"
    );
}

// =============================================================================
// 17.3 — node_exporter_memory
// =============================================================================

/// Compile parity: node_exporter_memory expands into five memory gauge
/// sub-signals, and the per-metric override layers an `owner` label onto
/// `node_memory_MemFree_bytes` without affecting siblings.
#[test]
fn compile_node_exporter_memory() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("node-exporter-memory.yaml");
    let expanded = compile_to_expanded(&yaml, &resolver);

    assert_eq!(
        expanded.entries.len(),
        5,
        "node_exporter_memory ships 5 memory gauge metrics"
    );
    assert_ids_are_unique("node_exporter_memory", &expanded.entries);

    // Spot-check a few known metrics.
    let names = metric_names(&expanded.entries);
    for expected in [
        "node_memory_MemTotal_bytes",
        "node_memory_MemFree_bytes",
        "node_memory_MemAvailable_bytes",
    ] {
        assert!(names.contains(expected), "missing metric {expected}");
    }

    // Entry-level label must flow through every sub-signal.
    for entry in &expanded.entries {
        let labels = entry
            .labels
            .as_ref()
            .unwrap_or_else(|| panic!("entry {:?} has no labels", entry.id));
        assert_eq!(
            labels.get("instance").map(String::as_str),
            Some("web-01:9100")
        );
    }

    // Override `owner: platform` applies only to MemFree_bytes.
    let free = expanded
        .entries
        .iter()
        .find(|e| e.name == "node_memory_MemFree_bytes")
        .expect("MemFree_bytes present");
    assert_eq!(
        free.labels
            .as_ref()
            .and_then(|l| l.get("owner"))
            .map(String::as_str),
        Some("platform"),
        "override must add the owner label on MemFree_bytes"
    );

    for entry in &expanded.entries {
        if entry.name != "node_memory_MemFree_bytes" {
            assert!(
                entry
                    .labels
                    .as_ref()
                    .is_none_or(|l| !l.contains_key("owner")),
                "sibling {:?} must not inherit the owner override",
                entry.name
            );
        }
    }
}

/// Runtime parity: the five sub-signals produce output containing every
/// pack metric name.
#[test]
fn runtime_node_exporter_memory() {
    let resolver = builtin_pack_resolver();
    let yaml = parity_fixture("node-exporter-memory.yaml");
    let mut entries =
        sonda_core::compile_scenario_file(&yaml, &resolver).expect("compile must succeed");
    shorten_duration(&mut entries, "500ms");

    let bytes = run_and_capture_stdout(entries);
    let bytes = normalize_timestamps(&bytes);
    assert!(!bytes.is_empty(), "runtime produced no output");

    let observed = metric_names_in_prometheus_output(&bytes);
    for expected in [
        "node_memory_MemTotal_bytes",
        "node_memory_MemFree_bytes",
        "node_memory_MemAvailable_bytes",
        "node_memory_Buffers_bytes",
        "node_memory_Cached_bytes",
    ] {
        assert!(
            observed.contains(expected),
            "runtime output missing {expected}; saw {observed:?}"
        );
    }
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
    let pack = load_repo_pack("telegraf-snmp-interface.yaml");
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

    let compiled = common::compile_to_compiled(yaml, &resolver);

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
    let pack = load_repo_pack("telegraf-snmp-interface.yaml");
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

    let expanded = compile_to_expanded(yaml, &resolver);

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
