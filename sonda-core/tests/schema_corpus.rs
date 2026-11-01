//! The schema must not reject what the parser accepts.
//!
//! `sonda-core/src/schema.rs` derives a JSON Schema from the same types the
//! parser deserializes into. That link holds automatically for every type
//! whose `Deserialize` is derived — and breaks silently for the two that
//! hand-write it, and for any third that is added later. A comment saying
//! "mirror your hand-written Deserialize in the JsonSchema impl" is a wish;
//! this file is the gate.
//!
//! The check runs both directions, because only one of them is cheap to get
//! right by accident:
//!
//! - **No false rejections** (`every_repo_scenario_validates`). Every v2 YAML
//!   file in `examples/` is parsed by the real parser and validated against
//!   the schema. A file the parser takes and the schema refuses would make
//!   the editor underline correct YAML in red, which is worse than no schema
//!   at all. This is the direction a wrong hand-written impl breaks.
//!
//! - **Still discriminating** (`the_schema_rejects_*`). A schema of `true`
//!   passes the first check perfectly. The negative cases below are documents
//!   the schema must refuse, each one a mistake a reader could plausibly make
//!   and each one chosen so that a specific piece of the schema is what
//!   catches it.
//!
//! What this does NOT claim: that schema-valid implies sonda-valid. The
//! parser enforces cross-field rules JSON Schema cannot express (id
//! uniqueness, `after.ref` resolvability, `delay:` requiring `while:`,
//! generator/pack mutual exclusion). The schema is an editor aid; `sonda`
//! is the validator.

// The generated schema is FEATURE-DEPENDENT, so these tests only mean
// anything on a build that can produce the full one.
//
// The delivery features do not merely add code — they add config shape. With
// `kafka` off, `SinkConfig` keeps a placeholder variant so `type: kafka`
// still deserializes into a "rebuild with the feature" error, but the
// variant carries no `tls:` or `sasl:` fields, and `KafkaTlsConfig` /
// `KafkaSaslConfig` / `OtlpSignalType` are absent from `$defs` entirely.
// A schema generated from a narrow build would therefore reject sink config
// that the released binary accepts (release.yml builds with
// `remote-write,kafka`; the Docker image adds `otlp`).
//
// So the committed schema is generated with `--all-features`, and the
// comparison below is compiled out of any build that could not have produced
// it. CI runs it in the `all-features` job.
#![cfg(all(
    feature = "schema",
    feature = "config",
    feature = "http",
    feature = "kafka",
    feature = "otlp",
    feature = "remote-write",
    feature = "gnmi"
))]

use std::path::{Path, PathBuf};

use jsonschema::Validator;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate has a parent directory")
        .to_path_buf()
}

fn validator() -> Validator {
    let schema = sonda_core::schema::scenario_file_schema().to_value();
    jsonschema::options()
        .build(&schema)
        .expect("the generated schema must itself be a valid JSON Schema")
}

/// Convert parsed YAML into the JSON value model the validator works on.
///
/// This is the same conversion an editor's YAML language server performs
/// before applying a JSON Schema, so doing it here keeps the test honest
/// about what a reader would actually experience.
fn yaml_to_json(value: serde_yaml_ng::Value) -> serde_json::Value {
    convert(value, false)
}

/// The key whose value is the one integer-keyed mapping a scenario file may
/// hold: the gnmi encoder's `enum: { 1: UP, 2: DOWN }` value map.
const INTEGER_KEYED: &str = "enum";

/// [`yaml_to_json`] for one value. `int_keys` is true only for the value of an
/// [`INTEGER_KEYED`] key, whose integer keys become their decimal text, as a
/// YAML language server renders them.
fn convert(value: serde_yaml_ng::Value, int_keys: bool) -> serde_json::Value {
    use serde_yaml_ng::Value as Y;
    match value {
        Y::Null => serde_json::Value::Null,
        Y::Bool(b) => serde_json::Value::Bool(b),
        Y::Number(n) => {
            // Prefer the integer rendering when YAML saw an integer: a
            // schema saying `"type": "integer"` (version, for one) would
            // reject 2.0.
            if let Some(i) = n.as_i64() {
                serde_json::Value::from(i)
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::from(u)
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            } else {
                serde_json::Value::Null
            }
        }
        Y::String(s) => serde_json::Value::String(s),
        Y::Sequence(items) => {
            serde_json::Value::Array(items.into_iter().map(|v| convert(v, false)).collect())
        }
        Y::Mapping(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    // Anywhere else a non-string key cannot appear in a
                    // scenario file, and rendering one as its debug form would
                    // silently produce a key no schema matches. Fail loudly.
                    let key = match k {
                        Y::String(s) => s,
                        Y::Number(n) if int_keys && (n.is_i64() || n.is_u64()) => n.to_string(),
                        other => panic!("scenario YAML must use string keys, found {other:?}"),
                    };
                    let child_int_keys = key == INTEGER_KEYED;
                    (key, convert(v, child_int_keys))
                })
                .collect(),
        ),
        Y::Tagged(tagged) => convert(tagged.value, int_keys),
    }
}

/// Every `.yaml`/`.yml` file under `dir`, recursively, sorted so a failure
/// names the same file on every machine.
fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(yaml_files(&path));
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        ) {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Every file the real parser accepts as a v2 scenario, across both roots.
///
/// The parser is the filter, deliberately. These directories also hold Compose
/// files, alert rules, Alertmanager config and deliberately-invalid fixtures —
/// YAML that was never meant to validate. Selecting by "does `parse` take it"
/// means the corpus is exactly the set the schema is claiming to describe, and
/// it grows on its own as scenarios are added.
///
/// `tests/fixtures/v2-examples/` is here because `examples/` alone cannot
/// cover the format. Every file in `examples/` is written in the canonical
/// `scenarios:` form, so the shorthand branch of the root `anyOf` — the one
/// that needed `flat_file_subschema` to describe at all — had no document
/// exercising it. `the_corpus_covers_every_root_branch` is what holds that
/// open now; this root is what lets it pass.
fn parser_accepted_scenarios() -> Vec<(PathBuf, String)> {
    let roots = [
        repo_root().join("examples"),
        repo_root().join("sonda-core/tests/fixtures/v2-examples"),
    ];
    let mut corpus = Vec::new();

    for root in roots {
        for path in yaml_files(&root) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if sonda_core::compiler::parse::detect_version(&text) != Some(2) {
                continue;
            }
            // `parse` runs on the interpolated text in the CLI. Files using
            // `${VAR}` would not parse here without the environment, so skip
            // those rather than assert on them. The `invalid-*` fixtures that
            // fail at a LATER compile phase do parse, and belong in the
            // corpus: the schema must accept anything the parser takes.
            if sonda_core::compiler::parse::parse(&text).is_ok() {
                corpus.push((path, text));
            }
        }
    }

    corpus
}

/// Every pack definition in the repo, selected by the real pack parser.
///
/// The scenario corpus above cannot reach these: a pack is not a scenario,
/// so `parse` refuses it and `parser_accepted_scenarios` filters it out.
/// That left the schema's third root branch — `MetricPackDef` — with no real
/// document behind it, checked only by the synthetic probes in
/// `the_corpus_covers_every_root_branch`. `packs/` is the corpus for it, and
/// it is the same directory `catalog::builtin` embeds, so every pack the
/// binary ships is a pack the schema has been held to.
fn parser_accepted_packs() -> Vec<(PathBuf, String)> {
    let mut corpus = Vec::new();
    for path in yaml_files(&repo_root().join("packs")) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if serde_yaml_ng::from_str::<sonda_core::packs::MetricPackDef>(&text).is_ok() {
            corpus.push((path, text));
        }
    }
    corpus
}

/// Guards `every_repo_pack_validates` against a renamed or emptied `packs/`.
///
/// The floor is the builtin count rather than a literal, so removing a pack
/// from the directory without removing it from the binary fails here instead
/// of shrinking the corpus in silence.
#[test]
fn the_pack_corpus_is_not_empty() {
    let corpus = parser_accepted_packs();
    assert!(
        corpus.len() >= sonda_core::catalog::builtin::PACK_COUNT,
        "expected at least the {} embedded packs under packs/, got {}",
        sonda_core::catalog::builtin::PACK_COUNT,
        corpus.len()
    );
}

