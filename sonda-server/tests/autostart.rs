//! E2E tests for `--autostart`: the catalog sweep that starts every
//! `kind: runnable` entry at server startup.

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::{http_client, run_server_expecting_exit, spawn_server_with, start_server_with};
use tempfile::TempDir;

fn runnable(scenario_name: &str, metric_names: &[&str]) -> String {
    let mut yaml = format!(
        "version: 2
kind: runnable
scenario_name: {scenario_name}
defaults:
  rate: 1
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
"
    );
    for name in metric_names {
        yaml.push_str(&format!(
            "  - signal_type: metrics
    name: {name}
    generator:
      type: constant
      value: 1.0
"
        ));
    }
    yaml
}

const COMPOSABLE_PACK: &str = "\
version: 2
kind: composable
name: aaa_test_pack
description: \"pack that must never be started\"
metrics:
  - name: pack_metric
    generator:
      type: constant
      value: 1.0
";

const V1_SHAPED: &str = "\
kind: runnable
name: aaa_legacy
rate: 1
duration: 300s
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

const UNCOMPILABLE: &str = "\
version: 2
kind: runnable
scenario_name: aaa_broken
defaults:
  rate: 1
  duration: 300s
scenarios:
  - signal_type: metrics
    name: broken_metric
    generator:
      type: no_such_generator
";

const CROSS_FILE_BASELINE: &str = "\
version: 2
kind: runnable
scenario_name: baseline_post
defaults:
  rate: 5
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: baseline_traffic
    signal_type: metrics
    name: baseline_traffic
    generator:
      type: constant
      value: 1.0
    while:
      ref: cascade_signal
      op: \">\"
      value: 0
      scenario_name: cascade_post
      if_unresolved: pending
";

const CROSS_FILE_CASCADE: &str = "\
version: 2
kind: runnable
scenario_name: cascade_post
defaults:
  rate: 5
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cascade_signal
    signal_type: metrics
    name: cascade_signal
    generator:
      type: constant
      value: 1.0
";

const UNPARSEABLE_YAML: &str = "kind: runnable\nversion: 2\n  this: [is, not\n   valid yaml\n";

fn catalog_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("must create temp catalog dir");
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).expect("must write catalog file");
    }
    dir
}

struct Run {
    metric_names: BTreeSet<String>,
    stderr: String,
}

impl Run {
    fn count(&self, needle: &str) -> usize {
        self.stderr
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }
}

fn run_catalog(catalog: &Path, extra_args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut args: Vec<&str> = vec!["--catalog", catalog.to_str().expect("utf-8 temp path")];
    args.extend_from_slice(extra_args);
    let (port, mut child) = spawn_server_with(&args, env);

    let body: serde_json::Value = http_client()
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .send()
        .expect("GET /scenarios must succeed")
        .json()
        .expect("GET /scenarios must return JSON");

    let metric_names = body["scenarios"]
        .as_array()
        .expect("scenarios must be an array")
        .iter()
        .map(|s| {
            s["name"]
                .as_str()
                .expect("name must be a string")
                .to_string()
        })
        .collect();

    child.kill().expect("must kill sonda-server");
    let output = child
        .wait_with_output()
        .expect("must collect sonda-server output");

    Run {
        metric_names,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn autostart_starts_runnable_entries() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("beta.yaml", &runnable("beta", &["beta_mem", "beta_disk"])),
    ]);

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(
        run.metric_names,
        BTreeSet::from([
            "alpha_cpu".to_string(),
            "beta_mem".to_string(),
            "beta_disk".to_string()
        ])
    );
}

#[test]
fn autostart_logs_one_launch_line_per_started_scenario() {
    let catalog = catalog_with(&[("beta.yaml", &runnable("beta", &["beta_mem", "beta_disk"]))]);

    let run = run_catalog(
        catalog.path(),
        &["--autostart"],
        &[("RUST_LOG", "sonda_server=info")],
    );

    assert_eq!(run.count("scenario launched"), 2, "stderr: {}", run.stderr);
    assert_eq!(
        run.count("beta.yaml"),
        2,
        "each launch line must name the catalog file: {}",
        run.stderr
    );
}

