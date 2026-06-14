use std::path::PathBuf;
use std::process::Command;

fn main() {
    let sha = git_head_sha().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SONDA_GIT_SHA={sha}");

    if let Some(head) = git_head_path() {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}

fn git_head_sha() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = sha.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn git_head_path() -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let candidate = PathBuf::from(manifest_dir).join("../.git/HEAD");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}