/// The pack half of "the schema must not reject what the parser accepts".
#[test]
fn every_repo_pack_validates() {
    let validator = validator();
    let mut failures = Vec::new();

    for (path, text) in parser_accepted_packs() {
        let yaml: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{}: YAML did not load: {e}", path.display()));
                continue;
            }
        };
        let json = yaml_to_json(yaml);

        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| format!("    at {}: {e}", e.instance_path()))
            .collect();

        if !errors.is_empty() {
            failures.push(format!(
                "{}: the pack parser accepts this file but the schema rejects it:\n{}",
                path.display(),
                errors.join("\n")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the schema must not reject packs sonda accepts:\n{}",
        failures.join("\n")
    );
}

/// Every pack the binary embeds must be one of the files on disk that the
/// check above validated — otherwise the schema gate covers `packs/` while
/// the binary ships something else.
#[test]
fn every_embedded_pack_is_a_validated_file_from_the_packs_directory() {
    let corpus = parser_accepted_packs();
    for pack in sonda_core::catalog::builtin::PACKS {
        let matched = corpus
            .iter()
            .find(|(path, _)| path.file_name().and_then(|n| n.to_str()) == Some(pack.file));
        let (_, text) = matched.unwrap_or_else(|| {
            panic!(
                "embedded pack {} is not among the validated files under packs/",
                pack.file
            )
        });
        assert_eq!(
            text, pack.yaml,
            "{} on disk differs from the copy compiled into the binary",
            pack.file
        );
    }
}

#[test]
fn the_corpus_is_not_empty() {
    // Guards the check below against the failure mode where a path change
    // makes `parser_accepted_scenarios` return nothing and the real test
    // passes by iterating over zero files.
    let corpus = parser_accepted_scenarios();
    assert!(
        corpus.len() >= 20,
        "expected the corpus roots to yield a substantial scenario corpus, got {}",
        corpus.len()
    );
}

/// Every branch of the root `anyOf` must be exercised by a real document.
///
/// Counting files is not enough, and that gap was live: with `examples/` as
/// the only root, 0 of 65 documents matched the shorthand branch. A mutation
/// that replaced that branch entirely — making the schema reject every
/// single-signal shorthand file the parser accepts — left all nineteen tests
/// green, because no corpus document could tell the difference and the
/// freshness gate is cleared by regenerating, which is exactly what a
/// developer changing the schema does.
///
/// So this asserts coverage per SHAPE rather than per file. It is deliberately
/// a floor of one: the point is that no branch may go dark, not that the
/// corpus stay balanced.
#[test]
fn the_corpus_covers_every_root_branch() {
    let root = sonda_core::schema::scenario_file_schema().to_value();
    let branches = root["anyOf"]
        .as_array()
        .expect("the root is an anyOf")
        .clone();

    // Each branch is a `$ref` into `$defs`. Validating against the branch
    // alone would fail to resolve it, so each probe carries the whole `$defs`
    // section with it.
    let probes: Vec<(String, Validator)> = branches
        .iter()
        .map(|branch| {
            let name = branch["$ref"]
                .as_str()
                .expect("each branch is a $ref")
                .trim_start_matches("#/$defs/")
                .to_string();
            let mut doc = branch.clone();
            doc.as_object_mut()
                .expect("a $ref branch is an object")
                .insert("$defs".to_string(), root["$defs"].clone());
            let validator = jsonschema::options()
                .build(&doc)
                .expect("a single branch plus $defs is a valid schema");
            (name, validator)
        })
        .collect();

    // Both corpora, because the root `anyOf` describes both shapes: the
    // scenario roots cannot cover `MetricPackDef` (a pack is not a scenario,
    // so `parse` refuses it) and `packs/` cannot cover the other two.
    let mut corpus = parser_accepted_scenarios();
    corpus.extend(parser_accepted_packs());
    let mut hits: Vec<usize> = vec![0; probes.len()];

    for (_, text) in &corpus {
        let Ok(yaml) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
            continue;
        };
        let json = yaml_to_json(yaml);
        for (index, (_, validator)) in probes.iter().enumerate() {
            if validator.is_valid(&json) {
                hits[index] += 1;
            }
        }
    }

    let dark: Vec<&str> = probes
        .iter()
        .zip(&hits)
        .filter(|(_, &count)| count == 0)
        .map(|((name, _), _)| name.as_str())
        .collect();

    assert!(
        dark.is_empty(),
        "these root branches are matched by no document in the corpus, so nothing \
         would notice if they were wrong: {dark:?}. Coverage over {} documents was {}",
        corpus.len(),
        probes
            .iter()
            .zip(&hits)
            .map(|((name, _), count)| format!("{name}={count}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn every_repo_scenario_validates() {
    let validator = validator();
    let mut failures = Vec::new();

    for (path, text) in parser_accepted_scenarios() {
        let yaml: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{}: YAML did not load: {e}", path.display()));
                continue;
            }
        };
        let json = yaml_to_json(yaml);

        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| format!("    at {}: {e}", e.instance_path()))
            .collect();

        if !errors.is_empty() {
            failures.push(format!(
                "{}: the parser accepts this file but the schema rejects it:\n{}",
                path.display(),
                errors.join("\n")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the schema must not reject what sonda accepts \u{2014} regenerate it with \
         `task schema:generate` after changing a config type, and mirror any \
         hand-written Deserialize in its JsonSchema impl:\n\n{}",
        failures.join("\n\n")
    );
}

/// Assert the schema refuses `yaml`, and say which mistake was meant to be
/// caught when it does not.
fn assert_rejected(label: &str, yaml: &str) {
    let validator = validator();
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(yaml).expect("the negative case must be loadable YAML");
    let json = yaml_to_json(value);

    assert!(
        !validator.is_valid(&json),
        "the schema accepted a document it must reject ({label}). A schema that \
         accepts everything passes every_repo_scenario_validates too, which is \
         why this case exists.\n\n{yaml}"
    );
}

/// The canonical branch denies unknown fields. A misspelled top-level key is
/// the single most common authoring mistake a schema can catch, and the one
/// an editor shows most usefully.
#[test]
fn the_schema_rejects_a_misspelled_top_level_key() {
    assert_rejected(
        "`scenerios` for `scenarios`",
        r#"
version: 2
kind: runnable
scenerios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
"#,
    );
}

/// `Kind` is a two-variant lowercase enum. Nothing else is a v2 file.
#[test]
fn the_schema_rejects_an_unknown_kind() {
    assert_rejected(
        "kind: template",
        r#"
version: 2
kind: template
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
"#,
    );
}

/// This is the case a DERIVED `WhileOp` schema gets wrong in both directions:
/// it would reject `>` (the real wire value) and accept `GreaterThan`. The
/// hand-written impl exists for this, so the test asserts the half that a
/// wrong impl would let through.
#[test]
fn the_schema_rejects_the_while_op_variant_name() {
    assert_rejected(
        "op: GreaterThan instead of >",
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: GreaterThan
      value: 90.0
"#,
    );
}

/// Non-strict comparison operators are refused at deserialize time with a
/// pointed error. The schema says the same thing before the file is run.
#[test]
fn the_schema_rejects_a_non_strict_while_operator() {
    assert_rejected(
        "op: >=",
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">="
      value: 90.0
"#,
    );
}

/// `close_snap_to` is a field on the Rust struct and NOT a key the parser
/// accepts — it is where the extended `close:` mapping unpacks to. A derived
/// `DelayClause` schema would offer it, and the reader would write a key
/// `sonda run` then rejects. This is the case the hand-written impl exists
/// for.
#[test]
fn the_schema_rejects_the_delay_struct_fields_that_are_not_wire_keys() {
    assert_rejected(
        "delay.close_snap_to",
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 90.0
    delay:
      open: 5s
      close_snap_to: 1.0
"#,
    );
}

/// The extended `close:` mapping is closed too. Getting `oneOf` right on the
/// duration-or-mapping shape is easy to do by widening it into `true`, which
/// this catches.
#[test]
fn the_schema_rejects_an_unknown_key_inside_the_extended_close_mapping() {
    assert_rejected(
        "delay.close.snapto typo",
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 90.0
    delay:
      close:
        duration: 5s
        snapto: 1.0
"#,
    );
}

/// Generators are an internally-tagged enum. An unknown `type:` is a typo
/// worth underlining, and it exercises the largest `$defs` subtree in the
/// document.
#[test]
fn the_schema_rejects_an_unknown_generator_type() {
    assert_rejected(
        "generator.type: sinusoid",
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: sinusoid
      amplitude: 5.0
"#,
    );
}

/// A flap's `phase:` is a string only, in the schema and the parser alike: a
/// bare or quoted duration passes both, `"0s"` included, and a YAML number
/// (`0`, `30`) is refused by both. The generator branches do not deny unknown
/// keys, so the schema accepting a document says nothing until the flap
/// branch is shown to declare `phase` with exactly that type.
#[rstest::rstest]
#[case::bare("30s", true)]
#[case::quoted("\"30s\"", true)]
#[case::quoted_zero("\"0s\"", true)]
#[case::numeric_zero("0", false)]
#[case::numeric("30", false)]
fn the_schema_and_the_parser_agree_on_a_flap_phase(#[case] phase: &str, #[case] valid: bool) {
    let schema = sonda_core::schema::scenario_file_schema().to_value();
    let flap = schema["$defs"]["GeneratorConfig"]["oneOf"]
        .as_array()
        .expect("GeneratorConfig is a oneOf")
        .iter()
        .find(|branch| branch["properties"]["type"]["const"] == "flap")
        .expect("the schema has a flap branch");
    assert_eq!(
        flap["properties"]["phase"]["type"],
        serde_json::json!(["string", "null"]),
        "the flap branch declares `phase` as a string only: {flap}"
    );

    let yaml = format!(
        r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: ifOperStatus
    rate: 1
    duration: 10s
    generator:
      type: flap
      up_duration: 60s
      down_duration: 5s
      enum: oper_state
      phase: {phase}
"#
    );
    let parsed = sonda_core::compiler::parse::parse(&yaml);
    assert_eq!(
        parsed.is_ok(),
        valid,
        "the parser on `phase: {phase}`: {parsed:?}"
    );
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        let errors: Vec<String> = validator()
            .iter_errors(&yaml_to_json(value))
            .map(|e| format!("  at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "the schema rejected `phase: {phase}`:\n{}",
            errors.join("\n")
        );
    } else {
        assert_rejected(&format!("phase: {phase}"), &yaml);
    }
}

// ---------------------------------------------------------------------------
// Duration fields: a string with a unit, in the schema as in the parser
// ---------------------------------------------------------------------------

/// Everything sonda does to a scenario file before it emits, through the
/// functions the CLI calls: `sonda test`'s read of `expect:`, the compile,
/// the gated or ungated preparation `sonda run --dry-run` picks by the same
/// test, and the scheduler's read of each entry's `gap_windows:`, which no
/// earlier step reads.
fn sonda_takes(yaml: &str) -> Result<(), String> {
    use sonda_core::compiler::expand::InMemoryPackResolver;

    sonda_core::verify::parse_expectations(yaml).map_err(|e| format!("expect: {e}"))?;
    let compiled = sonda_core::compile_scenario_file_compiled(yaml, &InMemoryPackResolver::new())
        .map_err(|e| format!("compile: {e}"))?;
    let entries: Vec<sonda_core::config::ScenarioEntry> =
        if compiled.entries.iter().any(|e| e.while_clause.is_some()) {
            sonda_core::schedule::multi_runner::validate_compiled_gated(compiled)
                .map_err(|e| format!("gated: {e}"))?
                .into_iter()
                .map(|(_, entry)| entry)
                .collect()
        } else {
            let entries = sonda_core::compiler::prepare::prepare(compiled)
                .map_err(|e| format!("prepare: {e}"))?;
            sonda_core::prepare_entries_grouped(entries)
                .map_err(|e| format!("prepare_entries: {e}"))?
                .into_iter()
                .flatten()
                .map(|prepared| prepared.entry)
                .collect()
        };
    for entry in &entries {
        for window in entry.base().gap_windows.as_deref().unwrap_or_default() {
            window.resolve().map_err(|e| format!("gap_windows: {e}"))?;
        }
    }
    Ok(())
}

/// The schema's verdict on `yaml`, with its errors.
fn schema_takes(validator: &Validator, yaml: &str) -> Result<(), String> {
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("YAML: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(&yaml_to_json(value))
        .map(|e| format!("at {}: {e}", e.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// What a bare `0` is at a duration position.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Zero {
    /// Refused like any other bare number.
    Refused,
    /// An offset whose parser reads a YAML number as its text and takes `0`
    /// as zero.
    Taken,
}

/// One duration field and a scenario file that sets it.
struct DurationRow {
    /// The position, as `sonda_core::schema::duration_fields` names it.
    field: &'static str,
    /// A scenario file with `{v}` where the field's value goes.
    document: String,
    /// A value with a unit that the schema and sonda both take.
    control: &'static str,
    zero: Zero,
}

/// One entry, emitting for 10s, with `extra` (entry-level lines) after it.
fn entry_document(extra: &str) -> String {
    format!(
        "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    name: cpu\n    \
         rate: 1\n    duration: 10s\n    generator: {{ type: constant, value: 1.0 }}\n{extra}"
    )
}

/// One entry whose `generator:` block is `generator` (indented lines).
fn generator_document(duration: &str, generator: &str) -> String {
    format!(
        "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    name: cpu\n    \
         rate: 1\n    duration: {duration}\n    generator:\n{generator}"
    )
}

/// Two entries: `up`, and `cpu` gated on it by `while:` with `delay` (a flow
/// mapping) beside it.
fn gated_document(delay: &str) -> String {
    format!(
        "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    id: up\n    \
         name: up\n    rate: 1\n    duration: 10s\n    generator: {{ type: constant, value: 1.0 }}\n  \
         - signal_type: metrics\n    name: cpu\n    rate: 1\n    duration: 10s\n    \
         generator: {{ type: constant, value: 1.0 }}\n    while: {{ ref: up, op: \">\", value: 0.5 }}\n    \
         delay: {delay}\n"
    )
}

/// The single-signal shorthand, with `extra` (top-level lines).
fn flat_document(extra: &str) -> String {
    format!(
        "version: 2\nkind: runnable\nsignal_type: metrics\nname: cpu\nrate: 1\n\
         generator: {{ type: constant, value: 1.0 }}\n{extra}"
    )
}

/// Every duration field the published schema marks, with a document that
/// sets it. `every_duration_field_has_a_corpus_row` holds this list to the
/// schema's, so a field marked without a row, or a row whose field lost its
/// marker, fails.
fn duration_rows() -> Vec<DurationRow> {
    use Zero::{Refused, Taken};
    let row = |field, document: String, control, zero| DurationRow {
        field,
        document,
        control,
        zero,
    };
    let generator = |kind: &str, field: &str| {
        generator_document(
            "10s",
            &format!("      type: {kind}\n      {field}: {{v}}\n"),
        )
    };
    let sink = |lines: &str| entry_document(&format!("    sink:\n{lines}"));
    let http = |lines: &str| {
        sink(&format!(
            "      type: http_push\n      url: http://127.0.0.1:1/write\n{lines}"
        ))
    };
    let rows = vec![
        row("Entry.duration", generator_document("{v}", "      type: constant\n      value: 1.0\n"), "5s", Refused),
        row("Entry.phase_offset", entry_document("    phase_offset: {v}\n"), "5s", Refused),
        row("Entry.start_time", entry_document("    start_time: {v}\n"), "\"+5s\"", Refused),
        row(
            "Defaults.duration",
            "version: 2\nkind: runnable\ndefaults:\n  duration: {v}\nscenarios:\n  - signal_type: metrics\n    \
             name: cpu\n    rate: 1\n    generator: { type: constant, value: 1.0 }\n"
                .to_string(),
            "5s",
            Refused,
        ),
        row(
            "Defaults.start_time",
            "version: 2\nkind: runnable\ndefaults:\n  start_time: {v}\nscenarios:\n  - signal_type: metrics\n    \
             name: cpu\n    rate: 1\n    duration: 10s\n    generator: { type: constant, value: 1.0 }\n"
                .to_string(),
            "\"+5s\"",
            Refused,
        ),
        row("FlatFile.duration", flat_document("duration: {v}\n"), "5s", Refused),
        row("FlatFile.phase_offset", flat_document("duration: 10s\nphase_offset: {v}\n"), "5s", Refused),
        row("FlatFile.start_time", flat_document("duration: 10s\nstart_time: {v}\n"), "\"+5s\"", Refused),
        row(
            "AfterClause.delay",
            "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    id: saw\n    name: saw\n    \
             rate: 1\n    duration: 30s\n    generator: { type: sawtooth, min: 0, max: 10, period_secs: 10 }\n  \
             - signal_type: metrics\n    name: cpu\n    rate: 1\n    duration: 10s\n    \
             generator: { type: constant, value: 1.0 }\n    after: { ref: saw, op: \">\", value: 5, delay: {v} }\n"
                .to_string(),
            "5s",
            Refused,
        ),
        row("DelayClause.open", gated_document("{ open: {v} }"), "5s", Taken),
        row("DelayClause.close.0", gated_document("{ close: {v} }"), "5s", Refused),
        row("DelayClause.close.1.duration", gated_document("{ close: { duration: {v} } }"), "5s", Refused),
        row("GapConfig.every", entry_document("    gaps: { every: {v}, for: 1s }\n"), "5s", Refused),
        row("GapConfig.for", entry_document("    gaps: { every: 10s, for: {v} }\n"), "5s", Refused),
        row("BurstConfig.every", entry_document("    bursts: { every: {v}, for: 1s, multiplier: 2 }\n"), "5s", Refused),
        row("BurstConfig.for", entry_document("    bursts: { every: 10s, for: {v}, multiplier: 2 }\n"), "5s", Refused),
        row(
            "CardinalitySpikeConfig.every",
            entry_document("    cardinality_spikes:\n      - { label: pod, every: {v}, for: 1s, cardinality: 3 }\n"),
            "5s",
            Refused,
        ),
        row(
            "CardinalitySpikeConfig.for",
            entry_document("    cardinality_spikes:\n      - { label: pod, every: 10s, for: {v}, cardinality: 3 }\n"),
            "5s",
            Refused,
        ),
        row("GapWindowConfig.at", entry_document("    gap_windows:\n      - { at: {v}, for: 1s }\n"), "5s", Taken),
        row("GapWindowConfig.for", entry_document("    gap_windows:\n      - { at: 1s, for: {v} }\n"), "5s", Refused),
        row("GeneratorConfig.flap.up_duration", generator("flap", "up_duration"), "5s", Refused),
        row("GeneratorConfig.flap.down_duration", generator("flap", "down_duration"), "5s", Refused),
        row("GeneratorConfig.flap.phase", generator("flap", "phase"), "5s", Refused),
        row("GeneratorConfig.saturation.time_to_saturate", generator("saturation", "time_to_saturate"), "5s", Refused),
        row(
            "GeneratorConfig.leak.time_to_ceiling",
            generator_document("1s", "      type: leak\n      time_to_ceiling: {v}\n"),
            "5s",
            Refused,
        ),
        row("GeneratorConfig.degradation.time_to_degrade", generator("degradation", "time_to_degrade"), "5s", Refused),
        row("GeneratorConfig.steady.period", generator("steady", "period"), "5s", Refused),
        row("GeneratorConfig.spike_event.spike_duration", generator("spike_event", "spike_duration"), "5s", Refused),
        row("GeneratorConfig.spike_event.spike_interval", generator("spike_event", "spike_interval"), "5s", Refused),
        row(
            "RetryConfig.initial_backoff",
            http("      retry: { max_attempts: 2, initial_backoff: {v}, max_backoff: 10s }\n"),
            "5s",
            Refused,
        ),
        row(
            "RetryConfig.max_backoff",
            http("      retry: { max_attempts: 2, initial_backoff: 1ms, max_backoff: {v} }\n"),
            "5s",
            Refused,
        ),
        row("SinkConfig.http_push.max_buffer_age", http("      max_buffer_age: {v}\n"), "5s", Refused),
        row(
            "SinkConfig.remote_write.max_buffer_age",
            sink("      type: remote_write\n      url: http://127.0.0.1:1/write\n      max_buffer_age: {v}\n"),
            "5s",
            Refused,
        ),
        row(
            "SinkConfig.loki.max_buffer_age",
            sink("      type: loki\n      url: http://127.0.0.1:1\n      max_buffer_age: {v}\n"),
            "5s",
            Refused,
        ),
        row(
            "SinkConfig.kafka.max_buffer_age",
            sink("      type: kafka\n      brokers: 127.0.0.1:1\n      topic: t\n      max_buffer_age: {v}\n"),
            "5s",
            Refused,
        ),
        row(
            "SinkConfig.otlp_grpc.max_buffer_age",
            sink(
                "      type: otlp_grpc\n      endpoint: http://127.0.0.1:1\n      signal_type: metrics\n      \
                 max_buffer_age: {v}\n",
            ),
            "5s",
            Refused,
        ),
        row(
            "SinkConfig.gnmi_target.sample_floor",
            entry_document(
                "    encoder: { type: gnmi, path: /a/b }\n    sink:\n      type: gnmi_target\n      \
                 listen: 127.0.0.1:0\n      sample_floor: {v}\n",
            ),
            "5s",
            Refused,
        ),
        row(
            "AlertExpectation.firing_within",
            entry_document("expect:\n  alerts:\n    - alert: A\n      firing_within: {v}\n"),
            "5s",
            Refused,
        ),
        row(
            "AlertExpectation.resolves_within",
            entry_document("expect:\n  alerts:\n    - alert: A\n      firing_within: 1m\n      resolves_within: {v}\n"),
            "5s",
            Refused,
        ),
    ];
    #[cfg(feature = "snmp")]
    let rows = {
        let mut rows = rows;
        rows.push(row(
            "SinkConfig.snmp_trap.timeout",
            snmp_trap_document("timeout: {v}"),
            "5s",
            Refused,
        ));
        rows
    };
    rows
}

/// The rows cover exactly the fields the published schema marks as
/// durations, each once.
#[test]
fn every_duration_field_has_a_corpus_row() {
    let marked = sonda_core::schema::duration_fields();
    let mut rows: Vec<&str> = Vec::new();
    let all = duration_rows();
    for row in &all {
        assert!(!rows.contains(&row.field), "{} has two rows", row.field);
        rows.push(row.field);
    }
    rows.sort_unstable();
    assert_eq!(
        rows, marked,
        "the duration rows and the schema's duration fields differ"
    );
}

/// A duration field takes a string with a unit, in the schema as in the
/// parser: a bare `5` is refused by both, the row's control is taken by both,
/// and a bare `0` is refused by both, except at an offset whose parser reads
/// the number as its text (`Zero::Taken`), where both take it. The control is
/// asserted first, so a refusal of the number is the number's.
#[test]
fn the_schema_and_sonda_agree_on_every_duration_field() {
    let validator = validator();
    let mut disagreements = Vec::new();
    for row in duration_rows() {
        let cases = [
            (row.control, true),
            ("5", false),
            ("0", row.zero == Zero::Taken),
        ];
        for (value, taken) in cases {
            let yaml = row.document.replace("{v}", value);
            let schema = schema_takes(&validator, &yaml);
            let sonda = sonda_takes(&yaml);
            if schema.is_ok() != taken || sonda.is_ok() != taken {
                disagreements.push(format!(
                    "{}: `{value}` should be {} by both; schema: {schema:?}; sonda: {sonda:?}",
                    row.field,
                    if taken { "taken" } else { "refused" }
                ));
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "duration fields where the schema or sonda disagrees with the rule:\n{}",
        disagreements.join("\n")
    );
}

/// The documented residue of an offset (`OFFSET_COMMENT`): a YAML zero with a
/// minus sign or a radix prefix is the JSON number 0 to the schema, which
/// takes it, and text the duration parser refuses. Pinned so the residue is
/// what the comment says and no wider.
#[rstest::rstest]
#[case::minus_zero("-0")]
#[case::minus_zero_float("-0.0")]
#[case::hex_zero("0x0")]
#[case::octal_zero("0o0")]
fn a_signed_or_radix_zero_offset_is_taken_by_the_schema_and_refused_by_sonda(#[case] zero: &str) {
    let validator = validator();
    let offsets: Vec<DurationRow> = duration_rows()
        .into_iter()
        .filter(|row| row.zero == Zero::Taken)
        .collect();
    assert_eq!(offsets.len(), 2, "the two offsets have rows");
    for row in offsets {
        let yaml = row.document.replace("{v}", zero);
        assert!(
            schema_takes(&validator, &yaml).is_ok(),
            "{}: the schema takes `{zero}`",
            row.field
        );
        let refused = sonda_takes(&yaml);
        assert!(refused.is_err(), "{}: sonda refuses `{zero}`", row.field);
    }
}

/// The three-branch `anyOf` must not degenerate into "anything goes". A
/// document matching none of the shapes has to fail.
#[test]
fn the_schema_rejects_a_document_matching_no_top_level_shape() {
    assert_rejected(
        "an unrelated YAML document",
        r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: sonda
"#,
    );
}

/// The `close:` duration shorthand and the extended mapping are both valid,
/// and the negative cases above would also pass if the schema rejected
/// everything with a `delay:` block. This is the positive control for them.
#[test]
fn the_schema_accepts_both_delay_close_shapes() {
    let validator = validator();

    for (label, close) in [
        ("duration shorthand", "close: 5s"),
        (
            "extended mapping",
            "close:\n        duration: 5s\n        snap_to: 1.0\n        stale_marker: false",
        ),
    ] {
        let yaml = format!(
            r#"
version: 2
kind: runnable
scenarios:
  - signal_type: metrics
    name: cpu
    rate: 1
    duration: 10s
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 90.0
    delay:
      open: 250ms
      {close}
"#
        );
        let value: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&yaml).expect("positive control must load");
        let json = yaml_to_json(value);

        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| format!("  at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "the schema rejected a valid `delay.{label}`:\n{}",
            errors.join("\n")
        );
    }
}

/// A `gnmi` encoder block with every field, including the integer-keyed
/// `enum:` value map, on a `gnmi_target` sink with every field. No file in
/// the corpus roots uses either, so this is the positive control for both.
#[test]
fn the_schema_accepts_a_gnmi_encoder() {
    const YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    origin: openconfig-interfaces
    target_label: device
    path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
    paths:
      oper_status: "/interfaces/interface[name={ifName}]/state/oper-status"
    values:
      in_octets: uint
      oper_status:
        enum: { 1: UP, 2: DOWN }
    drop_labels: [job]
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400
    max_subscribers: 4
    subscriber_buffer: 256
    sample_floor: 500ms
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
    labels:
      device: rtr-1
      ifName: Gi0/0/0
      job: edge
"#;
    // The parser accepts it, so the schema must too.
    sonda_core::compiler::parse::parse(YAML).expect("the parser accepts the gnmi encoder");

    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(YAML).expect("loads");
    let json = yaml_to_json(value);
    let errors: Vec<String> = validator()
        .iter_errors(&json)
        .map(|e| format!("  at {}: {e}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "the schema rejected a valid gnmi encoder:\n{}",
        errors.join("\n")
    );
}

/// `values:` entries are a closed set of names or an `enum:` map. `float` is
/// the plausible mistake (it is a deprecated gNMI `TypedValue` field).
#[test]
fn the_schema_rejects_an_unknown_gnmi_value_type() {
    assert_rejected(
        "values.in_octets: float",
        r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/a/{name}"
    values:
      in_octets: float
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
"#,
    );
}

/// `enum:` keys are integers. A name in key position is the mistake of
/// writing the map the wrong way round.
#[test]
fn the_schema_rejects_a_gnmi_enum_map_with_non_integer_keys() {
    assert_rejected(
        "enum: { UP: 1 }",
        r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/a/{name}"
    values:
      oper_status:
        enum: { UP: 1 }
scenarios:
  - signal_type: metrics
    name: oper_status
    generator:
      type: constant
      value: 1.0
"#,
    );
}

/// The sink block denies unknown fields. `listen_addr` is the plausible
/// misspelling, and without the rejection it would fall back to no listen
/// address at all.
#[test]
fn the_schema_rejects_a_misspelled_gnmi_target_key() {
    const YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/a/{name}"
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400
    listen_addr: 0.0.0.0:57400
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
"#;
    // The parser refuses it too, so the schema is not stricter than sonda.
    sonda_core::compiler::parse::parse(YAML).expect_err("the parser refuses `listen_addr`");
    assert_rejected("`listen_addr` beside `listen`", YAML);
}

/// The schema accepts any scalar for the sink's text fields, so the parser
/// must too: a numeric `listen` is a validation error naming the field, not
/// a parse error the schema did not predict. `sample_floor` is a duration, a
/// string only (`the_schema_and_sonda_agree_on_every_duration_field`).
#[test]
fn the_parser_takes_every_scalar_the_schema_takes_for_gnmi_target_text_fields() {
    const YAML: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/a/{name}"
  sink:
    type: gnmi_target
    listen: 57400
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
"#;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(YAML).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "the premise: the schema accepts numeric text fields"
    );
    sonda_core::compiler::parse::parse(YAML).expect("the parser accepts what the schema accepts");
}

/// A pack whose one metric carries the gnmi hint `hint`, one line per key.
fn hinted_pack_document(hint: &str) -> String {
    format!(
        "version: 2
kind: composable
name: hinted
description: one hinted metric
category: network
metrics:
  - name: in_octets
    gnmi:
{hint}
"
    )
}

/// A pack's `gnmi:` hint denies unknown keys, in the schema and the parser.
#[test]
fn the_schema_rejects_a_misspelled_gnmi_hint_key() {
    let yaml = hinted_pack_document("      path: /a\n      vaule: uint");
    serde_yaml_ng::from_str::<sonda_core::packs::MetricPackDef>(&yaml)
        .expect_err("the parser refuses `vaule`");
    assert_rejected("`vaule` for `value` in a gnmi hint", &yaml);
}

/// The schema accepts any scalar for a hint's `path`, so the parser must
/// too: a numeric path is a validation error, not a parse error the schema
/// did not predict.
#[test]
fn the_parser_takes_every_scalar_the_schema_takes_for_a_gnmi_hint_path() {
    let yaml = hinted_pack_document("      path: 200\n      value: uint");
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "the premise: the schema accepts a numeric hint path"
    );
    let pack = serde_yaml_ng::from_str::<sonda_core::packs::MetricPackDef>(&yaml)
        .expect("the parser accepts what the schema accepts");
    let hint = pack.metrics[0].gnmi.as_ref().expect("the hint");
    assert_eq!(hint.path, "200");
}

/// A `gnmi_target` sink whose block carries `extra`, one line under `listen`.
fn gnmi_target_document(extra: &str) -> String {
    format!(
        r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/a/{{name}}"
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400
    {extra}
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
"#
    )
}

/// The schema bounds the sink's numeric settings where validation does, so
/// an editor flags the values `sonda` refuses, and accepts the bounds.
#[rstest::rstest]
#[case::max_subscribers_zero("max_subscribers: 0", Some(0), None, false)]
#[case::max_subscribers_one("max_subscribers: 1", Some(1), None, true)]
#[case::subscriber_buffer_zero("subscriber_buffer: 0", None, Some(0), false)]
#[case::subscriber_buffer_at_the_cap("subscriber_buffer: 65536", None, Some(65_536), true)]
#[case::subscriber_buffer_over_the_cap("subscriber_buffer: 65537", None, Some(65_537), false)]
fn the_schema_bounds_gnmi_target_settings_like_validation(
    #[case] extra: &str,
    #[case] max_subscribers: Option<usize>,
    #[case] subscriber_buffer: Option<usize>,
    #[case] valid: bool,
) {
    use sonda_core::sink::gnmi::GnmiTargetConfig;
    use sonda_core::sink::SinkConfig;

    let cfg = GnmiTargetConfig {
        max_subscribers,
        subscriber_buffer,
        ..GnmiTargetConfig::new("0.0.0.0:57400")
    };
    let validated =
        sonda_core::config::validate::validate_sink_config(&SinkConfig::GnmiTarget(cfg));
    assert_eq!(
        validated.is_ok(),
        valid,
        "validation of `{extra}`: {validated:?}"
    );

    let yaml = gnmi_target_document(extra);
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        assert!(
            validator().is_valid(&yaml_to_json(value)),
            "the schema refuses `{extra}`"
        );
    } else {
        assert_rejected(extra, &yaml);
    }
}

/// An `snmp_agent` sink whose block carries `extra`, one line under `listen`.
#[cfg(feature = "snmp")]
fn snmp_agent_document(extra: &str) -> String {
    format!(
        r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: snmp
    oids:
      ifHCInOctets: "1.3.6.1.2.1.31.1.1.1.6.{{ifIndex}}"
  sink:
    type: snmp_agent
    listen: 0.0.0.0:1161
    {extra}
scenarios:
  - signal_type: metrics
    name: ifHCInOctets
    labels:
      ifIndex: "1"
    generator:
      type: constant
      value: 1.0
"#
    )
}

/// An snmp scenario whose `ifOperStatus` value type is `{TYPE}`, which each
/// case replaces. Everything else is valid, so a refusal can only come from
/// the replacement.
#[cfg(feature = "snmp")]
const SNMP_WITH_TYPE: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: snmp
    oids:
      ifOperStatus: "1.3.6.1.2.1.2.2.1.8.{ifIndex}"
    types:
      ifOperStatus: {TYPE}
  sink:
    type: snmp_agent
    listen: 0.0.0.0:1161
scenarios:
  - signal_type: metrics
    name: ifOperStatus
    labels:
      ifIndex: "1"
    generator:
      type: constant
      value: 1.0
"#;

/// The schema and the parser agree on a `types:` entry: the value type
/// names and the `enum:` map with integer keys, bare or quoted, are
/// accepted by both; a map with a non-integer key, an empty map, a key
/// beside `enum` and an unknown name are refused by both.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::opaque_float("opaque_float", true)]
#[case::opaque_double("opaque_double", true)]
#[case::ipaddress("ipaddress", true)]
#[case::string("string", true)]
#[case::enum_map("{ enum: { 1: up, 2: down } }", true)]
#[case::enum_map_string_key("{ enum: { \"1\": up } }", true)]
#[case::enum_map_non_integer_key("{ enum: { up: 1 } }", false)]
#[case::enum_map_empty("{ enum: {} }", false)]
#[case::enum_map_extra_key("{ enum: { 1: up }, extra: 1 }", false)]
#[case::unknown_name("float", false)]
fn the_schema_and_the_parser_agree_on_snmp_value_types(#[case] ty: &str, #[case] valid: bool) {
    let yaml = SNMP_WITH_TYPE.replace("{TYPE}", ty);
    let parsed = sonda_core::compiler::parse::parse(&yaml);
    assert_eq!(parsed.is_ok(), valid, "the parser: {parsed:?}");
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        let json = yaml_to_json(value);
        let errors: Vec<String> = validator()
            .iter_errors(&json)
            .map(|e| format!("  at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "the schema rejected `{ty}`:\n{}",
            errors.join("\n")
        );
    } else {
        assert_rejected(ty, &yaml);
    }
}

/// The `snmp_agent` block and its `system:` block deny unknown keys, in the
/// schema and the parser.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::sink_key("comunity: public")]
#[case::system_key("system: {nmae: rtr-1}")]
fn the_schema_rejects_a_misspelled_snmp_agent_key(#[case] extra: &str) {
    let yaml = snmp_agent_document(extra);
    sonda_core::compiler::parse::parse(&yaml).expect_err("the parser refuses the key");
    assert_rejected(extra, &yaml);
}

/// The schema accepts any scalar for the sink's text fields, so the parser
/// must too: a numeric `community` or system name is read as its text.
#[cfg(feature = "snmp")]
#[test]
fn the_parser_takes_every_scalar_the_schema_takes_for_snmp_agent_text_fields() {
    let yaml = snmp_agent_document(
        "community: 12345
    system: {name: 42, location: 7}",
    );
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "the premise: the schema accepts numeric text fields"
    );
    sonda_core::compiler::parse::parse(&yaml).expect("the parser accepts what the schema accepts");
}

/// An `snmp_trap` sink whose block carries `extra`, one line under
/// `destination`.
#[cfg(feature = "snmp")]
fn snmp_trap_document(extra: &str) -> String {
    format!(
        r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: snmp
    oids:
      ifOperStatus: "1.3.6.1.2.1.2.2.1.8.{{ifIndex}}"
    types:
      ifOperStatus: integer
  sink:
    type: snmp_trap
    destination: 127.0.0.1:1162
    {extra}
scenarios:
  - signal_type: metrics
    name: ifOperStatus
    labels:
      ifIndex: "1"
    generator:
      type: constant
      value: 1.0
"#
    )
}

/// The `snmp_trap` block denies unknown keys, in the schema and the parser.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::retries("retires: 2")]
#[case::inform("infrom: true")]
fn the_schema_rejects_a_misspelled_snmp_trap_key(#[case] extra: &str) {
    let good = snmp_trap_document("retries: 2");
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&good).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "guard: the schema takes the block spelt right"
    );
    sonda_core::compiler::parse::parse(&good).expect("guard: so does the parser");
    let yaml = snmp_trap_document(extra);
    sonda_core::compiler::parse::parse(&yaml).expect_err("the parser refuses the key");
    assert_rejected(extra, &yaml);
}

/// The schema accepts any scalar for the trap sink's text fields, so the
/// parser must too: a numeric `community` is read as its text, which
/// validation then judges. `timeout` is a duration, a string only
/// (`the_schema_and_sonda_agree_on_every_duration_field`).
#[cfg(feature = "snmp")]
#[test]
fn the_parser_takes_every_scalar_the_schema_takes_for_snmp_trap_text_fields() {
    let yaml = snmp_trap_document("community: 12345");
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "the premise: the schema accepts numeric text fields"
    );
    sonda_core::compiler::parse::parse(&yaml).expect("the parser accepts what the schema accepts");
}

/// The schema bounds `retries` where validation does, and accepts the
/// bounds.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::retries_zero(0, true)]
#[case::retries_at_the_maximum(10, true)]
#[case::retries_above_the_maximum(11, false)]
fn the_schema_bounds_snmp_trap_retries_like_validation(#[case] retries: u8, #[case] valid: bool) {
    use sonda_core::sink::snmp::SnmpTrapConfig;
    use sonda_core::sink::SinkConfig;

    let cfg = SnmpTrapConfig {
        retries,
        ..SnmpTrapConfig::new("127.0.0.1:1162")
    };
    let validated = sonda_core::config::validate::validate_sink_config(&SinkConfig::SnmpTrap(cfg));
    assert_eq!(
        validated.is_ok(),
        valid,
        "validation of {retries}: {validated:?}"
    );

    let extra = format!("retries: {retries}");
    let yaml = snmp_trap_document(&extra);
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        assert!(
            validator().is_valid(&yaml_to_json(value)),
            "the schema refuses `{extra}`"
        );
    } else {
        assert_rejected(&extra, &yaml);
    }
}

