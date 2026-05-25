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
