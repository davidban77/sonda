//! End-to-end integration tests for the bounded scheduler and /server/metrics endpoint.

mod common;

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const LONG_RUNNING_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cap_test
    signal_type: metrics
    name: cap_test
    generator:
      type: constant
      value: 1.0
"#;

const SHORT_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: short_scn
    signal_type: metrics
    name: short_scn
    generator:
      type: constant
      value: 1.0
"#;

fn unique_yaml(template: &str, name: &str) -> String {
    template
        .replace("id: cap_test", &format!("id: {name}"))
        .replace("name: cap_test", &format!("name: {name}"))
        .replace("id: short_scn", &format!("id: {name}"))
        .replace("name: short_scn", &format!("name: {name}"))
}

#[test]
fn delete_frees_slot_before_join_window_elapses() {
    let (port, _guard) = common::start_server_with(&["--max-scenarios", "1"], &[]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let body = unique_yaml(LONG_RUNNING_YAML, "slot_first");
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(body)
        .send()
        .expect("POST must succeed");
    assert_eq!(resp.status().as_u16(), 201);
    let json: serde_json::Value = resp.json().expect("JSON");
    let id = json["id"].as_str().expect("id").to_string();

    let delete_url = format!("{base}/scenarios/{id}");
    let delete_client = client.clone();
    let delete_started = Instant::now();
    let delete_join = thread::spawn(move || {
        delete_client
            .delete(delete_url)
            .send()
            .expect("DELETE must succeed")
            .status()
            .as_u16()
    });

    // Give DELETE 50ms to remove the row + drop the permit.
    thread::sleep(Duration::from_millis(50));

    let second_body = unique_yaml(LONG_RUNNING_YAML, "slot_second");
    let second_post_started = Instant::now();
    let second_resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(second_body)
        .send()
        .expect("second POST must succeed");
    let second_elapsed = second_post_started.elapsed();

    assert_eq!(
        second_resp.status().as_u16(),
        201,
        "second POST must succeed after DELETE removes the row, not 5s later (DELETE wall {:?})",
        delete_started.elapsed(),
    );
    assert!(
        second_elapsed < Duration::from_millis(500),
        "second POST returned in {second_elapsed:?}; expected < 500ms after slot release",
    );

    let delete_status = delete_join.join().expect("DELETE thread joined");
    assert_eq!(delete_status, 200, "DELETE must return 200");
}

#[test]
fn sixth_scenario_returns_429_when_max_scenarios_is_five() {
    let (port, _guard) = common::start_server_with(&["--max-scenarios", "5"], &[]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    for i in 0..5 {
        let body = unique_yaml(LONG_RUNNING_YAML, &format!("cap_scn_{i}"));
        let resp = client
            .post(format!("{base}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(body)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201, "POST #{i} must return 201");
    }

    let body = unique_yaml(LONG_RUNNING_YAML, "cap_scn_overflow");
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(body)
        .send()
        .expect("POST must succeed");
    assert_eq!(
        resp.status().as_u16(),
        429,
        "6th POST must return 429 Too Many Requests"
    );

    let body: serde_json::Value = resp.json().expect("JSON body");
    assert_eq!(body["error"], "capacity_exceeded");
    assert!(body["detail"].as_str().unwrap().contains("max 5"));
    let by_state = &body["by_state"];
    assert!(by_state.is_object(), "by_state must be an object");
    for label in [
        "pending",
        "running",
        "paused",
        "held",
        "unresolved",
        "finished",
    ] {
        assert!(
            by_state.get(label).is_some(),
            "by_state must contain '{label}'"
        );
    }
}

#[test]
fn finished_scenarios_count_against_cap() {
    let (port, _guard) = common::start_server_with(&["--max-scenarios", "5"], &[]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    for i in 0..5 {
        let body = unique_yaml(SHORT_YAML, &format!("done_{i}"));
        let resp = client
            .post(format!("{base}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(body)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201);
    }

    // Allow the short scenarios to finish.
    thread::sleep(Duration::from_millis(800));

    let body = unique_yaml(SHORT_YAML, "done_overflow");
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(body)
        .send()
        .expect("POST must succeed");
    assert_eq!(
        resp.status().as_u16(),
        429,
        "Finished rows still consume slots"
    );
    let json: serde_json::Value = resp.json().expect("JSON");
    let finished = json["by_state"]["finished"].as_u64().unwrap_or(0);
    assert_eq!(finished, 5, "all 5 should be in Finished state");

    let metrics = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET /server/metrics must succeed");
    assert_eq!(metrics.status().as_u16(), 200);
    let text = metrics.text().expect("body");
    assert!(
        !text.contains("sonda_server_active_scenarios{state=\"finished\"}"),
        "Finished scenarios must NOT appear in active_scenarios gauge. Got:\n{text}"
    );
    assert!(
        text.contains("sonda_server_scenarios_finished_total 5"),
        "scenarios_finished_total must report 5 after all rows reach Finished. Got:\n{text}"
    );
}

#[test]
fn max_scenarios_zero_allows_unlimited_posts() {
    let (port, mut child) = common::spawn_server_with(&["--max-scenarios", "0"], &[]);
    let stderr = child.stderr.take().expect("child stderr must be piped");
    let (tx, rx) = mpsc::channel::<String>();
    let stderr_thread = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    for i in 0..10 {
        let body = unique_yaml(LONG_RUNNING_YAML, &format!("unl_{i}"));
        let resp = client
            .post(format!("{base}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(body)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201, "POST #{i} must return 201");
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_warn = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if line.contains("scenario row cap disabled") || line.contains("max-scenarios 0") {
                    saw_warn = true;
                    break;
                }
            }
            Err(_) => continue,
        }
    }

    child.kill().ok();
    child.wait().ok();
    let _ = stderr_thread.join();

    assert!(
        saw_warn,
        "expected --max-scenarios 0 WARN line on stderr; none captured before timeout"
    );
}

#[test]
fn server_metrics_emits_all_nine_series_with_zero_state_rows() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let resp = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET must succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let text = resp.text().expect("body");

    for series in [
        "sonda_server_active_scenarios",
        "sonda_server_scenarios_finished_total",
        "sonda_server_worker_threads",
        "sonda_server_max_scenarios",
        "sonda_server_requests_total",
        "sonda_server_request_duration_seconds",
        "sonda_server_sink_errors_total",
        "sonda_server_uptime_seconds",
        "sonda_server_build_info",
    ] {
        assert!(
            text.contains(series),
            "/server/metrics must contain `{series}`. Got:\n{text}"
        );
    }

    for label in ["pending", "running", "paused", "held", "unresolved"] {
        let needle = format!("sonda_server_active_scenarios{{state=\"{label}\"}} 0");
        assert!(
            text.contains(&needle),
            "expected zero-row `{needle}`. Got:\n{text}"
        );
    }
}

#[test]
fn server_metrics_requires_bearer_token_when_api_key_set() {
    let (port, _guard) = common::start_server_with(&[], &[("SONDA_API_KEY", "topsecret")]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let resp = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET must succeed");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "GET /server/metrics without auth must return 401"
    );

    let resp = client
        .get(format!("{base}/server/metrics"))
        .header("authorization", "Bearer topsecret")
        .send()
        .expect("GET must succeed");
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn requests_total_uses_matched_path_for_route_label() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let body = unique_yaml(LONG_RUNNING_YAML, "route_lbl");
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(body)
        .send()
        .expect("POST must succeed");
    assert_eq!(resp.status().as_u16(), 201);
    let json: serde_json::Value = resp.json().expect("JSON");
    let id = json["id"].as_str().expect("id").to_string();

    let _ = client
        .get(format!("{base}/scenarios/{id}/stats"))
        .send()
        .expect("GET stats must succeed");

    let resp = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET /server/metrics must succeed");
    let text = resp.text().expect("body");

    let needle = "route=\"/scenarios/{id}/stats\"";
    assert!(
        text.contains(needle),
        "route label must be the matched-path template `{needle}`, NOT the concrete UUID. Got:\n{text}"
    );
    assert!(
        !text.contains(&format!("route=\"/scenarios/{id}/stats\"")),
        "route label MUST NOT include the concrete UUID"
    );
}

#[test]
fn body_limit_returns_413_with_structured_error() {
    let (port, _guard) = common::start_server_with(&["--max-body-bytes", "1024"], &[]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let huge_body = "x".repeat(8 * 1024);
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(huge_body)
        .send()
        .expect("POST must succeed");
    assert_eq!(
        resp.status().as_u16(),
        413,
        "oversized body must return 413 Payload Too Large"
    );
}

#[test]
fn request_timeout_zero_is_rejected_at_parse_time() {
    use std::process::{Command, Stdio};

    let binary = env!("CARGO_BIN_EXE_sonda-server");
    let out = Command::new(binary)
        .args([
            "--port",
            "0",
            "--bind",
            "127.0.0.1",
            "--request-timeout",
            "0",
        ])
        .env_remove("SONDA_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("must spawn sonda-server");
    assert!(
        !out.status.success(),
        "--request-timeout 0 must exit non-zero, got status {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("request-timeout") || stderr.contains("request_timeout"),
        "clap rejection must mention the flag, got stderr: {stderr}"
    );
}

#[test]
fn request_timeout_returns_408_with_structured_error() {
    let (port, _guard) = common::start_server_with(&["--request-timeout", "1"], &[]);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("HTTP client");
    let base = format!("http://127.0.0.1:{port}");

    // /events builds the sink in the handler; TEST-NET-1 drops SYNs so the
    // TCP connect hangs past `--request-timeout 1`.
    let body = serde_json::json!({
        "signal_type": "logs",
        "log": {"severity": "info", "message": "x"},
        "encoder": {"type": "json_lines"},
        "sink": {
            "type": "tcp",
            "address": "192.0.2.1:1"
        },
    });

    let resp = client
        .post(format!("{base}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("POST must succeed");

    assert_eq!(
        resp.status().as_u16(),
        408,
        "request awaiting beyond --request-timeout must return 408 Request Timeout"
    );
}

#[test]
fn single_worker_runtime_drains_three_scenarios_on_sigterm() {
    let (port, mut child) =
        common::spawn_server_with(&["--workers", "1", "--max-scenarios", "10"], &[]);
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    for i in 0..3 {
        let body = unique_yaml(LONG_RUNNING_YAML, &format!("workers1_{i}"));
        let resp = client
            .post(format!("{base}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(body)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201, "POST #{i} must return 201");
    }

    // Sanity-check the process is still alive before signalling.
    let health = client
        .get(format!("{base}/health"))
        .send()
        .expect("GET /health must succeed");
    assert_eq!(health.status().as_u16(), 200);

    let started = Instant::now();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = child.wait().expect("must wait for child");
    let elapsed = started.elapsed();

    assert!(
        elapsed <= Duration::from_secs(6),
        "single-worker runtime must drain 3 scenarios within 6s of SIGTERM; took {elapsed:?}"
    );
    assert!(
        status.success(),
        "SIGTERM must produce a clean exit, got {status:?}"
    );
}

#[test]
fn health_remains_responsive_when_control_plane_is_saturated() {
    let (port, _guard) = common::start_server_with(&["--max-inflight-requests", "1"], &[]);
    let base = format!("http://127.0.0.1:{port}");

    // Hold the /events permit open via a TCP connect to TEST-NET-1 (drops SYNs).
    let saturator_base = base.clone();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let saturator = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        let body = serde_json::json!({
            "signal_type": "logs",
            "log": {"severity": "info", "message": "saturator"},
            "encoder": {"type": "json_lines"},
            "sink": {
                "type": "tcp",
                "address": "192.0.2.1:1"
            },
        });
        let req = client
            .post(format!("{saturator_base}/events"))
            .header("content-type", "application/json")
            .body(body.to_string());
        started_tx.send(()).ok();
        req.send().ok();
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("saturator thread did not start");
    // Allow the saturator's send() to reach the server and await the TCP connect.
    thread::sleep(Duration::from_millis(500));

    let client = common::http_client();
    let health = client
        .get(format!("{base}/health"))
        .send()
        .expect("GET /health must succeed");
    assert_eq!(
        health.status().as_u16(),
        200,
        "/health must stay reachable while POST holds the concurrency permit"
    );

    let server_metrics = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET /server/metrics must succeed");
    assert_eq!(
        server_metrics.status().as_u16(),
        200,
        "/server/metrics must stay reachable while POST holds the concurrency permit"
    );

    // A second POST on the same route must be queued behind the saturator's permit.
    let gated_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .expect("HTTP client");
    let gated_body = serde_json::json!({
        "signal_type": "logs",
        "log": {"severity": "info", "message": "gated"},
        "encoder": {"type": "json_lines"},
        "sink": {
            "type": "tcp",
            "address": "192.0.2.1:1"
        },
    });
    let gated_started = Instant::now();
    let gated_result = gated_client
        .post(format!("{base}/events"))
        .header("content-type", "application/json")
        .body(gated_body.to_string())
        .send();
    let gated_elapsed = gated_started.elapsed();

    assert!(
        gated_elapsed >= Duration::from_millis(500),
        "second POST on the saturated route should have been queued; elapsed {gated_elapsed:?} (result: {gated_result:?})"
    );

    drop(saturator);
}

#[test]
fn global_inflight_limit_gates_across_routes() {
    let (port, _guard) = common::start_server_with(&["--max-inflight-requests", "1"], &[]);
    let base = format!("http://127.0.0.1:{port}");

    // Hold the only permit on /events via a TCP connect to TEST-NET-1 (drops SYNs).
    let saturator_base = base.clone();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let saturator = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        let body = serde_json::json!({
            "signal_type": "logs",
            "log": {"severity": "info", "message": "saturator"},
            "encoder": {"type": "json_lines"},
            "sink": {
                "type": "tcp",
                "address": "192.0.2.1:1"
            },
        });
        let req = client
            .post(format!("{saturator_base}/events"))
            .header("content-type", "application/json")
            .body(body.to_string());
        started_tx.send(()).ok();
        req.send().ok();
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("saturator thread did not start");
    // Allow the saturator's send() to reach the server and await the TCP connect.
    thread::sleep(Duration::from_millis(500));

    // POST /scenarios must be gated by the same global permit even though it is
    // a different route than /events.
    let gated_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .expect("HTTP client");
    let gated_body = unique_yaml(LONG_RUNNING_YAML, "cross_route");
    let gated_started = Instant::now();
    let gated_result = gated_client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(gated_body)
        .send();
    let gated_elapsed = gated_started.elapsed();

    assert!(
        gated_elapsed >= Duration::from_millis(500),
        "POST /scenarios should have been queued behind the /events permit; elapsed {gated_elapsed:?} (result: {gated_result:?})"
    );

    drop(saturator);
}

#[test]
fn observability_endpoints_reachable_under_concurrent_post_saturation() {
    let (port, _guard) = common::start_server_with(&["--max-inflight-requests", "1"], &[]);
    let base = format!("http://127.0.0.1:{port}");

    // First, register a scenario so /scenarios/{id}/metrics has a real id to hit.
    let client = common::http_client();
    let body = unique_yaml(LONG_RUNNING_YAML, "sat_obs");
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(body)
        .send()
        .expect("POST must succeed");
    assert_eq!(resp.status().as_u16(), 201);
    let json: serde_json::Value = resp.json().expect("JSON");
    let id = json["id"].as_str().expect("id").to_string();

    let saturator_base = base.clone();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let saturator = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        let body = serde_json::json!({
            "signal_type": "logs",
            "log": {"severity": "info", "message": "saturator"},
            "encoder": {"type": "json_lines"},
            "sink": {
                "type": "tcp",
                "address": "192.0.2.1:1"
            },
        });
        let req = client
            .post(format!("{saturator_base}/events"))
            .header("content-type", "application/json")
            .body(body.to_string());
        started_tx.send(()).ok();
        req.send().ok();
    });

    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("saturator thread did not start");
    thread::sleep(Duration::from_millis(500));

    let health = client
        .get(format!("{base}/health"))
        .send()
        .expect("GET /health");
    assert_eq!(health.status().as_u16(), 200);

    let server_metrics = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET /server/metrics");
    assert_eq!(server_metrics.status().as_u16(), 200);

    let scenario_metrics = client
        .get(format!("{base}/scenarios/{id}/metrics"))
        .send()
        .expect("GET /scenarios/{id}/metrics");
    assert_eq!(
        scenario_metrics.status().as_u16(),
        200,
        "scenario scrape must stay reachable while control-plane POST holds the permit"
    );

    drop(saturator);
}

#[test]
fn build_info_exposes_version_and_git_sha() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();
    let base = format!("http://127.0.0.1:{port}");

    let resp = client
        .get(format!("{base}/server/metrics"))
        .send()
        .expect("GET must succeed");
    let text = resp.text().expect("body");
    assert!(text.contains("sonda_server_build_info{version="));
    assert!(text.contains("git_sha="));
}