/// `system.object_id` is an OID and not widened: an unquoted number is
/// refused by both, and a dotted OID string is accepted by both.
#[cfg(feature = "snmp")]
#[test]
fn the_schema_and_the_parser_refuse_an_unquoted_numeric_object_id() {
    let numeric = snmp_agent_document("system: {object_id: 1.30}");
    sonda_core::compiler::parse::parse(&numeric).expect_err("the parser refuses a number");
    assert_rejected("a numeric system.object_id", &numeric);

    let dotted = snmp_agent_document("system: {object_id: 1.3.6.1.4.1.8072.3.2.10}");
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&dotted).expect("loads");
    assert!(
        validator().is_valid(&yaml_to_json(value)),
        "the schema takes a dotted OID"
    );
    sonda_core::compiler::parse::parse(&dotted).expect("the parser takes a dotted OID");
}

/// Where a label column is written: the snmp encoder's `label_columns:`, or
/// a pack's `snmp_label_columns:`.
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, Copy)]
enum ColumnMap {
    Encoder,
    Pack,
}

/// A document with the one label column `ifType: <column>` in `map`, and
/// what the parser makes of it.
#[cfg(feature = "snmp")]
fn label_column_document(map: ColumnMap, column: &str) -> (String, Result<(), String>) {
    match map {
        ColumnMap::Encoder => {
            let yaml = format!(
                r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: snmp
    oids:
      ifInOctets: "1.3.6.1.2.1.2.2.1.10.{{ifIndex}}"
    label_columns:
      ifType: {column}
  sink:
    type: snmp_agent
    listen: 0.0.0.0:1161
scenarios:
  - signal_type: metrics
    name: ifInOctets
    labels:
      ifIndex: "1"
      ifType: "6"
    generator:
      type: constant
      value: 1.0
"#
            );
            let parsed = sonda_core::compiler::parse::parse(&yaml)
                .map(|_| ())
                .map_err(|e| e.to_string());
            (yaml, parsed)
        }
        ColumnMap::Pack => {
            let yaml = format!(
                "version: 2
kind: composable
name: typed
description: one typed column
category: network
snmp_label_columns:
  ifType: {column}
metrics:
  - name: ifInOctets
"
            );
            let parsed = serde_yaml_ng::from_str::<sonda_core::packs::MetricPackDef>(&yaml)
                .map(|_| ())
                .map_err(|e| e.to_string());
            (yaml, parsed)
        }
    }
}

