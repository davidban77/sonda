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
