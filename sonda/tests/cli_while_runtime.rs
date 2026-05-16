//! End-to-end CLI tests for `sonda run` honoring `while:` clauses.

mod common;

use std::io::Write;
use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

#[test]
fn run_while_cascade_gates_downstream_emission() {
    let fixture = cli_fixtures_dir().join("while-cascade.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "sonda run must succeed; status={:?} stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    let primary_count = stdout
        .lines()
        .filter(|l| l.starts_with("primary_flap "))
        .count();
    let backup_count = stdout
        .lines()
        .filter(|l| l.starts_with("backup_saturation "))
        .count();

    assert!(
        primary_count >= 5,
        "primary_flap must emit a meaningful number of events, got {primary_count}\n\
         stdout:\n{stdout}"
    );
    assert!(
        (backup_count as f64) < (primary_count as f64) * 0.5,
        "while: gate must suppress downstream events; \
         backup_saturation={backup_count}, primary_flap={primary_count}, \
         expected backup < 50% of primary\nstdout:\n{stdout}"
    );
}

#[test]
fn op_le_returns_nonzero_on_cli() {
    let mut tmp = tempfile::Builder::new()
        .prefix("op_le_")
        .suffix(".v2.yaml")
        .tempfile()
        .expect("create temp YAML fixture");
    let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: src
    signal_type: metrics
    name: src
    generator:
      type: constant
      value: 1
  - id: gated
    signal_type: metrics
    name: gated
    generator:
      type: constant
      value: 1
    while:
      ref: src
      op: '<='
      value: 1
";
    tmp.write_all(yaml.as_bytes()).expect("write fixture");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(tmp.path())
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "sonda run must reject op:'<=' with a non-zero exit; status={:?}",
        output.status.code(),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported operator")
            && stderr.contains("strict")
            && stderr.contains("'<'")
            && stderr.contains("'>'"),
        "stderr must contain the locked operator-rejection wording, got:\n{stderr}"
    );
}

#[test]
fn dry_run_renders_flap_enum_oper_state_defaults() {
    let mut tmp = tempfile::Builder::new()
        .prefix("flap_enum_")
        .suffix(".v2.yaml")
        .tempfile()
        .expect("create temp YAML fixture");
    let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 30s
scenarios:
  - id: oper_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s
      enum: oper_state
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";
    tmp.write_all(yaml.as_bytes()).expect("write fixture");
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(tmp.path())
        .output()
        .expect("must spawn sonda");

    assert!(output.status.success(), "dry-run must succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("up_value: 1") && stderr.contains("down_value: 2"),
        "dry-run must render `enum: oper_state` as up=1, down=2, got:\n{stderr}"
    );
}

#[test]
fn dry_run_rejects_flap_enum_with_explicit_values() {
    let mut tmp = tempfile::Builder::new()
        .prefix("flap_mutex_")
        .suffix(".v2.yaml")
        .tempfile()
        .expect("create temp YAML fixture");
    let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 30s
scenarios:
  - id: bad
    signal_type: metrics
    name: bad
    generator:
      type: flap
      up_duration: 5s
      down_duration: 5s
      enum: oper_state
      up_value: 7
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";
    tmp.write_all(yaml.as_bytes()).expect("write fixture");
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(tmp.path())
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "dry-run must reject `enum:` + explicit `up_value` with non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutually exclusive"),
        "stderr must contain the locked mutual-exclusion message, got:\n{stderr}"
    );
}

#[test]
fn run_while_cascade_progress_emits_paused_line() {
    let fixture = cli_fixtures_dir().join("while-cascade.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "sonda run must succeed; status={:?} stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("PAUSED"),
        "stderr must contain a PAUSED progress line for the gated downstream during a flap close-window\n\
         stderr:\n{stderr}"
    );
}
