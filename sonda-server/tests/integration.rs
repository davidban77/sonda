//! Full lifecycle integration tests for sonda-server.
//!
//! This test exercises the complete scenario API: POST (create) -> GET (list) ->
//! GET (stats) -> DELETE (stop) for both metrics and logs scenario types.
//!
//! The server is started as a child process on a random port and all assertions
//! use real HTTP requests via `reqwest::blocking`.

mod common;

use std::time::Duration;

/// Minimal v2 metrics scenario YAML that runs at a low rate with stdout sink.
const METRICS_YAML: &str = r#"
version: 2
defaults:
  rate: 10
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: test_metric
    signal_type: metrics
    name: test_metric
    generator:
      type: constant
      value: 42.0
"#;

/// Minimal v2 logs scenario YAML that runs at a low rate with stdout sink.
const LOGS_YAML: &str = r#"
version: 2
defaults:
  rate: 10
  duration: 30s
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
  - id: test_log
    signal_type: logs
    name: test_log
    log_generator:
      type: template
      templates:
        - message: "integration test log line"
"#;

/// Full lifecycle integration test exercising both metrics and logs scenarios.
///
/// Steps:
/// 1. POST a metrics scenario -> 201
/// 2. POST a logs scenario -> 201
/// 3. GET /scenarios -> both listed as running
/// 4. Wait 3 seconds -> GET /scenarios/:id/stats -> total_events > 0 for both
/// 5. DELETE both -> 200, status "stopped"
/// 6. GET /scenarios -> both show as stopped
#[test]
fn full_lifecycle_metrics_and_logs() {
    let (port, _guard) = common::start_server();
    let base = format!("http://127.0.0.1:{port}");
    let client = common::http_client();

    // -- Step 1: POST metrics scenario -> 201 --
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(METRICS_YAML)
        .send()
        .expect("POST metrics scenario must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST metrics scenario must return 201 Created"
    );

    let metrics_body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let metrics_id = metrics_body["id"]
        .as_str()
        .expect("response must have an id field")
        .to_string();
    assert_eq!(
        metrics_body["name"].as_str(),
        Some("test_metric"),
        "metrics scenario name must match"
    );
    assert_eq!(
        metrics_body["status"].as_str(),
        Some("running"),
        "metrics scenario status must be running"
    );

    // -- Step 2: POST logs scenario -> 201 --
    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(LOGS_YAML)
        .send()
        .expect("POST logs scenario must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST logs scenario must return 201 Created"
    );

    let logs_body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let logs_id = logs_body["id"]
        .as_str()
        .expect("response must have an id field")
        .to_string();
    assert_eq!(
        logs_body["name"].as_str(),
        Some("test_log"),
        "logs scenario name must match"
    );
    assert_eq!(
        logs_body["status"].as_str(),
        Some("running"),
        "logs scenario status must be running"
    );

    // -- Step 3: GET /scenarios -> both listed --
    let resp = client
        .get(format!("{base}/scenarios"))
        .send()
        .expect("GET /scenarios must succeed");

    assert_eq!(resp.status().as_u16(), 200);

    let list: serde_json::Value = resp.json().expect("response must be valid JSON");
    let scenarios = list["scenarios"]
        .as_array()
        .expect("response must have a scenarios array");

    assert!(
        scenarios.len() >= 2,
        "GET /scenarios must list at least 2 scenarios, got {}",
        scenarios.len()
    );

    let ids_in_list: Vec<&str> = scenarios.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(
        ids_in_list.contains(&metrics_id.as_str()),
        "metrics scenario must be in list"
    );
    assert!(
        ids_in_list.contains(&logs_id.as_str()),
        "logs scenario must be in list"
    );

    // Verify both show as running.
    for s in scenarios {
        if s["id"].as_str() == Some(metrics_id.as_str())
            || s["id"].as_str() == Some(logs_id.as_str())
        {
            assert_eq!(
                s["status"].as_str(),
                Some("running"),
                "scenario {} must be running",
                s["id"]
            );
        }
    }

    // -- Step 4: Wait 3 seconds, then check stats --
    std::thread::sleep(Duration::from_secs(3));

    for (label, id) in [("metrics", &metrics_id), ("logs", &logs_id)] {
        let resp = client
            .get(format!("{base}/scenarios/{id}/stats"))
            .send()
            .unwrap_or_else(|_| panic!("GET /scenarios/{id}/stats must succeed"));

        assert_eq!(
            resp.status().as_u16(),
            200,
            "GET /scenarios/{id}/stats must return 200"
        );

        let stats: serde_json::Value = resp.json().expect("stats response must be valid JSON");
        let total_events = stats["total_events"]
            .as_u64()
            .expect("stats must have total_events");
        assert!(
            total_events > 0,
            "{label} scenario must have emitted events after 3 seconds, got total_events={total_events}"
        );
    }

    // -- Step 5: DELETE both -> 200, status "stopped" --
    for (label, id) in [("metrics", &metrics_id), ("logs", &logs_id)] {
        let resp = client
            .delete(format!("{base}/scenarios/{id}"))
            .send()
            .unwrap_or_else(|_| panic!("DELETE /scenarios/{id} must succeed"));

        assert_eq!(
            resp.status().as_u16(),
            200,
            "DELETE {label} scenario must return 200"
        );

        let del_body: serde_json::Value = resp.json().expect("delete response must be valid JSON");
        assert_eq!(
            del_body["id"].as_str(),
            Some(id.as_str()),
            "delete response must echo the scenario id"
        );
        assert!(
            del_body["status"].as_str() == Some("stopped")
                || del_body["status"].as_str() == Some("force_stopped"),
            "{label} scenario status must be stopped or force_stopped, got {:?}",
            del_body["status"]
        );
        assert!(
            del_body["total_events"].as_u64().unwrap_or(0) > 0,
            "{label} scenario must report non-zero total_events after deletion"
        );
    }

    // -- Step 6: GET /scenarios -> both removed after DELETE --
    let resp = client
        .get(format!("{base}/scenarios"))
        .send()
        .expect("GET /scenarios must succeed after deletions");

    assert_eq!(resp.status().as_u16(), 200);

    let list: serde_json::Value = resp.json().expect("response must be valid JSON");
    let scenarios = list["scenarios"]
        .as_array()
        .expect("response must have a scenarios array");

    assert!(
        scenarios.is_empty(),
        "GET /scenarios must return empty list after all scenarios are deleted, got {} entries",
        scenarios.len()
    );
}
