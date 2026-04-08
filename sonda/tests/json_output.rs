//! Integration tests for the `sonda scenarios list --json` output.
//!
//! Validates that the JSON structure emitted on stdout is a well-formed array
//! with the expected keys, and that the count matches the scenario files
//! discovered from the search path.

use std::process::Command;

/// Return the path to the `sonda` binary built by Cargo.
///
/// Uses the `CARGO_BIN_EXE_sonda` env var set by Cargo during `cargo test`,
/// falling back to building via `cargo build` artifact path.
fn sonda_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sonda") {
        return std::path::PathBuf::from(path);
    }
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("sonda crate must have a parent directory");
    workspace_root.join("target").join("debug").join("sonda")
}

/// Return the repo-root `scenarios/` directory path.
fn repo_scenarios_dir() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("sonda crate must have a parent directory")
        .join("scenarios")
}

/// Count the number of `.yaml` / `.yml` files in a directory.
fn count_yaml_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .expect("scenarios directory must be readable")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yaml") | Some("yml")
            )
        })
        .count()
}

/// `sonda scenarios list --json` should produce valid JSON on stdout.
#[test]
fn scenarios_list_json_is_valid_json() {
    let output = Command::new(sonda_bin())
        .args(["scenarios", "list", "--json"])
        .env("SONDA_SCENARIO_PATH", repo_scenarios_dir())
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda scenarios list --json should exit with status 0, got: {}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert!(
        parsed.is_array(),
        "JSON output must be an array, got: {parsed}"
    );
}

/// Each element in the JSON array must have the expected keys, all with
/// string values.
#[test]
fn scenarios_list_json_elements_have_required_keys() {
    let output = Command::new(sonda_bin())
        .args(["scenarios", "list", "--json"])
        .env("SONDA_SCENARIO_PATH", repo_scenarios_dir())
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    let arr = parsed.as_array().expect("output must be an array");
    assert!(!arr.is_empty(), "scenario list must not be empty");

    let required_keys = ["name", "category", "signal_type", "description", "source"];
    for (i, elem) in arr.iter().enumerate() {
        let obj = elem
            .as_object()
            .unwrap_or_else(|| panic!("element {i} must be an object, got: {elem}"));
        for key in &required_keys {
            let val = obj.get(*key).unwrap_or_else(|| {
                panic!("element {i} is missing required key {key:?}, got: {elem}")
            });
            assert!(
                val.is_string(),
                "element {i} key {key:?} must be a string, got: {val}"
            );
        }
        assert_eq!(
            obj.len(),
            required_keys.len(),
            "expected exactly {} fields per scenario, got {}",
            required_keys.len(),
            obj.len()
        );
    }
}

/// The JSON array length must match the total number of scenario YAML files
/// in the repo's `scenarios/` directory.
#[test]
fn scenarios_list_json_count_matches_scenario_files() {
    let scenarios_dir = repo_scenarios_dir();
    let output = Command::new(sonda_bin())
        .args(["scenarios", "list", "--json"])
        .env("SONDA_SCENARIO_PATH", &scenarios_dir)
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    let arr = parsed.as_array().expect("output must be an array");
    let expected_count = count_yaml_files(&scenarios_dir);
    assert_eq!(
        arr.len(),
        expected_count,
        "JSON array length ({}) must match scenario YAML file count ({})",
        arr.len(),
        expected_count
    );
}
