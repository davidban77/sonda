//! E2E tests for `GET /ready`: the readiness signal that reports whether the
//! `--autostart` sweep has anything left to start.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{http_client, spawn_server_tailed, start_server, start_server_with};
use tempfile::TempDir;

const SWEEP_SUMMARY: &str = "runnable catalog entries";
const SWEEP_TIMEOUT: Duration = Duration::from_secs(30);

const UNCOMPILABLE: &str = "\
version: 2
kind: runnable
scenario_name: broken
defaults:
  rate: 1
  duration: 300s
scenarios:
  - signal_type: metrics
    name: broken_metric
    generator:
      type: no_such_generator
";

fn runnable(scenario_name: &str) -> String {
    format!(
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
  - signal_type: metrics
    name: {scenario_name}_cpu
    generator:
      type: constant
      value: 1.0
"
    )
}

fn catalog_with(files: &[(String, String)]) -> TempDir {
    let dir = TempDir::new().expect("must create temp catalog dir");
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).expect("must write catalog file");
    }
    dir
}

fn catalog_of(entries: usize) -> TempDir {
    let files: Vec<(String, String)> = (0..entries)
        .map(|i| (format!("e{i:03}.yaml"), runnable(&format!("e{i:03}"))))
        .collect();
    catalog_with(&files)
}

fn dir_arg(catalog: &TempDir) -> &str {
    catalog.path().to_str().expect("utf-8 temp path")
}

#[derive(Debug)]
struct Probe {
    status: u16,
    body: serde_json::Value,
}

fn probe_ready(client: &reqwest::blocking::Client, port: u16) -> Probe {
    let response = client
        .get(format!("http://127.0.0.1:{port}/ready"))
        .send()
        .expect("GET /ready must succeed");
    Probe {
        status: response.status().as_u16(),
        body: response.json().expect("GET /ready must return JSON"),
    }
}

fn health_status(client: &reqwest::blocking::Client, port: u16) -> u16 {
    client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .expect("GET /health must succeed")
        .status()
        .as_u16()
}

fn scrape(client: &reqwest::blocking::Client, port: u16) -> String {
    client
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .expect("GET /metrics must succeed")
        .text()
        .expect("GET /metrics must return a body")
}

/// Start the server on `catalog` with `--autostart` and wait for the sweep summary.
fn swept_server(catalog: &Path) -> (u16, std::process::Child, common::StderrTail) {
    let (port, child, tail) = spawn_server_tailed(
        &[
            "--catalog",
            catalog.to_str().expect("utf-8 temp path"),
            "--autostart",
        ],
        &[("RUST_LOG", "sonda_server=info")],
    );
    assert!(
        tail.wait_for(SWEEP_SUMMARY, SWEEP_TIMEOUT),
        "the sweep never reported a summary: {}",
        tail.text()
    );
    (port, child, tail)
}

#[test]
fn a_server_with_no_catalog_is_ready_at_once() {
    let (port, _guard) = start_server();

    let probe = probe_ready(&http_client(), port);

    assert_eq!(probe.status, 200, "got {probe:?}");
    assert_eq!(probe.body["status"], "not_configured");
    assert_eq!(probe.body["autostart_started"], 0);
    assert_eq!(probe.body["autostart_expected"], 0);
}

#[test]
fn a_catalog_without_autostart_is_ready_at_once() {
    let catalog = catalog_of(3);
    let (port, _guard) = start_server_with(&["--catalog", dir_arg(&catalog)], &[]);

    let probe = probe_ready(&http_client(), port);

    assert_eq!(probe.status, 200, "got {probe:?}");
    assert_eq!(probe.body["status"], "not_configured");
    assert_eq!(
        probe.body["autostart_expected"], 0,
        "nothing is expected when the catalog is only there for pack: refs"
    );
}

