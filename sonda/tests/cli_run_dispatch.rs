//! Integration tests for `sonda run --scenario` v1/v2 dispatch (PR 7).
//!
//! Verifies that `sonda run` accepts both v1 and v2 scenario files
//! transparently, that `--dry-run` on v2 files emits the spec §5 pretty
//! output, and that `--format=json` emits a stable JSON DTO.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

/// v1 single-scenario file (no `version:`) runs end-to-end.
///
/// Uses the `inline-v1.yaml` fixture with a 300ms duration so the test
/// completes quickly.
#[test]
fn run_v1_scenario_succeeds() {
    let fixture = cli_fixtures_dir().join("inline-v1.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run", "--scenario"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v1 run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    // stdout should contain Prometheus text output for the v1 metric.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("v1_inline_metric"),
        "expected v1 metric name in stdout, got:\n{stdout}"
    );
}

/// v2 scenario file (`version: 2`) runs end-to-end via the v2 compile
/// pipeline.
#[test]
fn run_v2_scenario_succeeds() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run", "--scenario"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("v2_inline_metric"),
        "expected v2 metric name in stdout, got:\n{stdout}"
    );
}

/// `--dry-run` on a v2 file emits the spec §5 pretty output to stderr
/// and exits 0 without spawning any runners.
#[test]
fn run_v2_dry_run_emits_spec_pretty_output() {
    let fixture = cli_fixtures_dir().join("multi-after-chain.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run", "--scenario"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 dry-run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Spec §5 format markers.
    assert!(
        stderr.contains("[config] file:") && stderr.contains("version: 2"),
        "missing v2 header in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Validation: OK"),
        "missing validation footer:\n{stderr}"
    );
    // The after-chain produces a resolved phase_offset on `backup_util`.
    assert!(
        stderr.contains("phase_offset:"),
        "missing phase_offset annotation:\n{stderr}"
    );
    // Connected-component auto clock_group.
    assert!(
        stderr.contains("clock_group:") && stderr.contains("(auto)"),
        "missing auto clock_group line:\n{stderr}"
    );
    // Stdout should be empty (dry-run emits no events).
    assert!(
        output.stdout.is_empty(),
        "dry-run must not write to stdout, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `--dry-run --format=json` emits a stable JSON DTO on stdout.
#[test]
fn run_v2_dry_run_json_format_emits_stable_dto() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run", "--scenario"])
        .arg(&fixture)
        .args(["--dry-run", "--format=json"])
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 dry-run --format=json failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json output must parse");
    assert_eq!(json["version"], 2);
    assert_eq!(json["scenarios"][0]["name"], "v2_inline_metric");
    assert_eq!(json["scenarios"][0]["signal"], "metrics");
}

/// A flat v1 single-scenario file (top-level `name:` + `generator:`,
/// no `scenarios:` list) runs end-to-end. Spec §6.1 requires `sonda run
/// --scenario` to handle v1 single-scenario, multi-scenario, and
/// pack-scenario layouts transparently; PR 7's unified loader originally
/// only handled the multi and pack shapes.
#[test]
fn run_flat_v1_single_scenario_succeeds() {
    let fixture = cli_fixtures_dir().join("flat-v1-metrics.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run", "--scenario"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "flat v1 run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("flat_v1_metric"),
        "expected flat v1 metric name in stdout, got:\n{stdout}"
    );
}

/// `sonda catalog run <builtin-scenario>` resolves a flat v1 built-in
/// scenario like `cpu-spike` end-to-end. This is the end-to-end flow
/// the docs describe (`sonda catalog run cpu-spike`). The catalog
/// search path is pointed at the repo's `scenarios/` tree so the test
/// runs against the real built-in YAMLs.
#[test]
fn catalog_run_cpu_spike_builtin_succeeds() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate has a parent")
        .to_path_buf();
    let scenarios_dir = repo_root.join("scenarios");
    if !scenarios_dir.exists() {
        // Defensive: should always exist in the workspace. Skip
        // silently if a stripped-down checkout removed it.
        eprintln!("skipping: {} missing", scenarios_dir.display());
        return;
    }

    let output = Command::new(sonda_bin())
        .args(["--quiet", "--scenario-path"])
        .arg(&scenarios_dir)
        .args([
            "catalog",
            "run",
            "cpu-spike",
            "--duration",
            "300ms",
            "--rate",
            "1",
        ])
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "catalog run cpu-spike failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("node_cpu_usage_percent"),
        "expected cpu-spike metric name in stdout, got:\n{stdout}"
    );
}

/// v2 compile errors surface with actionable diagnostics: non-zero exit
/// plus stderr containing the source path or a typed error marker.
#[test]
fn v2_compile_error_surfaces_with_context() {
    let fixture = cli_fixtures_dir().join("broken-self-ref.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run", "--scenario"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "self-ref must produce non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broken-self-ref.v2.yaml") || stderr.to_lowercase().contains("self"),
        "error must identify the source file or the self-reference, got:\n{stderr}"
    );
}
