//! Snapshot coverage for `--dry-run` rendering of `while:` / `delay:` clauses.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

fn snapshot_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_sort_maps(true);
    // The fixture path varies by host; replace it with a stable token.
    s.add_filter(
        r"\[config\] file: [^ ]+ \(version: 2, (\d+ scenarios?)\)",
        "[config] file: <fixture> (version: 2, $1)",
    );
    s
}

fn dry_run_text(yaml_body: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir must be created");
    let path = dir.path().join("scenario.yaml");
    std::fs::write(&path, yaml_body).expect("scenario file must be written");
    let pack_dir = cli_fixtures_dir().join("catalog-packs");
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(&pack_dir)
        .args(["run"])
        .arg(&path)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "dry-run failed: exit {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stderr).expect("stderr utf-8")
}

const ANALYTICAL_UPSTREAM: &str = r#"version: 2
kind: runnable
defaults:
  rate: 5
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0
  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#;

const NON_ANALYTICAL_UPSTREAM: &str = r#"version: 2
kind: runnable
defaults:
  rate: 5
  duration: 1m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60.0
      offset: 50.0
  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#;

const MIXED_UPSTREAM: &str = r#"version: 2
kind: runnable
defaults:
  rate: 5
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: trigger
    signal_type: metrics
    name: trigger_metric
    generator:
      type: step
      start: 0.0
      step_size: 1.0
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0
  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    after:
      ref: trigger
      op: ">"
      value: 5.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#;

const DELAY_PRESENT: &str = r#"version: 2
kind: runnable
defaults:
  rate: 5
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0
  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
    delay:
      open: "5s"
      close: "10s"
"#;

#[test]
fn dry_run_while_with_analytical_upstream() {
    let stderr = dry_run_text(ANALYTICAL_UPSTREAM);
    snapshot_settings().bind(|| {
        insta::assert_snapshot!("while_analytical_upstream", stderr);
    });
}

#[test]
fn dry_run_while_with_non_analytical_upstream() {
    let stderr = dry_run_text(NON_ANALYTICAL_UPSTREAM);
    snapshot_settings().bind(|| {
        insta::assert_snapshot!("while_non_analytical_upstream", stderr);
    });
}

#[test]
fn dry_run_while_with_mixed_upstream() {
    let stderr = dry_run_text(MIXED_UPSTREAM);
    snapshot_settings().bind(|| {
        insta::assert_snapshot!("while_mixed_upstream", stderr);
    });
}

#[test]
fn dry_run_while_with_delay_present() {
    let stderr = dry_run_text(DELAY_PRESENT);
    snapshot_settings().bind(|| {
        insta::assert_snapshot!("while_delay_present", stderr);
    });
}
