//! Local-POST `while:` downstream finishes when its upstream exits.
//!
//! `while: { ref: <id> }` without `scenario_name:` resolves to a sibling
//! in the same `CompiledFile`. When that sibling's runner exits, the
//! downstream transitions to `Finished` (not `Unresolved`).

#![cfg(feature = "config")]

mod common;

use std::thread;
use std::time::{Duration, Instant};

use sonda_core::compile_scenario_file_compiled;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::schedule::handle::ScenarioHandle;
use sonda_core::schedule::multi_runner::launch_multi_compiled;
use sonda_core::schedule::stats::ScenarioState;

fn find_by_id<'a>(handles: &'a [ScenarioHandle], id: &str) -> &'a ScenarioHandle {
    handles
        .iter()
        .find(|h| h.id == id)
        .unwrap_or_else(|| panic!("no scenario handle for id {id}"))
}

fn wait_for_state(handle: &ScenarioHandle, expected: ScenarioState, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if handle.stats_snapshot().state == expected {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "scenario {} did not reach state {:?} within {:?} (last state = {:?})",
        handle.id,
        expected,
        timeout,
        handle.stats_snapshot().state
    );
}

fn wait_for_not_alive(handle: &ScenarioHandle, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !handle.is_alive() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "scenario {} runner thread still alive after {:?}",
        handle.id, timeout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn local_downstream_finishes_when_upstream_duration_expires() {
    let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream_a
    signal_type: metrics
    name: upstream_a
    duration: 200ms
    generator:
      type: constant
      value: 100.0
  - id: downstream_b
    signal_type: metrics
    name: downstream_b
    duration: 5s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_a
      op: ">"
      value: 50.0
"#;

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");
    let mut handles =
        launch_multi_compiled(compiled, None, tokio_util::sync::CancellationToken::new())
            .expect("launch must succeed");
    assert_eq!(handles.len(), 2);

    {
        let upstream = find_by_id(&handles, "upstream_a");
        let downstream = find_by_id(&handles, "downstream_b");
        wait_for_state(upstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_state(downstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_not_alive(upstream, Duration::from_secs(1));
        wait_for_not_alive(downstream, Duration::from_secs(1));
    }

    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn local_cascade_a_b_c_all_finish_when_a_finishes() {
    let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: a
    signal_type: metrics
    name: a
    duration: 200ms
    generator:
      type: constant
      value: 100.0
  - id: b
    signal_type: metrics
    name: b
    duration: 5s
    generator:
      type: constant
      value: 10.0
    while:
      ref: a
      op: ">"
      value: 50.0
  - id: c
    signal_type: metrics
    name: c
    duration: 5s
    generator:
      type: constant
      value: 1.0
    while:
      ref: b
      op: ">"
      value: 0.0
"#;

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");
    let mut handles =
        launch_multi_compiled(compiled, None, tokio_util::sync::CancellationToken::new())
            .expect("launch must succeed");
    assert_eq!(handles.len(), 3);

    for id in ["a", "b", "c"] {
        let h = find_by_id(&handles, id);
        wait_for_state(h, ScenarioState::Finished, Duration::from_secs(3));
        wait_for_not_alive(h, Duration::from_secs(1));
    }

    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn local_downstream_paused_finishes_when_upstream_finishes() {
    // A non-repeating sequence drops below threshold on tick 1 and stays
    // there for the rest of the upstream's life. The downstream is
    // therefore reliably Paused (not Running) at the moment the upstream
    // exits, anchoring the Paused-arm `UpstreamGone → UpstreamFinished`
    // branch in core_loop.rs.
    let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    duration: 400ms
    generator:
      type: sequence
      values: [100.0, 0.0]
      repeat: false
  - id: downstream
    signal_type: metrics
    name: downstream
    duration: 5s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 50.0
"#;

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");
    let mut handles =
        launch_multi_compiled(compiled, None, tokio_util::sync::CancellationToken::new())
            .expect("launch must succeed");
    assert_eq!(handles.len(), 2);

    {
        let upstream = find_by_id(&handles, "upstream");
        let downstream = find_by_id(&handles, "downstream");
        wait_for_state(downstream, ScenarioState::Paused, Duration::from_secs(2));
        wait_for_state(upstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_state(downstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_not_alive(upstream, Duration::from_secs(1));
        wait_for_not_alive(downstream, Duration::from_secs(1));
    }

    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn local_downstream_running_finishes_when_upstream_finishes() {
    // Constant value > threshold keeps the downstream Running until the
    // upstream's duration expires.
    let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 100
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    duration: 200ms
    generator:
      type: constant
      value: 100.0
  - id: downstream
    signal_type: metrics
    name: downstream
    duration: 5s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 10.0
"#;

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");
    let mut handles =
        launch_multi_compiled(compiled, None, tokio_util::sync::CancellationToken::new())
            .expect("launch must succeed");
    assert_eq!(handles.len(), 2);

    {
        let upstream = find_by_id(&handles, "upstream");
        let downstream = find_by_id(&handles, "downstream");
        wait_for_state(upstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_state(downstream, ScenarioState::Finished, Duration::from_secs(2));
        wait_for_not_alive(upstream, Duration::from_secs(1));
        wait_for_not_alive(downstream, Duration::from_secs(1));
        // Must have produced events while Running.
        assert!(
            downstream.stats_snapshot().total_events > 0,
            "downstream should have emitted events while gate was open"
        );
    }

    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }
}
