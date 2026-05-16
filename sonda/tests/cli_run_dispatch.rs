//! Integration tests for `sonda run` v2-only dispatch.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

#[test]
fn run_v1_scenario_is_rejected_with_migration_hint() {
    let fixture = cli_fixtures_dir().join("inline-v1.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "v1 multi-scenario must not succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v2"),
        "rejection must mention v2 requirement, got:\n{stderr}"
    );
}

#[test]
fn run_v2_scenario_succeeds() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
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

#[test]
fn run_v2_dry_run_emits_spec_pretty_output() {
    let fixture = cli_fixtures_dir().join("multi-after-chain.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
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
    assert!(
        stderr.contains("[config] file:") && stderr.contains("version: 2"),
        "missing v2 header in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Validation: OK"),
        "missing validation footer:\n{stderr}"
    );
    assert!(
        stderr.contains("phase_offset:"),
        "missing phase_offset annotation:\n{stderr}"
    );
    assert!(
        stderr.contains("clock_group:") && stderr.contains("(auto)"),
        "missing auto clock_group line:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "dry-run must not write to stdout, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn run_v2_dry_run_json_format_emits_stable_dto() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
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

#[test]
fn run_flat_v1_single_scenario_is_rejected_with_migration_hint() {
    let fixture = cli_fixtures_dir().join("flat-v1-metrics.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "flat v1 file must not succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v2"),
        "rejection must mention v2 requirement, got:\n{stderr}"
    );
}

#[test]
fn v2_compile_error_surfaces_with_context() {
    let fixture = cli_fixtures_dir().join("broken-self-ref.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
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
