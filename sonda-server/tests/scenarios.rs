//! Integration tests for sonda-server Slice 3.2 -- POST /scenarios.
//!
//! These tests verify the POST /scenarios endpoint by starting an actual
//! sonda-server binary and making real HTTP requests against it.

mod common;

/// Valid v2 metrics YAML (short duration for quick tests).
const VALID_METRICS_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: integration_metric
    signal_type: metrics
    name: integration_metric
    generator:
      type: constant
      value: 42.0
";

/// Valid v2 logs YAML (short duration for quick tests).
const VALID_LOGS_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
  - id: integration_logs
    signal_type: logs
    name: integration_logs
    log_generator:
      type: template
      templates:
        - message: \"integration test log\"
          field_pools: {}
      seed: 0
";

/// Valid v2 metrics YAML with an explicit `signal_type: metrics` entry.
const VALID_TAGGED_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: tagged_integration
    signal_type: metrics
    name: tagged_integration
    generator:
      type: constant
      value: 1.0
";

// ---- Test: POST valid metrics YAML -> 201, scenario ID returned ---------------

/// POST a valid metrics YAML body to the real server returns 201 with a scenario ID.
#[test]
fn post_valid_metrics_yaml_returns_201() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_METRICS_YAML)
        .send()
        .expect("POST /scenarios must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid metrics YAML must return 201 Created"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert!(
        body["id"].is_string() && !body["id"].as_str().unwrap().is_empty(),
        "response must contain a non-empty scenario ID"
    );
    assert_eq!(body["name"], "integration_metric");
    let s = body["state"].as_str().unwrap_or("");
    assert!(
        matches!(s, "pending" | "running"),
        "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
    );
}

// ---- Test: POST valid logs YAML -> 201 ----------------------------------------

/// POST a valid logs YAML body returns 201 with the logs scenario name.
#[test]
fn post_valid_logs_yaml_returns_201() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "text/yaml")
        .body(VALID_LOGS_YAML)
        .send()
        .expect("POST /scenarios must succeed for logs");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid logs YAML must return 201 Created"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["name"], "integration_logs");
    let s = body["state"].as_str().unwrap_or("");
    assert!(
        matches!(s, "pending" | "running"),
        "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
    );
}

// ---- Test: POST with signal_type: metrics -> 201 (ScenarioEntry format) -------

/// POST a YAML body with explicit signal_type: metrics returns 201.
#[test]
fn post_tagged_metrics_yaml_returns_201() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_TAGGED_YAML)
        .send()
        .expect("POST /scenarios must succeed for tagged YAML");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST tagged metrics YAML must return 201"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["name"], "tagged_integration");
}

// ---- Test: POST invalid YAML -> 400 with error message ------------------------

/// POST garbage text returns 400 Bad Request with a descriptive error message.
#[test]
fn post_invalid_yaml_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "text/yaml")
        .body("this is total garbage: [}{")
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "POST garbage YAML must return 400 Bad Request"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["error"], "bad_request");
    assert!(
        body["detail"].is_string() && !body["detail"].as_str().unwrap().is_empty(),
        "400 response must include a non-empty detail message"
    );
}

// ---- Test: POST valid YAML with rate=0 -> 422 with validation detail ----------

/// POST YAML with rate=0 returns 422 Unprocessable Entity.
#[test]
fn post_yaml_with_zero_rate_returns_422() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let zero_rate_yaml = "\
version: 2
defaults:
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: bad_rate
    signal_type: metrics
    name: bad_rate
    rate: 0
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(zero_rate_yaml)
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        422,
        "POST YAML with rate=0 must return 422 Unprocessable Entity"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["error"], "unprocessable_entity");
    assert!(
        body["detail"].is_string() && !body["detail"].as_str().unwrap().is_empty(),
        "422 response must include a non-empty detail message"
    );
}

// ---- Test: POST with application/json content type ----------------------------