/// The schema and the parser agree on a label column in both maps: a
/// template string and the `{ oid, type }` object are accepted by both; an
/// object without `oid:` or `type:`, with a key beside the two (which only
/// the schema's `additionalProperties` and the parser's unknown-key refusal
/// catch: `required` holds), with an unquoted numeric template or an
/// unknown type, and a bare unquoted number, are refused by both.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::template_string(r#""1.3.6.1.2.1.2.2.1.3.{ifIndex}""#, true)]
#[case::typed_integer(r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", type: integer }"#, true)]
#[case::typed_string(r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", type: string }"#, true)]
#[case::object_missing_oid("{ type: integer }", false)]
#[case::object_missing_type(r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}" }"#, false)]
#[case::object_misspelled_key(r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", typ: integer }"#, false)]
#[case::object_extra_key(
    r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", type: integer, extra: 1 }"#,
    false
)]
#[case::object_numeric_oid("{ oid: 1.30, type: integer }", false)]
#[case::object_unknown_type(r#"{ oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", type: float }"#, false)]
#[case::numeric_template("1.30", false)]
fn the_schema_and_the_parser_agree_on_label_columns(
    #[values(ColumnMap::Encoder, ColumnMap::Pack)] map: ColumnMap,
    #[case] column: &str,
    #[case] valid: bool,
) {
    let (yaml, parsed) = label_column_document(map, column);
    assert_eq!(parsed.is_ok(), valid, "the parser, {map:?}: {parsed:?}");
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        let errors: Vec<String> = validator()
            .iter_errors(&yaml_to_json(value))
            .map(|e| format!("  at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "the schema rejected `{column}` in {map:?}:\n{}",
            errors.join("\n")
        );
    } else {
        assert_rejected(column, &yaml);
    }
}

