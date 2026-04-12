# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 4 — `after` compiler + dependency graph (complete, pending merge)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 6 — runtime wiring + parity tests

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | Done | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Done | PR 3 (#199) | 2026-04-12 |
| 3 | Pack expansion in scenarios | Done | PR 4 | 2026-04-12 |
| 4 | `after` compiler + dependency graph | Done | PR 5 | 2026-04-12 |
| 5 | Runtime wiring + parity tests | Not Started | PR 6 | |
| 6 | CLI unification | Not Started | PR 7 | |
| 7 | Built-ins migration + docs | Not Started | PR 8 | |
| 8 | Server API + final cleanup | Not Started | PR 9 | |

## PR Log

| PR | Title | Branch | Target | Status | Date |
|----|-------|--------|--------|--------|------|
| 1 | Compile snapshot harness + test foundation | (direct) | integration | Merged | 2026-04-11 |
| 2 | Compiler AST, parser, and version dispatch | `feat/v2-ast-parser` | integration (#198) | Merged | 2026-04-11 |
| 3 | Defaults resolution + `parse_v2 → parse` rename | `feat/defaults-resolution` | integration (#199) | Merged | 2026-04-12 |
| 4 | Pack expansion inside `scenarios:` | `feat/pack-expansion` | integration | Merged | 2026-04-12 |
| 5 | `after` compiler + dependency graph + timing port | `feat/after-compilation` | integration | Pending Review | 2026-04-12 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| Compiler parser unit tests | 49 | AST parsing, validation, shorthand, edge cases |
| Compiler normalize unit tests | 34 | Defaults inheritance, label merge (inline eager / pack deferred), built-in fallbacks, missing-rate error, defaults-labels surfacing |
| Compiler expand unit tests | 33 | Pack expansion, label precedence, auto-IDs (including duplicate-name disambiguation), post-expansion id uniqueness, override validation, after propagation, resolver trait |
| Compiler timing unit tests | 44 | Crossing math for every supported generator (sawtooth/step/sequence/spike/flap/saturation/leak/degradation/spike_event/constant) and blanket rejections (sine/steady/uniform/csv_replay); inactive-max wrap-around regression |
| Compiler compile_after unit tests | 37 | Reference resolution, self-ref, cycles, transitive chains, delay + phase_offset additivity, step/sequence crossings, cross-signal-type, alias desugaring, clock group auto-assignment + conflicts (including whitespace/empty-string handling), dotted/ambiguous pack refs, `InvalidDuration` coverage for after.delay/phase_offset/alias-param code paths, format_duration_secs round-trip |
| Compiler fixture integration tests | 15 | Valid/invalid YAML examples parsed + normalized from disk |
| Compiler expand fixture integration tests | 5 | Pack expansion fixtures with golden snapshots + invalid-override rejection |
| Compiler compile_after fixture integration tests | 16 | 7 valid fixtures with CompiledFile golden snapshots + 9 invalid fixtures asserting specific CompileAfterError variants |
| Pack parity bridge integration tests | 5 | 3 pack compile parity (17.1–17.3) + 2 compile_after resolution tests (11.12 override, 11.13 entry-level propagation) |
| Story parity bridge integration test | 1 | 16.12 compile parity: `stories/link-failover.yaml` v1 math vs v2 compile agree on phase_offset to the millisecond |
| Compile snapshot golden files | 12 | v1 parity baseline (6 fixtures x raw+prepared) |
| Normalize snapshot golden files | 3 | Resolved defaults snapshots (label merge, logs default encoder, pack entry) |
| Expand snapshot golden files | 4 | Phase 3 snapshots (overrides, file-path, multi-pack, anonymous pack) |
| Compile_after snapshot golden files | 7 | CompiledFile JSON snapshots covering simple/transitive chain, step/sequence targets, cross-signal-type, phase_offset + delay sum, dotted pack ref |
| **New in refactor** | **264** | |
| Workspace total | 2,704 | All existing + new |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 178-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 48 of 178 rows addressed so far.

| Section | Rows | Addressed | Notes |
|---------|------|-----------|-------|
| 1-10. Feature parity | 98 | 26 | 5.8 (PR 3), 8.2 (PR 5 compile-time, runtime observability PR 6), 9.1–9.8, 9.12 (PR 4), 10.1–10.15 (PR 3 + PR 5) — rest need runtime wiring |
| 11. New v2 features | 18 | 18 | All 11.1–11.18 addressed; 11.7/11.9/11.11/11.12/11.13/11.14/11.15/11.16/11.17/11.18 land in PR 5 with `compile_after` |
| 12-15. CLI/Server/UX/Deploy | 47 | 0 | Later PRs (7-9) |
| **16. Scenario parity bridge** | **12** | **1** | **16.12 compile parity Pass (PR 5); runtime parity for all rows lands in PR 6** |
| **17. Pack parity bridge** | **3** | **3** | **compile parity passes for all three built-in packs; runtime parity is PR 6** |

## Completed Work

### PR 5 — `after` compiler + dependency graph (2026-04-12, pending review)
- `sonda-core/src/compiler/timing.rs` — pure threshold-crossing math per §3.3 table. Ported verbatim from the v1 `sonda/src/story/timing.rs` (flap / sawtooth / spike / steady) and extended to cover every supported generator: `step_crossing_secs`, `sequence_crossing_secs`, `constant_crossing_secs`, plus blanket `sine_crossing_secs`, `uniform_crossing_secs`, `csv_replay_crossing_secs`. Module compiles without the `config` feature so `no-config` builds remain intact.
- `sonda-core/src/compiler/compile_after.rs` — spec §4.4 (`after`-clause compilation) + §4.5 (clock-group assignment) compile pass. Exports `compile_after()`, `CompiledFile`, `CompiledEntry`, `CompileAfterError` (typed variants: `UnknownRef`, `AmbiguousSubSignalRef`, `SelfReference`, `CircularDependency`, `UnsupportedGenerator`, `OutOfRangeThreshold`, `AmbiguousAtT0`, `ConflictingClockGroup`, `NonMetricsTarget`, `InvalidDuration`). The type itself witnesses "after is resolved" — same discipline as `ExpandedFile`/`NormalizedFile`. Note: the milestone table below reserves execution-plan "Phase 5" for runtime wiring (PR 6); this file closes the *spec* §4.4 and §4.5 compile-time passes only.
- **Reference resolution** keyed on `ExpandedEntry.id` (inline ids + pack sub-signal ids like `{entry}.{metric}#{n}`). Bare `{entry}.{metric}` against duplicate-name packs surfaces `AmbiguousSubSignalRef` with the concrete `#N` candidates listed.
- **Topological sort + cycle detection.** Kahn's algorithm produces the resolution order; when `sorted.len() < n`, a DFS with gray/black coloring reconstructs the full cycle path (`[A, B, C, A]`) for `CircularDependency.cycle`.
- **Offset formula** (matrix row 11.14): `total = user_phase_offset + Σ crossing_time + Σ delay`. `after.delay` and explicit `phase_offset` are parsed via `sonda_core::config::validate::parse_duration`. Output is formatted back to a parseable duration string (`"1m"`, `"85s"`, `"152.308s"`) by `format_duration_secs` — millisecond precision, round-trips cleanly through `parse_duration`.
- **Alias desugaring** happens inside `crossing_secs` before dispatch: `flap` → flap math, `saturation`/`leak`/`degradation` → sawtooth math, `spike_event` → spike math, `steady` → sine (rejected). Keeps the compiler pure — no `ScenarioConfig` fabrication needed.
- **Clock-group derivation** (spec §4.5): connected components of the undirected `after` graph receive a shared group. If any member has an explicit `clock_group` that value is adopted for the whole component; if two distinct values appear, `ConflictingClockGroup` fires with both values and entries named. Auto-name is `chain_{lowest_lex_id}` among component members with `Some(id)`.
- **Cross-signal-type after** (§3.5, matrix 11.11): the dependent can be metrics/logs/histogram/summary; the target must be metrics — `NonMetricsTarget` rejects logs/histogram/summary targets with a clear diagnostic.
- **v1 story path preserved.** `sonda/src/story/timing.rs` was deleted; `sonda/src/story/after_resolve.rs` now imports the shared math from `sonda_core::compiler::timing`. All 67 v1 story tests still pass.
- `sonda-core/tests/v2_compile_after_fixtures.rs` — 16 integration tests: 7 valid fixtures with `CompiledFile` golden JSON snapshots (simple chain, transitive chain, step target, sequence target, cross-signal-type, phase_offset + delay sum, pack dotted ref) + 9 invalid fixtures asserting each distinct `CompileAfterError` variant (unknown ref, cycle, self-reference, unsupported sine, out-of-range, ambiguous-at-t=0, conflicting clock_group, ambiguous pack ref, non-metrics target).
- `sonda-core/tests/v2_story_parity.rs` — compile-parity bridge for matrix row 16.12: `stories/link-failover.yaml` equivalent compiles to `phase_offset` values matching v1 story math (same `timing::*_crossing_secs` calls) to millisecond precision.
- `sonda-core/tests/v2_pack_parity.rs` — extended with two `compile_after`-level tests closing matrix 11.12 and 11.13: override-level `after` lands on the specific pack sub-signal; entry-level `after` propagates to every expanded sub-signal and they all receive the same resolved offset and shared clock group.
- Validation matrix rows closed: 10.1–10.11 (`after` semantics), 11.7 (dotted ref), 11.9 (delay), 11.11 (cross-signal-type), 11.12–11.13 (full Pass with resolved offsets), 11.14 (offset sum), 11.15–11.16 (clock groups), 11.17–11.18 (step/sequence targets). 16.12 compile parity Pass; runtime parity deferred to PR 6.
- No `v2` prefix on any symbol inside `sonda-core::compiler` (matches PR 3/4 discipline).

### PR 1 — Compile snapshot harness (2026-04-11)
- `sonda-core/src/config/snapshot.rs` — deterministic JSON snapshot serializer
- `Serialize` derives added to all config types (feature-gated)
- 6 semantic YAML fixtures + 12 golden-file integration tests
- `KafkaSaslConfig.password` skip_serializing for security
- Reviewer findings fixed: feature gates, trailing newlines, password masking

### PR 2 — Compiler AST and parser (2026-04-11, merged)
- `sonda-core/src/compiler/mod.rs` — AST types: `ScenarioFile`, `Defaults`, `Entry`, `AfterClause`, `AfterOp`
- `sonda-core/src/compiler/parse.rs` — parser with 9 validation rules, `detect_version()`
- Single-signal shorthand wrapping (inline + pack)
- Deterministic parse dispatch via `ShapeProbe` (no ambiguous fallback)
- Cross-generator mutual exclusion validation
- `MetricOverride.labels` aligned to `BTreeMap` for determinism
- 45 unit tests + 11 fixture integration tests (5 valid, 6 invalid YAML examples)
- Module named `compiler` (describes function, not version number)

### PR 4 — Pack expansion inside `scenarios:` (2026-04-12, pending review)
- `sonda-core/src/compiler/expand.rs` — `expand()`, `ExpandedFile`, `ExpandedEntry`, `ExpandError`
- `PackResolver` trait with classification helper (`classify_pack_reference`) and `PackResolveOrigin` (Name | FilePath); `InMemoryPackResolver` test/embedded impl
- Five-level label precedence chain applied per spec §2.2 (defaults → pack shared → pack per-metric → entry → override) using `BTreeMap<String, String>` for determinism
- Entry-level `after` propagated to every expanded metric; override-level `after` replaces entry-level for that specific metric (resolution deferred to PR 5)
- Auto-ID scheme: anonymous pack entries receive `"{pack_def_name}_{entry_index}"`; sub-signal IDs are `"{effective_entry_id}.{metric_name}"` for unique-by-name packs and `"{effective_entry_id}.{metric_name}#{spec_index}"` for packs shipping multiple `MetricSpec`s under the same name (e.g. `node_exporter_cpu`)
- Post-expansion id uniqueness check: `ExpandError::DuplicateEntryId` fires when a user-authored inline id collides with an auto-synthesized pack-entry id (the parser's id check only sees user-provided ids, so this pass closes the gap)
- Override key validation — unknown override keys produce `ExpandError::UnknownOverrideKey` with pack name and valid metric list, matching v1 `expand_pack` diagnostic shape
- `MetricOverride` gained an optional `after: Option<AfterClause>` field (backward-compatible `#[serde(default)]`); v1 `expand_pack` ignores it
- 33 expand unit tests + 5 new fixture integration tests (4 valid with golden snapshots, 1 invalid) + 3 pack parity bridge tests (matrix rows 17.1–17.3 compile parity, plus v2-only sub-signal id uniqueness assertion)
- Addresses validation matrix rows 9.1–9.8, 9.12, 11.6, 11.8, 17.1, 17.2, 17.3 (compile-parity only); 11.12 and 11.13 Pass for the carry-through portion — actual `after` resolution is PR 5
- `parse_v2 → parse` alignment from PR 3 reused; no v1/v2 prefix on any symbol inside `sonda-core::compiler`
- Snapshot golden `valid-defaults-pack-entry.json` updated because `MetricOverride` now serializes with `after: null`

### PR 3 — Defaults resolution and normalization (2026-04-12, pending review)
- `sonda-core/src/compiler/normalize.rs` — `normalize()`, `NormalizedFile`, `NormalizedEntry`, `NormalizeError`
- Precedence for `rate`/`duration`/`encoder`/`sink`: entry-level > `defaults:` > built-in fallback (eager, both inline and pack entries)
- Built-in encoder per signal type: `prometheus_text` for metrics/histogram/summary, `json_lines` for logs
- Built-in sink: `stdout`
- **Label composition is asymmetric** (rationale in `normalize.rs` module docs under "Labels merge"):
  - Inline entries: eager merge — `defaults.labels ∪ entry.labels`, entry wins on key conflict
  - Pack entries: no merge — `NormalizedEntry.labels` = entry's own labels; `NormalizedFile.defaults_labels` surfaces the source map so pack expansion can layer it correctly against pack `shared_labels` / per-metric / override labels
- Pack entries' `pack:` and `overrides:` fields carried through untouched (pack expansion is PR 4)
- Required-field validation: missing `rate` identifies the offending entry by index + name/id/pack
- Rename `parse_v2 → parse` workspace-wide (module prefix carries the version)
- 34 normalize unit tests + 4 new fixtures (3 valid with golden snapshots, 1 invalid)
- Addresses validation matrix rows 5.8, 10.12, 10.13, 10.14, 10.15, 11.2, 11.3
- Reviewer NOTE (pack-label precedence collision) resolved inline via Option 2 — documented in `normalize.rs` module docs (see "Labels merge")
- Reviewer NITs addressed: stale `V2 AST types` comment renamed; no-op `serde(deny_unknown_fields)` dropped from `NormalizedFile`/`NormalizedEntry`; snapshot-harness `expect()` calls converted to `unwrap_or_else(panic!)` with OS error detail

## PR 6 Preparation Notes

These notes capture decisions and handoff context from PR 5 that PR 6 (runtime wiring + parity tests) must not re-litigate. A future session starting cold on PR 6 should read this section first.

### What PR 5 already hands off

PR 5 produces `CompiledFile { version, entries: Vec<CompiledEntry> }` — a flat list of concrete signals with every `after:` clause resolved into `phase_offset` and every dependency chain member assigned a shared `clock_group`. The runtime never sees `AfterClause` objects: the causal graph has been compiled down to a pair of `Option<String>` fields that the existing scheduler already understands.

Every `CompiledEntry` guarantees:
- **Reference resolution complete.** No unresolved `after.ref` survives; `CompileAfterError::UnknownRef` / `SelfReference` / `CircularDependency` have already fired if anything was wrong.
- **`phase_offset` is the full offset.** It equals `user_phase_offset + Σ crossing_time + Σ delay` across the entire transitive chain. PR 6 can feed this directly into `prepare_entries`; no further timing math is needed at runtime.
- **`clock_group` is authoritative.** Dependency chain members all share a group (user-set or auto-assigned `chain_{lowest_lex_id}`); independents keep their explicit group or `None`.
- **Generator aliases are still present.** PR 5's internal desugaring only touches timing math; `CompiledEntry.generator` carries the same `GeneratorConfig` variant the user wrote. The existing runtime pipeline already handles aliases via `config::aliases::desugar_entry`.

### What PR 6 must build

1. **Wire `CompiledFile` into `prepare_entries`.** The CLI currently routes through `sonda_core::schedule::launch::prepare_entries(Vec<ScenarioEntry>)`. PR 6 must add a conversion from `Vec<CompiledEntry>` to `Vec<ScenarioEntry>` (the runtime's existing input shape) — this is largely a field-for-field mapping since PR 2/3/4 already mirrored the shapes. The simplest path is a `CompiledEntry → ScenarioEntry` `TryFrom` impl or a helper in `sonda-core::compiler`. Do not duplicate the scheduler; reuse `prepare_entries` and `launch_scenario` as-is.

2. **Runtime parity for built-in scenarios (rows 16.1–16.11 single-signal).** Each of the eleven built-in scenario YAMLs has an existing v1 baseline. Hand-write a v2 equivalent (`scenarios/<name>.v2.yaml`), pipe both through `prepare_entries` → `launch_scenario` with a deterministic seed and tick count, and assert the stdout output is byte-identical.

3. **Runtime parity for link-failover story (row 16.12 runtime).** PR 5 already closed compile parity for `link-failover.yaml`. PR 6 must close the runtime half: same seed, same tick count, same byte-for-byte stdout. Live story runtime still uses the v1 `compile_story` path — point the runtime comparison at both paths.

4. **Runtime parity for packs (rows 17.1–17.3 runtime).** The three built-in packs already pass compile parity (PR 4). Same drill: build the v2 scenario from the existing fixture, run both paths with a seed, assert identical stdout.

### What `sonda/src/story/` looks like after PR 5

- `timing.rs` is **gone**. The math now lives in `sonda_core::compiler::timing` and is shared by v1 and v2.
- `after_resolve.rs` remains as-is, but now imports from `sonda_core::compiler::timing`. It still owns the v1 story-specific parsing of free-form `after:` strings (`"metric < 1"`) and the `SignalParams` data model, because the v1 story YAML grammar differs from the v2 compiler AST.
- `mod.rs` (top-level story module) is unchanged — the CLI entrypoint `sonda story --file` still works.

This split is deliberate: the shared math is now in one place, while v1-specific YAML plumbing stays in the binary crate until PR 9 removes the `story` subcommand.

### Scope for PR 6 (target matrix rows)

- 16.1–16.11 (single-signal built-in scenarios — runtime parity)
- 16.12 (link-failover story runtime parity; compile parity already Pass)
- 17.1–17.3 (pack runtime parity; compile parity already Pass)
- Any row from sections 1–6 that tests runtime behavior (metrics/logs/histogram/summary signal types, generators, encoders, sinks, dynamic/cardinality features) — these are runtime-observable and should flip from "Not Tested" to "Pass" where the v2 pipeline already produces correct output.

### Scope discipline for PR 6

Do NOT also:
- Migrate built-in scenario YAMLs to v2 format (PR 8).
- Remove v1 CLI subcommands (PR 9).
- Add v2 scenario server API (PR 9).

PR 6 is the runtime parity proof — it shows the v2 pipeline produces correct output when driven through the existing scheduler. Migrations and CLI changes follow once parity is nailed down.

## Active Risks
- Snapshot format stability — must be deterministic and survive refactor
- `deny_unknown_fields` on parse-time AST prevents forward-compatible parsing (deliberate); `NormalizedFile`/`NormalizedEntry` are Serialize-only projections and intentionally do not carry that attribute

## Process Notes
- All PRs target integration branch (`refactor/unified-scenarios-v2`), not `main`
- Integration branch merges to `main` only after full validation matrix passes (178/178)
- Progress file and validation status updated at end of every PR
- Every PR includes example YAML fixtures for reviewability
- Implementation plans get user approval before launching implementer
- Implementer gets requirements and constraints, not exact code blueprints
