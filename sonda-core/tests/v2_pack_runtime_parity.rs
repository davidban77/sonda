#![cfg(feature = "config")]
//! Pack runtime parity for built-in packs (validation matrix rows 17.1–17.3).
//!
//! For each built-in pack, drive a scenario through:
//!
//! 1. v1 path — [`sonda_core::packs::expand_pack`] against a
//!    [`PackScenarioConfig`] whose fields match the v2 fixture.
//! 2. v2 path — the new [`sonda_core::compile_scenario_file`] one-shot
//!    against the hand-written `tests/fixtures/v2-parity/<pack>.yaml`.
//!
//! Both sides feed their `Vec<ScenarioEntry>` through
//! [`common::run_and_capture_stdout`] and compare line multisets (packs
//! always expand into >1 concurrent metric signal per pack entry).
//!
//! Runtime byte equality is the extension of the existing compile parity
//! (rows 17.1–17.3) in `v2_pack_parity.rs` — that file asserts shape
//! equivalence, this one asserts the runtime produces identical bytes for
//! that same shape.

use std::collections::HashMap;

use sonda_core::config::ScenarioEntry;
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::packs::{expand_pack, MetricOverride, PackScenarioConfig};
use sonda_core::sink::SinkConfig;

mod common;

use common::{
    assert_line_multisets_equal, load_repo_pack, normalize_timestamps, parity_fixture,
    resolver_with, run_and_capture_stdout,
};

/// Compile the named v2 parity fixture and override every entry's
/// `duration` to `override_duration` so the runtime-parity harness stays
/// fast.
fn v2_entries_with_duration(fixture: &str, override_duration: &str) -> Vec<ScenarioEntry> {
    let yaml = parity_fixture(fixture);
    let resolver = {
        let pack_file = match fixture {
            "telegraf-snmp-interface.yaml" => "telegraf-snmp-interface.yaml",
            "node-exporter-cpu.yaml" => "node-exporter-cpu.yaml",
            "node-exporter-memory.yaml" => "node-exporter-memory.yaml",
            other => panic!("unknown parity fixture {other}"),
        };
        let pack_name = match fixture {
            "telegraf-snmp-interface.yaml" => "telegraf_snmp_interface",
            "node-exporter-cpu.yaml" => "node_exporter_cpu",
            "node-exporter-memory.yaml" => "node_exporter_memory",
            _ => unreachable!(),
        };
        resolver_with(pack_name, load_repo_pack(pack_file))
    };
    let mut entries =
        sonda_core::compile_scenario_file(&yaml, &resolver).expect("v2 compile must succeed");
    for entry in &mut entries {
        let base = match entry {
            ScenarioEntry::Metrics(c) => &mut c.base,
            ScenarioEntry::Logs(c) => &mut c.base,
            ScenarioEntry::Histogram(c) => &mut c.base,
            ScenarioEntry::Summary(c) => &mut c.base,
        };
        base.duration = Some(override_duration.to_string());
    }
    entries
}

/// Replace every entry's `duration` with `override_duration`. Used on the
/// v1 `expand_pack` output to match the shortened v2 window.
fn apply_duration_override(entries: &mut [ScenarioEntry], override_duration: &str) {
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

// =============================================================================
// 17.1 — telegraf_snmp_interface runtime parity
// =============================================================================

/// Byte-identical stdout after expanding the telegraf_snmp_interface pack
/// through both v1 `expand_pack` and v2 `compile_scenario_file`.
#[test]
fn runtime_parity_telegraf_snmp_interface() {
    let pack = load_repo_pack("telegraf-snmp-interface.yaml");
    let duration = "500ms";

    // v1 config mirrors the v2 fixture exactly.
    let mut v1_user_labels = HashMap::new();
    v1_user_labels.insert("device".to_string(), "rtr-edge-01".to_string());
    v1_user_labels.insert("ifName".to_string(), "GigabitEthernet0/0/0".to_string());
    v1_user_labels.insert("ifIndex".to_string(), "1".to_string());

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
        duration: Some(duration.to_string()),
        labels: Some(v1_user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: Some(overrides),
    };

    let mut v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");
    apply_duration_override(&mut v1_entries, duration);

    let v2_entries = v2_entries_with_duration("telegraf-snmp-interface.yaml", duration);

    let v1_bytes = run_and_capture_stdout(v1_entries);
    let v2_bytes = run_and_capture_stdout(v2_entries);

    assert_line_multisets_equal(
        "telegraf_snmp_interface runtime",
        &normalize_timestamps(&v1_bytes),
        &normalize_timestamps(&v2_bytes),
    );
}

// =============================================================================
// 17.2 — node_exporter_cpu runtime parity
// =============================================================================

/// Byte-identical stdout after expanding the node_exporter_cpu pack
/// through both paths.
#[test]
fn runtime_parity_node_exporter_cpu() {
    let pack = load_repo_pack("node-exporter-cpu.yaml");
    let duration = "500ms";

    let mut v1_user_labels = HashMap::new();
    v1_user_labels.insert("instance".to_string(), "web-01:9100".to_string());

    let v1_config = PackScenarioConfig {
        pack: "node_exporter_cpu".to_string(),
        rate: 1.0,
        duration: Some(duration.to_string()),
        labels: Some(v1_user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: None,
    };

    let mut v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");
    apply_duration_override(&mut v1_entries, duration);

    let v2_entries = v2_entries_with_duration("node-exporter-cpu.yaml", duration);

    let v1_bytes = run_and_capture_stdout(v1_entries);
    let v2_bytes = run_and_capture_stdout(v2_entries);

    assert_line_multisets_equal(
        "node_exporter_cpu runtime",
        &normalize_timestamps(&v1_bytes),
        &normalize_timestamps(&v2_bytes),
    );
}

// =============================================================================
// 17.3 — node_exporter_memory runtime parity
// =============================================================================

/// Byte-identical stdout after expanding the node_exporter_memory pack
/// through both paths, including an override that adds a label to one
/// sub-signal.
#[test]
fn runtime_parity_node_exporter_memory() {
    use std::collections::BTreeMap;

    let pack = load_repo_pack("node-exporter-memory.yaml");
    let duration = "500ms";

    let mut v1_user_labels = HashMap::new();
    v1_user_labels.insert("instance".to_string(), "web-01:9100".to_string());

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
        duration: Some(duration.to_string()),
        labels: Some(v1_user_labels),
        sink: SinkConfig::Stdout,
        encoder: EncoderConfig::PrometheusText { precision: None },
        overrides: Some(overrides),
    };

    let mut v1_entries = expand_pack(&pack, &v1_config).expect("v1 expansion must succeed");
    apply_duration_override(&mut v1_entries, duration);

    let v2_entries = v2_entries_with_duration("node-exporter-memory.yaml", duration);

    let v1_bytes = run_and_capture_stdout(v1_entries);
    let v2_bytes = run_and_capture_stdout(v2_entries);

    assert_line_multisets_equal(
        "node_exporter_memory runtime",
        &normalize_timestamps(&v1_bytes),
        &normalize_timestamps(&v2_bytes),
    );
}
