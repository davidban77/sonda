//! End-to-end CLI tests for `sonda run` honoring `while:` clauses.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

#[test]
fn run_while_cascade_gates_downstream_emission() {
    let fixture = cli_fixtures_dir().join("while-cascade.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["--quiet", "run", "--scenario"])
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
fn run_while_cascade_progress_emits_paused_line() {
    let fixture = cli_fixtures_dir().join("while-cascade.v2.yaml");
    let output = Command::new(sonda_bin())
        .args(["run", "--scenario"])
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
