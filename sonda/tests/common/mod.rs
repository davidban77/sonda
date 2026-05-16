//! Shared helpers for the CLI integration test suite.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn sonda_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sonda"))
}

pub fn cli_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli")
}

/// Build a `sonda` command pre-populated with `--catalog <dir>` when supplied.
pub fn sonda_command(catalog_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new(sonda_bin());
    if let Some(p) = catalog_dir {
        cmd.arg("--catalog").arg(p);
    }
    cmd
}

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

pub fn sonda_ok(args: &[&str]) -> Output {
    let mut cmd = Command::new(sonda_bin());
    cmd.args(args);
    run_and_check(cmd, true)
}
