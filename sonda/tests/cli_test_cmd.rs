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

/// Serve scripted query responses. `respond(n, request_line)` picks the
/// body and an artificial delay for the n-th request (0-based); the raw
/// request line carries the path and query, so responders distinguish
/// instant queries from `/api/v1/query_range` and script per-alert behavior
/// by matching on the alert name. Each connection is handled on its own
/// thread so a stalled response never blocks the next request. The server
/// runs until the listener drops at test end.
fn mock_prometheus(
    respond: impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static,
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

/// Extract `(start, end, step)` from a `/api/v1/query_range` request line;
/// `None` for instant queries.
fn parse_range_params(request_line: &str) -> Option<(f64, f64, f64)> {
    if !request_line.contains("/api/v1/query_range") {
        return None;
    }
    let target = request_line.split_whitespace().nth(1)?;
    let (_, params) = target.split_once('?')?;
    let (mut start, mut end, mut step) = (None, None, None);
    for pair in params.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "start" => start = value.parse().ok(),
            "end" => end = value.parse().ok(),
            "step" => step = value.parse().ok(),
            _ => {}
        }
    }
    Some((start?, end?, step?))
}

/// Build a range-query matrix body: one firing series (with `host`) whose
/// samples cover the grid points where `unix_ts - anchor_unix` falls inside
/// `[fire_at, resolve_at)` seconds.
fn matrix_body(
    start: f64,
    end: f64,
    step: f64,
    anchor_unix: f64,
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &str,
) -> String {
    let mut values = Vec::new();
    let steps = ((end - start) / step).floor() as u64;
    for index in 0..=steps {
        let ts = start + index as f64 * step;
        let offset = ts - anchor_unix;
        let firing = fire_at.is_some_and(|f| offset >= f) && resolve_at.is_none_or(|r| offset < r);
        if firing {
            values.push(format!("[{ts:.3},\"1\"]"));
        }
    }
    if values.is_empty() {
        return r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#.to_string();
    }
    format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[{{"metric":{{"alertname":"HighCpuUsage","alertstate":"firing","host":"{host}"}},"values":[{}]}}]}}}}"#,
        values.join(",")
    )
}

/// A time-consistent fake alert: firing during `[fire_at, resolve_at)`
/// seconds measured from the mock's first request. Instant and range
/// queries answer from the same clock, so live polling and post-hoc range
/// acquisition see one coherent world.
fn alert_world(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
) -> impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static {
    skewed_alert_world(fire_at, resolve_at, host, 0.0)
}

