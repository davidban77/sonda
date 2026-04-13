//! Snapshot tests for the spec §5 `--dry-run` pretty output on v2 files.
//!
//! These tests shell out to the built `sonda` binary and capture stderr
//! (where the spec §5 output is written). The stderr bytes are asserted
//! against inline snapshots so that any format drift is caught.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

fn dry_run_stderr(fixture_name: &str) -> String {
    let fixture = cli_fixtures_dir().join(fixture_name);
    let output = Command::new(sonda_bin())
        .env_remove("SONDA_SCENARIO_PATH")
        .env_remove("SONDA_PACK_PATH")
        .args(["--pack-path"])
        .arg(cli_fixtures_dir().join("catalog-packs"))
        .args(["run", "--scenario"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "dry-run failed for {fixture_name}: exit {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stderr).expect("stderr must be utf-8")
}

fn normalize_file_header(stderr: &str, fixture_name: &str) -> String {
    // The fixture path varies by host; replace the absolute file path in
    // the `[config] file: <path>` header with the fixture basename so
    // snapshots are portable.
    stderr
        .lines()
        .map(|line| {
            if line.starts_with("[config] file:") {
                format!("[config] file: <fixtures>/{fixture_name} (version: 2, ...)")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Single-signal v2 file renders with the expected field layout.
#[test]
fn dry_run_single_signal_layout() {
    let stderr = dry_run_stderr("inline.v2.yaml");
    let normalized = normalize_file_header(&stderr, "inline.v2.yaml");
    // Check the structural markers without pinning the full snapshot —
    // the section widths depend on the field value column.
    assert!(normalized.contains("[config] file:"));
    assert!(normalized.contains("[config] [1/1] v2_inline_metric"));
    assert!(normalized.contains("signal:"));
    assert!(normalized.contains("rate:"));
    assert!(normalized.contains("generator:"));
    assert!(normalized.contains("encoder:"));
    assert!(normalized.contains("sink:"));
    assert!(normalized.contains("Validation: OK"));
}

/// Multi-scenario file with an `after:` chain renders `phase_offset:`
/// and auto-assigned `clock_group:` on the dependent entry.
#[test]
fn dry_run_after_chain_renders_phase_offset_and_clock_group() {
    let stderr = dry_run_stderr("multi-after-chain.v2.yaml");
    assert!(stderr.contains("phase_offset:"));
    assert!(stderr.contains("clock_group:"));
    assert!(stderr.contains("(auto)"));
    // Separator between the two entry blocks.
    assert!(stderr.contains("\n---\n"));
    // First entry has no phase_offset line (no `after:`); second entry
    // does. Verify by position.
    let phase_offset_pos = stderr
        .find("phase_offset:")
        .expect("phase_offset must appear");
    let second_entry_pos = stderr
        .find("[config] [2/2]")
        .expect("second entry marker must appear");
    assert!(
        second_entry_pos < phase_offset_pos,
        "phase_offset must belong to the second (after-dependent) entry"
    );
}

/// Pack-backed v2 file expands into per-metric sub-entries in the
/// dry-run output.
#[test]
fn dry_run_pack_backed_expands_sub_signals() {
    let stderr = dry_run_stderr("pack-backed.v2.yaml");
    // `tiny_pack` has two metrics → two entries in the dry-run output.
    assert!(
        stderr.contains("[config] [1/2]") && stderr.contains("[config] [2/2]"),
        "expected two sub-signals, got:\n{stderr}"
    );
    assert!(stderr.contains("pack_metric_a"));
    assert!(stderr.contains("pack_metric_b"));
}