/// The schema bounds the sink's numeric settings where validation does, so
/// an editor flags the values `sonda` refuses, and accepts the bounds.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::message_size_below_the_minimum("max_message_size: 483", 483, 72, false)]
#[case::message_size_at_the_minimum("max_message_size: 484", 484, 72, true)]
#[case::message_size_at_the_maximum("max_message_size: 65507", 65_507, 72, true)]
#[case::message_size_above_the_maximum("max_message_size: 65508", 65_508, 72, false)]
#[case::services_at_the_maximum("system: {services: 127}", 1472, 127, true)]
#[case::services_above_the_maximum("system: {services: 128}", 1472, 128, false)]
fn the_schema_bounds_snmp_agent_settings_like_validation(
    #[case] extra: &str,
    #[case] max_message_size: usize,
    #[case] services: u8,
    #[case] valid: bool,
) {
    use sonda_core::sink::snmp::{SnmpAgentConfig, SystemGroup};
    use sonda_core::sink::SinkConfig;

    let cfg = SnmpAgentConfig {
        max_message_size,
        system: SystemGroup {
            services,
            ..SystemGroup::default()
        },
        ..SnmpAgentConfig::new("0.0.0.0:1161")
    };
    let validated = sonda_core::config::validate::validate_sink_config(&SinkConfig::SnmpAgent(cfg));
    assert_eq!(
        validated.is_ok(),
        valid,
        "validation of `{extra}`: {validated:?}"
    );

    let yaml = snmp_agent_document(extra);
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        assert!(
            validator().is_valid(&yaml_to_json(value)),
            "the schema refuses `{extra}`"
        );
    } else {
        assert_rejected(extra, &yaml);
    }
}

