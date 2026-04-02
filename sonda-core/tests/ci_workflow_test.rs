#![cfg(feature = "config")]
/// Tests that validate the GitHub Actions CI workflow file for slice 0.0.
///
/// The spec requires:
/// - The CI YAML file exists and is valid YAML.
/// - It triggers on push and pull_request events.
/// - It runs build, test, clippy, and fmt steps — in that order.
/// - Clippy uses `-D warnings`.
use serde_yaml_ng::Value;
use std::fs;
use std::path::PathBuf;

fn ci_yaml_path() -> PathBuf {
    // The test runs from the workspace root during `cargo test`.
    // CARGO_MANIFEST_DIR points to sonda-core; go two levels up to the workspace root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("sonda-core has a parent directory")
        .join(".github")
        .join("workflows")
        .join("ci.yml")
}

fn load_ci_yaml() -> Value {
    let path = ci_yaml_path();
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {:?}: {e}", path));
    serde_yaml_ng::from_str(&content).unwrap_or_else(|e| panic!("ci.yml is not valid YAML: {e}"))
}

#[test]
fn ci_yml_file_exists() {
    assert!(
        ci_yaml_path().exists(),
        "expected .github/workflows/ci.yml to exist"
    );
}

#[test]
fn ci_yml_is_valid_yaml() {
    // load_ci_yaml() panics if the file is missing or malformed.
    let _ = load_ci_yaml();
}

#[test]
fn ci_yml_triggers_on_push() {
    let yaml = load_ci_yaml();
    let on = &yaml["on"];
    assert!(
        !on["push"].is_null(),
        "ci.yml must trigger on push events; 'on.push' was not found"
    );
}

#[test]
fn ci_yml_triggers_on_pull_request() {
    let yaml = load_ci_yaml();
    let on = &yaml["on"];
    assert!(
        !on["pull_request"].is_null(),
        "ci.yml must trigger on pull_request events; 'on.pull_request' was not found"
    );
}

#[test]
fn ci_yml_has_build_step() {
    let yaml = load_ci_yaml();
    let steps = ci_steps(&yaml);
    assert!(
        steps.iter().any(|s| step_run(s).contains("cargo build")),
        "ci.yml must have a step that runs 'cargo build'"
    );
}

#[test]
fn ci_yml_has_test_step() {
    let yaml = load_ci_yaml();
    let steps = ci_steps(&yaml);
    assert!(
        steps.iter().any(|s| step_run(s).contains("cargo test")),
        "ci.yml must have a step that runs 'cargo test'"
    );
}

#[test]
fn ci_yml_has_clippy_step_with_deny_warnings() {
    let yaml = load_ci_yaml();
    let steps = ci_steps(&yaml);
    let clippy_step = steps
        .iter()
        .find(|s| step_run(s).contains("cargo clippy"))
        .expect("ci.yml must have a step that runs 'cargo clippy'");
    let run_cmd = step_run(clippy_step);
    assert!(
        run_cmd.contains("-D warnings"),
        "clippy step must use '-D warnings'; got: {run_cmd}"
    );
}

#[test]
fn ci_yml_has_fmt_check_step() {
    let yaml = load_ci_yaml();
    let steps = ci_steps(&yaml);
    assert!(
        steps
            .iter()
            .any(|s| step_run(s).contains("cargo fmt") && step_run(s).contains("--check")),
        "ci.yml must have a step that runs 'cargo fmt --all -- --check'"
    );
}

#[test]
fn ci_yml_steps_order_build_test_clippy_fmt() {
    // build must come before test, test before clippy, clippy before fmt.
    let yaml = load_ci_yaml();
    let steps = ci_steps(&yaml);

    let pos = |needle: &str| -> usize {
        steps
            .iter()
            .position(|s| step_run(s).contains(needle))
            .unwrap_or_else(|| panic!("could not find step containing '{needle}'"))
    };

    let build_pos = pos("cargo build");
    let test_pos = pos("cargo test");
    let clippy_pos = pos("cargo clippy");
    let fmt_pos = pos("cargo fmt");

    assert!(
        build_pos < test_pos,
        "build step (pos {build_pos}) must come before test step (pos {test_pos})"
    );
    assert!(
        test_pos < clippy_pos,
        "test step (pos {test_pos}) must come before clippy step (pos {clippy_pos})"
    );
    assert!(
        clippy_pos < fmt_pos,
        "clippy step (pos {clippy_pos}) must come before fmt step (pos {fmt_pos})"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the list of steps from the first (and only expected) job.
fn ci_steps(yaml: &Value) -> Vec<Value> {
    let jobs = &yaml["jobs"];
    // The CI file should have exactly one job. Grab the first one regardless of its key name.
    let job = jobs
        .as_mapping()
        .and_then(|m| m.values().next())
        .expect("ci.yml must define at least one job");
    job["steps"]
        .as_sequence()
        .expect("job must have a 'steps' sequence")
        .to_vec()
}

/// Extract the `run:` string from a step, returning empty string for non-run steps.
fn step_run(step: &Value) -> String {
    step["run"].as_str().unwrap_or("").to_string()
}