/// POST a valid JSON body with application/json content type returns 201.
#[test]
fn post_valid_json_returns_201() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let json_body = serde_json::json!({
        "version": 2,
        "defaults": {
            "rate": 10,
            "duration": "500ms",
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        },
        "scenarios": [
            {
                "id": "json_integration",
                "signal_type": "metrics",
                "name": "json_integration",
                "generator": { "type": "constant", "value": 1.0 }
            }
        ]
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/json")
        .body(json_body.to_string())
        .send()
        .expect("POST JSON must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid JSON with application/json must return 201"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["name"], "json_integration");
    let s = body["state"].as_str().unwrap_or("");
    assert!(
        matches!(s, "pending" | "running"),
        "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
    );
}

// ---- Test: Response ID is a valid UUID ----------------------------------------

/// The scenario ID returned in the 201 response is a valid UUID v4.
#[test]
fn post_response_id_is_valid_uuid() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "text/yaml")
        .body(VALID_METRICS_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let id_str = body["id"].as_str().expect("id must be a string");
    let parsed = uuid::Uuid::parse_str(id_str);
    assert!(
        parsed.is_ok(),
        "returned id must be a valid UUID, got: {id_str}"
    );
}

// ---- Test: POST empty body -> 400 ---------------------------------------------

/// POST with an empty body returns 400.
#[test]
fn post_empty_body_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body("")
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "POST empty body must return 400"
    );
}

// ---- Test: POST multi-scenario YAML -> 201 with scenarios array ---------------

/// Valid v2 multi-scenario YAML with two metrics entries.
const VALID_MULTI_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: multi_integ_a
    signal_type: metrics
    name: multi_integ_a
    generator:
      type: constant
      value: 1.0
  - id: multi_integ_b
    signal_type: metrics
    name: multi_integ_b
    generator:
      type: constant
      value: 2.0
";

/// POST a valid multi-scenario YAML returns 201 with a scenarios array.
#[test]
fn post_multi_scenario_yaml_returns_201_with_scenarios_array() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_MULTI_YAML)
        .send()
        .expect("POST /scenarios must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid multi-scenario YAML must return 201 Created"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let scenarios = body["scenarios"]
        .as_array()
        .expect("response must contain 'scenarios' array");
    assert_eq!(
        scenarios.len(),
        2,
        "multi-scenario response must contain 2 entries"
    );

    // Verify each entry has the expected fields.
    for entry in scenarios {
        assert!(entry["id"].is_string());
        assert!(entry["name"].is_string());
        let s = entry["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
        );
    }

    // Verify names match input order.
    assert_eq!(scenarios[0]["name"], "multi_integ_a");
    assert_eq!(scenarios[1]["name"], "multi_integ_b");
}

// ---- Test: POST multi-scenario JSON -> 201 ------------------------------------

/// POST a valid multi-scenario JSON returns 201 with a scenarios array.
#[test]
fn post_multi_scenario_json_returns_201() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let json_body = serde_json::json!({
        "version": 2,
        "defaults": {
            "rate": 10,
            "duration": "500ms",
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        },
        "scenarios": [
            {
                "id": "json_multi_integ_a",
                "signal_type": "metrics",
                "name": "json_multi_integ_a",
                "generator": { "type": "constant", "value": 1.0 }
            },
            {
                "id": "json_multi_integ_b",
                "signal_type": "metrics",
                "name": "json_multi_integ_b",
                "generator": { "type": "constant", "value": 2.0 }
            }
        ]
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/json")
        .body(json_body.to_string())
        .send()
        .expect("POST must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid multi-scenario JSON must return 201"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let scenarios = body["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 2);
    let names: Vec<&str> = scenarios
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"json_multi_integ_a"));
    assert!(names.contains(&"json_multi_integ_b"));
}

// ---- Test: POST empty scenarios array -> 400 ----------------------------------

/// POST with an empty v2 scenarios array returns 400. The v2 parser rejects
/// zero-entry scenarios up front, so this surfaces as a compile-phase error.
#[test]
fn post_multi_scenario_empty_array_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body("version: 2\nscenarios: []\n")
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "POST with empty scenarios array must return 400"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["error"], "bad_request");
}

// ---- Test: POST multi-scenario with invalid entry -> 422 ----------------------

/// POST a multi-scenario batch with one invalid entry returns 422.
#[test]
fn post_multi_scenario_invalid_entry_returns_422() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: valid_multi
    signal_type: metrics
    name: valid_multi
    rate: 10
    generator:
      type: constant
      value: 1.0
  - id: invalid_multi
    signal_type: metrics
    name: invalid_multi
    rate: 0
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        422,
        "POST multi-scenario with invalid entry must return 422"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["error"], "unprocessable_entity");
}

