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
    assert_eq!(body["status"], "running");
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
    assert_eq!(body["status"], "running");
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
    assert_eq!(body["status"], "running");
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
        assert_eq!(entry["status"], "running");
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
    assert_eq!(body["status"], "running");
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
    assert_eq!(body["status"], "running");

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