/// [`alert_world`] whose *server clock* runs `skew` seconds ahead of the
/// local one (negative = behind). `time()` queries, matrix sample
/// timestamps, and range-window interpretation all live on that server
/// clock; alert behaviour stays fixed relative to first contact. A world
/// with skew is the discriminating fixture for verdict-anchor bugs: the
/// reported transition times must not move with the skew.
fn skewed_alert_world(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
    skew: f64,
) -> impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static {
    let anchor: Arc<std::sync::OnceLock<(std::time::Instant, f64)>> =
        Arc::new(std::sync::OnceLock::new());
    move |_, request_line| {
        let (mono0, unix0_local) = *anchor.get_or_init(|| {
            (
                std::time::Instant::now(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("epoch")
                    .as_secs_f64(),
            )
        });
        let unix0_server = unix0_local + skew;
        if request_line.contains("query=time") {
            let server_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs_f64()
                + skew;
            (
                format!(
                    r#"{{"status":"success","data":{{"resultType":"scalar","result":[{server_now:.3},"{server_now:.3}"]}}}}"#
                ),
                NO_STALL,
            )
        } else if let Some((start, end, step)) = parse_range_params(request_line) {
            (
                matrix_body(start, end, step, unix0_server, fire_at, resolve_at, host),
                NO_STALL,
            )
        } else {
            let offset = mono0.elapsed().as_secs_f64();
            let firing =
                fire_at.is_some_and(|f| offset >= f) && resolve_at.is_none_or(|r| offset < r);
            (
                (if firing { FIRING_RESULT } else { EMPTY_RESULT }).to_string(),
                NO_STALL,
            )
        }
    }
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
    // Firing from the first instant, resolving one second in — shortly
    // after the 300ms scenario ends.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), Some(1.0), "sonda-test"));
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
        .args(["--query-step", "200ms"])
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
    let (url, _hits) = mock_prometheus(alert_world(None, None, "sonda-test"));
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
        .args(["--query-step", "200ms"])
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
    // #516 round-1 blocker 1, reproduction B: the alert fires, but only
    // after firing_within has passed — must be a failure, not a pass. The
    // world fires at 300ms against a 200ms deadline; the range grid has
    // pre-deadline coverage (inactive samples) and then the late firing
    // sample, so the verdict is Late.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.3), None, "sonda-test"));
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
        .args(["--query-step", "200ms"])
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
    // is still firing through its whole 200ms window while SlowToResolve's
    // 4s deadline passes. The world is anchored on the mock's first
    // request, not process spawn (#527 review M5).
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), Some(1.2), "sonda-test"));
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
        .args(["--query-step", "200ms"])
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
    // #516 round-2 blocker heritage: SlowLane keeps firing for 1.5s against
    // a 3s resolution deadline while FastLane resolves right at scenario
    // end against a 300ms one. Both must PASS — no expectation's window may
    // be blinded by another's wait, whether by concurrent polling or by the
    // range-acquired verdict timelines.
    let slow = alert_world(Some(0.0), Some(1.5), "sonda-test");
    let fast = alert_world(Some(0.0), Some(0.4), "sonda-test");
    let (url, _hits) = mock_prometheus(move |n, request_line| {
        if request_line.contains("SlowLane") {
            slow(n, request_line)
        } else {
            fast(n, request_line)
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
        .args(["--query-step", "200ms"])
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
    let (url, _hits) =
        mock_prometheus(|_, _| (EMPTY_RESULT.to_string(), std::time::Duration::from_secs(60)));
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
            (EMPTY_RESULT.to_string(), NO_STALL)
        } else {
            (EMPTY_RESULT.to_string(), std::time::Duration::from_secs(30))
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
        .args(["--query-step", "300ms"])
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
    // With the range API also stalled, the verdict falls back to the live
    // poll timeline — and says so.
    assert!(
        stderr.contains("uses the live poll timeline"),
        "stderr: {stderr}"
    );
}

#[test]
fn firing_verdict_uses_sample_time_not_poll_time() {
    // The range-acquisition contract: transition times come from the stored
    // samples, not from when polling happened to look. The alert fires at
    // 300ms against a 1s deadline, but the live poll interval is a huge 2s
    // — poll-based verdicts would see the firing at ~2s and call it Late.
    // The range grid shows the 400ms sample, so this must PASS. Goes red if
    // verdicts revert to the poll timeline.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.3), None, "sonda-test"));
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
        .args(["--prometheus-url", &url, "--interval", "2s"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sample-time verdict must pass despite the coarse poll interval\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("firing after 400ms (within 1s)"),
        "stderr: {stderr}"
    );
}

#[test]
fn verdicts_survive_a_server_clock_running_ahead() {
    // #528 review blocker, direction 1: the server's clock is 1s ahead of
    // the local one. The alert fires 400ms in (real time) against a 1s
    // deadline — a clear pass. A local-anchored conversion would read the
    // firing sample as landing at 1.4s and flip the verdict to Late.
    let (url, _hits) = mock_prometheus(skewed_alert_world(Some(0.3), None, "sonda-test", 1.0));
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
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "server-clock skew must not move the verdict\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("firing after 400ms (within 1s)"),
        "stderr: {stderr}"
    );
}

#[test]
fn verdicts_survive_a_server_clock_running_behind() {
    // #528 review blocker, direction 2 — the dangerous one: the server's
    // clock is 1.5s behind. The alert fires at 2s (real time) against a 1s
    // deadline, so FAIL is the only correct verdict. A local-anchored
    // conversion would see the firing sample at 2 - 1.5 = 0.5s and report
    // a false PASS with the deadline apparently met.
    let (url, _hits) = mock_prometheus(skewed_alert_world(Some(2.0), None, "sonda-test", -1.5));
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
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a missed deadline must not become a pass under negative skew\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("did not fire within 1s"),
        "stderr: {stderr}"
    );
}

#[test]
fn warns_when_matched_series_carries_foreign_label_values() {
    // #527 review, W2 follow-up: ALERTS is global, so an expectation can
    // match an alert another series caused. When the matched firing series
    // carries a label value the scenario never emits, say so.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), None, "intruder-host"));
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
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
    labels:
      host: sonda-test
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
";
    let path = write_scenario(&dir, yaml);

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The firing check itself passes — the warning is advisory.
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("WARN")
            && stderr.contains("host=\"intruder-host\"")
            && stderr.contains("scope `expect.labels`"),
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
    let (url, _hits) = mock_prometheus(|_, _| (EMPTY_RESULT.to_string(), NO_STALL));
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