#[test]
fn ready_is_503_while_the_sweep_runs_and_200_once_it_finishes() {
    const ENTRIES: usize = 500;
    let catalog = catalog_of(ENTRIES);
    let client = http_client();
    let (port, mut child, tail) = spawn_server_tailed(
        &["--catalog", dir_arg(&catalog), "--autostart"],
        &[("RUST_LOG", "sonda_server=info")],
    );

    let during = probe_ready(&client, port);
    let health_during = health_status(&client, port);
    let swept = tail.wait_for(SWEEP_SUMMARY, SWEEP_TIMEOUT);
    let after = probe_ready(&client, port);
    let health_after = health_status(&client, port);

    child.kill().expect("must kill sonda-server");
    child.wait().expect("must reap sonda-server");
    let stderr = tail.finish();

    assert!(swept, "the sweep never finished: {stderr}");
    assert_eq!(
        during.body["autostart_expected"], ENTRIES,
        "the denominator must be known before the listener binds, not filled in later: {during:?}"
    );
    assert_eq!(
        during.status, 503,
        "the first request landed after the sweep had already finished — check whether the \
         summary line precedes it in the log: {during:?}\n{stderr}"
    );
    assert_eq!(during.body["status"], "in_progress");
    assert!(
        during.body["autostart_started"]
            .as_u64()
            .expect("autostart_started must be a number")
            <= ENTRIES as u64,
        "the sweep starts each entry at most once, so it can never overshoot its own \
         denominator: {during:?}"
    );
    assert_eq!(
        health_during, 200,
        "/health is the liveness signal: it answers while the sweep is still running"
    );

    assert_eq!(after.status, 200, "got {after:?}\n{stderr}");
    assert_eq!(after.body["status"], "finished");
    assert_eq!(after.body["autostart_started"], ENTRIES);
    assert_eq!(after.body["autostart_expected"], ENTRIES);
    assert_eq!(health_after, 200);
}

#[test]
fn a_sweep_that_skipped_a_bad_file_is_still_ready() {
    let catalog = catalog_with(&[
        ("alpha.yaml".to_string(), runnable("alpha")),
        ("broken.yaml".to_string(), UNCOMPILABLE.to_string()),
    ]);
    let (port, mut child, tail) = swept_server(catalog.path());

    let probe = probe_ready(&http_client(), port);

    child.kill().expect("must kill sonda-server");
    child.wait().expect("must reap sonda-server");
    let stderr = tail.finish();

    assert_eq!(
        probe.status, 200,
        "one file that does not compile must not pull the whole server out of service: \
         {probe:?}\n{stderr}"
    );
    assert_eq!(probe.body["status"], "finished");
    assert_eq!(probe.body["autostart_started"], 1);
    assert_eq!(
        probe.body["autostart_expected"], 2,
        "the gap between started and expected is what reports the skip"
    );
}

#[test]
fn metrics_report_what_the_sweep_started_and_what_it_expected() {
    let catalog = catalog_with(&[
        ("alpha.yaml".to_string(), runnable("alpha")),
        ("broken.yaml".to_string(), UNCOMPILABLE.to_string()),
    ]);
    let (port, mut child, tail) = swept_server(catalog.path());

    let text = scrape(&http_client(), port);

    child.kill().expect("must kill sonda-server");
    child.wait().expect("must reap sonda-server");
    let stderr = tail.finish();

    for line in [
        "# TYPE sonda_server_autostart_started gauge",
        "sonda_server_autostart_started 1",
        "# TYPE sonda_server_autostart_expected gauge",
        "sonda_server_autostart_expected 2",
    ] {
        assert!(
            text.contains(line),
            "/metrics must contain `{line}`. Got:\n{text}\n{stderr}"
        );
    }
}

#[test]
fn metrics_report_zero_autostart_counts_when_autostart_is_off() {
    let (port, _guard) = start_server();

    let text = scrape(&http_client(), port);

    for line in [
        "sonda_server_autostart_started 0",
        "sonda_server_autostart_expected 0",
    ] {
        assert!(
            text.contains(line),
            "/metrics must contain `{line}`. Got:\n{text}"
        );
    }
}
