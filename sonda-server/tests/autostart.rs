//! E2E tests for `--autostart`: the catalog sweep that starts every
//! `kind: runnable` entry at server startup.

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::{
    http_client, run_server_expecting_exit, spawn_server_tailed, start_server_with,
    terminate_gracefully,
};
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

const SWEEP_SUMMARY: &str = "runnable catalog entries";
const SWEEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the server on `catalog`, wait for the sweep to report its summary, then
/// read `GET /scenarios`. The sweep runs alongside the server, so a bare GET
/// races it.
fn run_catalog(catalog: &Path, extra_args: &[&str], env: &[(&str, &str)]) -> Run {
    let run = collect_catalog_run(catalog, extra_args, env, Sweep::Expected);
    assert!(
        run.count(SWEEP_SUMMARY) > 0,
        "the sweep never reported a summary: {}",
        run.stderr
    );
    run
}

/// Same, for a server that must not sweep at all. SIGTERM rather than kill,
/// because a sweep that had been spawned always reports itself on the way out:
/// zero `autostart:` lines then proves no sweep ever ran.
fn run_idle_catalog(catalog: &Path, extra_args: &[&str], env: &[(&str, &str)]) -> Run {
    let run = collect_catalog_run(catalog, extra_args, env, Sweep::None);
    assert_eq!(
        run.count("autostart:"),
        0,
        "autostart is off, so no sweep may report anything: {}",
        run.stderr
    );
    run
}

#[derive(PartialEq, Eq)]
enum Sweep {
    Expected,
    None,
}