/// The schema and the parser agree on the SNMPv3 settings: both take every
/// protocol name, `sha` included, a user without auth, a `priv` block and a
/// hex engine ID; both refuse a misspelt user key, a numeric password and a
/// numeric engine ID.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::sha(
    "versions: [v3]\n    users: [{name: t, auth: {protocol: sha, password: lab-password}}]",
    true
)]
#[case::sha512(
    "versions: [v3]\n    users: [{name: t, auth: {protocol: sha512, password: lab-password}}]",
    true
)]
#[case::no_auth("versions: [v3]\n    users: [{name: watcher}]", true)]
#[case::engine_id(
    "versions: [v3]\n    users: [{name: t}]\n    engine_id: \"80001f8804616161\"",
    true
)]
#[case::priv_block("versions: [v3]\n    users: [{name: t, auth: {protocol: md5, password: lab-password}, priv: {protocol: aes, password: lab-password}}]", true)]
#[case::misspelt_user_key("versions: [v3]\n    users: [{nmae: t}]", false)]
#[case::numeric_password(
    "versions: [v3]\n    users: [{name: t, auth: {protocol: md5, password: 123456789}}]",
    false
)]
#[case::numeric_engine_id(
    "versions: [v3]\n    users: [{name: t}]\n    engine_id: 8000000002",
    false
)]
#[case::unknown_protocol(
    "versions: [v3]\n    users: [{name: t, auth: {protocol: sha999, password: lab-password}}]",
    false
)]
fn the_schema_and_the_parser_agree_on_snmp_v3_settings(#[case] extra: &str, #[case] valid: bool) {
    let yaml = snmp_agent_document(extra);
    let parsed = sonda_core::compiler::parse::parse(&yaml);
    assert_eq!(parsed.is_ok(), valid, "the parser on `{extra}`: {parsed:?}");
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        assert!(
            validator().is_valid(&yaml_to_json(value)),
            "the schema refuses `{extra}`"
        );
    } else {
        assert_rejected(extra, &yaml);
    }
}

/// The schema bounds the SNMPv3 settings validation bounds, so an editor
/// flags what `sonda` refuses and accepts what it takes: the engine ID's
/// exact pattern (5 to 32 octets of hex, `0x` allowed), a user name of 1 to
/// 32 characters, and both passwords of at least 8. Validation runs on the
/// same block, parsed.
#[cfg(feature = "snmp")]
#[rstest::rstest]
#[case::engine_id_of_5_with_0x("engine_id: \"0x80001f8804\"", true)]
#[case::engine_id_of_5_bare("engine_id: \"80001F8804\"", true)]
#[case::engine_id_of_32(
    "engine_id: \"abababababababababababababababababababababababababababababababab\"",
    true
)]
#[case::engine_id_of_4("engine_id: \"80001f88\"", false)]
#[case::engine_id_of_33(
    "engine_id: \"ababababababababababababababababababababababababababababababababab\"",
    false
)]
#[case::engine_id_of_odd_digits("engine_id: \"80001f8804a\"", false)]
#[case::engine_id_not_hex("engine_id: \"80001f88zz\"", false)]
#[case::name_of_32("users: [{name: uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu}]", true)]
#[case::name_of_33("users: [{name: uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu}]", false)]
#[case::empty_name("users: [{name: \"\"}]", false)]
#[case::auth_password_of_8(
    "users: [{name: t, auth: {protocol: sha256, password: \"12345678\"}}]",
    true
)]
#[case::auth_password_of_7(
    "users: [{name: t, auth: {protocol: sha256, password: \"1234567\"}}]",
    false
)]
#[case::priv_password_of_8("users: [{name: t, auth: {protocol: sha256, password: lab-password}, priv: {protocol: des, password: \"12345678\"}}]", true)]
#[case::priv_password_of_7("users: [{name: t, auth: {protocol: sha256, password: lab-password}, priv: {protocol: des, password: \"1234567\"}}]", false)]
fn the_schema_bounds_snmp_v3_settings_like_validation(#[case] extra: &str, #[case] valid: bool) {
    use sonda_core::sink::SinkConfig;

    let users = if extra.starts_with("users") {
        String::new()
    } else {
        "users: [{name: t}]\n".to_string()
    };
    let block = format!("type: snmp_agent\nlisten: 0.0.0.0:1161\nversions: [v3]\n{users}{extra}\n");
    let sink: SinkConfig = serde_yaml_ng::from_str(&block).expect("the block parses");
    let validated = sonda_core::config::validate::validate_sink_config(&sink);
    assert_eq!(
        validated.is_ok(),
        valid,
        "validation of `{extra}`: {validated:?}"
    );

    let yaml = snmp_agent_document(&format!(
        "versions: [v3]\n    {}{extra}",
        users.replace('\n', "\n    ")
    ));
    if valid {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).expect("loads");
        assert!(
            validator().is_valid(&yaml_to_json(value)),
            "the schema refuses `{extra}`"
        );
    } else {
        assert_rejected(extra, &yaml);
    }
}

