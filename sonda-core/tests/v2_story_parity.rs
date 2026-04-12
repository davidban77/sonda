#![cfg(feature = "config")]
//! Story → v2 compile-parity bridge (validation matrix row 16.12).
//!
//! The v1 `sonda story --file` path and the v2 scenario pipeline both use
//! the same timing math in `sonda_core::compiler::timing`, so identical
//! input must produce identical `phase_offset` values on equivalent
//! signals. This test encodes the expected offsets for the built-in
//! `stories/link-failover.yaml` story and asserts the v2 compile produces
//! them to millisecond precision — the common precision to which both v1
//! and v2 round-trip their offsets through `format_duration_secs`.
//!
//! Runtime parity (identical stdout output for the whole story) is PR 6
//! scope; this file asserts compile-time equivalence only.

mod common;

use common::parity_fixture;
use sonda_core::compiler::compile_after::compile_after;
use sonda_core::compiler::expand::{expand, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::compiler::timing::{flap_crossing_secs, sawtooth_crossing_secs, Operator};

/// Compile the v2 link-failover equivalent and compare every signal's
/// `phase_offset` to the value produced by applying the v1 story math
/// (same `timing::*_crossing_secs` functions) manually.
///
/// Step-by-step expected offsets from the story definition:
///
/// - `interface_oper_state` (flap up=60s, down=30s) → no `after`, offset 0.
/// - `backup_link_utilization` depends on `interface_oper_state < 1`:
///   `flap_crossing_secs(<, 1, up=60s, down=30s, up=1, down=0) = 60s`.
///   Its total offset is 60s.
/// - `latency_ms` depends on `backup_link_utilization > 70`:
///   `sawtooth_crossing_secs(>, 70, baseline=20, ceiling=85, period=120s) =
///   (70-20)/(85-20)*120 ≈ 92.307s`.
///   Accumulated with its parent's 60s, total ≈ 152.308s.
#[test]
fn link_failover_compile_parity() {
    // ------------------------------------------------------------------
    // v1-equivalent offsets via the shared timing module.
    // ------------------------------------------------------------------
    let v1_interface_oper_state_secs = 0.0;
    let v1_backup_crossing = flap_crossing_secs(Operator::LessThan, 1.0, 60.0, 30.0, 1.0, 0.0)
        .expect("flap crossing for '< 1' must succeed");
    let v1_backup_total_secs = v1_interface_oper_state_secs + v1_backup_crossing;

    let v1_latency_crossing =
        sawtooth_crossing_secs(Operator::GreaterThan, 70.0, 20.0, 85.0, 120.0)
            .expect("sawtooth crossing for '> 70' must succeed");
    let v1_latency_total_secs = v1_backup_total_secs + v1_latency_crossing;

    // ------------------------------------------------------------------
    // v2 compile of the hand-written parity equivalent.
    // ------------------------------------------------------------------
    let yaml = parity_fixture("link-failover.yaml");
    let resolver = InMemoryPackResolver::new();
    let parsed = parse(&yaml).expect("fixture parses");
    let normalized = normalize(parsed).expect("fixture normalizes");
    let expanded = expand(normalized, &resolver).expect("fixture expands");
    let compiled = compile_after(expanded).expect("fixture compiles after");

    assert_eq!(compiled.entries.len(), 3);

    let iface = &compiled.entries[0];
    let backup = &compiled.entries[1];
    let latency = &compiled.entries[2];

    assert_eq!(iface.id.as_deref(), Some("interface_oper_state"));
    assert_eq!(backup.id.as_deref(), Some("backup_link_utilization"));
    assert_eq!(latency.id.as_deref(), Some("latency_ms"));

    // Parse the compiled phase_offset strings back to seconds and compare
    // against the v1 reference values.
    assert!(iface.phase_offset.is_none());
    assert_eq!(
        parse_offset_secs(backup.phase_offset.as_deref()),
        v1_backup_total_secs,
        "backup_link_utilization offset should match v1 story math"
    );

    // Millisecond tolerance: `phase_offset` strings are formatted with
    // millisecond precision (both v1 `format_duration_secs` and the v2
    // compiler's `format_duration_secs`), so the round-tripped value is
    // snapped to the nearest ms.
    let v2_latency_secs = parse_offset_secs(latency.phase_offset.as_deref());
    assert!(
        (v2_latency_secs - v1_latency_total_secs).abs() < 1e-3,
        "latency_ms offset mismatch: v2={v2_latency_secs}, v1={v1_latency_total_secs}"
    );

    // All three share the auto-assigned clock group keyed on the
    // lex-smallest id in the component.
    let expected_group = "chain_backup_link_utilization";
    assert_eq!(iface.clock_group.as_deref(), Some(expected_group));
    assert_eq!(backup.clock_group.as_deref(), Some(expected_group));
    assert_eq!(latency.clock_group.as_deref(), Some(expected_group));
}

/// Parse a `phase_offset` string back to fractional seconds for
/// tolerance-friendly comparisons.
fn parse_offset_secs(s: Option<&str>) -> f64 {
    match s {
        None => 0.0,
        Some(s) => sonda_core::config::validate::parse_duration(s)
            .unwrap_or_else(|e| panic!("parse_duration({s:?}) failed: {e}"))
            .as_secs_f64(),
    }
}
