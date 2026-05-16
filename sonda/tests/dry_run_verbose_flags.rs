//! Integration tests for the `--dry-run`, `--verbose`, and `--quiet` global flags.

mod common;

use std::io::Write;
use std::process::Command;

use common::sonda_bin;
use tempfile::NamedTempFile;

fn write_fixture(yaml: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    f.write_all(yaml.as_bytes()).expect("write");
    f.flush().expect("flush");
    f
}

const METRICS_FIXTURE: &str = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 100ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: m
    signal_type: metrics
    name: test_dry
    generator:
      type: constant
      value: 1.0
";

const LOGS_FIXTURE: &str = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 100ms
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
  - id: l
    signal_type: logs
    name: test_logs
    log_generator:
      type: template
      templates:
        - message: \"hello world\"
          severity: info
";

#[test]
fn dry_run_exits_zero_with_no_stdout() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "sonda --dry-run should exit 0, got: {}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must be empty in dry-run mode, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn dry_run_stderr_contains_config_and_validation() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "missing [config], got: {stderr}"
    );
    assert!(
        stderr.contains("Validation:") && stderr.contains("OK"),
        "missing validation OK, got: {stderr}"
    );
}

#[test]
fn dry_run_stderr_contains_scenario_name() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("test_dry"),
        "stderr must contain scenario name, got: {stderr}"
    );
}

#[test]
fn verbose_produces_stdout_and_config_on_stderr() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--verbose", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success(), "verbose run should succeed");
    assert!(
        !output.stdout.is_empty(),
        "verbose mode (no --dry-run) must produce events"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "verbose stderr must show config"
    );
}

#[test]
fn verbose_stderr_contains_banners() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--verbose", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("test_dry"),
        "verbose mode must show scenario name in banner, got: {stderr}"
    );
    assert!(
        stderr.contains("completed in") || stderr.contains("STOPPED"),
        "verbose mode must show stop output, got: {stderr}"
    );
}

#[test]
fn quiet_and_verbose_conflict_is_rejected() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--verbose", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(
        !output.status.success(),
        "--quiet + --verbose must conflict"
    );
}

#[test]
fn quiet_and_verbose_conflict_shows_error() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--verbose", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used") || stderr.contains("conflicts"),
        "stderr must mention the flag conflict, got: {stderr}"
    );
}

#[test]
fn dry_run_with_logs_subcommand() {
    let f = write_fixture(LOGS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "dry-run logs should succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[config]"));
    assert!(stderr.contains("Validation:") && stderr.contains("OK"));
}

#[test]
fn verbose_dry_run_shows_config() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--verbose", "--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "verbose dry-run stderr must include config block, got: {stderr}"
    );
}

#[test]
fn verbose_dry_run_produces_no_stdout() {
    let f = write_fixture(METRICS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--verbose", "--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "verbose dry-run must not emit events, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn verbose_dry_run_logs_shows_config() {
    let f = write_fixture(LOGS_FIXTURE);
    let output = Command::new(sonda_bin())
        .args(["--verbose", "--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "verbose dry-run logs must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "verbose dry-run logs stderr must include config block, got: {stderr}"
    );
}
