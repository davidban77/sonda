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
    /// The claim's own line plus the block it introduces — see [`claim_block`].
    block: String,
}

/// The text a verb-count claim is actually making a claim *about*.
///
/// The claim line itself, plus the block it introduces: contiguous following
/// lines, and a fenced code block if one opens within a line or two (the
/// `Primary CLI surface (six verbs):` / ```` ``` ```` shape). Bounded, so a
/// document with no blank lines cannot swallow itself whole.
///
/// #567 review W1: the previous version searched the WHOLE FILE for each verb.
/// Five of the six — run, list, show, new, test — are ordinary English
/// fragments (runner, listing, shows, newline, latest), so only `completions`
/// discriminated at all. The red-verification passed by luck of the corpus:
/// `architecture.md` happened to contain that word exactly once, on the line
/// the mutation deleted. Applied to `cli-flags.md`, where the word survives in
/// its own section heading, the identical defect was invisible.
fn claim_block(lines: &[&str], claim_index: usize) -> String {
    const MAX_BLOCK_LINES: usize = 40;
    let mut block = vec![lines[claim_index]];
    let mut in_fence = false;
    let mut blanks = 0usize;

    for line in lines.iter().skip(claim_index + 1).take(MAX_BLOCK_LINES) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            block.push(line);
            if in_fence {
                break; // the fence this claim introduced has closed
            }
            in_fence = true;
            blanks = 0;
            continue;
        }
        if in_fence {
            block.push(line);
            continue;
        }
        if line.trim().is_empty() {
            blanks += 1;
            // One blank line may separate a claim from the fence it
            // introduces; two means the block is over.
            if blanks > 1 {
                break;
            }
            continue;
        }
        // Reset on content: the comment above says CONSECUTIVE blanks end the
        // block, and until this line the counter never reset, so two blank
        // lines anywhere did — one ordinary sentence between a claim and the
        // fence it introduces was enough to lose the fence (#567 r2 W1).
        blanks = 0;
        block.push(line);
    }
    block.join("\n")
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
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
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
                        block: claim_block(&lines, index),
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
fn a_block_that_lists_the_verbs_lists_all_of_them() {
    // A correct count above a stale list is the same defect one level down.
    //
    // Scoped to the claim's own block rather than the whole file (#567 review
    // W1). Whole-file search made this check almost inert: five of the six
    // verbs are ordinary English fragments, so only `completions` ever
    // discriminated, and only in a file that happened to contain it once.
    //
    // "Is this block a listing?" is decided by how many verbs it already
    // names. A block naming a MAJORITY of them is enumerating the surface, so
    // it must name all of them. A block naming few or none is prose that
    // mentions a count in passing — "the CLI exposes six verbs, all thin
    // wrappers over sonda-core" is true and useful, and compelling it to
    // recite the list would be the check being wrong in the other direction
    // (#567 review M1).
    let mut parser = parser_subcommands();
    parser.retain(|v| v != "help");

    let mut problems = Vec::new();
    for claim in verb_count_claims() {
        let Some(named) = enumerated_verbs(&claim.line, &claim.block, &parser) else {
            continue; // a passing mention, not a listing
        };
        let missing: Vec<&str> = parser
            .iter()
            .filter(|verb| !named.contains(verb))
            .map(|v| v.as_str())
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "{}:{} enumerates {} of the {} verbs, omitting {:?} — {:?}",
                claim.path.display(),
                claim.line_number,
                named.len(),
                parser.len(),
                missing,
                claim.line
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "a block that enumerates the CLI's verbs is missing some of them:\n{}",
        problems.join("\n")
    );
}

/// The verbs a claim actually *enumerates*, if it enumerates any.
///
/// Two shapes occur in this repo, and both attach a list to the count:
///
/// ```text
/// The `sonda` binary has six verbs: `run`, `list`, ... and `completions`.
///
/// Primary CLI surface (six verbs):
/// ```                       <- a fence whose lines are `sonda <verb> ...`
/// ```
///
/// Returns None when the claim carries no enumeration — a passing mention
/// like "the CLI exposes six verbs, all thin wrappers over sonda-core" has
/// nothing to be inconsistent with, and demanding a recitation there is the
/// check being wrong in the other direction (#567 review M1).
///
/// This is deliberately narrower than "does the surrounding prose mention the
/// word somewhere". #567 review W1 defeated that: dropping `completions` from
/// this very list left the word intact in a later sentence on the same line,
/// so the paragraph still named all six while the list named five. The count
/// and the list it introduces have to be compared to each other.
fn enumerated_verbs(claim_line: &str, block: &str, known: &[String]) -> Option<Vec<String>> {
    // Shape 1: an inline list introduced by a colon straight after "verbs".
    if let Some(after) = claim_line.split_once("verbs:").map(|(_, rest)| rest) {
        // The list ends at the first sentence break.
        let list = after.split_once(". ").map_or(after, |(head, _)| head);
        let named: Vec<String> = known
            .iter()
            .filter(|verb| block_names_verb(list, verb))
            .cloned()
            .collect();
        if is_enumeration(&named, known) {
            return Some(named);
        }
    }

    // Shape 2: a fenced usage block. Its lines invoke the binary by name, so
    // the verb is the token straight after `sonda`.
    let fenced: Vec<String> = block
        .lines()
        .filter_map(|line| line.trim().strip_prefix("sonda "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|token| known.iter().any(|verb| verb == token))
        .map(|token| token.to_string())
        .collect();
    if is_enumeration(&fenced, known) {
        return Some(fenced);
    }

    None
}

/// Whether a set of named verbs is an enumeration of the surface.
///
/// A majority means the text is listing the verbs, so it must list all of
/// them. Fewer means a passing mention with an example or two, and demanding
/// a full recitation there is the check being wrong in the other direction.
///
/// #567 r2 M1: this rule was stated in a doc comment and implemented as
/// `!named.is_empty()` — so a passing mention followed by a single
/// `sonda run scenario.yaml` example was compelled to recite all six. The
/// rule now lives in one function that both the prose and the callers point
/// at, because a claim in a comment that the code does not implement is the
/// defect this repo keeps naming.
fn is_enumeration(named: &[String], known: &[String]) -> bool {
    named.len() * 2 > known.len()
}

/// Whether `block` names `verb` as a verb rather than as a substring.
///
/// `run`, `list`, `show`, `new` and `test` all occur inside ordinary words —
/// runner, listing, shows, newline, latest — which is exactly why the
/// whole-file `contains` this replaces could not discriminate. Requiring a
/// non-alphanumeric boundary on both sides is what makes the check mean
/// "names the verb".
fn block_names_verb(block: &str, verb: &str) -> bool {
    let boundary =
        |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
    let bytes = block.as_bytes();
    let mut from = 0usize;
    while let Some(found) = block[from..].find(verb) {
        let start = from + found;
        let end = start + verb.len();
        let before = block[..start].chars().next_back();
        let after = block[end..].chars().next();
        if boundary(before) && boundary(after) {
            return true;
        }
        from = start + 1;
        if from >= bytes.len() {
            break;
        }
    }
    false
}
