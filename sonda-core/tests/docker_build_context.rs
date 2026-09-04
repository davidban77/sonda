//! Everything the crates embed must be inside the Docker build context.
//!
//! `include_str!` resolves at compile time against the source tree. A path
//! that leaves the crate directory — `catalog/builtin.rs` reaching the
//! root `packs/` through the `sonda-core/packs` symlink — compiles fine from
//! a git checkout and fails inside the image unless the Dockerfile also
//! copies that directory.
//!
//! That went wrong exactly once, and it cost two CI checks: moving the packs
//! from `sonda-core/tests/fixtures/packs/` (inside a directory the Dockerfile
//! copies) to `packs/` at the root (which it did not) broke `docker-build`
//! and `Live Infra UAT` while every cargo gate stayed green. Nothing in the
//! workspace connected the two files.
//!
//! This compares them: the escaping `include_str!` paths are one source of
//! truth, the Dockerfile's own `COPY` lines are the other. It is exact —
//! there is no pattern-matching over prose — so it converges rather than
//! growing a tail of shapes.
//!
//! Only the stage that runs the workspace `cargo build` counts. A `COPY` in the
//! second context-copying stage, which compiles nothing, would otherwise satisfy
//! an escape in the stage that does.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate has a parent directory")
        .to_path_buf()
}

/// Everything the Dockerfile's `cargo build` compiles: each crate's `src/`
/// and its build script.
///
/// The narrowing to `src/` is load-bearing rather than convenient — the image
/// runs `cargo build --release -p sonda -p sonda-server`, which never
/// compiles `tests/`, `benches/` or `examples/`. An `include_str!` under
/// `tests/` genuinely does not need to be in the build context:
/// `sonda/tests/cli_subcommand_parity.rs` embeds
/// `scripts/validate_docs_commands.py` and is correct as it stands.
///
/// **`build.rs` is neither `src/` nor one of those three**, and cargo
/// compiles it before everything else, so it belongs here. No build script
/// embeds anything today; the entry exists so the next one that does is
/// covered without anyone remembering this.
///
/// `sonda-wasm` is included although the image does not build it: the
/// Dockerfile copies it, and if it ever were built the same rule would hold.
///
/// Entries may be directories (walked) or single files.
const COMPILED_SOURCES: [&str; 5] = [
    "sonda-core/src",
    "sonda/src",
    "sonda-server/src",
    "sonda-wasm/src",
    "sonda-server/build.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Every `include_str!` / `include_bytes!` target, resolved to a repo-relative
/// path, that lies outside the crate directory containing it.
fn escaping_includes() -> Vec<(PathBuf, PathBuf)> {
    let root = repo_root();
    let mut found = Vec::new();

    for entry in COMPILED_SOURCES {
        let path = root.join(entry);
        // The crate directory is the parent of `src/` or of `build.rs` alike.
        let crate_dir = path
            .parent()
            .expect("a compiled source has a parent")
            .to_path_buf();
        let mut files = Vec::new();
        if path.is_dir() {
            rust_files(&path, &mut files);
        } else if path.is_file() {
            files.push(path);
        }
        files.sort();

        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for macro_name in ["include_str!(", "include_bytes!("] {
                let mut from = 0;
                while let Some(at) = text[from..].find(macro_name) {
                    let open = from + at + macro_name.len();
                    let rest = &text[open..];
                    // Only plain string literals; a macro-built path is not
                    // something this check can resolve, and none exist.
                    let Some(start) = rest.find('"') else { break };
                    let Some(len) = rest[start + 1..].find('"') else {
                        break;
                    };
                    let literal = &rest[start + 1..start + 1 + len];
                    from = open + start + 1 + len;

                    let containing_dir = file.parent().expect("a file has a parent");
                    let target = resolve(&root, &containing_dir.join(literal));
                    if !target.starts_with(&crate_dir) {
                        let rel = target.strip_prefix(&root).unwrap_or(&target).to_path_buf();
                        found.push((file.strip_prefix(&root).unwrap_or(&file).to_path_buf(), rel));
                    }
                }
            }
        }
    }
    found
}

/// Where an include really lands, repo-relative.
///
/// Lexical normalization alone would read `sonda-core/src/catalog/../../packs`
/// as staying inside the crate. It does not: `sonda-core/packs` is a symlink to
/// the root `packs/`, so the Docker build context still needs that directory.
/// Resolving it keeps the escape visible to [`escaping_includes`].
///
/// A path that does not exist keeps its lexical form — a missing include is the
/// compiler's error to report, not this check's.
fn resolve(root: &Path, path: &Path) -> PathBuf {
    let lexical = normalize(path);
    let (Ok(real), Ok(real_root)) = (lexical.canonicalize(), root.canonicalize()) else {
        return lexical;
    };
    match real.strip_prefix(&real_root) {
        Ok(rel) => root.join(rel),
        Err(_) => lexical,
    }
}

/// Resolve `..` segments lexically. `Path::canonicalize` would need the file
/// to exist, and a missing include is the compiler's error to report, not
/// this check's.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

struct Stage {
    name: Option<String>,
    compiles_the_workspace: bool,
    copy_sources: BTreeSet<PathBuf>,
}

/// Instructions with their line continuations joined, comments dropped.
fn instructions(text: &str) -> Vec<String> {
    let mut joined = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() && pending.is_empty() {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                pending.push_str(head.trim_end());
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                joined.push(std::mem::take(&mut pending));
            }
        }
    }
    if !pending.is_empty() {
        joined.push(pending);
    }
    joined
}

fn dockerfile_stages() -> Vec<Stage> {
    let text =
        std::fs::read_to_string(repo_root().join("Dockerfile")).expect("the repo has a Dockerfile");
    parse_stages(&text)
}