#[test]
fn autostart_leaves_composable_packs_alone() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("pack.yaml", COMPOSABLE_PACK),
    ]);

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    assert_eq!(
        run.count("skipping catalog entry"),
        0,
        "a pack is not a candidate, so it must not be reported as skipped: {}",
        run.stderr
    );
}

#[test]
fn catalog_without_autostart_starts_nothing() {
    let catalog = catalog_with(&[("alpha.yaml", &runnable("alpha", &["alpha_cpu"]))]);

    let run = run_catalog(catalog.path(), &[], &[]);

    assert!(
        run.metric_names.is_empty(),
        "expected no scenarios, got {:?}",
        run.metric_names
    );
}

#[test]
fn autostart_env_var_starts_the_same_scenarios_as_the_flag() {
    let catalog = catalog_with(&[("alpha.yaml", &runnable("alpha", &["alpha_cpu"]))]);

    let run = run_catalog(catalog.path(), &[], &[("SONDA_AUTOSTART", "true")]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
}

#[test]
fn autostart_skips_uncompilable_file_and_starts_the_rest() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("broken.yaml", UNCOMPILABLE),
    ]);

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    assert_eq!(
        run.count("does not compile, skipping catalog entry"),
        1,
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn unparseable_yaml_never_becomes_a_catalog_entry() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("garbage.yaml", UNPARSEABLE_YAML),
    ]);

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    assert_eq!(
        run.count("skipping catalog entry"),
        0,
        "enumeration drops the file, so the sweep never sees it: {}",
        run.stderr
    );
}

#[test]
fn autostart_skips_v1_shaped_file_and_starts_the_rest() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("legacy.yaml", V1_SHAPED),
    ]);

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    assert!(
        run.stderr.contains("version: 2"),
        "the skip reason must carry the v2 migration hint: {}",
        run.stderr
    );
}

#[test]
fn autostart_with_duplicate_entry_names_fails_before_binding() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("shared", &["alpha_cpu"])),
        ("beta.yaml", &runnable("shared", &["beta_mem"])),
    ]);
    let dir = catalog.path().to_str().expect("utf-8 temp path");

    let exit = run_server_expecting_exit(&["--catalog", dir, "--autostart"], &[]);

    assert_eq!(exit.code, Some(1), "stderr was: {}", exit.stderr);
    assert!(
        exit.stderr.contains("duplicate entry name")
            && exit.stderr.contains("alpha.yaml")
            && exit.stderr.contains("beta.yaml"),
        "error must name the clash and both files, got: {}",
        exit.stderr
    );
    assert!(
        !exit.announced_a_port(),
        "no port may be bound, stdout was: {}",
        exit.stdout
    );
}

#[test]
fn duplicate_entry_names_without_autostart_still_start_the_server() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("shared", &["alpha_cpu"])),
        ("beta.yaml", &runnable("shared", &["beta_mem"])),
    ]);

    let run = run_catalog(catalog.path(), &[], &[]);

    assert!(
        run.metric_names.is_empty(),
        "expected no scenarios, got {:?}",
        run.metric_names
    );
}

