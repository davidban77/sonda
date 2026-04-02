//! Integration tests for the `--dry-run` and `--verbose` CLI flags.
//!
//! Verifies observable behavior: exit codes, stdout/stderr content, and flag
//! interactions. Follows the same pattern as `quiet_flag.rs`.

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

// ---------------------------------------------------------------------------
// --dry-run flag
// ---------------------------------------------------------------------------

/// `--dry-run` should exit with status 0 and produce no stdout output.
///
/// In dry-run mode, the config is validated and printed to stderr, but no
/// events are emitted, so stdout must be empty.
#[test]
fn dry_run_exits_zero_with_no_stdout() {
    let output = Command::new(sonda_bin())
        .args([
            "--dry-run",
            "metrics",
            "--name",
            "test_dry",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda --dry-run should exit with status 0, got: {}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "stdout must be empty in dry-run mode (no events emitted), got: {stdout}"
    );
}

/// `--dry-run` stderr should contain the config header `[config]` and
/// `Validation: OK`.
///
/// The config display uses `[config]` as its header prefix, and the dry-run
/// mode prints a validation result after the config.
#[test]
fn dry_run_stderr_contains_config_and_validation() {
    let output = Command::new(sonda_bin())
        .args([
            "--dry-run",
            "metrics",
            "--name",
            "test_dry_config",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda --dry-run should exit successfully, got: {}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("[config]"),
        "dry-run stderr must contain the [config] header, got: {stderr}"
    );
    assert!(
        stderr.contains("Validation:"),
        "dry-run stderr must contain 'Validation:' result, got: {stderr}"
    );
    assert!(
        stderr.contains("OK"),
        "dry-run stderr must contain 'OK' for valid config, got: {stderr}"
    );
}

/// `--dry-run` stderr should show the resolved scenario name.
#[test]
fn dry_run_stderr_contains_scenario_name() {
    let output = Command::new(sonda_bin())
        .args([
            "--dry-run",
            "metrics",
            "--name",
            "my_dry_run_metric",
            "--rate",
            "5",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("my_dry_run_metric"),
        "dry-run stderr must show the metric name, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --verbose flag
// ---------------------------------------------------------------------------

/// `--verbose` should produce stdout output (events are emitted) and stderr
/// should contain the `[config]` header.
///
/// Unlike `--dry-run`, verbose mode actually runs the scenario, so stdout
/// should contain metric data and stderr should show the config before the
/// start banner.
#[test]
fn verbose_produces_stdout_and_config_on_stderr() {
    let output = Command::new(sonda_bin())
        .args([
            "--verbose",
            "metrics",
            "--name",
            "test_verbose",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda --verbose should exit successfully, got: {}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "stdout must contain metric output in verbose mode"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "verbose stderr must contain the [config] header, got: {stderr}"
    );
}

/// `--verbose` stderr should also contain start/stop banners.
#[test]
fn verbose_stderr_contains_banners() {
    let output = Command::new(sonda_bin())
        .args([
            "--verbose",
            "metrics",
            "--name",
            "test_verbose_banners",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The stop banner should contain "completed in".
    assert!(
        stderr.contains("completed in"),
        "verbose stderr must contain 'completed in' from the stop banner, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --quiet --verbose conflict
// ---------------------------------------------------------------------------

/// `--quiet --verbose` must be rejected by clap with a non-zero exit code.
///
/// The two flags are declared as `conflicts_with` in the clap definition, so
/// passing both at once is a user error.
#[test]
fn quiet_and_verbose_conflict_is_rejected() {
    let output = Command::new(sonda_bin())
        .args([
            "--quiet",
            "--verbose",
            "metrics",
            "--name",
            "test_conflict",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        !output.status.success(),
        "sonda --quiet --verbose must exit with non-zero status, got: {}",
        output.status
    );
}

/// `--quiet --verbose` stderr should contain a clap error message.
#[test]
fn quiet_and_verbose_conflict_shows_error() {
    let output = Command::new(sonda_bin())
        .args([
            "--quiet",
            "--verbose",
            "metrics",
            "--name",
            "test_conflict_err",
            "--rate",
            "10",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Clap error messages mention the conflicting flags.
    assert!(
        stderr.contains("--quiet") || stderr.contains("--verbose"),
        "clap error must mention the conflicting flag(s), got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --dry-run with logs subcommand
// ---------------------------------------------------------------------------

/// `--dry-run` works with the `logs` subcommand too.
#[test]
fn dry_run_with_logs_subcommand() {
    let output = Command::new(sonda_bin())
        .args([
            "--dry-run",
            "logs",
            "--mode",
            "template",
            "--message",
            "test log line",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda --dry-run logs should exit with status 0, got: {}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "stdout must be empty in dry-run mode for logs, got: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[config]"),
        "dry-run logs stderr must contain [config], got: {stderr}"
    );
    assert!(
        stderr.contains("OK"),
        "dry-run logs stderr must contain validation OK, got: {stderr}"
    );
}
