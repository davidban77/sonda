//! Integration tests for `sonda list` and `sonda show`.

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
  rate: 1
  duration: 1s
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

const PACK_YAML: &str = "version: 2
kind: composable
scenario_name: tiny-pack
description: A small pack
tags: [network]

name: tiny_pack
category: network
metrics:
  - name: pack_metric_a
    generator:
      type: constant
      value: 1.0
";

fn write_catalog() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let runnable_path = dir.path().join("cpu-spike.yaml");
    std::fs::File::create(&runnable_path)
        .expect("create")
        .write_all(RUNNABLE_YAML.as_bytes())
        .expect("write");
    let pack_path = dir.path().join("tiny-pack.yaml");
    std::fs::File::create(&pack_path)
        .expect("create")
        .write_all(PACK_YAML.as_bytes())
        .expect("write");
    dir
}

#[test]
fn list_prints_all_entries() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cpu-spike"),
        "must list cpu-spike, got: {stdout}"
    );
    assert!(
        stdout.contains("tiny-pack"),
        "must list tiny-pack, got: {stdout}"
    );
    assert!(
        stdout.contains("KIND"),
        "must include header, got: {stdout}"
    );
    assert!(stdout.contains("runnable"));
    assert!(stdout.contains("composable"));
    assert!(
        stdout.contains("infrastructure"),
        "tags must be present, got: {stdout}"
    );
}

#[test]
fn list_filters_by_kind_runnable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--kind", "runnable"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu-spike"), "got: {stdout}");
    assert!(
        !stdout.contains("tiny-pack"),
        "composable must be filtered out, got: {stdout}"
    );
}

#[test]
fn list_filters_by_kind_composable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--kind", "composable"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny-pack"));
    assert!(!stdout.contains("cpu-spike"));
}

#[test]
fn list_filters_by_tag() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--tag", "network"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny-pack"), "got: {stdout}");
    assert!(!stdout.contains("cpu-spike"));
}

#[test]
fn list_json_output_is_machine_readable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--json"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("must be valid JSON");
    let arr = parsed.as_array().expect("must be array");
    assert_eq!(arr.len(), 2);
    for entry in arr {
        let obj = entry.as_object().expect("object");
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("kind"));
        assert!(obj.contains_key("description"));
        assert!(obj.contains_key("tags"));
    }
}

#[test]
fn show_runnable_prints_raw_yaml_that_round_trips() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["show", "@cpu-spike"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "show runnable must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu_usage"), "got: {stdout}");
    assert!(
        stdout.contains("kind: runnable"),
        "expected kind: runnable in output, got: {stdout}"
    );

    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), stdout.as_bytes()).expect("write tempfile");
    let dry = Command::new(sonda_bin())
        .arg("--dry-run")
        .args(["run"])
        .arg(tmp.path())
        .output()
        .expect("spawn sonda --dry-run");
    assert!(
        dry.status.success(),
        "show output must round-trip through `sonda --dry-run run`; stderr:\n{}",
        String::from_utf8_lossy(&dry.stderr)
    );
}

#[test]
fn show_composable_prints_raw_yaml() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["show", "@tiny-pack"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "show composable must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kind: composable"),
        "raw YAML expected, got: {stdout}"
    );
    assert!(stdout.contains("pack_metric_a"));
}