/// The committed schema is a build output. This is the same comparison
/// `task schema:check` makes, run inside `cargo test` so a contributor who
/// never runs the task still learns before CI does.
#[test]
fn the_committed_schema_is_current() {
    let path = repo_root().join("docs/site/docs/schema/sonda-scenario.schema.json");
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} must exist \u{2014} generate it with `task schema:generate`: {e}",
            path.display()
        )
    });

    assert_eq!(
        committed,
        sonda_core::schema::scenario_file_schema_json(),
        "the committed schema is behind the config types \u{2014} run `task schema:generate` \
         and commit the result"
    );
}

// ---------------------------------------------------------------------------
// Extension branches (W4 phase 1c)
// ---------------------------------------------------------------------------

/// The positive control for the two negatives below.
///
/// No pack in `packs/` uses `deviations:` (`if_mib` uses `extends:` alone),
/// so `every_repo_pack_validates` says nothing about that branch — a schema
/// that got it wrong would still be green. This drives both directly.
#[test]
fn the_schema_accepts_an_extension_with_both_deviation_forms() {
    let validator = validator();
    const YAML: &str = r#"
version: 2
kind: composable
name: cisco_iosxe_interface
description: IOS-XE interface metrics
category: network
extends: telegraf_snmp_interface
shared_labels:
  platform: iosxe
metrics:
  - name: ifInDiscards
    generator:
      type: step
      start: 0.0
      step_size: 0.05
deviations:
  - metric: ifOperStatus
    replace:
      generator:
        type: constant
        value: 2.0
      labels:
        note: replaced
  - metric: ifHCOutOctets
    not_supported: true
"#;
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(YAML).expect("positive control must load");
    let json = yaml_to_json(value);
    let errors: Vec<String> = validator
        .iter_errors(&json)
        .map(|e| format!("  at {}: {e}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "the schema rejected a valid extension:\n{}",
        errors.join("\n")
    );
}

/// A pack with no `metrics:` at all is valid — that is how a pure-deviation
/// extension is written — so the schema must not require the key. The
/// compiler is what refuses a *resolved* pack with no metrics.
#[test]
fn the_schema_accepts_an_extension_that_only_deviates() {
    let validator = validator();
    const YAML: &str = r#"
version: 2
kind: composable
name: stripped_down
description: removes one metric and adds nothing
category: network
extends: telegraf_snmp_interface
deviations:
  - metric: ifInErrors
    not_supported: true
"#;
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(YAML).expect("positive control must load");
    assert!(
        validator.is_valid(&yaml_to_json(value)),
        "a pure-deviation extension must validate"
    );
}

#[test]
fn the_schema_rejects_a_deviation_without_a_metric_selector() {
    assert_rejected(
        "`deviations` entry with no `metric:`",
        r#"
version: 2
kind: composable
name: p
description: d
category: network
extends: base
deviations:
  - not_supported: true
"#,
    );
}

/// `extends:` takes one pack, not a list. A list is what someone writing
/// multiple inheritance would try first, so the schema should say no rather
/// than let it reach a confusing parse error.
#[test]
fn the_schema_rejects_a_list_of_bases() {
    assert_rejected(
        "`extends:` given a sequence",
        r#"
version: 2
kind: composable
name: p
description: d
category: network
extends:
  - base_one
  - base_two
"#,
    );
}

/// A pack declaring neither `metrics:` nor `extends:` is refused.
///
/// This is the half `metrics`-leaving-`required` gave up, restored as the
/// disjunction the compiler actually enforces. Without it, a plain pack with
/// `metricss:` typed by hand validates and fails later as an empty pack.
#[test]
fn the_schema_rejects_a_pack_with_neither_metrics_nor_extends() {
    assert_rejected(
        "`metricss` for `metrics` on a plain pack",
        r#"
version: 2
kind: composable
name: p
description: d
category: network
metricss:
  - name: a
    generator:
      type: constant
      value: 1.0
"#,
    );
}

/// The positive control: both branches of that disjunction still validate,
/// so the rule discriminates rather than refusing packs generally.
#[test]
fn the_schema_accepts_a_pack_with_either_metrics_or_extends() {
    let validator = validator();
    for (label, body) in [
        (
            "metrics, no extends",
            "metrics:\n  - name: a\n    generator:\n      type: constant\n      value: 1.0\n",
        ),
        ("extends, no metrics", "extends: base\n"),
    ] {
        let yaml = format!(
            "version: 2\nkind: composable\nname: p\ndescription: d\ncategory: network\n{body}"
        );
        let value: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&yaml).expect("positive control must load");
        assert!(
            validator.is_valid(&yaml_to_json(value)),
            "the schema rejected a valid pack ({label}):\n{yaml}"
        );
    }
}

/// Integer keys are text only under `enum:`; the positive half proves the
/// conversion runs, so the panic below cannot come from a broken fixture.
#[test]
fn integer_keys_convert_under_enum() {
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str("values:\n  m:\n    enum: { 1: UP, 2: DOWN }\n").unwrap();
    let json = yaml_to_json(yaml);
    assert_eq!(json["values"]["m"]["enum"]["1"], "UP");
    assert_eq!(json["values"]["m"]["enum"]["2"], "DOWN");
}

#[test]
#[should_panic(expected = "scenario YAML must use string keys")]
fn integer_keys_outside_enum_still_panic() {
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str("labels: { 200: ok }\n").unwrap();
    yaml_to_json(yaml);
}

/// A gnmi scenario whose encoder block ends with `{EXTRA}`, which each case
/// below replaces. Everything else is valid, so a rejection can only come
/// from the replacement.
const GNMI_WITH_EXTRA: &str = r#"
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: gnmi
    path: "/interfaces/interface[name={ifName}]/{name}"
    {EXTRA}
scenarios:
  - signal_type: metrics
    name: in_octets
    generator:
      type: constant
      value: 1.0
    labels:
      ifName: Gi0/0/0
      job: edge
"#;

fn schema_errors(yaml: &str) -> Vec<String> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).expect("loads");
    let json = yaml_to_json(value);
    validator()
        .iter_errors(&json)
        .map(|e| format!("  at {}: {e}", e.instance_path()))
        .collect()
}

/// `drop_labels` is a list of label names. The positive control proves the
/// surrounding document is valid, so the negatives fail on the field alone.
#[test]
fn the_schema_checks_gnmi_drop_labels() {
    let valid = GNMI_WITH_EXTRA.replace("{EXTRA}", "drop_labels: [job]");
    sonda_core::compiler::parse::parse(&valid).expect("the parser accepts drop_labels");
    let errors = schema_errors(&valid);
    assert!(
        errors.is_empty(),
        "the schema rejected a valid drop_labels list:\n{}",
        errors.join("\n")
    );

    assert_rejected(
        "drop_labels as a string, not a list",
        &GNMI_WITH_EXTRA.replace("{EXTRA}", "drop_labels: job"),
    );
    assert_rejected(
        "`drop_label` for `drop_labels`",
        &GNMI_WITH_EXTRA.replace("{EXTRA}", "drop_label: [job]"),
    );
}

/// Quoted enum codes are what JSON clients send. The schema has always
/// accepted them; the parser must too, so the two agree.
#[test]
fn the_parser_and_schema_accept_quoted_gnmi_enum_codes() {
    let yaml = GNMI_WITH_EXTRA.replace(
        "{EXTRA}",
        "values: { in_octets: { enum: { \"1\": UP, \"2\": DOWN } } }",
    );
    sonda_core::compiler::parse::parse(&yaml).expect("the parser accepts quoted enum codes");
    let errors = schema_errors(&yaml);
    assert!(
        errors.is_empty(),
        "the schema rejected quoted enum codes:\n{}",
        errors.join("\n")
    );
}

/// The schema accepts a number or boolean wherever it accepts a string, so an
/// enum label written as `false` or `10` must parse too. It is stored as that
/// scalar's text.
#[rustfmt::skip]
#[rstest::rstest]
#[case::booleans("{ 0: false, 1: true }", &[(0, "false"), (1, "true")])]
#[case::integers("{ 1: 10, 2: 100 }",     &[(1, "10"), (2, "100")])]
#[case::mixed(   "{ 0: false, 1: 10 }",   &[(0, "false"), (1, "10")])]
fn the_parser_and_schema_accept_scalar_gnmi_enum_labels(
    #[case] map: &str,
    #[case] want: &[(i64, &str)],
) {
    let yaml = GNMI_WITH_EXTRA.replace(
        "{EXTRA}",
        &format!("values: {{ in_octets: {{ enum: {map} }} }}"),
    );
    let file = sonda_core::compiler::parse::parse(&yaml)
        .unwrap_or_else(|e| panic!("the parser rejected enum labels {map}: {e}"));
    let encoder = file
        .defaults
        .and_then(|d| d.encoder)
        .expect("the defaults carry the encoder");
    let sonda_core::encoder::EncoderConfig::Gnmi(cfg) = encoder else {
        panic!("not the gnmi encoder: {encoder:?}")
    };
    let want: std::collections::BTreeMap<i64, String> =
        want.iter().map(|(k, v)| (*k, v.to_string())).collect();
    assert_eq!(
        cfg.values.get("in_octets"),
        Some(&sonda_core::encoder::gnmi::GnmiValueType::Enum(want))
    );

    let errors = schema_errors(&yaml);
    assert!(
        errors.is_empty(),
        "the schema rejected enum labels {map}:\n{}",
        errors.join("\n")
    );
}

