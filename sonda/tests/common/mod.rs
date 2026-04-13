//! Shared helpers for the CLI integration test suite.
//!
//! Integration tests in `sonda/tests/*.rs` spawn the built `sonda` binary
//! as a subprocess (via `CARGO_BIN_EXE_sonda`) and assert on stdout /
//! stderr / exit status. This module consolidates the plumbing so each
//! test file only needs to describe what it is testing.
//!
//! Per-test isolation: callers should use `tempfile::TempDir` for any
//! files they produce, and clean up in a `Drop` scope so parallel test
//! runs don't collide.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Absolute path to the `sonda` binary built by Cargo for tests.
pub fn sonda_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sonda"))
}

/// Absolute path to this crate's `tests/fixtures/cli/` directory.
///
/// Every fixture consumed by CLI tests lives here. The path is computed
/// from `CARGO_MANIFEST_DIR` so tests work regardless of where Cargo
/// invokes them from.
pub fn cli_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli")
}

/// Build a `Command` for the `sonda` binary pre-populated with common
/// test args: `--scenario-path` / `--pack-path` pointing at the
/// supplied directories so the test catalog is fully isolated from the
/// user's filesystem.
///
/// Any `SONDA_*` env vars in the current process are scrubbed so the
/// defaults search path (`~/.sonda/...`) never interferes with tests.
pub fn sonda_command(scenario_dir: Option<&Path>, pack_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new(sonda_bin());
    cmd.env_remove("SONDA_SCENARIO_PATH");
    cmd.env_remove("SONDA_PACK_PATH");
    // Point scenario and pack search paths at the caller-supplied dirs
    // (or an empty temp dir so no host fixtures leak into tests).
    if let Some(p) = scenario_dir {
        cmd.arg("--scenario-path").arg(p);
    }
    if let Some(p) = pack_dir {
        cmd.arg("--pack-path").arg(p);
    }
    cmd
}

/// Run the given [`Command`] and assert on the outcome. Returns the
/// captured [`Output`] on success; panics with a readable diagnostic on
/// non-zero exit (unless `expect_success` is `false`).
pub fn run_and_check(mut cmd: Command, expect_success: bool) -> Output {
    let output = cmd.output().expect("must spawn sonda binary");
    let succeeded = output.status.success();
    if succeeded != expect_success {
        panic!(
            "unexpected exit: success={succeeded}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    output
}

/// Convenience: spawn `sonda` with the given args and return the Output,
/// panicking if the exit status is non-zero.
pub fn sonda_ok(args: &[&str]) -> Output {
    let mut cmd = Command::new(sonda_bin());
    cmd.env_remove("SONDA_SCENARIO_PATH");
    cmd.env_remove("SONDA_PACK_PATH");
    cmd.args(args);
    run_and_check(cmd, true)
}
