//! Integration tests for the unified `sonda catalog` subcommand (PR 7,
//! spec §6.3).
//!
//! Exercises `list` / `show` / `run` against isolated scenario and pack
//! search paths so the tests don't depend on the user's filesystem.

mod common;

use std::process::Command;

use common::{cli_fixtures_dir, sonda_bin};

fn scenarios_dir() -> std::path::PathBuf {
    cli_fixtures_dir().join("catalog-scenarios")
}

fn packs_dir() -> std::path::PathBuf {
    cli_fixtures_dir().join("catalog-packs")
}

/// `sonda catalog list` shows both scenarios and packs merged.
#[test]
fn catalog_list_shows_scenarios_and_packs() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "list"])
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("scn-a"), "missing scenario A:\n{stdout}");
    assert!(stdout.contains("scn-b"), "missing scenario B:\n{stdout}");
    assert!(stdout.contains("tiny_pack"), "missing pack:\n{stdout}");
}

/// `--type scenario` hides packs; `--type pack` hides scenarios.
#[test]
fn catalog_list_type_filter_scenarios() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "list", "--type", "scenario"])
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("scn-a"));
    assert!(
        !stdout.contains("tiny_pack"),
        "pack must not appear:\n{stdout}"
    );
}

#[test]
fn catalog_list_type_filter_packs() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "list", "--type", "pack"])
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny_pack"));
    assert!(
        !stdout.contains("scn-a"),
        "scenario must not appear:\n{stdout}"
    );
}

/// `--category` filters on exact case-sensitive match.
#[test]
fn catalog_list_category_filter() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "list", "--category", "network"])
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // scn-a is network, scn-b is infrastructure; tiny_pack is network.
    assert!(stdout.contains("scn-a"));
    assert!(stdout.contains("tiny_pack"));
    assert!(
        !stdout.contains("scn-b"),
        "infrastructure entry must be filtered out:\n{stdout}"
    );
}

/// `--json` emits a stable JSON array with the six metadata fields per
/// row (name, type, category, signal, description, runnable).
#[test]
fn catalog_list_json_emits_stable_dto() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "list", "--json"])
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("json must parse");

    // Find the scenario and pack rows by name.
    let scn = entries
        .iter()
        .find(|e| e["name"] == "scn-a")
        .expect("scn-a must appear");
    assert_eq!(scn["type"], "scenario");
    assert_eq!(scn["category"], "network");
    assert_eq!(scn["signal"], "metrics");
    assert_eq!(scn["runnable"], true);

    let pack = entries
        .iter()
        .find(|e| e["name"] == "tiny_pack")
        .expect("tiny_pack must appear");
    assert_eq!(pack["type"], "pack");
    assert_eq!(pack["runnable"], false);
}

/// `catalog show <scenario_name>` prints the YAML on stdout with a
/// metadata header on stderr.
#[test]
fn catalog_show_scenario_prints_yaml() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "show", "scn-a"])
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("scn_a_metric"),
        "must dump YAML content on stdout:\n{stdout}"
    );
}

/// `catalog show <pack_name>` prints the pack YAML.
#[test]
fn catalog_show_pack_prints_yaml() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "show", "tiny_pack"])
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("pack_metric_a") && stdout.contains("pack_metric_b"),
        "must dump pack YAML content on stdout:\n{stdout}"
    );
}

/// `catalog run <scenario>` executes the scenario end-to-end.
#[test]
fn catalog_run_scenario_succeeds() {
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "run", "scn-a"])
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "scenario run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("scn_a_metric"),
        "expected metric output, got:\n{stdout}"
    );
}

/// `catalog run <pack>` with `--label` overrides expands the pack and
/// emits metric output with the supplied labels.
#[test]
fn catalog_run_pack_with_labels_succeeds() {
    let output = Command::new(sonda_bin())
        .args(["--quiet", "--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args([
            "catalog",
            "run",
            "tiny_pack",
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--label",
            "device=rtr-test-01",
        ])
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "pack run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("pack_metric_a"),
        "expected pack metric output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("rtr-test-01"),
        "expected label value in output, got:\n{stdout}"
    );
}

