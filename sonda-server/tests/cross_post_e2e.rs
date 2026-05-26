//! Workshop-shape end-to-end coverage for cross-POST `while:` refs.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const BASELINE_YAML: &str = r#"
version: 2
kind: runnable
scenario_name: baseline_post
defaults:
  rate: 100
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: baseline_traffic
    signal_type: metrics
    name: baseline_traffic
    generator:
      type: constant
      value: 1.0
    while:
      ref: cascade_signal
      op: ">"
      value: 0
      scenario_name: cascade_post
      if_unresolved: pending
"#;

const CASCADE_YAML: &str = r#"
version: 2
kind: runnable
scenario_name: cascade_post
defaults:
  rate: 50
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cascade_signal
    signal_type: metrics
    name: cascade_signal
    generator:
      type: constant
      value: 1.0
"#;

fn wait_for_state(
    client: &reqwest::blocking::Client,
    base: &str,
    id: &str,
    expected: &str,
    timeout: Duration,
) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last: Value = Value::Null;
    while Instant::now() < deadline {
        let resp = client
            .get(format!("{base}/scenarios/{id}"))
            .send()
            .expect("GET scenario detail");
        let body: Value = resp.json().expect("detail must be JSON");
        if body["state"] == expected {
            return body;
        }
        last = body;
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for state {expected:?}; last detail = {last}");
}

#[test]
fn t23_workshop_cross_post_lifecycle_with_re_resolution() {
    let (port, _guard) = common::start_server();
    let base = format!("http://127.0.0.1:{port}");
    let client = common::http_client();

    // POST baseline — depends on cascade_post which is not yet running.
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(BASELINE_YAML)
        .send()
        .expect("POST baseline must succeed");
    assert_eq!(resp.status().as_u16(), 201);
    let baseline_body: Value = resp.json().unwrap();
    let baseline_id = baseline_body["id"].as_str().unwrap().to_string();

    // Baseline starts in Unresolved because cascade_post is missing.
    let detail = wait_for_state(
        &client,
        &base,
        &baseline_id,
        "unresolved",
        Duration::from_secs(2),
    );
    assert!(detail["pending_ref"].is_object());
    assert_eq!(detail["pending_ref"]["scenario_name"], "cascade_post");

    // POST cascade — sets scenario_name = cascade_post, registering bus.
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(CASCADE_YAML)
        .send()
        .expect("POST cascade must succeed");
    assert_eq!(resp.status().as_u16(), 201);
    let cascade_body: Value = resp.json().unwrap();
    let cascade_id = cascade_body["id"].as_str().unwrap().to_string();

    // Baseline must transition Unresolved -> Pending -> Running.
    wait_for_state(
        &client,
        &base,
        &baseline_id,
        "running",
        Duration::from_secs(3),
    );

    // Let baseline accumulate events while running.
    thread::sleep(Duration::from_millis(500));
    let snap_before_delete: Value = client
        .get(format!("{base}/scenarios/{baseline_id}/stats"))
        .send()
        .expect("GET stats")
        .json()
        .unwrap();
    let bytes_before = snap_before_delete["bytes_emitted"].as_u64().unwrap();
    assert!(
        bytes_before > 0,
        "baseline must emit while running, got bytes={bytes_before}",
    );

    // DELETE cascade — baseline must drop back to Unresolved.
    let resp = client
        .delete(format!("{base}/scenarios/{cascade_id}"))
        .send()
        .expect("DELETE cascade");
    assert_eq!(resp.status().as_u16(), 200);

    wait_for_state(
        &client,
        &base,
        &baseline_id,
        "unresolved",
        Duration::from_secs(2),
    );

    // POST cascade AGAIN — re-resolution must wire the existing baseline subscriber.
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(CASCADE_YAML)
        .send()
        .expect("POST cascade again");
    assert_eq!(resp.status().as_u16(), 201);
    let cascade_body_2: Value = resp.json().unwrap();
    let cascade_id_2 = cascade_body_2["id"].as_str().unwrap().to_string();

    wait_for_state(
        &client,
        &base,
        &baseline_id,
        "running",
        Duration::from_secs(3),
    );

    // After re-resolution, baseline keeps emitting.
    thread::sleep(Duration::from_millis(500));
    let snap_after_re_resolve: Value = client
        .get(format!("{base}/scenarios/{baseline_id}/stats"))
        .send()
        .expect("GET stats")
        .json()
        .unwrap();
    let bytes_after = snap_after_re_resolve["bytes_emitted"].as_u64().unwrap();
    assert!(
        bytes_after >= bytes_before,
        "bytes_emitted must be monotonic across re-resolution; before={bytes_before}, after={bytes_after}",
    );

    // Clean up both scenarios.
    let _ = client
        .delete(format!("{base}/scenarios/{cascade_id_2}"))
        .send();
    let _ = client
        .delete(format!("{base}/scenarios/{baseline_id}"))
        .send();
}
