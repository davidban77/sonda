//! Integration tests for the `--quiet` / `-q` CLI flag.
//!
//! Verifies that quiet mode suppresses status banners on stderr while still
//! producing metric data on stdout.

use std::process::Command;

/// Return the path to the `sonda` binary built by Cargo.
///
/// Uses the `CARGO_BIN_EXE_sonda` env var set by Cargo during `cargo test`,
/// falling back to building via `cargo build` artifact path.
fn sonda_bin() -> std::path::PathBuf {
    // When running under `cargo test`, CARGO_BIN_EXE_sonda is set automatically
    // for binaries defined in the same package.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sonda") {
        return std::path::PathBuf::from(path);
    }
    // Fallback: build the binary and find it in the target directory.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("sonda crate must have a parent directory");
    workspace_root.join("target").join("debug").join("sonda")
}

/// Running with `-q` should suppress status banners on stderr.
///
/// We run a very short metrics scenario (100ms duration) with `-q` and verify
/// that stderr does not contain the start/stop banner markers (the Unicode
/// play and stop symbols).
#[test]
fn quiet_flag_suppresses_status_banners() {
    let output = Command::new(sonda_bin())
        .args([
            "-q",
            "metrics",
            "--name",
            "test_quiet",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The start banner uses a play symbol (U+25B6) and the stop banner uses
    // a square symbol (U+25A0). Neither should appear in quiet mode.
    assert!(
        !stderr.contains('\u{25b6}'),
        "stderr must not contain start banner in quiet mode, got: {stderr}"
    );
    assert!(
        !stderr.contains('\u{25a0}'),
        "stderr must not contain stop banner in quiet mode, got: {stderr}"
    );

    // Stderr should be completely empty in quiet mode (no errors expected).
    assert!(
        stderr.is_empty(),
        "stderr must be empty in quiet mode for a successful run, got: {stderr}"
    );
}

/// Running without `-q` should produce status banners on stderr.
///
/// We run a very short metrics scenario and verify that stderr contains the
/// scenario name and some recognizable status output.
#[test]
fn without_quiet_flag_produces_status_banners() {
    let output = Command::new(sonda_bin())
        .args([
            "metrics",
            "--name",
            "test_banner",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The scenario name should appear in both start and stop banners.
    assert!(
        stderr.contains("test_banner"),
        "stderr must contain scenario name in normal mode, got: {stderr}"
    );

    // The stop banner should contain "completed in".
    assert!(
        stderr.contains("completed in"),
        "stderr must contain 'completed in' from the stop banner, got: {stderr}"
    );
}

/// The `-q` flag should produce metric data on stdout.
///
/// Even in quiet mode, the actual metric output must still go to stdout.
#[test]
fn quiet_flag_still_produces_stdout_output() {
    let output = Command::new(sonda_bin())
        .args([
            "-q",
            "metrics",
            "--name",
            "test_output",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should have at least some metric output on stdout.
    assert!(
        !stdout.is_empty(),
        "stdout must contain metric output even in quiet mode"
    );

    // The metric name should appear in the output.
    assert!(
        stdout.contains("test_output"),
        "stdout must contain the metric name, got: {stdout}"
    );
}

/// The long-form `--quiet` flag is accepted by the CLI parser.
#[test]
fn long_quiet_flag_is_accepted() {
    let output = Command::new(sonda_bin())
        .args([
            "--quiet",
            "metrics",
            "--name",
            "test_long_quiet",
            "--rate",
            "10",
            "--duration",
            "100ms",
        ])
        .output()
        .expect("failed to execute sonda binary");

    assert!(
        output.status.success(),
        "sonda --quiet should exit successfully, status: {}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "stderr must be empty with --quiet, got: {stderr}"
    );
}