// ---- Test: Multi-scenario entries visible in GET /scenarios --------------------

/// POST multi-scenario, then GET /scenarios lists all of them.
#[test]
fn post_multi_scenario_all_visible_in_get_list() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_MULTI_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);

    let post_body: serde_json::Value = resp.json().unwrap();
    let posted_ids: Vec<&str> = post_body["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();

    // GET /scenarios
    let list_resp = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .send()
        .expect("GET must succeed");

    assert_eq!(list_resp.status().as_u16(), 200);

    let list_body: serde_json::Value = list_resp.json().unwrap();
    let listed_ids: Vec<&str> = list_body["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();

    for id in &posted_ids {
        assert!(
            listed_ids.contains(id),
            "posted scenario id={id} must appear in GET /scenarios list"
        );
    }
}

// ---- Test: Multi-scenario entries stoppable via DELETE -------------------------

/// POST multi-scenario, then DELETE each one succeeds.
#[test]
fn post_multi_scenario_stoppable_via_delete() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_MULTI_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);

    let post_body: serde_json::Value = resp.json().unwrap();
    let ids: Vec<String> = post_body["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();

    // DELETE each.
    for id in &ids {
        let del_resp = client
            .delete(format!("http://127.0.0.1:{port}/scenarios/{id}"))
            .send()
            .expect("DELETE must succeed");

        assert_eq!(
            del_resp.status().as_u16(),
            200,
            "DELETE for multi-scenario id={id} must return 200"
        );
    }
}

// ---- Test: Single-scenario POST backward compat after multi-scenario change ---

/// Single-scenario POST still returns the flat {id, name, status} response.
#[test]
fn post_single_scenario_backward_compat() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_METRICS_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);

    let body: serde_json::Value = resp.json().unwrap();

    // Must NOT have a "scenarios" key (backward compat).
    assert!(
        body.get("scenarios").is_none(),
        "single-scenario POST must not return 'scenarios' wrapper"
    );
    assert!(body["id"].is_string());
    assert_eq!(body["name"], "integration_metric");
    let s = body["state"].as_str().unwrap_or("");
    assert!(
        matches!(s, "pending" | "running"),
        "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
    );
}

// ---- Test: v2 end-to-end acceptance ------------------------------------------

/// POST a v2 YAML body, observe the scenario in GET /scenarios, and DELETE it.
///
/// Guards the v2 body-acceptance path end-to-end: compile → launch → list →
/// stop. Complements `full_lifecycle_metrics_and_logs` in `integration.rs`
/// with an explicit v2-only shape so a regression that breaks v2 compilation
/// surfaces here even if the older test happens to still pass.
#[test]
fn post_v2_yaml_end_to_end_runs_scenario() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_METRICS_YAML)
        .send()
        .expect("POST v2 YAML must succeed");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST valid v2 metrics YAML must return 201 Created"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let id = body["id"]
        .as_str()
        .expect("response must carry a scenario id")
        .to_string();
    assert_eq!(body["name"], "integration_metric");
    let s = body["state"].as_str().unwrap_or("");
    assert!(
        matches!(s, "pending" | "running"),
        "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
    );

    // The scenario appears in the GET /scenarios listing.
    let list = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .send()
        .expect("GET /scenarios must succeed");
    assert_eq!(list.status().as_u16(), 200);
    let list_body: serde_json::Value = list.json().expect("list response must be JSON");
    let listed_ids: Vec<&str> = list_body["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(
        listed_ids.contains(&id.as_str()),
        "posted v2 scenario must appear in GET /scenarios"
    );

    // Stop the scenario cleanly to release its thread before the guard drops.
    let del = client
        .delete(format!("http://127.0.0.1:{port}/scenarios/{id}"))
        .send()
        .expect("DELETE must succeed");
    assert_eq!(del.status().as_u16(), 200);
}

// ---- Test: v1 body rejection with migration hint -----------------------------