fn collect_catalog_run(
    catalog: &Path,
    extra_args: &[&str],
    env: &[(&str, &str)],
    sweep: Sweep,
) -> Run {
    let mut args: Vec<&str> = vec!["--catalog", catalog.to_str().expect("utf-8 temp path")];
    args.extend_from_slice(extra_args);
    let mut full_env: Vec<(&str, &str)> = vec![("RUST_LOG", "sonda_server=info")];
    full_env.extend_from_slice(env);
    let (port, mut child, tail) = spawn_server_tailed(&args, &full_env);

    if sweep == Sweep::Expected {
        tail.wait_for(SWEEP_SUMMARY, SWEEP_TIMEOUT);
    }

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

    match sweep {
        Sweep::Expected => {
            child.kill().expect("must kill sonda-server");
            child.wait().expect("must reap sonda-server");
        }
        Sweep::None => {
            let code = terminate_gracefully(&mut child);
            assert_eq!(code, Some(0), "SIGTERM must produce a clean exit");
        }
    }

    Run {
        metric_names,
        stderr: tail.finish(),
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

    let run = run_idle_catalog(catalog.path(), &[], &[]);

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

    let run = run_idle_catalog(catalog.path(), &[], &[]);

    assert!(
        run.metric_names.is_empty(),
        "expected no scenarios, got {:?}",
        run.metric_names
    );
}

#[test]
fn autostart_skips_an_unreadable_file_and_starts_the_rest() {
    let catalog = catalog_with(&[
        ("alpha.yaml", &runnable("alpha", &["alpha_cpu"])),
        ("locked.yaml", &runnable("locked", &["locked_cpu"])),
    ]);
    let locked = catalog.path().join("locked.yaml");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
        .expect("must drop read permission");
    if std::fs::File::open(&locked).is_ok() {
        eprintln!("skipping: this process can open a 0o000 file (running as root?)");
        return;
    }

    let run = run_catalog(catalog.path(), &["--autostart"], &[]);

    assert_eq!(run.metric_names, BTreeSet::from(["alpha_cpu".to_string()]));
    // The wording is the unified `SkipReason` form — `catalog: skipping
    // <path>: unreadable: <os error>` — shared by all four skip reasons
    // rather than one sentence per reason.
    //
    // The leading `: ` is load-bearing. A bare `unreadable:` would also match
    // `<unreadable: …>`, which `catalog::CatalogPackResolver` emits when it
    // cannot read a catalog directory while listing an unknown pack's
    // candidates. That path cannot fire here — nothing resolves a pack
    // reference — so this decouples the two rather than fixing a collision.
    assert_eq!(
        run.count(": unreadable:"),
        1,
        "the file the server could not open must be reported: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("locked.yaml"),
        "the warning must name the file it skipped: {}",
        run.stderr
    );
    assert_eq!(
        run.count("catalog could not be read, starting nothing"),
        0,
        "one unreadable file must not cost the whole catalog: {}",
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
        let run = if expected_autostart {
            run_catalog(catalog.path(), &[], &[("SONDA_AUTOSTART", value)])
        } else {
            run_idle_catalog(catalog.path(), &[], &[("SONDA_AUTOSTART", value)])
        };

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

#[test]
fn the_server_answers_while_the_sweep_is_still_starting_entries() {
    const ENTRIES: usize = 500;

    let files: Vec<(String, String)> = (0..ENTRIES)
        .map(|i| {
            (
                format!("e{i:03}.yaml"),
                runnable(&format!("e{i:03}"), &[&format!("e{i:03}_cpu")]),
            )
        })
        .collect();
    let catalog = catalog_with(
        &files
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
            .collect::<Vec<_>>(),
    );
    let dir = catalog.path().to_str().expect("utf-8 temp path");

    let (port, mut child, tail) = spawn_server_tailed(
        &["--catalog", dir, "--autostart"],
        &[("RUST_LOG", "sonda_server=info")],
    );
    let client = http_client();

    let during_sweep = scenario_count(&client, port);
    let swept = tail.wait_for(SWEEP_SUMMARY, SWEEP_TIMEOUT);
    let after_sweep = scenario_count(&client, port);

    child.kill().expect("must kill sonda-server");
    child.wait().expect("must reap sonda-server");
    let stderr = tail.finish();

    assert!(swept, "the sweep never finished: {stderr}");
    assert!(
        during_sweep < ENTRIES,
        "the first request saw all {ENTRIES} entries already started — either the sweep held \
         the accept loop, or it completed before the request landed (check whether the summary \
         line precedes the request in the log): during_sweep={during_sweep}\n{stderr}"
    );
    assert_eq!(
        after_sweep, ENTRIES,
        "every entry must be running once the sweep reports its summary: {stderr}"
    );
}

fn scenario_count(client: &reqwest::blocking::Client, port: u16) -> usize {
    let body: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .send()
        .expect("GET /scenarios must succeed")
        .json()
        .expect("GET /scenarios must return JSON");
    body["scenarios"]
        .as_array()
        .expect("scenarios must be an array")
        .len()
}

#[test]
fn sigterm_during_the_sweep_exits_cleanly_and_joins_what_it_started() {
    const ENTRIES: usize = 1000;

    let files: Vec<(String, String)> = (0..ENTRIES)
        .map(|i| {
            (
                format!("e{i:04}.yaml"),
                runnable(&format!("e{i:04}"), &[&format!("e{i:04}_cpu")]),
            )
        })
        .collect();
    let catalog = catalog_with(
        &files
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
            .collect::<Vec<_>>(),
    );
    let dir = catalog.path().to_str().expect("utf-8 temp path");

    let (_port, mut child, tail) = spawn_server_tailed(
        &["--catalog", dir, "--autostart"],
        &[("RUST_LOG", "sonda_server=info")],
    );

    let launched_before_signal =
        tail.wait_for_lines("scenario launched", ENTRIES / 5, SWEEP_TIMEOUT);
    let code = terminate_gracefully(&mut child);
    let stderr = tail.finish();

    assert!(
        launched_before_signal,
        "the sweep never got far enough to be interrupted: {stderr}"
    );

    let sweep_lines = stderr
        .lines()
        .filter(|line| line.contains("autostart:"))
        .count();
    let launched = stderr
        .lines()
        .filter(|line| line.contains("scenario launched"))
        .count();
    let accounted_for = stderr
        .lines()
        .filter(|line| line.contains("scenario task joined") || line.contains("join failed"))
        .count();

    assert_eq!(code, Some(0), "SIGTERM must produce a clean exit: {stderr}");
    assert_eq!(
        sweep_lines, 1,
        "the sweep must report itself exactly once, whether it finished or was cut short: {stderr}"
    );
    assert_eq!(
        accounted_for, launched,
        "shutdown must join every scenario the sweep started, so the sweep has to be joined \
         before the drain: launched={launched} accounted_for={accounted_for}"
    );
    assert!(
        stderr.contains("sonda-server shut down cleanly"),
        "the shutdown path must run to the end: {stderr}"
    );
}
