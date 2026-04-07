//! Integration tests for sonda-server Slice 3.2 -- POST /scenarios.
//!
//! These tests verify the POST /scenarios endpoint by starting an actual
//! sonda-server binary and making real HTTP requests against it.

mod common;

/// Valid metrics YAML (short duration for quick tests).
const VALID_METRICS_YAML: &str = "\
name: integration_metric
rate: 10
duration: 500ms
generator:
  type: constant
  value: 42.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

/// Valid logs YAML (short duration for quick tests).
const VALID_LOGS_YAML: &str = "\
name: integration_logs
rate: 10
duration: 500ms
generator:
  type: template
  templates:
    - message: \"integration test log\"
      field_pools: {}
  seed: 0
encoder:
  type: json_lines
sink:
  type: stdout
";

/// Valid metrics YAML with explicit signal_type tag.
const VALID_TAGGED_YAML: &str = "\
signal_type: metrics
name: tagged_integration
rate: 10
duration: 500ms
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
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
name: bad_rate
rate: 0
duration: 1s
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
        "signal_type": "metrics",
        "name": "json_integration",
        "rate": 10,
        "duration": "500ms",
        "generator": { "type": "constant", "value": 1.0 },
        "encoder": { "type": "prometheus_text" },
        "sink": { "type": "stdout" }
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

/// Valid multi-scenario YAML with two metrics entries.
const VALID_MULTI_YAML: &str = "\
scenarios:
  - signal_type: metrics
    name: multi_integ_a
    rate: 10
    duration: 500ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: multi_integ_b
    rate: 10
    duration: 500ms
    generator:
      type: constant
      value: 2.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
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
        "scenarios": [
            {
                "signal_type": "metrics",
                "name": "json_multi_integ",
                "rate": 10,
                "duration": "500ms",
                "generator": { "type": "constant", "value": 1.0 },
                "encoder": { "type": "prometheus_text" },
                "sink": { "type": "stdout" }
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
    assert_eq!(scenarios.len(), 1);
    assert_eq!(scenarios[0]["name"], "json_multi_integ");
}

// ---- Test: POST empty scenarios array -> 400 ----------------------------------

/// POST with empty scenarios array returns 400.
#[test]
fn post_multi_scenario_empty_array_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/scenarios"))
        .header("content-type", "application/x-yaml")
        .body("scenarios: []\n")
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
scenarios:
  - signal_type: metrics
    name: valid_multi
    rate: 10
    duration: 500ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: invalid_multi
    rate: 0
    duration: 500ms
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