fn parse_stages(text: &str) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();

    for line in instructions(text) {
        let mut tokens = line.split_whitespace();
        let Some(instruction) = tokens.next() else {
            continue;
        };
        match instruction.to_ascii_uppercase().as_str() {
            "FROM" => {
                let rest: Vec<&str> = tokens.collect();
                let name = rest
                    .windows(2)
                    .find(|pair| pair[0].eq_ignore_ascii_case("as"))
                    .map(|pair| pair[1].to_string());
                stages.push(Stage {
                    name,
                    compiles_the_workspace: false,
                    copy_sources: BTreeSet::new(),
                });
            }
            "RUN" if line.contains("cargo build") => {
                if let Some(stage) = stages.last_mut() {
                    stage.compiles_the_workspace = true;
                }
            }
            "COPY" => {
                let Some(stage) = stages.last_mut() else {
                    continue;
                };
                // `--from=<stage>` copies between stages, not from the context.
                let args: Vec<&str> = tokens
                    .skip_while(|token| token.starts_with("--"))
                    .filter(|token| !token.starts_with("<<"))
                    .collect();
                if line.contains("--from") || args.len() < 2 {
                    continue;
                }
                for src in &args[..args.len() - 1] {
                    stage
                        .copy_sources
                        .insert(PathBuf::from(src.trim_end_matches('/')));
                }
            }
            _ => {}
        }
    }
    stages
}

/// The stage that runs the workspace `cargo build`, located by what it does
/// rather than by its name so a rename cannot silently empty this check.
fn compiling_stage() -> Stage {
    let mut compiling: Vec<Stage> = dockerfile_stages()
        .into_iter()
        .filter(|stage| stage.compiles_the_workspace)
        .collect();
    assert_eq!(
        compiling.len(),
        1,
        "expected exactly one Dockerfile stage to run the workspace `cargo build`, found {}: {:?}",
        compiling.len(),
        compiling
            .iter()
            .map(|stage| &stage.name)
            .collect::<Vec<_>>()
    );
    compiling.remove(0)
}

/// Guards both checks below against a scan that silently found nothing.
///
/// A renamed crate directory, or an `include_str!` spelling this file does
/// not recognise, would otherwise make the coverage check iterate over an
/// empty set and report success — which is the shape that let the original
/// defect through.
#[test]
fn the_scan_finds_the_escaping_includes_that_are_known_to_exist() {
    let escaping = escaping_includes();
    assert!(
        escaping.len() >= 3,
        "expected at least the three embedded packs to escape sonda-core, found {}: {escaping:?}",
        escaping.len()
    );
    assert!(
        escaping
            .iter()
            .any(|(_, target)| target.starts_with("packs")),
        "the builtin packs must show up as escaping includes: {escaping:?}"
    );

    let stage = compiling_stage();
    assert!(
        stage.name.is_some(),
        "the compiling stage must be named so the runtime stage can copy from it"
    );
    assert!(
        stage.copy_sources.len() >= 4,
        "the compiling stage should copy at least the crate directories, parsed {}: {:?}",
        stage.copy_sources.len(),
        stage.copy_sources
    );
}

#[test]
fn a_copy_in_a_stage_that_compiles_nothing_does_not_count_as_coverage() {
    let stages = parse_stages(
        "FROM scratch AS prebuilt\n\
         COPY packs/ packs/\n\
         FROM rust AS builder\n\
         COPY sonda-core/ sonda-core/\n\
         RUN cargo build --release -p sonda\n",
    );

    let compiling: Vec<&Stage> = stages
        .iter()
        .filter(|stage| stage.compiles_the_workspace)
        .collect();
    assert_eq!(compiling.len(), 1);
    assert_eq!(compiling[0].name.as_deref(), Some("builder"));
    assert!(!compiling[0].copy_sources.contains(&PathBuf::from("packs")));
    assert!(compiling[0]
        .copy_sources
        .contains(&PathBuf::from("sonda-core")));
}

#[test]
fn a_heredoc_copy_contributes_no_build_context_source() {
    let stages = parse_stages(
        "FROM scratch AS prebuilt\n\
         COPY --chmod=0755 dist/amd64/sonda /out/sonda\n\
         COPY <<EOF /tmp/passwd.sonda\n\
         sonda:x:65532:65532::/:\n\
         EOF\n",
    );

    assert_eq!(stages.len(), 1);
    assert_eq!(
        stages[0].copy_sources,
        BTreeSet::from([PathBuf::from("dist/amd64/sonda")]),
        "the heredoc marker was read as a path into the build context"
    );

    for stage in dockerfile_stages() {
        for source in &stage.copy_sources {
            assert!(
                !source.to_string_lossy().starts_with("<<"),
                "stage {:?} takes {} as a build-context path",
                stage.name,
                source.display()
            );
        }
    }
}

/// The check itself: every escaping include must be inside something the
/// Dockerfile copies.
#[test]
fn every_escaping_include_is_inside_the_docker_build_context() {
    let escaping = escaping_includes();
    let copies = compiling_stage().copy_sources;

    let mut uncovered = Vec::new();
    for (source_file, target) in &escaping {
        let covered = copies
            .iter()
            .any(|copied| target == copied || target.starts_with(copied));
        if !covered {
            uncovered.push(format!(
                "{} embeds {}, which no Dockerfile COPY brings into the build context",
                source_file.display(),
                target.display()
            ));
        }
    }

    assert!(
        uncovered.is_empty(),
        "add a `COPY <dir>/ <dir>/` to the Dockerfile beside the crate COPY lines:\n  {}",
        uncovered.join("\n  ")
    );
}