/// POST a v1 flat YAML body — must be rejected with a 400 carrying the v2
/// migration hint. Guards against silent v1 acceptance drifting back into
/// the server.
#[test]
fn post_v1_yaml_body_returns_400_with_migration_hint() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let v1_body = "\
name: legacy_integration
rate: 10
duration: 200ms
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(v1_body)
        .send()
        .expect("POST v1 YAML must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "POST v1 YAML body must return 400 Bad Request"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(body["error"], "bad_request");
    let detail = body["detail"].as_str().expect("detail must be a string");
    assert!(
        detail.contains("v2"),
        "detail must mention v2 requirement, got: {detail}"
    );
    assert!(
        detail.contains("v2-scenarios.md") || detail.contains("Migrate"),
        "detail must point at the migration guide, got: {detail}"
    );
}

// ---- FU-2: loopback sink pre-flight warnings ---------------------------------

/// POST a scenario whose sink points at `localhost` — the server still
/// returns 201 (warnings are informational, not errors) but the response
/// body carries a `warnings` array explaining the container-loopback trap.
#[test]
fn post_tcp_localhost_sink_returns_warning() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: localhost:9000
scenarios:
  - id: loopback_tcp
    signal_type: metrics
    name: loopback_tcp
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        201,
        "loopback sink must still return 201 — warning, not rejection"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let warnings = body["warnings"]
        .as_array()
        .expect("response must carry a warnings array");
    assert_eq!(
        warnings.len(),
        1,
        "single loopback sink must produce one warning"
    );
    let msg = warnings[0].as_str().unwrap();
    assert!(
        msg.contains("loopback_tcp"),
        "warning must name the entry: {msg}"
    );
    assert!(
        msg.contains("tcp"),
        "warning must mention the sink type: {msg}"
    );
    assert!(
        msg.contains("localhost:9000"),
        "warning must echo the offending address: {msg}"
    );
    assert!(
        msg.contains("deployment/endpoints"),
        "warning must point at the docs: {msg}"
    );
}

/// POST a scenario whose sink points at a real service name — no warnings
/// in the response.
#[test]
fn post_tcp_real_hostname_sink_has_no_warnings() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: syslog.example.com:514
scenarios:
  - id: real_host_tcp
    signal_type: metrics
    name: real_host_tcp
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    // Empty warnings vec is skipped via skip_serializing_if — the key must be absent.
    assert!(
        body.get("warnings").is_none(),
        "clean sink must not emit a warnings field"
    );
}

/// POST a `stdout` sink — no warnings (sink carries no address).
#[test]
fn post_stdout_sink_has_no_warnings() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_METRICS_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert!(
        body.get("warnings").is_none(),
        "stdout sink must not emit a warnings field"
    );
}

/// POST a UDP sink pointed at `localhost:9000` — produces a warning.
#[test]
fn post_udp_localhost_sink_returns_warning() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: udp
    address: localhost:9000
scenarios:
  - id: loopback_udp
    signal_type: metrics
    name: loopback_udp
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let warnings = body["warnings"]
        .as_array()
        .expect("warnings array required");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("udp"));
}

/// POST a multi-scenario batch with one localhost sink and one service-name
/// sink — batch response carries exactly one warning at the top level.
#[test]
fn post_multi_scenario_mixed_sinks_returns_one_warning() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
scenarios:
  - id: multi_loopback
    signal_type: metrics
    name: multi_loopback
    sink:
      type: tcp
      address: localhost:9000
    generator:
      type: constant
      value: 1.0
  - id: multi_clean
    signal_type: metrics
    name: multi_clean
    sink:
      type: tcp
      address: collector.internal:9000
    generator:
      type: constant
      value: 2.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    // Multi-scenario shape: top-level `warnings` next to `scenarios`.
    let warnings = body["warnings"]
        .as_array()
        .expect("multi-scenario warnings must live at the top level");
    assert_eq!(
        warnings.len(),
        1,
        "mixed batch must surface exactly one warning"
    );
    assert!(warnings[0].as_str().unwrap().contains("multi_loopback"));
}

/// POST a multi-scenario batch with two clean sinks — response has no
/// `warnings` field at all.
#[test]
fn post_multi_scenario_both_clean_has_no_warnings() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(VALID_MULTI_YAML)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert!(
        body.get("warnings").is_none(),
        "all-clean batch must not emit a warnings field"
    );
}

