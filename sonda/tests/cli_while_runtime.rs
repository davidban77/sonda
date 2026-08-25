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

    // The bound comes from the fixture rather than from a round number, and
    // it is READ from the fixture rather than transcribed out of it.
    //
    // `primary_flap` is a `flap` generator and the gate is `while primary < 1`,
    // so the downstream is entitled to emit for the down phase of each cycle:
    // `down_duration / (up_duration + down_duration)`. Writing that fraction as
    // a literal would be a second copy of the YAML, correct until someone edits
    // the fixture — the exact shape this repo keeps finding defects in. So the
    // two durations are parsed out of the file the test already loads.
    //
    // Gate transitions are not free: the downstream learns about an edge on its
    // own next tick, so each cycle can carry an extra tick either side.
    // Observed 40% locally and 50% on a loaded CI runner against the 33% the
    // duty cycle implies. Doubling allows that and no more — an ungated run
    // emits on every tick, which is 100%, and still fails loudly.
    //
    // The old bound was a bare `< 50%`, and it broke the moment the scheduler
    // stopped emitting one tick past the declared duration: `rate: 5` for `4s`
    // is 20 ticks, not 21, which moved the threshold from `< 10.5` to `< 10.0`
    // onto an observed 10. The denominator had been inflated by a bug.
    let (up_secs, down_secs) = flap_phases(&fixture);
    let duty_cycle = down_secs / (up_secs + down_secs);
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

/// Read `up_duration` and `down_duration` out of the cascade fixture, in
/// seconds.
///
/// The test's bound is derived from the fixture's own flap timings, so it has
/// to read them rather than restate them. Both keys are asserted present: if
/// the fixture is restructured and the walk stops finding them, this panics
/// instead of handing back a plausible default that would quietly weaken the
/// assertion it feeds.
fn flap_phases(fixture: &std::path::Path) -> (f64, f64) {
    let text = std::fs::read_to_string(fixture).expect("fixture must be readable");
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).expect("fixture must be valid YAML");

    let generator = doc
        .get("scenarios")
        .and_then(|s| s.as_sequence())
        .expect("fixture has a scenarios list")
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some("primary_flap"))
        .and_then(|e| e.get("generator"))
        .expect("fixture has a primary_flap entry with a generator");

    let secs = |key: &str| -> f64 {
        let raw = generator
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("primary_flap generator must declare {key}"));
        parse_secs(raw)
            .unwrap_or_else(|| panic!("{key} = {raw:?} is not a duration this test can read"))
    };
    (secs("up_duration"), secs("down_duration"))
}

/// Minimal duration reader for the two fixture fields above (`1s`, `500ms`).
///
/// Deliberately narrow: it returns `None` rather than guessing, and every
/// caller panics on `None`, so an unrecognised unit stops the test instead of
/// silently changing the bound.
fn parse_secs(raw: &str) -> Option<f64> {
    if let Some(ms) = raw.strip_suffix("ms") {
        return ms.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(s) = raw.strip_suffix('s') {
        return s.parse::<f64>().ok();
    }
    None
}
