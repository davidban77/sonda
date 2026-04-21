//! Integration tests for `sonda init` v2 output.
//!
//! Every `--signal-type` variant must emit a v2 YAML file (`version: 2`
//! + `defaults:` + `scenarios:`) that round-trips through
//! `sonda run --scenario --dry-run` without error.

mod common;

use std::fs;
use std::process::Command;

use tempfile::TempDir;

use common::sonda_bin;

fn run_init(args: &[&str], out_path: &std::path::Path) {
    let mut cmd = Command::new(sonda_bin());
    cmd.env_remove("SONDA_SCENARIO_PATH");
    cmd.env_remove("SONDA_PACK_PATH");
    cmd.arg("init");
    cmd.args(args);
    cmd.arg("-o").arg(out_path);
    let output = cmd.output().expect("must spawn sonda init");
    assert!(
        output.status.success(),
        "sonda init failed: exit {:?}\nstderr:\n{}\nstdout:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
}

fn dry_run_emitted(out_path: &std::path::Path) {
    let output = Command::new(sonda_bin())
        .env_remove("SONDA_SCENARIO_PATH")
        .env_remove("SONDA_PACK_PATH")
        .args(["run", "--scenario"])
        .arg(out_path)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda run --dry-run");
    assert!(
        output.status.success(),
        "dry-run on init output failed: exit {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_v2_shape(yaml: &str) {
    assert!(yaml.contains("version: 2"), "missing version: 2:\n{yaml}");
    assert!(
        yaml.contains("defaults:"),
        "missing defaults block:\n{yaml}"
    );
    assert!(
        yaml.contains("scenarios:"),
        "missing scenarios block:\n{yaml}"
    );
}

/// Single metric: `--signal-type metrics` produces a v2 file that
/// dry-runs cleanly.
#[test]
fn init_single_metric_emits_v2() {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("single.yaml");

    run_init(
        &[
            "--signal-type",
            "metrics",
            "--domain",
            "infrastructure",
            "--metric",
            "cpu_usage",
            "--situation",
            "steady",
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--encoder",
            "prometheus_text",
            "--sink",
            "stdout",
        ],
        &out,
    );

    let yaml = fs::read_to_string(&out).expect("init must write file");
    assert_v2_shape(&yaml);
    dry_run_emitted(&out);
}

/// Logs: `--signal-type logs` produces a v2 file that dry-runs cleanly.
#[test]
fn init_logs_emits_v2() {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("logs.yaml");

    run_init(
        &[
            "--signal-type",
            "logs",
            "--domain",
            "application",
            "--metric",
            "app_logs",
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--encoder",
            "json_lines",
            "--sink",
            "stdout",
            "--message-template",
            "event happened",
            "--severity",
            "balanced",
        ],
        &out,
    );

    let yaml = fs::read_to_string(&out).expect("init must write file");
    assert_v2_shape(&yaml);
    assert!(
        yaml.contains("signal_type: logs"),
        "logs YAML must tag signal_type:\n{yaml}"
    );
    dry_run_emitted(&out);
}

/// Histogram: `--signal-type histogram` produces a v2 file that
/// dry-runs cleanly.
#[test]
fn init_histogram_emits_v2() {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("histogram.yaml");

    run_init(
        &[
            "--signal-type",
            "histogram",
            "--domain",
            "application",
            "--metric",
            "latency_seconds",
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--encoder",
            "prometheus_text",
            "--sink",
            "stdout",
        ],
        &out,
    );

    let yaml = fs::read_to_string(&out).expect("init must write file");
    assert_v2_shape(&yaml);
    assert!(
        yaml.contains("signal_type: histogram"),
        "histogram YAML must tag signal_type:\n{yaml}"
    );
    dry_run_emitted(&out);
}

/// Summary: `--signal-type summary` produces a v2 file that dry-runs
/// cleanly.
#[test]
fn init_summary_emits_v2() {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("summary.yaml");

    run_init(
        &[
            "--signal-type",
            "summary",
            "--domain",
            "application",
            "--metric",
            "rpc_latency_seconds",
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--encoder",
            "prometheus_text",
            "--sink",
            "stdout",
        ],
        &out,
    );

    let yaml = fs::read_to_string(&out).expect("init must write file");
    assert_v2_shape(&yaml);
    assert!(
        yaml.contains("signal_type: summary"),
        "summary YAML must tag signal_type:\n{yaml}"
    );
    dry_run_emitted(&out);
}
