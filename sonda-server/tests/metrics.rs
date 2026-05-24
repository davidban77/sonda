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
