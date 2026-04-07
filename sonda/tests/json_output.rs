//! Integration tests for the `sonda scenarios list --json` output.
//!
//! Validates that the JSON structure emitted on stdout is a well-formed array
//! with the expected keys, and that the count matches the embedded catalog.

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

/// `sonda scenarios list --json` should produce valid JSON on stdout.
#[test]
fn scenarios_list_json_is_valid_json() {
    let output = Command::new(sonda_bin())
        .args(["scenarios", "list", "--json"])
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
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    let arr = parsed.as_array().expect("output must be an array");
    assert!(!arr.is_empty(), "scenario list must not be empty");

    let required_keys = ["name", "category", "signal_type", "description"];
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
    }
}

/// The JSON array length must match the total number of built-in scenarios
/// from `sonda_core::scenarios::list()`.
#[test]
fn scenarios_list_json_count_matches_core_catalog() {
    let output = Command::new(sonda_bin())
        .args(["scenarios", "list", "--json"])
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    let arr = parsed.as_array().expect("output must be an array");
    let expected_count = sonda_core::scenarios::list().len();
    assert_eq!(
        arr.len(),
        expected_count,
        "JSON array length ({}) must match sonda_core::scenarios::list().len() ({})",
        arr.len(),
        expected_count
    );
}
