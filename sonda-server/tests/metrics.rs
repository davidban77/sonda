//! End-to-end integration tests for `GET /metrics` (aggregate scrape).

mod common;

use std::thread;
use std::time::Duration;

const SRL1_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: srl1_up
    signal_type: metrics
    name: srl1_up
    labels:
      device: srl1
    generator:
      type: constant
      value: 1.0
"#;

const SRL2_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: srl2_up
    signal_type: metrics
    name: srl2_up
    labels:
      device: srl2
    generator:
      type: constant
      value: 2.0
"#;

#[test]
fn aggregate_metrics_end_to_end_with_label_filter() {
    let (port, _guard) = common::start_server();
    let base = format!("http://127.0.0.1:{port}");
    let client = common::http_client();

    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(SRL1_YAML)
        .send()
        .expect("POST srl1 must succeed");
    assert_eq!(resp.status().as_u16(), 201, "POST srl1 must return 201");

    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(SRL2_YAML)
        .send()
        .expect("POST srl2 must succeed");
    assert_eq!(resp.status().as_u16(), 201, "POST srl2 must return 201");

    thread::sleep(Duration::from_millis(500));

    let resp = client
        .get(format!("{base}/metrics"))
        .send()
        .expect("GET /metrics must succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .expect("Content-Type must be present")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(ct, "text/plain; version=0.0.4; charset=utf-8");
    let body_all = resp.text().expect("body must be UTF-8");
    assert!(
        body_all.contains("srl1_up") && body_all.contains("srl2_up"),
        "aggregate body must include both scenarios, got: {body_all}"
    );

    let resp = client
        .get(format!("{base}/metrics?label=device:srl1"))
        .send()
        .expect("GET /metrics?label=device:srl1 must succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let body_filtered = resp.text().expect("body must be UTF-8");
    assert!(
        body_filtered.contains("srl1_up"),
        "filtered body must include srl1, got: {body_filtered}"
    );
    assert!(
        !body_filtered.contains("srl2_up"),
        "filtered body must exclude srl2, got: {body_filtered}"
    );
}

const METRICS_TYPE_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#;

const HISTOGRAM_TYPE_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 5
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: req_latency
    signal_type: histogram
    name: req_latency
    distribution:
      type: exponential
      rate: 10.0
    observations_per_tick: 50
    seed: 1
"#;

const SUMMARY_TYPE_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 5
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: rpc_duration
    signal_type: summary
    name: rpc_duration
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    observations_per_tick: 50
    seed: 1
"#;

#[test]
fn aggregate_metrics_e2e_includes_type_lines_per_signal_type() {
    let (port, _guard) = common::start_server();
    let base = format!("http://127.0.0.1:{port}");
    let client = common::http_client();

    for yaml in [METRICS_TYPE_YAML, HISTOGRAM_TYPE_YAML, SUMMARY_TYPE_YAML] {
        let resp = client
            .post(format!("{base}/scenarios"))
            .header("Content-Type", "text/yaml")
            .body(yaml)
            .send()
            .expect("POST must succeed");
        assert_eq!(resp.status().as_u16(), 201, "POST must return 201");
    }

    thread::sleep(Duration::from_millis(800));

    let resp = client
        .get(format!("{base}/metrics"))
        .send()
        .expect("GET /metrics must succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().expect("body must be UTF-8");

    assert!(
        body.contains("# TYPE cpu_usage gauge"),
        "body must contain '# TYPE cpu_usage gauge', got:\n{body}"
    );
    assert!(
        body.contains("# TYPE req_latency histogram"),
        "body must contain '# TYPE req_latency histogram', got:\n{body}"
    );
    assert!(
        body.contains("# TYPE rpc_duration summary"),
        "body must contain '# TYPE rpc_duration summary', got:\n{body}"
    );

    assert!(
        body.contains("req_latency_bucket{") && body.contains("le=\"+Inf\""),
        "body must contain histogram bucket samples including +Inf, got:\n{body}"
    );
    assert!(
        body.contains("req_latency_sum"),
        "body must contain histogram _sum samples, got:\n{body}"
    );
    assert!(
        body.contains("req_latency_count"),
        "body must contain histogram _count samples, got:\n{body}"
    );
    assert!(
        body.contains("rpc_duration{") && body.contains("quantile=\""),
        "body must contain summary quantile samples, got:\n{body}"
    );
    assert!(
        body.contains("rpc_duration_sum"),
        "body must contain summary _sum samples, got:\n{body}"
    );
    assert!(
        body.contains("rpc_duration_count"),
        "body must contain summary _count samples, got:\n{body}"
    );

    let type_lines: Vec<&str> = body.lines().filter(|l| l.starts_with("# TYPE ")).collect();
    let mut seen = std::collections::HashSet::new();
    for line in &type_lines {
        assert!(
            seen.insert(*line),
            "duplicate TYPE line found: {line}\nfull body:\n{body}"
        );
    }
}

const YAML_METRIC_TYPE_FROM_YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: memory_utilization
    signal_type: metrics
    name: memory_utilization
    metric_type: counter
    help: "Memory usage percent on the device."
    labels:
      device: srl1
    generator:
      type: constant
      value: 42
"#;

#[test]
fn e2e_yaml_metric_type_field_reaches_scrape_output() {
    let (port, _guard) = common::start_server();
    let base = format!("http://127.0.0.1:{port}");
    let client = common::http_client();

    let resp = client
        .post(format!("{base}/scenarios"))
        .header("Content-Type", "text/yaml")
        .body(YAML_METRIC_TYPE_FROM_YAML)
        .send()
        .expect("POST must succeed");
    assert_eq!(resp.status().as_u16(), 201, "POST must return 201");

    thread::sleep(Duration::from_millis(500));

    let resp = client
        .get(format!("{base}/metrics"))
        .send()
        .expect("GET /metrics must succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().expect("body must be UTF-8");

    assert!(
        body.contains("# TYPE memory_utilization counter"),
        "YAML metric_type:counter must reach scrape output, got:\n{body}"
    );
    assert!(
        !body.contains("# TYPE memory_utilization gauge"),
        "scrape must not fall back to gauge default, got:\n{body}"
    );
    assert!(
        body.contains("# HELP memory_utilization Memory usage percent on the device."),
        "YAML help: field must reach scrape output, got:\n{body}"
    );
}
