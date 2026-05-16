//! Integration tests for the `--quiet` / `-q` CLI flag.

mod common;

use std::io::Write;
use std::process::Command;

use common::sonda_bin;
use tempfile::NamedTempFile;

const FIXTURE: &str = "version: 2
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
    name: test_banner
    generator:
      type: constant
      value: 1.0
";

fn write_fixture() -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    f.write_all(FIXTURE.as_bytes()).expect("write");
    f.flush().expect("flush");
    f
}

#[test]
fn quiet_flag_suppresses_status_banners() {
    let f = write_fixture();
    let output = Command::new(sonda_bin())
        .args(["-q", "run"])
        .arg(f.path())
        .output()
        .expect("failed to execute sonda binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains('\u{25b6}'),
        "stderr must not contain start banner in quiet mode, got: {stderr}"
    );
    assert!(
        !stderr.contains('\u{25a0}'),
        "stderr must not contain stop banner in quiet mode, got: {stderr}"
    );
    assert!(
        stderr.is_empty(),
        "stderr must be empty in quiet mode for a successful run, got: {stderr}"
    );
}

#[test]
fn without_quiet_flag_produces_status_banners() {
    let f = write_fixture();
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(f.path())
        .output()
        .expect("failed to execute sonda binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("test_banner"),
        "stderr must contain scenario name in normal mode, got: {stderr}"
    );
    assert!(
        stderr.contains("completed in"),
        "stderr must contain 'completed in' from the stop banner, got: {stderr}"
    );
}

#[test]
fn quiet_flag_still_produces_stdout_output() {
    let f = write_fixture();
    let output = Command::new(sonda_bin())
        .args(["-q", "run"])
        .arg(f.path())
        .output()
        .expect("failed to execute sonda binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "stdout must contain metric output even in quiet mode"
    );
    assert!(
        stdout.contains("test_banner"),
        "stdout must contain the metric name, got: {stdout}"
    );
}

#[test]
fn long_quiet_flag_is_accepted() {
    let f = write_fixture();
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(f.path())
        .output()
        .expect("failed to execute sonda binary");
    assert!(
        output.status.success(),
        "sonda --quiet should exit successfully, status: {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "stderr must be empty with --quiet, got: {stderr}"
    );
}
