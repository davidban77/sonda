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
    feature = "remote-write"
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
            serde_json::Value::Array(items.into_iter().map(yaml_to_json).collect())
        }
        Y::Mapping(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    // Non-string keys cannot appear in a scenario file, and
                    // rendering one as its debug form would silently produce
                    // a key no schema matches. Fail loudly instead.
                    let key = match k {
                        Y::String(s) => s,
                        other => panic!("scenario YAML must use string keys, found {other:?}"),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        Y::Tagged(tagged) => yaml_to_json(tagged.value),
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
/// No pack in `packs/` uses `extends:` or `deviations:` yet, so
/// `every_repo_pack_validates` says nothing about those branches — a schema
/// that got them wrong would still be green. This drives them directly.
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