/// POST a scenario pointing at `127.0.0.1` — produces a warning.
#[test]
fn post_tcp_127_0_0_1_sink_returns_warning() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: 127.0.0.1:9000
scenarios:
  - id: loopback_v4
    signal_type: metrics
    name: loopback_v4
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let warnings = body["warnings"]
        .as_array()
        .expect("warnings array required");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("127.0.0.1"));
}

/// POST a scenario pointing at `[::1]` (IPv6 loopback) — produces a warning.
#[test]
fn post_tcp_ipv6_loopback_sink_returns_warning() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let yaml = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: \"[::1]:9000\"
scenarios:
  - id: loopback_v6
    signal_type: metrics
    name: loopback_v6
    generator:
      type: constant
      value: 1.0
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(yaml)
        .send()
        .expect("POST must succeed");

    assert_eq!(resp.status().as_u16(), 201);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let warnings = body["warnings"]
        .as_array()
        .expect("warnings array required");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("::1"));
}

/// POST a v1 multi-scenario YAML (top-level `scenarios:` without
/// `version: 2`) — also rejected with 400 + migration hint. Companion to
/// the flat-v1 case above.
#[test]
fn post_v1_multi_scenario_body_returns_400_with_migration_hint() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let v1_multi = "\
scenarios:
  - signal_type: metrics
    name: legacy_a
    rate: 10
    duration: 200ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body(v1_multi)
        .send()
        .expect("POST v1 multi YAML must succeed at HTTP level");

    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    let detail = body["detail"].as_str().expect("detail must be a string");
    assert!(
        detail.contains("v2"),
        "detail must mention v2 requirement, got: {detail}"
    );
}

// ---- Gated launch through POST /scenarios -----------------------------------

mod gated_scenarios {
    use super::common;
    use std::time::{Duration, Instant};

    /// 2-entry cascade: a flap upstream + downstream gated by `while:`.
    /// `delay: { open: 0s, close: 0s }` strips the debounce so state edges
    /// land on `/stats` deterministically. Duration 2s leaves a comfortable
    /// margin for the polling loop.
    const FLAP_CASCADE_YAML: &str = "\
version: 2
defaults:
  rate: 50
  duration: 2s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: primary_flap
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 200ms
  - id: gated_downstream
    signal_type: metrics
    name: gated_downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: primary_flap
      op: \"<\"
      value: 1
    delay:
      open: 0s
      close: 0s
";

    fn poll_state(client: &reqwest::blocking::Client, port: u16, id: &str) -> Option<String> {
        let resp = client
            .get(format!("http://127.0.0.1:{port}/scenarios/{id}/stats"))
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().ok()?;
        body["state"].as_str().map(|s| s.to_string())
    }

    #[test]
    fn post_gated_cascade_observes_pending_running_paused_states() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(FLAP_CASCADE_YAML)
            .send()
            .expect("POST cascade must succeed");
        assert_eq!(resp.status().as_u16(), 201, "POST must return 201");

        let body: serde_json::Value = resp.json().expect("response must be JSON");
        let scenarios = body["scenarios"]
            .as_array()
            .expect("response must contain scenarios array");
        assert_eq!(scenarios.len(), 2);
        let downstream_id = scenarios
            .iter()
            .find(|s| s["name"] == "gated_downstream")
            .expect("downstream entry present in response")["id"]
            .as_str()
            .expect("downstream id is a string")
            .to_string();

        let mut observed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline {
            if let Some(s) = poll_state(&client, port, &downstream_id) {
                observed.insert(s);
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            observed.contains("running"),
            "downstream must reach 'running' during the upstream's down-phase, observed: {observed:?}"
        );
        assert!(
            observed.contains("paused"),
            "downstream must reach 'paused' during the upstream's up-phase, observed: {observed:?}"
        );
    }

