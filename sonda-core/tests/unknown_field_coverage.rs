//! Pins the set of types that carry `#[serde(flatten)]`.
//!
//! The parser rejects unknown YAML keys by wrapping its deserializer in
//! `serde_ignored` (see `compiler::parse::deserialize_recording_unknown`).
//! That wrapper has one blind spot: on a struct that itself carries
//! `#[serde(flatten)]`, serde routes leftover keys to the flattened field's
//! own deserializer, so unknown keys there are never reported.
//!
//! The blind spot is therefore exactly the set of flatten-carrying types
//! reachable from the parser. Today that is `DynamicLabelConfig` alone. This
//! gate fails when that set changes, so adding a `flatten` forces a decision
//! about the coverage it costs rather than silently widening the hole.

use std::collections::BTreeSet;
use std::path::Path;

/// Every type in sonda-core declaring `#[serde(flatten)]`, with whether the
/// v2 parser can reach it from a scenario file.
///
/// Reachable entries are blind spots: unknown keys inside them parse silently.
/// Unreachable entries are the runtime config structs, which are built in code
/// by the compiler and never deserialized from user YAML.
const DECLARED_FLATTEN_TYPES: &[(&str, Reach)] = &[
    ("DynamicLabelConfig", Reach::ParserReachable),
    ("ScenarioConfig", Reach::NotDeserialized),
    ("HistogramScenarioConfig", Reach::NotDeserialized),
    ("SummaryScenarioConfig", Reach::NotDeserialized),
    ("LogScenarioConfig", Reach::NotDeserialized),
];

#[derive(Debug, PartialEq, Eq)]
enum Reach {
    /// Deserialized from user YAML by the v2 parser — unknown keys here are
    /// invisible.
    ParserReachable,
    /// Never deserialized from user YAML; constructed by the compiler.
    NotDeserialized,
}

/// Walk `src/` and return `(type name, file, line)` for every `serde(flatten)`.
fn find_flatten_types(src: &Path) -> Vec<(String, String, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![src.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                // An attribute line, not a doc comment mentioning one.
                let trimmed = line.trim_start();
                if !trimmed.starts_with("#[") || !trimmed.contains("serde(flatten)") {
                    continue;
                }
                // Nearest preceding type declaration owns this field.
                let owner = lines[..i]
                    .iter()
                    .rev()
                    .find_map(|l| parse_type_decl(l))
                    .unwrap_or_else(|| {
                        panic!(
                            "{}:{}: flatten with no enclosing type",
                            path.display(),
                            i + 1
                        )
                    });
                found.push((owner, path.display().to_string(), i + 1));
            }
        }
    }
    found
}

/// Extract the name from a `pub struct X` / `pub enum X` line.
fn parse_type_decl(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub struct ")
        .or_else(|| line.strip_prefix("pub enum "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

#[test]
fn flatten_types_match_the_declared_set() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(src.is_dir(), "source tree missing at {}", src.display());

    let found = find_flatten_types(&src);
    assert!(
        !found.is_empty(),
        "no `serde(flatten)` found in {} — the scan is broken, not the code",
        src.display()
    );

    let actual: BTreeSet<&str> = found.iter().map(|(t, _, _)| t.as_str()).collect();
    let declared: BTreeSet<&str> = DECLARED_FLATTEN_TYPES.iter().map(|(t, _)| *t).collect();

    let undeclared: Vec<_> = actual.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "undeclared `serde(flatten)` on {undeclared:?}.\n\
         A flatten hides unknown YAML keys from the parser's unknown-field check.\n\
         Add it to DECLARED_FLATTEN_TYPES marking whether the parser reaches it, \
         and if it does, document the gap in docs/site/docs/reference/scenario-fields.md.\n\
         Sites: {found:?}"
    );

    let stale: Vec<_> = declared.difference(&actual).collect();
    assert!(
        stale.is_empty(),
        "DECLARED_FLATTEN_TYPES lists {stale:?}, which no longer carry `serde(flatten)` — \
         drop them from the list"
    );
}

#[test]
fn only_dynamic_label_config_is_a_parser_reachable_blind_spot() {
    let reachable: Vec<&str> = DECLARED_FLATTEN_TYPES
        .iter()
        .filter(|(_, r)| *r == Reach::ParserReachable)
        .map(|(t, _)| *t)
        .collect();

    assert_eq!(
        reachable,
        vec!["DynamicLabelConfig"],
        "the set of parser-reachable flatten types changed; \
         update docs/site/docs/reference/scenario-fields.md's limitation note to match"
    );
}
