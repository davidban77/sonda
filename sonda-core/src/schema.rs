//! JSON Schema for the v2 scenario file format.
//!
//! The schema is derived from the same Rust types the parser deserializes
//! into, so it tracks the wire format automatically for every type whose
//! `Deserialize` is derived. Two types hand-write `Deserialize`
//! ([`WhileOp`](crate::compiler::WhileOp) and
//! [`DelayClause`](crate::compiler::DelayClause)) and therefore hand-write
//! their `JsonSchema` too — see the comments above those impls.
//!
//! # What this schema is for
//!
//! Editor completion and inline validation for people writing scenario YAML.
//! It is **not** the validator: `sonda` itself validates by parsing, and the
//! parser enforces rules no JSON Schema can express (id uniqueness across
//! entries, `after.ref` resolvability, `delay:` requiring `while:`, generator
//! and pack mutual exclusion). A document this schema accepts can still be
//! rejected by `sonda run`. The converse must never happen, which is what
//! `tests/schema_corpus.rs` checks.
//!
//! # The schema depends on the feature set
//!
//! Build sonda-core with `kafka` off and `SinkConfig`'s kafka variant loses
//! its `tls:` and `sasl:` fields — the placeholder that remains exists only
//! to give a "rebuild with the feature" error instead of a serde one. The
//! same goes for `otlp` and `remote-write`. A schema generated from a narrow
//! build would therefore reject sink config that a released binary accepts.
//!
//! The committed schema is generated with `--all-features` for that reason:
//! it is the union of every shape any build of sonda can take, which is the
//! only version safe to hand an editor. `task schema:generate` does this;
//! `sonda-core/tests/schema_corpus.rs` refuses to compile on a build that
//! could not have produced it.
//!
//! # Why `anyOf` and not `oneOf`
//!
//! [`parse`](crate::compiler::parse::parse) accepts three top-level shapes:
//! the canonical form with a `scenarios:` list, the single-signal shorthand
//! with entry fields at the top level, and a `kind: composable` pack
//! definition. `oneOf` means *exactly one* branch may match, so any document
//! matching two branches would be reported invalid.
//!
//! The overlap is between the two CLOSED branches, which is the opposite of
//! where it looks like it should be. Measured, pairwise:
//!
//! ```text
//! ScenarioFileCanonical  required=[version, kind]  additionalProperties=false
//! FlatFile               required=[version]        additionalProperties=false
//! MetricPackDef          required=[name, description, category, metrics]
//!                                                  additionalProperties=unset
//!
//! canonical vs pack       DISJOINT  canonical denies unknowns, has no `name`
//! shorthand vs pack       DISJOINT  shorthand denies unknowns, has no `description`
//! canonical vs shorthand  OVERLAP   {version: 2, kind: runnable} matches BOTH
//! ```
//!
//! So the pack branch's permissiveness is irrelevant: its four required keys
//! are ones the other two forbid, which makes it disjoint from both. What
//! overlaps is canonical against shorthand — `FlatFile` requires only
//! `version` while still carrying a `kind` property, so the header of any
//! canonical file satisfies it too.
//!
//! That overlap is not reachable by a valid file today: `{version: 2, kind:
//! runnable}` with no body is refused by the parser (`RunnableMissingBody`),
//! and a canonical file with `scenarios:` fails `FlatFile`'s
//! `deny_unknown_fields`. Switching to `oneOf` would therefore reject nothing
//! in the corpus, and this is deliberately not claimed as a bug avoided.
//! `anyOf` is chosen because the branches are not disjoint BY CONSTRUCTION —
//! one new optional field on `FlatFile` that a canonical file also carries
//! makes the overlap reachable — and because `oneOf` reports "matched more
//! than one" for a document that is simply valid, which is a worse thing to
//! put in front of a reader than the question `anyOf` asks: is this any of
//! the shapes the parser takes?

use schemars::{JsonSchema, Schema, SchemaGenerator};

/// The `$id` published for the scenario schema.
///
/// Stable across regeneration — tooling and SchemaStore key off it, and a
/// generator that moved it on every run would break both.
pub const SCENARIO_SCHEMA_ID: &str =
    "https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json";