    /// Upstream flap with a long up_duration keeps the gate closed for the
    /// duration of the test, so the downstream's POST response carries
    /// `state: "pending"` for its initial snapshot. The upstream's response
    /// reports `pending` or `running` depending on whether the runner has
    /// posted its first tick by the time the snapshot is taken.
    const PENDING_DOWNSTREAM_YAML: &str = "\
version: 2
defaults:
  rate: 50
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream_high
    signal_type: metrics
    name: upstream_high
    generator:
      type: flap
      up_duration: 60s
      down_duration: 1s
      up_value: 1.0
      down_value: 0.0
  - id: downstream_gated
    signal_type: metrics
    name: downstream_gated
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_high
      op: \"<\"
      value: 1
";

    #[test]
    fn post_gated_downstream_response_reports_pending_state() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(PENDING_DOWNSTREAM_YAML)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201);

        let body: serde_json::Value = resp.json().expect("body is JSON");
        let scenarios = body["scenarios"]
            .as_array()
            .expect("multi response carries scenarios array");
        let downstream = scenarios
            .iter()
            .find(|s| s["name"] == "downstream_gated")
            .expect("downstream present");
        let state = downstream["state"].as_str().unwrap_or("");
        assert!(
            matches!(state, "pending" | "paused"),
            "downstream must report 'pending' or 'paused' at POST-response time when its upstream \
             gate has never opened (must NOT be 'running'), got {state:?}"
        );

        let upstream = scenarios
            .iter()
            .find(|s| s["name"] == "upstream_high")
            .expect("upstream present");
        let upstream_state = upstream["state"].as_str().unwrap_or("");
        assert!(
            matches!(upstream_state, "pending" | "running"),
            "upstream must report 'pending' or 'running' at POST time, got {upstream_state:?}"
        );
    }

    const TWO_ENTRY_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: dup_a
    signal_type: metrics
    name: dup_a
    generator:
      type: constant
      value: 1.0
  - id: dup_b
    signal_type: metrics
    name: dup_b
    generator:
      type: constant
      value: 2.0
";

    #[test]
    fn post_same_yaml_twice_returns_distinct_uuids() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let post_once = || -> Vec<String> {
            let resp = client
                .post(format!("http://127.0.0.1:{port}/scenarios"))
                .header("content-type", "application/x-yaml")
                .body(TWO_ENTRY_YAML)
                .send()
                .expect("POST must succeed");
            assert_eq!(resp.status().as_u16(), 201);
            let body: serde_json::Value = resp.json().expect("body is JSON");
            body["scenarios"]
                .as_array()
                .expect("scenarios array")
                .iter()
                .map(|s| s["id"].as_str().expect("id is string").to_string())
                .collect()
        };

        let first = post_once();
        let second = post_once();
        let mut all = Vec::new();
        all.extend(first);
        all.extend(second);
        assert_eq!(all.len(), 4);
        let unique: std::collections::BTreeSet<&String> = all.iter().collect();
        assert_eq!(unique.len(), 4, "all 4 ids must be distinct, got {all:?}");
        for id in &all {
            assert!(
                uuid::Uuid::parse_str(id).is_ok(),
                "id must be a valid UUID, got {id}"
            );
        }

        let resp = client
            .get(format!("http://127.0.0.1:{port}/scenarios"))
            .send()
            .expect("GET /scenarios must succeed");
        let body: serde_json::Value = resp.json().expect("body is JSON");
        let listed: std::collections::BTreeSet<&str> = body["scenarios"]
            .as_array()
            .expect("scenarios array")
            .iter()
            .filter_map(|s| s["id"].as_str())
            .collect();
        for id in &all {
            assert!(
                listed.contains(id.as_str()),
                "posted id {id} must appear in GET /scenarios"
            );
        }
    }

    /// Cover all four signal types using alias generators where applicable —
    /// proves `launch_multi_compiled`'s desugar+expand+validate pipeline is
    /// semantically equivalent to the previous non-gated `prepare_entries`
    /// path.
    const ALL_SIGNAL_TYPES_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 500ms
