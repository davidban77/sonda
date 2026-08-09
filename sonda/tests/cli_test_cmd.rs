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

/// Serve scripted instant-query responses. `respond(n, request_line)` picks
/// the body and an artificial delay for the n-th request (0-based); the raw
/// request line carries the query, so tests with several expectations can
/// script per-alert behavior by matching on the alert name. Each connection
/// is handled on its own thread so a stalled response never blocks the next
/// request. The server runs until the listener drops at test end.
fn mock_prometheus(
    respond: impl Fn(usize, &str) -> (&'static str, std::time::Duration) + Send + Sync + 'static,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock prometheus");
    let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    let respond = Arc::new(respond);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let n = hits_clone.fetch_add(1, Ordering::SeqCst);
            let respond = Arc::clone(&respond);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                // Drain the remaining headers.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() {
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    line.clear();
                }
                let (body, stall) = respond(n, &request_line);
                std::thread::sleep(stall);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    (base_url, hits)
}

const NO_STALL: std::time::Duration = std::time::Duration::ZERO;

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
    let (url, _hits) =
        mock_prometheus(|n, _| (if n < 3 { FIRING_RESULT } else { EMPTY_RESULT }, NO_STALL));
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
    let (url, _hits) = mock_prometheus(|_, _| (EMPTY_RESULT, NO_STALL));
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
fn fails_when_alert_fires_after_deadline() {
    // Round-1 blocker 1, reproduction B: the alert eventually reports
    // firing, but only after firing_within has passed — must be a failure,
    // not a pass. Request 0 is the preflight (fast); the poller's first
    // query returns inactive immediately (window coverage — without it the
    // verdict is Undecided, not Late), then queries stall 400ms against the
    // 200ms deadline.
    let (url, _hits) = mock_prometheus(|n, _| match n {
        0 | 1 => (EMPTY_RESULT, NO_STALL),
        _ => (FIRING_RESULT, std::time::Duration::from_millis(400)),
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 200ms
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
        "late firing must fail\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("later than firing_within 200ms"),
        "stderr: {stderr}"
    );
}

#[test]
fn fails_when_second_resolution_is_delayed_past_deadline() {
    // A genuinely blown resolution deadline must be a failure: both alerts
    // stay firing until 1200ms after the first request, so MustResolveFast
    // is observed still firing through its whole 200ms window (concurrent
    // resolution polling covers it from scenario end — round-2 review
    // blocker) while SlowToResolve's 4s deadline still passes. The switch
    // is anchored on the mock's first request, not process spawn, so binary
    // startup time doesn't skew the fixture (round-2 review M5).
    let first_hit = Arc::new(std::sync::OnceLock::new());
    let (url, _hits) = mock_prometheus(move |_, _| {
        let started = *first_hit.get_or_init(std::time::Instant::now);
        (
            if started.elapsed() < std::time::Duration::from_millis(1200) {
                FIRING_RESULT
            } else {
                EMPTY_RESULT
            },
            NO_STALL,
        )
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: SlowToResolve
      firing_within: 5s
      resolves_within: 4s
    - alert: MustResolveFast
      firing_within: 5s
      resolves_within: 200ms
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
        "blown second resolution deadline must fail\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("MustResolveFast still firing 200ms after scenario end"),
        "stderr: {stderr}"
    );
    // The first expectation's generous deadline still passes.
    assert!(
        stderr.contains("SlowToResolve resolved after"),
        "stderr: {stderr}"
    );
}

#[test]
fn parallel_resolutions_do_not_blind_each_others_windows() {
    // Round-2 review blocker, discriminating case: SlowLane needs ~10 polls
    // (~1s) to resolve against a 3s deadline while FastLane resolves on its
    // very first resolution query against a 300ms one. With concurrent
    // resolution polling both PASS. If polling regresses to sequential,
    // SlowLane's wait pushes FastLane's first query past 300ms and the run
    // fails — this test must go red.
    let slow_hits = Arc::new(AtomicUsize::new(0));
    let fast_hits = Arc::new(AtomicUsize::new(0));
    let (url, _hits) = mock_prometheus(move |_, request_line| {
        if request_line.contains("SlowLane") {
            // Hits 0 (preflight) and 1 (firing poll) must show firing; then
            // ~10 firing resolution polls before resolving.
            let n = slow_hits.fetch_add(1, Ordering::SeqCst);
            (if n < 12 { FIRING_RESULT } else { EMPTY_RESULT }, NO_STALL)
        } else {
            let n = fast_hits.fetch_add(1, Ordering::SeqCst);
            (if n < 2 { FIRING_RESULT } else { EMPTY_RESULT }, NO_STALL)
        }
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: SlowLane
      firing_within: 5s
      resolves_within: 3s
    - alert: FastLane
      firing_within: 5s
      resolves_within: 300ms
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
        "both resolutions meet their own deadlines and must pass\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("2 alert expectation(s) verified"),
        "stderr: {stderr}"
    );
}

#[test]
fn bounded_failure_when_endpoint_stalls_before_answering() {
    // Review blocker 2, reproduction C: an endpoint that accepts the
    // connection and never answers in time must produce a bounded non-zero
    // exit (preflight timeout), not a SIGINT-proof hang.
    let (url, _hits) = mock_prometheus(|_, _| (EMPTY_RESULT, std::time::Duration::from_secs(60)));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 200ms
",
        ),
    );

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url])
        .args(["--interval", "100ms", "--query-timeout", "300ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "stall must fail\nstderr: {stderr}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must fail in bounded time, took {:?}",
        started.elapsed()
    );
    assert!(stderr.contains("preflight"), "stderr: {stderr}");
}

#[test]
fn mid_run_stall_surfaces_query_errors_on_missed_deadline() {
    // Stall only after preflight: every poll times out, the deadline passes,
    // and the failure must carry the query error instead of hanging or
    // silently passing.
    let (url, _hits) = mock_prometheus(|n, _| {
        if n == 0 {
            (EMPTY_RESULT, NO_STALL)
        } else {
            (EMPTY_RESULT, std::time::Duration::from_secs(30))
        }
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 600ms
",
        ),
    );

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url])
        .args(["--interval", "100ms", "--query-timeout", "300ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must fail in bounded time, took {:?}",
        started.elapsed()
    );
    assert!(
        stderr.contains("did not fire within 600ms") && stderr.contains("last query error"),
        "stderr: {stderr}"
    );
}

#[test]
fn dry_run_works_without_prometheus_url() {
    // Review M2: --dry-run never contacts the endpoint, so it must not
    // demand one.
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
        .args(["--dry-run", "test", path.to_str().expect("utf8 path")])
        .env_remove("SONDA_PROMETHEUS_URL")
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
fn missing_prometheus_url_is_rejected_outside_dry_run() {
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
        .args(["test", path.to_str().expect("utf8 path")])
        .env_remove("SONDA_PROMETHEUS_URL")
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("--prometheus-url is required"),
        "stderr: {stderr}"
    );
}

#[test]
fn failed_scenario_reports_immediately_without_waiting_out_deadlines() {
    // Review W3: a scenario that dies at launch (unreachable TCP sink) must
    // surface its own error promptly instead of blocking behind a long
    // firing_within.
    let (url, _hits) = mock_prometheus(|_, _| (EMPTY_RESULT, NO_STALL));
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: 127.0.0.1:1
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 60s
";
    let path = write_scenario(&dir, yaml);

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "run failure must not wait out firing_within, took {:?}",
        started.elapsed()
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