/// Enum keys that are not digits with an optional `-` are refused by the
/// schema's key pattern and by the parser alike.
#[rustfmt::skip]
#[rstest::rstest]
#[case::plus_sign("\"+1\"")]
#[case::padded("\" 1\"")]
#[case::decimal("\"1.0\"")]
fn the_parser_and_schema_both_reject_non_integer_enum_codes(#[case] key: &str) {
    let yaml = GNMI_WITH_EXTRA.replace(
        "{EXTRA}",
        &format!("values: {{ in_octets: {{ enum: {{ {key}: UP }} }} }}"),
    );
    assert!(
        sonda_core::compiler::parse::parse(&yaml).is_err(),
        "the parser accepted enum key {key}"
    );
    assert_rejected(&format!("enum key {key}"), &yaml);
}

/// One document per value type that has an `enum:` map (the gnmi encoder's
/// `values:` and, with `snmp`, the snmp encoder's `types:`), the map holding
/// `entries`. Everything else is valid, so a refusal can only come from them.
fn enum_documents(entries: &str) -> Vec<(&'static str, String)> {
    #[cfg_attr(not(feature = "snmp"), allow(unused_mut))]
    let mut documents = vec![(
        "gnmi",
        GNMI_WITH_EXTRA.replace(
            "{EXTRA}",
            &format!("values: {{ in_octets: {{ enum: {{ {entries} }} }} }}"),
        ),
    )];
    #[cfg(feature = "snmp")]
    documents.push((
        "snmp",
        SNMP_WITH_TYPE.replace("{TYPE}", &format!("{{ enum: {{ {entries} }} }}")),
    ));
    documents
}

/// The codes the parser read from the enum map of an [`enum_documents`]
/// document written for the value type `ty`, or the parser's refusal.
fn parsed_enum_codes(ty: &str, yaml: &str) -> Result<Vec<i64>, String> {
    use sonda_core::encoder::EncoderConfig;
    let file = sonda_core::compiler::parse::parse(yaml).map_err(|e| e.to_string())?;
    let encoder = file
        .defaults
        .and_then(|d| d.encoder)
        .expect("the defaults carry the encoder");
    let codes = match (ty, encoder) {
        ("gnmi", EncoderConfig::Gnmi(cfg)) => match cfg.values.get("in_octets") {
            Some(sonda_core::encoder::gnmi::GnmiValueType::Enum(map)) => {
                map.keys().copied().collect()
            }
            other => panic!("gnmi: not an enum value type: {other:?}"),
        },
        #[cfg(feature = "snmp")]
        ("snmp", EncoderConfig::Snmp(cfg)) => match cfg.types.get("ifOperStatus") {
            Some(sonda_core::encoder::snmp::SnmpValueType::Enum(map)) => {
                map.keys().copied().collect()
            }
            other => panic!("snmp: not an enum value type: {other:?}"),
        },
        (ty, encoder) => panic!("{ty}: not the expected encoder: {encoder:?}"),
    };
    Ok(codes)
}

/// Enum keys both value types' schemas and parsers accept, read as the code
/// they spell: a negative code, zero, and both ends of the `i64` range.
#[rustfmt::skip]
#[rstest::rstest]
#[case::negative(      "\"-5\": a",                   -5)]
#[case::zero(          "\"0\": a",                    0)]
#[case::i64_max_bare(  "9223372036854775807: a",      i64::MAX)]
#[case::i64_max_quoted("\"9223372036854775807\": a",  i64::MAX)]
#[case::i64_min_quoted("\"-9223372036854775808\": a", i64::MIN)]
fn the_schema_and_the_parser_accept_an_enum_code_that_fits_i64(
    #[case] entries: &str,
    #[case] code: i64,
) {
    for (ty, yaml) in enum_documents(entries) {
        assert_eq!(
            parsed_enum_codes(ty, &yaml),
            Ok(vec![code]),
            "{ty}: the parser on {{ {entries} }}"
        );
        let errors = schema_errors(&yaml);
        assert!(
            errors.is_empty(),
            "{ty}: the schema rejected {{ {entries} }}:\n{}",
            errors.join("\n")
        );
    }
}

/// Enum keys both value types' schemas refuse: a code spelled with a leading
/// zero or as a signed zero, and every code of 20 digits or more. `parses` is
/// the parser's verdict on the same map. To it `"01"` alone is code 1 and
/// `"-0"` alone code 0, which the schema refuses so that one code has one
/// spelling; beside `1` `"01"` is that code given twice, which the parser
/// refuses as well, as it refuses every code too long for an `i64`. The same
/// document with the key `1` is accepted by both, so each refusal is the key's.
#[rustfmt::skip]
#[rstest::rstest]
#[case::leading_zero(                "\"01\": a",                    true)]
#[case::zero_padded(                 "\"00\": a",                    true)]
#[case::negative_leading_zero(       "\"-01\": a",                   true)]
#[case::signed_zero(                 "\"-0\": a",                    true)]
#[case::leading_zero_beside_its_code("1: a, \"01\": b",              false)]
#[case::twenty_digits(               "\"12345678901234567890\": a",  false)]
#[case::twenty_digits_negative(      "\"-12345678901234567890\": a", false)]
fn the_schema_refuses_an_enum_key_with_a_leading_zero_a_signed_zero_or_twenty_digits(
    #[case] entries: &str,
    #[case] parses: bool,
) {
    let documents = enum_documents(entries);
    for ((ty, control), (_, yaml)) in enum_documents("1: a").into_iter().zip(documents) {
        assert_eq!(parsed_enum_codes(ty, &control), Ok(vec![1]), "{ty}: the control parses");
        let errors = schema_errors(&control);
        assert!(errors.is_empty(), "{ty}: the control is schema-valid:\n{}", errors.join("\n"));
        assert_eq!(
            parsed_enum_codes(ty, &yaml).is_ok(),
            parses,
            "{ty}: the parser on {{ {entries} }}: {:?}",
            parsed_enum_codes(ty, &yaml)
        );
        assert_rejected(&format!("{ty} enum {{ {entries} }}"), &yaml);
    }
}

/// The first documented residue, the enum key pattern's: a 19-digit code
/// outside the `i64` range passes the schema, whose pattern bounds the digit
/// count, and fails the parser, which reads an `i64`. The schema says so in
/// the pattern's `$comment`.
#[rustfmt::skip]
#[rstest::rstest]
#[case::i64_max_plus_one_bare(  "9223372036854775808: a")]
#[case::i64_max_plus_one_quoted("\"9223372036854775808\": a")]
#[case::largest_19_digits(      "\"9999999999999999999\": a")]
#[case::i64_min_minus_one(      "\"-9223372036854775809\": a")]
fn the_first_documented_residue_a_19_digit_code_outside_i64_passes_the_schema_and_fails_the_parser(
    #[case] entries: &str,
) {
    for (ty, yaml) in enum_documents(entries) {
        let errors = schema_errors(&yaml);
        assert!(
            errors.is_empty(),
            "{ty}: the schema refused {{ {entries} }}, so it is no longer the residue:\n{}",
            errors.join("\n")
        );
        let refusal = parsed_enum_codes(ty, &yaml)
            .expect_err("the parser refuses a code outside the i64 range");
        assert!(
            refusal.contains("does not fit in i64"),
            "{ty}: refused for another reason: {refusal}"
        );
    }
}

/// The second documented residue: one code written bare and quoted,
/// `{ 1: a, "1": b }`, passes the schema and fails the parser. YAML keeps the
/// two keys apart, but as JSON both are `"1"`, one key, so no key rule sees
/// the second; the parser reads both as code 1 and refuses the code as given
/// twice.
#[test]
fn the_second_documented_residue_a_code_written_bare_and_quoted_passes_the_schema_and_fails_the_parser(
) {
    for (ty, yaml) in enum_documents("1: a, \"1\": b") {
        let json = yaml_to_json(serde_yaml_ng::from_str(&yaml).expect("loads"));
        let map = match ty {
            "gnmi" => &json["defaults"]["encoder"]["values"]["in_octets"]["enum"],
            _ => &json["defaults"]["encoder"]["types"]["ifOperStatus"]["enum"],
        };
        assert_eq!(
            map.as_object().map(|keys| keys.len()),
            Some(1),
            "{ty}: the schema sees one key: {map}"
        );
        let errors = schema_errors(&yaml);
        assert!(
            errors.is_empty(),
            "{ty}: the schema refused a code written bare and quoted, so it is no longer the \
             residue:\n{}",
            errors.join("\n")
        );
        let refusal = parsed_enum_codes(ty, &yaml)
            .expect_err("the parser refuses a code written bare and quoted");
        assert!(
            refusal.contains("enum code 1 is given more than once"),
            "{ty}: refused for another reason: {refusal}"
        );
    }
}