// BEFORE SUBMITTING THIS TO SCHEMASTORE: the scalar widening below emits union
// types (`"type": ["string", "number", "boolean"]`), and ajv refuses those
// under strict mode without `allowUnionTypes` — measured with ajv 8.20 against
// the 2020-12 meta-schema, which reports `strictTypes` at
// `#/properties/category`. It compiles fine with strict mode off, and Python's
// `jsonschema` 4.26 `check_schema` accepts it as-is; the documented consumer
// (the YAML language server behind the VS Code extension, yamlls, Helix and
// Zed) does not use ajv at all. But a registry may run one, so know the shape
// of the objection before the submission rather than after it.
//
// The `format: uint8 / double / uint64` annotations schemars emits are
// likewise non-standard and ignored by both validators — harmless, and
// consistent with this schema not being the validator.

/// Build the JSON Schema describing a v2 scenario file.
///
/// The returned schema is a complete document: it carries `$schema`, `$id`,
/// a title and description, the three-branch `anyOf`, and a `$defs` section
/// holding every referenced type.
pub fn scenario_file_schema() -> Schema {
    // `inline_subschemas` stays off: the type graph is recursive-ish and
    // wide (every generator, encoder and sink variant), and inlining would
    // produce a document too large for an editor to keep re-walking.
    let mut generator = SchemaGenerator::default();

    let canonical = crate::compiler::ScenarioFile::json_schema(&mut generator);
    let canonical = register(&mut generator, "ScenarioFileCanonical", canonical);
    let shorthand = crate::compiler::parse::flat_file_subschema(&mut generator);
    let composable = generator.subschema_for::<crate::packs::MetricPackDef>();

    let mut root = schemars::json_schema!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCENARIO_SCHEMA_ID,
        "title": "Sonda v2 scenario file",
        "description": "A synthetic-telemetry scenario for the sonda CLI and server. \
                        Accepts three top-level shapes: a canonical file with a `scenarios:` \
                        list, a single-signal shorthand with entry fields at the top level, \
                        or a `kind: composable` metric-pack definition.",
        "anyOf": [canonical, shorthand, composable],
    });

    // `take_definitions` drains the generator, so this must come last.
    let defs: serde_json::Map<String, serde_json::Value> =
        generator.take_definitions(false).into_iter().collect();
    if !defs.is_empty() {
        root.insert("$defs".to_string(), serde_json::Value::Object(defs));
    }

    let mut value = root.to_value();
    widen_scalar_strings(&mut value);
    Schema::try_from(value).expect("widening only edits `type` keywords, never the shape")
}

