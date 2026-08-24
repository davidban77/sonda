//! Integration tests for `sonda run` v2-only dispatch.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

#[test]
fn run_v1_scenario_is_rejected_with_migration_hint() {
    let fixture = cli_fixtures_dir().join("inline-v1.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "v1 multi-scenario must not succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v2"),
        "rejection must mention v2 requirement, got:\n{stderr}"
    );
}

#[test]
fn run_v2_scenario_succeeds() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("v2_inline_metric"),
        "expected v2 metric name in stdout, got:\n{stdout}"
    );
}

#[test]
fn run_v2_dry_run_emits_spec_pretty_output() {
    let fixture = cli_fixtures_dir().join("multi-after-chain.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 dry-run failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config] file:") && stderr.contains("version: 2"),
        "missing v2 header in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Validation: OK"),
        "missing validation footer:\n{stderr}"
    );
    assert!(
        stderr.contains("phase_offset:"),
        "missing phase_offset annotation:\n{stderr}"
    );
    assert!(
        stderr.contains("clock_group:") && stderr.contains("(auto)"),
        "missing auto clock_group line:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "dry-run must not write to stdout, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn run_v2_dry_run_json_format_emits_stable_dto() {
    let fixture = cli_fixtures_dir().join("inline.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .args(["--dry-run", "--format=json"])
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "v2 dry-run --format=json failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json output must parse");
    assert_eq!(json["version"], 2);
    assert_eq!(json["scenarios"][0]["name"], "v2_inline_metric");
    assert_eq!(json["scenarios"][0]["signal"], "metrics");
}

#[test]
fn run_flat_v1_single_scenario_is_rejected_with_migration_hint() {
    let fixture = cli_fixtures_dir().join("flat-v1-metrics.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "flat v1 file must not succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v2"),
        "rejection must mention v2 requirement, got:\n{stderr}"
    );
}

#[test]
fn v2_compile_error_surfaces_with_context() {
    let fixture = cli_fixtures_dir().join("broken-self-ref.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run"])
        .arg(&fixture)
        .arg("--dry-run")
        .output()
        .expect("must spawn sonda");

    assert!(
        !output.status.success(),
        "self-ref must produce non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broken-self-ref.v2.yaml") || stderr.to_lowercase().contains("self"),
        "error must identify the source file or the self-reference, got:\n{stderr}"
    );
}

// ---- `--dry-run` and `run` must agree on what they refuse --------------------

/// `--dry-run` must refuse every scenario `run` refuses.
///
/// It used to return before `prepare_entries`, so it never ran
/// `expand_scenario` — where csv_replay derives its rate from the file's
/// timestamps and rejects a file it cannot replay. A scenario `sonda run`
/// errors on printed "Validation: OK", which is the opposite of what the flag
/// is for: CI reaches for `--dry-run` precisely to catch this class before a
/// deploy.
///
/// The test drives both verbs over the same file and compares their verdicts
/// rather than asserting a message, so it keeps holding as rules are added to
/// the expansion path — which is the failure mode a transcribed list of rules
/// would have.
#[test]
fn dry_run_refuses_what_run_refuses() {
    use std::io::Write;

    let mut csv = tempfile::Builder::new()
        .prefix("nonmono_")
        .suffix(".csv")
        .tempfile()
        .expect("create temp CSV");
    // Timestamps run backwards, so the replay rate cannot be derived. This is
    // rejected inside `expand_scenario`, which is the step `--dry-run` skipped.
    writeln!(csv, "Time,cpu").expect("write header");
    writeln!(csv, "1700000010,1").expect("write row");
    writeln!(csv, "1700000000,2").expect("write row");
    csv.flush().expect("flush");

    let mut yaml = tempfile::Builder::new()
        .prefix("dry_run_parity_")
        .suffix(".v2.yaml")
        .tempfile()
        .expect("create temp YAML");
    write!(
        yaml,
        "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    \
         name: cpu\n    rate: 1\n    duration: 5s\n    generator:\n      \
         type: csv_replay\n      file: {}\n",
        csv.path().display()
    )
    .expect("write YAML");
    yaml.flush().expect("flush");

    let run = Command::new(sonda_bin())
        .args(["--quiet", "run"])
        .arg(yaml.path())
        .output()
        .expect("must spawn sonda");
    let dry = Command::new(sonda_bin())
        .args(["--quiet", "--dry-run", "run"])
        .arg(yaml.path())
        .output()
        .expect("must spawn sonda");

    // The premise: if `run` ever stops refusing this file, the comparison below
    // would pass vacuously with both succeeding.
    assert!(
        !run.status.success(),
        "premise: `run` must refuse a non-monotonic capture; stderr:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    assert_eq!(
        dry.status.success(),
        run.status.success(),
        "`--dry-run` and `run` must agree. run={:?} dry-run={:?}\n\
         run stderr:\n{}\ndry-run stderr:\n{}\ndry-run stdout:\n{}",
        run.status.code(),
        dry.status.code(),
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&dry.stderr),
        String::from_utf8_lossy(&dry.stdout),
    );
}

/// The other direction: a scenario `run` accepts must still dry-run cleanly.
///
/// Without this, the parity above could be satisfied by making `--dry-run`
/// refuse everything.
#[test]
fn dry_run_still_accepts_what_run_accepts() {
    let fixture = cli_fixtures_dir().join("while-cascade.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--dry-run", "run"])
        .arg(&fixture)
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "a valid scenario must still dry-run cleanly; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
