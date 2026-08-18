//! Every copy of "which verbs does the CLI have" must agree with the parser.
//!
//! There are three, and the `Commands` enum is the only one that is real:
//!
//! * `sonda-server`'s `SONDA_SUBCOMMANDS` — it shells out to the sibling
//!   `sonda` binary for anything in that list, so a verb missing from it is
//!   not forwarded and fails as an unknown argument.
//! * `validate_docs_commands.py`'s `KNOWN_SUBCOMMANDS` — it rejects any
//!   `sonda <verb>` in the documentation it does not recognise, so a verb
//!   missing from it makes the gate reject correct docs.
//!
//! Both were kept correct by memory. The repo's own `sonda/CLAUDE.md` told the
//! next author to remember the first; nothing at all mentioned the second.
//! Adding `completions` was the first new verb since either was written, and it
//! found both — the server list via that doc comment, the validator list only
//! by going red on documentation that was not wrong. This file replaces the
//! instruction with a check.
//!
//! It reads the two sources as text rather than linking them: they are a
//! separate binary crate and a Python script, with no shared definition
//! available to import, so comparing the literals is the honest option. If
//! either constant is restructured, this fails loudly — which is correct. A
//! silent pass on a constant it can no longer find would be the exact failure
//! it exists to prevent, so both parses assert they found something.

mod common;

use common::sonda_bin;
use std::process::Command;

/// The verbs `sonda --help` advertises, read from the live parser.
fn parser_subcommands() -> Vec<String> {
    let output = Command::new(sonda_bin())
        .arg("--help")
        .output()
        .expect("must spawn sonda binary");
    let help = String::from_utf8(output.stdout).expect("help must be UTF-8");

    let commands_section = help
        .split("Commands:")
        .nth(1)
        .expect("`sonda --help` must have a Commands: section");

    let mut verbs = Vec::new();
    for line in commands_section.lines() {
        // Subcommand lines are indented and start with the verb; the section
        // ends at the first non-indented line (`Options:`).
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with("  ") {
            break;
        }
        let first = line.split_whitespace().next().unwrap_or_default();
        if first.is_empty() || first.starts_with('-') {
            continue;
        }
        verbs.push(first.to_string());
    }
    assert!(
        !verbs.is_empty(),
        "parsed no subcommands out of `sonda --help` — the parse is wrong, \
         and an empty list would make every comparison below pass vacuously"
    );
    verbs
}

/// The verbs `sonda-server` forwards, read from its source constant.
fn server_dispatch_list() -> Vec<String> {
    let source = include_str!("../../sonda-server/src/main.rs");
    let line = source
        .lines()
        .find(|l| l.contains("const SONDA_SUBCOMMANDS"))
        .expect(
            "sonda-server must still declare SONDA_SUBCOMMANDS — if it was renamed or removed, \
             this gate needs updating rather than deleting",
        );
    // Split on `= &[`, not on the first `[` — the first one belongs to the
    // `&[&str]` type annotation, and splitting there swallows `&str] = &[`
    // into the first element. Caught by this test's own comparison on the
    // first run, which is the argument for comparing real values rather than
    // asserting the parse "looks right".
    let list = line
        .split_once("= &[")
        .and_then(|(_, rest)| rest.rsplit_once(']'))
        .map(|(inner, _)| inner)
        .expect("SONDA_SUBCOMMANDS must be a bracketed slice literal");

    let verbs: Vec<String> = list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect();
    assert!(
        !verbs.is_empty(),
        "parsed no verbs out of SONDA_SUBCOMMANDS — see the note above about \
         vacuous passes"
    );
    verbs
}

/// The verbs the docs-command validator will accept, read from its source.
///
/// A third copy of the same list, found the same way the second was: by adding
/// a verb and watching something unrelated go red. It rejects any `sonda <verb>`
/// in the documentation that it does not recognise, so falling behind here means
/// correct documentation is reported as broken.
fn docs_validator_list() -> Vec<String> {
    let source = include_str!("../../scripts/validate_docs_commands.py");
    let start = source
        .find("KNOWN_SUBCOMMANDS")
        .expect("validate_docs_commands.py must still declare KNOWN_SUBCOMMANDS");
    let rest = &source[start..];
    let open = rest
        .find('{')
        .expect("KNOWN_SUBCOMMANDS must contain a set literal");
    let close = rest[open..]
        .find('}')
        .expect("KNOWN_SUBCOMMANDS set literal must be closed")
        + open;

    let verbs: Vec<String> = rest[open + 1..close]
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect();
    assert!(
        !verbs.is_empty(),
        "parsed no verbs out of KNOWN_SUBCOMMANDS — an empty list would make \
         the comparison pass vacuously"
    );
    verbs
}

#[test]
fn help_lists_the_verbs_this_test_expects_to_find() {
    // Guards the parse itself. If `sonda --help` changes shape and the
    // extraction silently returns something plausible-but-wrong, the parity
    // check below could pass while comparing nonsense.
    let verbs = parser_subcommands();
    for expected in ["run", "list", "show", "new", "test", "completions"] {
        assert!(
            verbs.iter().any(|v| v == expected),
            "expected `{expected}` among the parsed verbs, got: {verbs:?}"
        );
    }
}

#[test]
fn the_server_forwards_every_verb_the_cli_defines() {
    let mut parser = parser_subcommands();
    let mut server = server_dispatch_list();

    // `help` is clap's built-in; the shim has no reason to forward it.
    parser.retain(|v| v != "help");
    server.retain(|v| v != "help");

    parser.sort();
    parser.dedup();
    server.sort();
    server.dedup();

    assert_eq!(
        parser, server,
        "sonda-server's SONDA_SUBCOMMANDS has drifted from the CLI's verbs. \
         A verb missing from the server list is not forwarded to the sibling \
         binary and fails as an unknown argument; a verb in the list that the \
         CLI does not define forwards a command that cannot run."
    );
}

#[test]
fn the_docs_validator_accepts_every_verb_the_cli_defines() {
    let mut parser = parser_subcommands();
    let mut validator = docs_validator_list();

    // The validator has no reason to accept clap's built-in `help` in a
    // documented command line.
    parser.retain(|v| v != "help");
    validator.retain(|v| v != "help");

    parser.sort();
    parser.dedup();
    validator.sort();
    validator.dedup();

    assert_eq!(
        parser, validator,
        "validate_docs_commands.py's KNOWN_SUBCOMMANDS has drifted from the \
         CLI's verbs. A verb missing here makes the gate reject documentation \
         that is actually correct; a verb here that the CLI does not define \
         lets a typo through as a real command."
    );
}