/// Let every string-typed position also accept a YAML number or boolean.
///
/// This is not a convenience. `serde_yaml_ng` coerces any plain scalar into a
/// `String` field, universally — measured, not assumed:
///
/// ```text
/// labels: { status: 200 }   -> parses, label value is "200"
/// name: 200                 -> parses, name is "200"
/// duration: 10              -> parses, duration is "10"
/// ```
///
/// `examples/json-tcp.yaml` has shipped `status: 200` under `labels:` since
/// long before this schema existed. A derived `"type": "string"` would have
/// an editor underline that file, and every file like it, as invalid — while
/// `sonda run` takes it happily. Over-rejection is the one failure mode that
/// makes a schema worse than no schema, so the schema follows the parser
/// rather than the Rust type.
///
/// Positions constrained by `enum` or `const` are left alone: the value set
/// is already closed, so widening the type buys nothing and would only make
/// the failure message vaguer.
fn widen_scalar_strings(value: &mut serde_json::Value) {
    use serde_json::Value as J;

    match value {
        J::Object(map) => {
            let closed = map.contains_key("enum") || map.contains_key("const");
            if !closed {
                if let Some(ty) = map.get_mut("type") {
                    match ty {
                        J::String(s) if s == "string" => {
                            *ty = serde_json::json!(["string", "number", "boolean"]);
                        }
                        J::Array(variants) if variants.iter().any(|v| v == "string") => {
                            for extra in ["number", "boolean"] {
                                if !variants.iter().any(|v| v == extra) {
                                    variants.push(J::String(extra.to_string()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            for (key, child) in map.iter_mut() {
                // `properties` and `$defs` are maps whose KEYS are names, not
                // schema keywords — but their values are schemas, so the
                // recursion is the same. `description` and friends are
                // strings and arrays of strings; recursing into them is
                // harmless because they are never objects with a `type` key.
                let _ = key;
                widen_scalar_strings(child);
            }
        }
        J::Array(items) => {
            for item in items {
                widen_scalar_strings(item);
            }
        }
        _ => {}
    }
}

/// Serialize [`scenario_file_schema`] to the exact bytes the repository
/// commits: pretty-printed, two-space indent, one trailing newline.
///
/// The generator binary and the freshness gate both go through this, so a
/// formatting difference between "what we write" and "what we compare" is
/// not expressible.
pub fn scenario_file_schema_json() -> String {
    let schema = scenario_file_schema();
    let mut text = serde_json::to_string_pretty(&schema)
        .expect("a schemars Schema is a serde_json::Value and always serializes");
    text.push('\n');
    text
}

/// Put a schema into `$defs` under `name` and return a `$ref` to it.
///
/// `ScenarioFile::json_schema` returns the object inline rather than a
/// reference (it is the type being asked for, not a dependency of one), and
/// an `anyOf` branch that is a 40-property object literal while its two
/// siblings are one-line `$ref`s reads badly and diffs worse.
fn register(generator: &mut SchemaGenerator, name: &str, schema: Schema) -> Schema {
    generator
        .definitions_mut()
        .insert(name.to_string(), schema.to_value());
    schemars::json_schema!({ "$ref": format!("#/$defs/{name}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pieces a consumer keys off. `$id` in particular is a published
    /// identifier — SchemaStore and any `$schema:` comment in a user's YAML
    /// point at it, so moving it is a breaking change, not a rename.
    #[test]
    fn root_carries_the_published_identity() {
        let schema = scenario_file_schema();
        let value = schema.to_value();

        assert_eq!(
            value["$id"], SCENARIO_SCHEMA_ID,
            "the $id is published; changing it breaks every editor pointed at the old one"
        );
        assert_eq!(
            value["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert!(
            value["$defs"].is_object(),
            "referenced types must be reachable from the document"
        );
    }

    /// `anyOf`, not `oneOf` — see the module docs for why, and for the
    /// measurement that shows the overlap is between canonical and shorthand
    /// rather than anywhere near the pack branch.
    #[test]
    fn root_offers_all_three_top_level_shapes_as_any_of() {
        let value = scenario_file_schema().to_value();

        assert!(
            value.get("oneOf").is_none(),
            "oneOf means exactly-one-branch, and these branches are not disjoint by \
             construction — canonical and shorthand are both closed, but FlatFile \
             requires only `version` while carrying a `kind` property"
        );
        let branches = value["anyOf"]
            .as_array()
            .expect("the root must be a three-branch anyOf");
        assert_eq!(branches.len(), 3, "canonical, shorthand, composable");
        for branch in branches {
            assert!(
                branch.get("$ref").is_some(),
                "each branch should be a $ref into $defs, got {branch}"
            );
        }
    }

    /// The module docs justify `anyOf` with a pairwise-disjointness claim.
    /// The first version of that claim named the wrong branch, and nothing
    /// caught it because the corpus contains no document matching two
    /// branches — so `oneOf` would have produced zero rejections and the
    /// rationale could say anything. This pins the three structural facts the
    /// argument actually rests on.
    #[test]
    fn the_branch_shapes_the_any_of_rationale_rests_on() {
        let value = scenario_file_schema().to_value();
        let defs = &value["$defs"];

        let required = |name: &str| -> Vec<String> {
            defs[name]["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };

        // Both runnable branches are CLOSED. This is what makes the pack
        // branch disjoint from them: its required keys are unknown fields here.
        for closed in ["ScenarioFileCanonical", "FlatFile"] {
            assert_eq!(
                defs[closed]["additionalProperties"], false,
                "{closed} must deny unknown fields"
            );
        }

        // The pack branch is disjoint from each closed branch because it
        // REQUIRES at least one key that branch does not declare — and a
        // closed branch rejects any key it does not declare.
        //
        // Not "declares none of them": canonical carries `description` and
        // `category` as catalog metadata, and shorthand carries `name` as an
        // entry field. The overlap in vocabulary is real; the disjointness
        // comes from what is left over. Writing this as "none of the four
        // appear" is how the first version of this test failed.
        let pack_required = required("MetricPackDef");
        for closed in ["ScenarioFileCanonical", "FlatFile"] {
            let declared = defs[closed]["properties"]
                .as_object()
                .expect("a closed branch is an object schema");
            let unsatisfiable: Vec<&String> = pack_required
                .iter()
                .filter(|key| !declared.contains_key(*key))
                .collect();
            assert!(
                !unsatisfiable.is_empty(),
                "{closed} declares every key the pack branch requires, so a document \
                 could satisfy both and the module docs' disjointness claim is stale"
            );
        }

        // And the overlap that DOES exist: shorthand requires only `version`
        // while carrying `kind`, so a canonical file's header satisfies it.
        assert_eq!(required("FlatFile"), vec!["version".to_string()]);
        assert!(defs["FlatFile"]["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("kind")));
    }

    /// The two hand-written `Deserialize` impls are the schema's known
    /// divergence risk. These assert the wire shape, not the Rust shape:
    /// a derived schema would say `["LessThan", "GreaterThan"]` here.
    #[test]
    fn while_op_advertises_the_operator_glyphs_not_the_variant_names() {
        let value = scenario_file_schema().to_value();
        let while_op = &value["$defs"]["WhileOp"];

        assert_eq!(while_op["type"], "string");
        let accepted = while_op["enum"]
            .as_array()
            .expect("WhileOp is a string enum on the wire");
        assert_eq!(accepted, &vec!["<", ">"]);
    }

    /// `DelayClause` has four struct fields and two wire keys. A derived
    /// schema would offer `close_snap_to` and `close_stale_marker` at the
    /// top level, which the parser rejects with `deny_unknown_fields`.
    #[test]
    fn delay_clause_advertises_only_the_two_keys_the_parser_accepts() {
        let value = scenario_file_schema().to_value();
        let delay = &value["$defs"]["DelayClause"];

        let properties = delay["properties"]
            .as_object()
            .expect("DelayClause is an object schema");
        let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["close", "open"],
            "close_snap_to/close_stale_marker are where the extended `close:` mapping \
             unpacks to, not keys a reader may write"
        );
        assert_eq!(
            delay["additionalProperties"], false,
            "the parser denies unknown fields here; the schema must say so"
        );
        assert!(
            properties["close"]["oneOf"].is_array(),
            "`close` takes a duration string or an extended mapping"
        );
    }

    /// The widening pass, at the exact spot that motivated it: label maps
    /// are `BTreeMap<String, String>` in Rust, and `examples/json-tcp.yaml`
    /// has carried `status: 200` under `labels:` for a long time.
    #[test]
    fn string_positions_also_accept_the_scalars_serde_yaml_coerces() {
        let value = scenario_file_schema().to_value();
        let labels = &value["$defs"]["Entry"]["properties"]["labels"];

        let rendered = labels.to_string();
        assert!(
            rendered.contains("number") && rendered.contains("boolean"),
            "a label value typed `string` alone would underline `status: 200`, \
             which sonda accepts: {rendered}"
        );
    }

    /// The widening must not touch closed value sets. `WhileOp` is the one
    /// that would be actively harmed: widening its type keeps the `enum`
    /// constraint but makes the editor's message vaguer for no gain.
    #[test]
    fn widening_leaves_enum_constrained_positions_alone() {
        let value = scenario_file_schema().to_value();

        assert_eq!(
            value["$defs"]["WhileOp"]["type"], "string",
            "an enum-constrained position is already closed; leave its type narrow"
        );
    }

    /// Byte-for-byte what the generator writes. If this drifts from the
    /// generator, the freshness gate compares two different renderings and
    /// reports drift that is really just formatting.
    #[test]
    fn json_rendering_is_pretty_printed_with_one_trailing_newline() {
        let text = scenario_file_schema_json();

        assert!(text.ends_with("}\n"), "exactly one trailing newline");
        assert!(!text.ends_with("}\n\n"));
        assert!(
            text.contains("\n  \"$id\""),
            "two-space indent at the top level"
        );
    }
}
