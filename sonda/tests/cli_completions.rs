//! `sonda completions <shell>` — the generated script, not just the exit code.
//!
//! The value of generating completions from the `Cli` derive is that they
//! cannot fall behind the parser. A test that only asserted "exit 0, non-empty
//! output" would pass just as happily against a hand-written static script,
//! which is exactly the thing worth ruling out — so these assert that content
//! only the derive knows about is present.

mod common;

use common::sonda_bin;
use std::process::Command;

fn completions_for(shell: &str) -> String {
    let output = Command::new(sonda_bin())
        .args(["completions", shell])
        .output()
        .expect("must spawn sonda binary");
    assert!(
        output.status.success(),
        "completions {shell} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("completion script must be UTF-8")
}

#[test]
fn every_supported_shell_produces_a_script() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let script = completions_for(shell);
        assert!(
            script.len() > 500,
            "{shell} script is suspiciously short ({} bytes) — a near-empty \
             script would still exit 0 and complete nothing",
            script.len()
        );
        assert!(
            script.contains("sonda"),
            "{shell} script must name the binary it completes"
        );
    }
}

#[test]
fn the_script_carries_subcommands_the_parser_defines() {
    // If someone replaces generation with a checked-in static script, these
    // are what stop it silently rotting: they are facts about the live parser.
    let script = completions_for("bash");
    for subcommand in ["run", "list", "show", "new", "test", "completions"] {
        assert!(
            script.contains(subcommand),
            "bash completions must offer the `{subcommand}` subcommand"
        );
    }
}

#[test]
fn the_script_carries_flags_added_long_after_the_command_was_written() {
    // `--alertmanager-url` landed in #552, well after `sonda test` existed.
    // Its presence is evidence the script is derived rather than transcribed:
    // nobody updated a completion list to add it.
    let script = completions_for("bash");
    assert!(
        script.contains("--alertmanager-url"),
        "completions must include flags added after the subcommand was first \
         written — otherwise they are not being derived from the parser"
    );
    assert!(
        script.contains("--prometheus-url"),
        "completions must include the sibling acquisition flag too"
    );
}

#[test]
fn an_unknown_shell_is_refused_rather_than_guessed() {
    let output = Command::new(sonda_bin())
        .args(["completions", "cmd.exe"])
        .output()
        .expect("must spawn sonda binary");
    assert!(
        !output.status.success(),
        "an unsupported shell must fail rather than emit a script for some \
         other shell"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused shell must not write a partial script to stdout"
    );
}

#[test]
fn completions_needs_no_scenario_and_no_catalog() {
    // The subcommand is handled before the tokio runtime is built, so it must
    // work with no arguments of any other kind and in any directory.
    let output = Command::new(sonda_bin())
        .args(["completions", "bash"])
        .current_dir(std::env::temp_dir())
        .output()
        .expect("must spawn sonda binary");
    assert!(
        output.status.success(),
        "completions must not depend on the working directory: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
