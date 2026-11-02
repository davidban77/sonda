//! Integration tests for `sonda run`.

mod common;

use std::io::Write;
use std::process::Command;

use common::sonda_bin;
use tempfile::TempDir;

const RUNNABLE_YAML: &str = "version: 2
kind: runnable
scenario_name: cpu-spike
description: A CPU spike scenario
tags: [infrastructure, cpu]

defaults:
  rate: 5
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: m
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
";

fn write_catalog() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let p = dir.path().join("cpu-spike.yaml");
    std::fs::File::create(&p)
        .expect("create")
        .write_all(RUNNABLE_YAML.as_bytes())
        .expect("write");
    dir
}

#[test]
fn run_file_succeeds() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(RUNNABLE_YAML.as_bytes()).expect("write");
    f.flush().expect("flush");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(f.path())
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "run <file> must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu_usage"), "metric must appear in stdout");
}

#[test]
fn run_at_name_with_catalog_succeeds() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--catalog"])
        .arg(cat.path())
        .args(["run", "@cpu-spike"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "run @name must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu_usage"));
}

#[test]
fn run_at_name_without_catalog_errors() {
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run", "@cpu-spike"])
        .output()
        .expect("spawn sonda");
    assert!(
        !output.status.success(),
        "run @name without --catalog must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--catalog"),
        "error must mention --catalog, got: {stderr}"
    );
}

#[test]
fn run_at_unknown_name_errors_with_catalog_and_name() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--catalog"])
        .arg(cat.path())
        .args(["run", "@no-such-thing"])
        .output()
        .expect("spawn sonda");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no-such-thing"),
        "error must name missing entry, got: {stderr}"
    );
    assert!(
        stderr.contains(&cat.path().display().to_string()) || stderr.contains("available"),
        "error must mention catalog dir or list candidates, got: {stderr}"
    );
}

#[test]
fn run_at_name_with_overrides_applies_them() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--catalog"])
        .arg(cat.path())
        .args(["run", "@cpu-spike", "--duration", "100ms", "--rate", "20"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "overrides on @name must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const CROSS_POST_WHILE_YAML: &str = "version: 2
kind: runnable
scenario_name: downstream
defaults:
  duration: 200ms
scenarios:
  - id: tail
    signal_type: metrics
    name: tail_metric
    rate: 5
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_metric
      op: \">\"
      value: 0
      scenario_name: upstream
";

#[test]
fn run_rejects_cross_post_while_scenario_name() {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(CROSS_POST_WHILE_YAML.as_bytes())
        .expect("write");
    f.flush().expect("flush");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(f.path())
        .output()
        .expect("spawn sonda");
    assert!(
        !output.status.success(),
        "cross-POST while: in CLI must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires a running sonda-server"),
        "error must point at sonda-server, got: {stderr}"
    );
    assert!(
        stderr.contains("cross-post-while-refs"),
        "error must include docs link, got: {stderr}"
    );
}

/// A file with one `gap_windows:` entry whose offset is `at`, written as the
/// YAML scalar given, so `5` arrives as the text "5".
fn gap_window_yaml(at: &str) -> String {
    format!(
        "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - signal_type: metrics
    name: gap_window_check
    generator:
      type: constant
      value: 1
    gap_windows:
      - at: {at}
        for: 1s
"
    )
}

fn sonda_on_yaml(args: &[&str], yaml: &str) -> std::process::Output {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(yaml.as_bytes()).expect("write");
    f.flush().expect("flush");
    Command::new(sonda_bin())
        .args(args)
        .arg(f.path())
        .output()
        .expect("spawn sonda")
}

/// Whether `run` printed its start banner (the `▶ … signal_type:` line).
fn started(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.contains('\u{25b6}') && line.contains("signal_type:"))
}

const BAD_GAP_WINDOW_REFUSAL: &str = r#"invalid gap_windows[0].at "5": "#;

#[test]
fn dry_run_refuses_a_gap_window_offset_without_a_unit() {
    let control = sonda_on_yaml(&["--dry-run", "run"], &gap_window_yaml("5s"));
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert!(
        control.status.success() && control_stderr.contains("Validation: OK"),
        "the same file with `at: 5s` must pass dry-run; stderr:\n{control_stderr}"
    );

    let output = sonda_on_yaml(&["--dry-run", "run"], &gap_window_yaml("5"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dry-run must refuse `at: 5`; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(BAD_GAP_WINDOW_REFUSAL),
        "the refusal must name the entry, field and value; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Validation: OK"),
        "a refused file must not print Validation: OK; stderr:\n{stderr}"
    );
}

#[test]
fn run_refuses_a_gap_window_offset_without_a_unit_before_anything_starts() {
    let control = sonda_on_yaml(&["run"], &gap_window_yaml("5s"));
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert!(
        control.status.success() && started(&control_stderr),
        "the same file with `at: 5s` must start and finish; stderr:\n{control_stderr}"
    );

    let output = sonda_on_yaml(&["run"], &gap_window_yaml("5"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "run must refuse `at: 5`; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(BAD_GAP_WINDOW_REFUSAL),
        "the refusal must name the entry, field and value; stderr:\n{stderr}"
    );
    assert!(
        !started(&stderr),
        "the scenario must be refused before it starts; stderr:\n{stderr}"
    );
}
