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
        .args(["--catalog"])
        .arg(cli_fixtures_dir().join("catalog-packs"))
        .args(["run"])
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

/// The gnmi encoder paired with the gnmi_target sink validates, and the
/// dry-run names both. A dry run binds nothing, so the fixed port is never
/// opened.
#[cfg(feature = "gnmi")]
#[test]
fn dry_run_shows_the_gnmi_encoder_and_the_gnmi_target_sink() {
    let stderr = dry_run_stderr("gnmi.v2.yaml");
    assert!(stderr.contains("gnmi (openconfig-interfaces)"), "{stderr}");
    assert!(stderr.contains("gnmi_target (127.0.0.1:57400)"), "{stderr}");
    assert!(stderr.contains("Validation: OK (1 scenario)"), "{stderr}");
}

/// The same valid gnmi block on a stdout sink is rejected by the pairing
/// rule, which runs only after the block itself has validated, so the
/// message proves both.
#[cfg(feature = "gnmi")]
#[test]
fn dry_run_rejects_the_gnmi_encoder_without_a_gnmi_target_sink() {
    let source = std::fs::read_to_string(cli_fixtures_dir().join("gnmi.v2.yaml"))
        .expect("read the gnmi fixture");
    let sink = "  sink:\n    type: gnmi_target\n    listen: 127.0.0.1:57400\n";
    assert!(
        source.contains(sink),
        "the fixture must carry the sink block"
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("gnmi-stdout.v2.yaml");
    std::fs::write(
        &fixture,
        source.replace(sink, "  sink:\n    type: stdout\n"),
    )
    .expect("write the stdout variant");

    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("the gnmi encoder can only be used with the gnmi_target sink"),
        "{stderr}"
    );
}

/// Dry-run `fixture` and return its stderr, asserting it failed.
#[cfg(feature = "gnmi")]
fn failed_dry_run_stderr(fixture: &str) -> String {
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(cli_fixtures_dir().join(fixture))
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{stderr}");
    stderr
}

/// The same `values:` typo fails the dry-run when its entry is gated by
/// `while:`, which takes the gated launch path, and when it is chained with
/// `after:`, which does not.
#[cfg(feature = "gnmi")]
#[test]
fn dry_run_rejects_a_gnmi_values_key_typo_with_while_and_after() {
    for (fixture, clause) in [
        ("gnmi-values-key-typo-while.v2.yaml", "while:"),
        ("gnmi-values-key-typo-after.v2.yaml", "after:"),
    ] {
        let source = std::fs::read_to_string(cli_fixtures_dir().join(fixture))
            .expect("fixture must be readable");
        assert!(source.contains(clause), "{fixture} must use {clause}");

        let stderr = failed_dry_run_stderr(fixture);
        assert!(
            stderr.contains(
                "scenario[1]: gnmi encoder values: key `in_octet` names no metric this encoder serves"
            ),
            "{fixture}: {stderr}"
        );
        assert!(
            stderr.contains("did you mean `in_octets`?"),
            "{fixture}: {stderr}"
        );
    }
}

/// A `values:` key that names no metric in the file fails the dry-run, naming
/// the key and the metric it was probably meant to be.
#[cfg(feature = "gnmi")]
#[test]
fn dry_run_rejects_a_gnmi_values_key_naming_no_metric() {
    let fixture = cli_fixtures_dir().join("gnmi-values-key-typo.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("values: key `in_octet`"), "{stderr}");
    assert!(stderr.contains("did you mean `in_octets`?"), "{stderr}");
}
