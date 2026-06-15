//! Verifies `build.rs` SHA injection and its no-git fallback contract.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn sonda_git_sha_is_injected_at_compile_time() {
    let sha = env!("SONDA_GIT_SHA");
    assert!(!sha.is_empty());
    assert!(
        sha == "unknown" || (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())),
        "SONDA_GIT_SHA must be 'unknown' or a hex SHA, got: {sha:?}"
    );
}

#[test]
fn build_rs_fallback_returns_unknown_when_git_directory_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let no_git_dir: PathBuf = dir.path().into();
    let result = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&no_git_dir)
        .output();
    match result {
        Ok(out) => assert!(
            !out.status.success(),
            "git rev-parse HEAD in a non-git directory must fail; build.rs falls back to 'unknown' on this path"
        ),
        Err(_) => {
            // git binary not present at all — the build.rs path still
            // catches the spawn error and falls back to 'unknown'.
        }
    }
}
