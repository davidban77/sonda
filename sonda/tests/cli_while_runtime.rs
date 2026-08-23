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

    // The bound comes from the fixture rather than from a round number.
    //
    // `primary_flap` is `up_duration: 1s, down_duration: 500ms`, and the gate
    // is `while primary < 1`, so the downstream is entitled to emit for the
    // down third of each cycle: 500ms / 1500ms = 1/3 of the ticks.
    //
    // Gate transitions are not free — the downstream learns about an edge on
    // its own next tick — so each of the ~3 cycles in a 4s run can carry an
    // extra tick either side. Observed 40% locally and 50% on a loaded CI
    // runner, against the 33% the duty cycle implies.
    //
    // The old bound was a bare `< 50%`, which had ten points of headroom over
    // the observed range and none at all over the worst case. It failed the
    // moment the scheduler stopped emitting one tick past the declared
    // duration: `rate: 5` for `4s` is 20 ticks, not 21, and the correction
    // moved the threshold from `< 10.5` to `< 10.0` onto an observed 10.
    // The denominator had been inflated by the bug.
    //
    // 2/3 keeps this loud where it matters — an ungated run emits on every
    // tick, which is 100% — while leaving room for the latency the fixture
    // actually produces.
    let duty_cycle = 500.0 / 1500.0;
    let ceiling = duty_cycle * 2.0;
    assert!(
        (backup_count as f64) < (primary_count as f64) * ceiling,
        "while: gate must suppress downstream events; \
         backup_saturation={backup_count}, primary_flap={primary_count}, \
         expected backup below {:.0}% of primary (duty cycle {:.0}%, doubled for \
         gate-transition latency)\nstdout:\n{stdout}",
        ceiling * 100.0,
        duty_cycle * 100.0,
    );
    // And the other direction: a gate that suppressed everything would sail
    // through the bound above, so the downstream must actually run.
    assert!(
        backup_count > 0,
        "while: gate must let the downstream emit during the down phase; \
         backup_saturation=0\nstdout:\n{stdout}"
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