#[test]
fn autostart_with_an_unreadable_catalog_file_warns_and_keeps_serving() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: file permissions do not constrain root");
        return;
    }

    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("locked.yaml", &runnable("locked", &["locked_cpu"])),
    ]);
    std::fs::set_permissions(
        catalog.path().join("locked.yaml"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("must drop read permission");

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert!(
        run.metric_names.is_empty(),
        "expected no scenarios, got {:?}",
        run.metric_names
    );
    assert_eq!(
        run.count("catalog could not be read, starting nothing"),
        1,
        "starting nothing must never be silent: {}",
        run.stderr
    );
}

#[test]
fn autostart_resolves_a_cross_file_while_reference() {
    let catalog = catalog_with(&[
        ("baseline.yaml", CROSS_FILE_BASELINE),
        ("cascade.yaml", CROSS_FILE_CASCADE),
    ]);
    let dir = catalog.path().to_str().expect("utf-8 temp path");
    let (port, _guard) = start_server_with(&["--catalog", dir, "--autostart"], &[]);
    let client = http_client();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(format!("http://127.0.0.1:{port}/scenarios"))
            .send()
            .expect("GET /scenarios must succeed")
            .json()
            .expect("GET /scenarios must return JSON");

        let running: BTreeSet<String> = body["scenarios"]
            .as_array()
            .expect("scenarios must be an array")
            .iter()
            .filter(|s| s["state"] == "running")
            .map(|s| s["name"].as_str().unwrap_or_default().to_string())
            .collect();
        if running.len() == 2 {
            assert_eq!(
                running,
                BTreeSet::from(["baseline_traffic".to_string(), "cascade_signal".to_string()])
            );
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cross-file while: never resolved; last list = {body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn autostart_logs_a_summary_even_when_nothing_starts() {
    let catalog = catalog_with(&[("pack.yaml", COMPOSABLE_PACK)]);

    let run = run_catalog(
        catalog.path(),
        &["--autostart"],
        &[("RUST_LOG", "sonda_server=info")],
    );

    assert!(
        run.metric_names.is_empty(),
        "expected no scenarios, got {:?}",
        run.metric_names
    );
    assert_eq!(
        run.count("autostart: started 0 of 0 runnable catalog entries"),
        1,
        "a sweep that starts nothing must say so: {}",
        run.stderr
    );
}

#[test]
fn autostart_env_var_accepts_the_shapes_operators_actually_set() {
    let catalog = catalog_with(&[("alpha.yaml", &runnable("alpha", &["alpha_cpu"]))]);
    let started = BTreeSet::from(["alpha_cpu".to_string()]);

    for (value, expected_autostart) in [
        ("", false),
        ("0", false),
        ("no", false),
        ("off", false),
        ("false", false),
        ("FALSE", false),
        ("n", false),
        ("1", true),
        ("yes", true),
        ("on", true),
        ("true", true),
        ("True", true),
    ] {
        let run = run_catalog(catalog.path(), &[], &[("SONDA_AUTOSTART", value)]);

        let expected = if expected_autostart {
            started.clone()
        } else {
            BTreeSet::new()
        };
        assert_eq!(
            run.metric_names,
            expected,
            "SONDA_AUTOSTART={value:?} must start the server and {}",
            if expected_autostart {
                "autostart"
            } else {
                "stay idle"
            }
        );
    }
}

#[test]
fn autostart_honours_max_scenarios() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("beta.yaml", &runnable("beta", &["beta_mem"])),
    ]);

    let run = run_catalog(
        catalog.path(),
        &["--autostart", "--max-scenarios", "1"],
        &[],
    );

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    assert_eq!(
        run.count("scenario cap reached"),
        1,
        "the skipped entry must say why, with its own path: {}",
        run.stderr
    );
    assert_eq!(
        run.count("beta.yaml"),
        1,
        "the cap warning must name the file it rejected: {}",
        run.stderr
    );
}

#[test]
fn autostart_without_catalog_fails_before_binding() {
    let exit = run_server_expecting_exit(&["--autostart"], &[]);

    assert_eq!(exit.code, Some(1), "stderr was: {}", exit.stderr);
    assert!(
        exit.stderr.contains("--autostart") && exit.stderr.contains("--catalog"),
        "error must name both flags, got: {}",
        exit.stderr
    );
    assert!(
        !exit.announced_a_port(),
        "no port may be bound, stdout was: {}",
        exit.stdout
    );
}

#[test]
fn autostart_env_var_without_catalog_fails_before_binding() {
    let exit = run_server_expecting_exit(&[], &[("SONDA_AUTOSTART", "true")]);

    assert_eq!(exit.code, Some(1), "stderr was: {}", exit.stderr);
    assert!(
        exit.stderr.contains("SONDA_CATALOG"),
        "error must name the env var, got: {}",
        exit.stderr
    );
    assert!(
        !exit.announced_a_port(),
        "no port may be bound, stdout was: {}",
        exit.stdout
    );
}
