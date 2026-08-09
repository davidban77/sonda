//! Integration tests for `sonda test`.
//!
//! A minimal in-process HTTP responder plays the role of the Prometheus
//! query API, scripted per test: how many polls return "inactive" before the
//! alert reports firing, and whether it eventually resolves.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::sonda_bin;
use tempfile::TempDir;

const EMPTY_RESULT: &str = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
const FIRING_RESULT: &str = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{"alertname":"HighCpuUsage","alertstate":"firing"},"value":[1,"1"]}]}}"#;

/// Serve canned instant-query responses. `body_for(n)` picks the body for
/// the n-th request (0-based); the server runs until the listener drops at
/// the end of the test.
fn mock_prometheus(
    body_for: impl Fn(usize) -> &'static str + Send + 'static,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock prometheus");
    let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            // Drain request headers; the query itself doesn't matter here.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() {
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                line.clear();
            }
            let n = hits_clone.fetch_add(1, Ordering::SeqCst);
            let body = body_for(n);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (base_url, hits)
}

fn scenario_with_expect(expect_block: &str) -> String {
    format!(
        "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
{expect_block}"
    )
}

fn write_scenario(dir: &TempDir, yaml: &str) -> std::path::PathBuf {
    let path = dir.path().join("scenario.yaml");
    std::fs::write(&path, yaml).expect("write scenario");
    path
}

#[test]
fn passes_when_alert_fires_and_resolves() {
    // Firing from the first poll; inactive again once resolution polling
    // starts (from request 3 on).
    let (url, _hits) = mock_prometheus(|n| if n < 3 { FIRING_RESULT } else { EMPTY_RESULT });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
      resolves_within: 5s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {stderr}",
        output.status
    );
    assert!(stderr.contains("firing after"), "stderr: {stderr}");
    assert!(stderr.contains("resolved after"), "stderr: {stderr}");
    assert!(
        stderr.contains("1 alert expectation(s) verified"),
        "stderr: {stderr}"
    );
}

#[test]
fn fails_when_alert_never_fires() {
    let (url, _hits) = mock_prometheus(|_| EMPTY_RESULT);
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 1s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "must exit non-zero\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("did not fire within 1s"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 alert expectation(s) failed"),
        "stderr: {stderr}"
    );
}

#[test]
fn rejects_scenario_without_expect_block() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(""));

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", "http://127.0.0.1:1"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("no `expect:` block"), "stderr: {stderr}");
}

#[test]
fn dry_run_validates_without_polling() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
",
        ),
    );

    // Unreachable URL proves --dry-run never talks to Prometheus.
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", "http://127.0.0.1:1"])
        .output()
        .expect("run sonda test --dry-run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("1 alert expectation(s) parsed OK"),
        "stderr: {stderr}"
    );
}

#[test]
fn run_still_accepts_files_with_expect_blocks() {
    // The expect block is pure metadata — `sonda run` must not reject it.
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["run", path.to_str().expect("utf8 path")])
        .output()
        .expect("run sonda run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