scenarios:
  - id: alias_metric
    signal_type: metrics
    name: alias_metric
    generator:
      type: flap
      up_duration: 100ms
      down_duration: 100ms
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - id: plain_logs
    signal_type: logs
    name: plain_logs
    log_generator:
      type: template
      templates:
        - message: \"alias log line\"
          field_pools: {}
      seed: 0
    encoder:
      type: json_lines
    sink:
      type: stdout
  - id: hist_metric
    signal_type: histogram
    name: hist_metric
    distribution:
      type: exponential
      rate: 10.0
    observations_per_tick: 16
    seed: 1
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - id: summary_metric
    signal_type: summary
    name: summary_metric
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    observations_per_tick: 16
    seed: 2
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";

    #[test]
    fn post_all_signal_types_with_alias_generators_returns_201() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(ALL_SIGNAL_TYPES_YAML)
            .send()
            .expect("POST mixed signal types must succeed");

        assert_eq!(
            resp.status().as_u16(),
            201,
            "all four signal types with alias generators must return 201"
        );

        let body: serde_json::Value = resp.json().expect("response is JSON");
        let scenarios = body["scenarios"]
            .as_array()
            .expect("multi response carries scenarios array");
        assert_eq!(scenarios.len(), 4);
        let names: std::collections::BTreeSet<&str> = scenarios
            .iter()
            .filter_map(|s| s["name"].as_str())
            .collect();
        assert!(names.contains("alias_metric"));
        assert!(names.contains("plain_logs"));
        assert!(names.contains("hist_metric"));
        assert!(names.contains("summary_metric"));
    }

    /// 2-entry cyclic `while:` body — compile_after rejects with cycle error,
    /// the handler maps that to 400 Bad Request.
    const CYCLIC_WHILE_YAML: &str = "\
version: 2
defaults:
  rate: 10
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: a
    signal_type: metrics
    name: a
    generator:
      type: flap
      up_duration: 100ms
      down_duration: 100ms
    while:
      ref: b
      op: \"<\"
      value: 1
  - id: b
    signal_type: metrics
    name: b
    generator:
      type: flap
      up_duration: 100ms
      down_duration: 100ms
    while:
      ref: a
      op: \"<\"
      value: 1
";

    #[test]
    fn post_cyclic_while_returns_400() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(CYCLIC_WHILE_YAML)
            .send()
            .expect("POST cyclic while must reach the server");
        assert_eq!(
            resp.status().as_u16(),
            400,
            "cyclic while: must surface as 400 Bad Request"
        );
    }

    #[test]
    fn paused_scenario_does_not_mutate_consecutive_failures() {
        let (port, _guard) = common::start_server();
        let client = common::http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/scenarios"))
            .header("content-type", "application/x-yaml")
            .body(FLAP_CASCADE_YAML)
            .send()
            .expect("POST cascade must succeed");
        assert_eq!(resp.status().as_u16(), 201);

        let body: serde_json::Value = resp.json().expect("body is JSON");
        let downstream_id = body["scenarios"]
            .as_array()
            .expect("scenarios array")
            .iter()
            .find(|s| s["name"] == "gated_downstream")
            .expect("downstream entry")["id"]
            .as_str()
            .expect("downstream id")
            .to_string();

        // Wait for the downstream to enter `paused` (during upstream's up-phase).
        let mut found_paused_failures: Option<u64> = None;
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            let stats = client
                .get(format!(
                    "http://127.0.0.1:{port}/scenarios/{downstream_id}/stats"
                ))
                .send()
                .expect("GET stats must succeed");
            let body: serde_json::Value = stats.json().expect("stats body is JSON");
            if body["state"].as_str() == Some("paused") {
                found_paused_failures = body["consecutive_failures"].as_u64();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let baseline = found_paused_failures
            .expect("downstream must reach paused state within 800ms for the assertion to fire");

        // Hold paused for 500ms (longer than the 200ms up-phase, so the
        // sample window may straddle a transition). The stronger guarantee
        // is: across consecutive paused samples, the failure counter does
        // not advance — the runner is not running ticks.
        std::thread::sleep(Duration::from_millis(500));

        let stats = client
            .get(format!(
                "http://127.0.0.1:{port}/scenarios/{downstream_id}/stats"
            ))
            .send()
            .expect("GET stats must succeed");
        let body: serde_json::Value = stats.json().expect("stats body is JSON");
        let after = body["consecutive_failures"].as_u64().expect("u64 field");
        // For a stdout sink, baseline is 0 and after must also be 0.
        assert_eq!(
            after, baseline,
            "consecutive_failures must not advance while paused (baseline={baseline}, after={after})"
        );
    }
}
