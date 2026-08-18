//! Every copy of "which verbs does the CLI have" must agree with the parser.
//!
//! The `Commands` enum is the only one that is real. Two machine-readable
//! copies and an open-ended set of prose claims are checked against it here.
//!
//! Deliberately no count is stated in this sentence. An earlier version said
//! "there are three", `sonda-server`'s comment said "a fourth", and #565's PR
//! body said "a fifth" — three hand-maintained tallies of the hand-maintained
//! copies, disagreeing with each other, in the file arguing that hand-
//! maintained lists drift (#565 review M2). The tests below enumerate; this
//! comment does not.
//!
//! The machine-readable copies:
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
//!
//! The prose claims are handled differently, and the difference is the point.
//! #565 enumerated the copies BY HAND and missed one: `docs/architecture.md`
//! had said "four verbs" since `sonda test` shipped, unnoticed because it sits
//! outside the docs gate's root. Naming three files here would repeat that
//! mistake with a larger three. So the prose check does not take a file list:
//! it walks every tracked Markdown file and holds any file making a
//! "<number> verbs" claim to the parser's count. A new document inherits the
//! gate by existing.

mod common;

use common::sonda_bin;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Number words a prose verb-count claim might spell out, plus digits.
fn spelled_number(word: &str) -> Option<usize> {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    let lower = word.to_ascii_lowercase();
    if let Some(index) = WORDS.iter().position(|w| *w == lower) {
        return Some(index);
    }
    lower.parse::<usize>().ok()
}

/// Repo root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the sonda crate must live inside the workspace")
        .to_path_buf()
}

/// Every Markdown file in the repository, skipping build and vendor trees.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // Build output, vendored JS, git internals and virtualenvs are not
            // ours to gate.
            //
            // Note `docs/site/site/` — mkdocs' build output per .gitignore —
            // and NOT any directory merely named `site`: excluding that name
            // outright silently swallowed `docs/site/docs/**`, which is where
            // the user-facing reference lives. The guard test below caught it
            // on the first run by naming a file it expects to find, which is
            // the whole reason that guard exists.
            let is_mkdocs_output = name == "site" && dir.file_name().is_some_and(|d| d == "site");
            if is_mkdocs_output
                || matches!(name.as_ref(), "target" | "node_modules" | ".git" | ".venv")
            {
                continue;
            }
            markdown_files(&path, out);
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
}

/// A prose claim of the form "<number> verbs", with where it was found.
struct VerbCountClaim {
    path: PathBuf,
    line_number: usize,
    claimed: usize,
    line: String,
}

/// Every unexempted "<number> verbs" claim in the repository's Markdown.
///
/// Two exemptions, both narrow and both visible in the source they exempt:
///
/// * `CHANGELOG.md` — an append-only record of what was true at each release.
///   Rewriting past entries to match today would falsify history, so the file
///   is skipped wholesale rather than line by line.
/// * Any line carrying `<!-- verbs:historical -->` — prose that deliberately
///   describes an earlier surface. This is an opt-out a human must type, and
///   it is visible in the diff when they do; it is not a silent exemption.
fn verb_count_claims() -> Vec<VerbCountClaim> {
    let root = repo_root();
    let mut files = Vec::new();
    markdown_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "found no Markdown files under {} — the walk is broken, and an empty \
         set would make this gate pass vacuously",
        root.display()
    );

    let mut claims = Vec::new();
    for path in files {
        if path.file_name().is_some_and(|n| n == "CHANGELOG.md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if line.contains("<!-- verbs:historical -->") {
                continue;
            }
            let mut words = line.split_whitespace().peekable();
            while let Some(word) = words.next() {
                let next_is_verbs = words
                    .peek()
                    .is_some_and(|w| w.trim_end_matches([':', '.', ',', ')']) == "verbs");
                if !next_is_verbs {
                    continue;
                }
                if let Some(claimed) = spelled_number(word.trim_start_matches('(')) {
                    claims.push(VerbCountClaim {
                        path: path.clone(),
                        line_number: index + 1,
                        claimed,
                        line: line.trim().to_string(),
                    });
                }
            }
        }
    }
    claims
}

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

#[test]
fn the_walk_finds_the_claims_this_test_expects_to_check() {
    // Guards the walk itself, for the same reason `help_lists_the_verbs…`
    // guards the help parse: if the traversal or the phrase match silently
    // stops finding anything, every assertion below passes on an empty set.
    let claims = verb_count_claims();
    assert!(
        !claims.is_empty(),
        "found no '<number> verbs' claims anywhere in the repo's Markdown. \
         Either the walk broke or the phrasing changed — both make the gate \
         below vacuous."
    );
    let files: Vec<String> = claims
        .iter()
        .map(|c| {
            c.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for expected in ["cli-flags.md", "CLAUDE.md", "architecture.md"] {
        assert!(
            files.iter().any(|f| f == expected),
            "expected a verb-count claim in {expected}; found claims in {files:?}"
        );
    }
}

#[test]
fn every_prose_verb_count_matches_the_parser() {
    let mut parser = parser_subcommands();
    parser.retain(|v| v != "help");
    parser.sort();
    parser.dedup();

    let mut wrong = Vec::new();
    for claim in verb_count_claims() {
        if claim.claimed != parser.len() {
            wrong.push(format!(
                "{}:{} claims {} verbs, parser has {} — {:?}",
                claim.path.display(),
                claim.line_number,
                claim.claimed,
                parser.len(),
                claim.line
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "prose verb counts have drifted from the CLI.\n{}\n\nThis is what \
         #565 missed: docs/architecture.md had said \"four verbs\" since \
         `sonda test` shipped, because nothing checked it and it sits outside \
         the docs gate's root. If a claim is deliberately describing an older \
         surface, mark that line `<!-- verbs:historical -->`.",
        wrong.join("\n")
    );
}

#[test]
fn a_document_that_counts_the_verbs_also_names_them() {
    // A correct count with a stale list is the same defect one level down:
    // `cli-flags.md` would have said "six verbs" while listing four.
    let mut parser = parser_subcommands();
    parser.retain(|v| v != "help");

    let mut missing = Vec::new();
    for claim in verb_count_claims() {
        let Ok(text) = std::fs::read_to_string(&claim.path) else {
            continue;
        };
        for verb in &parser {
            if !text.contains(verb.as_str()) {
                missing.push(format!(
                    "{} counts the verbs but never names `{verb}`",
                    claim.path.display()
                ));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