/// `catalog run <pack> -o <path>` writes metric output to the supplied
/// file and leaves stdout empty — regression for PR 7's pack dispatch
/// which silently dropped `-o` because `PacksRunArgs` had no `output`
/// field.
#[test]
fn catalog_run_pack_honors_output_flag() {
    let tmp = tempfile::tempdir().expect("must create temp dir");
    let out_path = tmp.path().join("pack-out.prom");

    let output = Command::new(sonda_bin())
        .args(["--quiet", "--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "run", "tiny_pack", "-o"])
        .arg(&out_path)
        .args([
            "--rate",
            "1",
            "--duration",
            "300ms",
            "--label",
            "device=rtr-test-01",
        ])
        .output()
        .expect("must spawn sonda");

    assert!(
        output.status.success(),
        "pack run -o failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        out_path.exists(),
        "output file must be created at {}",
        out_path.display()
    );
    let contents = std::fs::read_to_string(&out_path).expect("must read output file");
    assert!(
        !contents.is_empty(),
        "output file must have metric lines, got empty file"
    );
    // The pack contains two metrics; each runs on its own thread with a
    // File sink pointed at the same path. We only assert that at least
    // one pack metric landed (matching either name) — the regression
    // being guarded against is the silently-dropped `-o` flag, not the
    // file-sink concurrency semantics.
    assert!(
        contents.contains("pack_metric_a") || contents.contains("pack_metric_b"),
        "output file must contain a pack metric, got:\n{contents}"
    );
    assert!(
        contents.contains("device=\"rtr-test-01\""),
        "output file must carry the --label override, got:\n{contents}"
    );
    // Stdout must be empty — metrics went to the file. stderr may carry
    // status banners (even under `--quiet` some paths print; we only
    // assert stdout).
    assert!(
        output.stdout.is_empty(),
        "stdout must be empty when -o redirects to file, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Unknown name → non-zero exit with an error mentioning the name.
#[test]
fn catalog_run_unknown_name_errors() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["catalog", "run", "does-not-exist"])
        .output()
        .expect("must spawn sonda");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist") || stderr.contains("unknown"),
        "error must mention the bad name:\n{stderr}"
    );
}

/// Legacy `sonda scenarios` subcommand still works (hidden, not
/// deleted). Matrix row 16.12 and existing v1 workflows depend on this.
#[test]
fn legacy_scenarios_subcommand_still_works() {
    let output = Command::new(sonda_bin())
        .args(["--scenario-path"])
        .arg(scenarios_dir())
        .args(["scenarios", "list"])
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "scenarios list must still work: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("scn-a"));
}

/// Legacy `sonda packs` subcommand still works.
#[test]
fn legacy_packs_subcommand_still_works() {
    let output = Command::new(sonda_bin())
        .args(["--pack-path"])
        .arg(packs_dir())
        .args(["packs", "list"])
        .output()
        .expect("must spawn sonda");
    assert!(
        output.status.success(),
        "packs list must still work: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny_pack"));
}

/// The hidden subcommands do not appear in `--help`.
#[test]
fn hidden_subcommands_are_absent_from_top_level_help() {
    let output = Command::new(sonda_bin())
        .arg("--help")
        .output()
        .expect("must spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The `catalog` subcommand must be visible.
    assert!(stdout.contains("catalog"), "catalog must appear in help");
    // Hidden subcommands (scenarios, packs, story) must NOT be in the
    // top-level help. They remain callable; clap just doesn't list them.
    assert!(
        !stdout.contains("\n  scenarios "),
        "scenarios should be hidden, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("\n  packs "),
        "packs should be hidden, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("\n  story "),
        "story should be hidden, got:\n{stdout}"
    );
}
