#![cfg(feature = "config")]
//! End-to-end env-var interpolation through `compile_scenario_file`.

mod common;

use sonda_core::compile_scenario_file;
use sonda_core::compiler::expand::InMemoryPackResolver;
#[cfg(feature = "http")]
use sonda_core::sink::SinkConfig;
use sonda_core::{CompileError, ScenarioEntry};
use std::env;

const TEST_VAR: &str = "SONDA_E2E_INTERPOLATION_URL_2026";

fn unique_var(suffix: &str) -> String {
    format!("{TEST_VAR}_{suffix}")
}

fn yaml_with_var(var_name: &str) -> String {
    format!(
        r#"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
    labels:
      url_label: "${{{var_name}:-http://localhost:8428/api/v1/import/prometheus}}"
"#
    )
}

#[test]
fn unset_var_resolves_to_default_in_compiled_scenario() {
    let var = unique_var("DEFAULT_PATH");
    // SAFETY: unique-to-this-test var name; mutates process-global state.
    unsafe {
        env::remove_var(&var);
    }
    let yaml = yaml_with_var(&var);
    let resolver = InMemoryPackResolver::new();
    let entries =
        compile_scenario_file(&yaml, &resolver).expect("compile must succeed when default is used");
    let url_label = label_value(&entries, "url_label");
    assert_eq!(
        url_label, "http://localhost:8428/api/v1/import/prometheus",
        "unset var should fall back to the literal default"
    );
}

#[test]
fn set_var_overrides_default_in_compiled_scenario() {
    let var = unique_var("OVERRIDE_PATH");
    let override_value = "http://victoriametrics.svc.cluster.local:8428/api/v1/import/prometheus";
    // SAFETY: unique-to-this-test var name; mutates process-global state.
    unsafe {
        env::set_var(&var, override_value);
    }
    let yaml = yaml_with_var(&var);
    let resolver = InMemoryPackResolver::new();
    let result = compile_scenario_file(&yaml, &resolver);
    // SAFETY: see comment above.
    unsafe {
        env::remove_var(&var);
    }
    let entries = result.expect("compile must succeed when var is set");
    let url_label = label_value(&entries, "url_label");
    assert_eq!(
        url_label, override_value,
        "set var should override the default"
    );
}

#[test]
fn required_var_unset_surfaces_as_compile_error() {
    let var = unique_var("REQUIRED_UNSET");
    // SAFETY: see comment above.
    unsafe {
        env::remove_var(&var);
    }
    // Use a required reference (no default) for this test.
    let yaml = format!(
        r#"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
    labels:
      url_label: "${{{var}}}"
"#
    );
    let resolver = InMemoryPackResolver::new();
    let err = compile_scenario_file(&yaml, &resolver)
        .expect_err("required-but-unset var must fail to compile");
    let inner = match &err {
        CompileError::EnvInterpolate(inner) => inner,
        other => panic!("expected CompileError::EnvInterpolate, got {other:?}"),
    };
    let msg = inner.to_string();
    assert!(
        msg.contains(&var) && msg.contains("not set"),
        "error message must name the missing variable, got: {msg}"
    );
}

#[cfg(feature = "http")]
#[test]
fn env_var_substitution_reaches_http_push_sink_url() {
    let var = unique_var("HTTP_PUSH_URL");
    let override_value = "http://victoriametrics:8428/api/v1/import/prometheus";
    // SAFETY: see comment above.
    unsafe {
        env::set_var(&var, override_value);
    }
    let yaml = format!(
        r#"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "${{{var}:-http://localhost:8428/api/v1/import/prometheus}}"

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#
    );
    let resolver = InMemoryPackResolver::new();
    let result = compile_scenario_file(&yaml, &resolver);
    // SAFETY: see comment above.
    unsafe {
        env::remove_var(&var);
    }
    let entries = result.expect("compile must succeed");
    let entry = entries.first().expect("at least one entry");
    match &entry.base().sink {
        SinkConfig::HttpPush { url, .. } => {
            assert_eq!(
                url, override_value,
                "sink URL must be the env-overridden value"
            );
        }
        other => panic!("expected HttpPush sink, got {other:?}"),
    }
}

fn label_value(entries: &[ScenarioEntry], key: &str) -> String {
    let entry = entries.first().expect("at least one compiled entry");
    let labels = entry
        .base()
        .labels
        .as_ref()
        .expect("entry must carry labels");
    labels
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("label {key:?} not found on compiled entry"))
}
