# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 3 — Pack expansion (complete, pending merge)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 5 — `after` compiler + dependency graph

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | Done | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Done | PR 3 (#199) | 2026-04-12 |
| 3 | Pack expansion in scenarios | Done | PR 4 | 2026-04-12 |
| 4 | `after` compiler + dependency graph | Not Started | PR 5 | |
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
| 4 | Pack expansion inside `scenarios:` | `feat/pack-expansion` | integration | Pending Review | 2026-04-12 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| Compiler parser unit tests | 49 | AST parsing, validation, shorthand, edge cases |
| Compiler normalize unit tests | 34 | Defaults inheritance, label merge (inline eager / pack deferred), built-in fallbacks, missing-rate error, defaults-labels surfacing |
| Compiler expand unit tests | 28 | Pack expansion, label precedence, auto-IDs, override validation, after propagation, resolver trait |
| Compiler fixture integration tests | 15 | Valid/invalid YAML examples parsed + normalized from disk |
| Compiler expand fixture integration tests | 5 | Pack expansion fixtures with golden snapshots + invalid-override rejection |
| Pack parity bridge integration tests | 3 | Built-in pack compile parity (telegraf_snmp_interface, node_exporter_cpu, node_exporter_memory) |
| Compile snapshot golden files | 12 | v1 parity baseline (6 fixtures x raw+prepared) |
| Normalize snapshot golden files | 3 | Resolved defaults snapshots (label merge, logs default encoder, pack entry) |
| Expand snapshot golden files | 4 | Phase 3 snapshots (overrides, file-path, multi-pack, anonymous pack) |
| **New in refactor** | **149** | |
| Workspace total | 2,621 | All existing + new |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 162-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 25 of 162 rows addressed so far.

| Section | Rows | Addressed | Notes |
|---------|------|-----------|-------|
| 1-10. Feature parity | 97 | 14 | 5.8 (PR 3), 9.1–9.8, 9.12 (PR 4), 10.12–10.15 (PR 3) — rest need runtime wiring |
| 11. New v2 features | 18 | 8 | 11.1, 11.2, 11.3, 11.4, 11.5, 11.6, 11.8, 11.10; 11.12/11.13 partial (PR 5 resolves `after`) |
| 12-15. CLI/Server/UX/Deploy | 47 | 0 | Later PRs (7-9) |
| **16. Scenario parity bridge** | **12** | **0** | **v1→v2 compile + runtime for all built-ins + story** |
| **17. Pack parity bridge** | **3** | **3** | **compile parity passes for all three built-in packs; runtime parity is PR 6** |

## Completed Work

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
- Auto-ID scheme: anonymous pack entries receive `"{pack_def_name}_{entry_index}"`; sub-signal IDs are always `"{effective_entry_id}.{metric_name}"`
- Override key validation — unknown override keys produce `ExpandError::UnknownOverrideKey` with pack name and valid metric list, matching v1 `expand_pack` diagnostic shape
- `MetricOverride` gained an optional `after: Option<AfterClause>` field (backward-compatible `#[serde(default)]`); v1 `expand_pack` ignores it
- 28 expand unit tests + 5 new fixture integration tests (4 valid with golden snapshots, 1 invalid) + 3 pack parity bridge tests (matrix rows 17.1–17.3 compile parity)
- Addresses validation matrix rows 9.1–9.8, 9.12, 11.6, 11.8, 17.1, 17.2, 17.3 (compile-parity only); 11.12 and 11.13 Pass for the carry-through portion — actual `after` resolution is PR 5
- `parse_v2 → parse` alignment from PR 3 reused; no v1/v2 prefix on any symbol inside `sonda-core::compiler`
- Snapshot golden `valid-defaults-pack-entry.json` updated because `MetricOverride` now serializes with `after: null`

### PR 3 — Defaults resolution and normalization (2026-04-12, pending review)
- `sonda-core/src/compiler/normalize.rs` — `normalize()`, `NormalizedFile`, `NormalizedEntry`, `NormalizeError`
- Precedence for `rate`/`duration`/`encoder`/`sink`: entry-level > `defaults:` > built-in fallback (eager, both inline and pack entries)
- Built-in encoder per signal type: `prometheus_text` for metrics/histogram/summary, `json_lines` for logs
- Built-in sink: `stdout`
- **Label composition is asymmetric** (see PR 4 Preparation Notes below):
  - Inline entries: eager merge — `defaults.labels ∪ entry.labels`, entry wins on key conflict
  - Pack entries: no merge — `NormalizedEntry.labels` = entry's own labels; `NormalizedFile.defaults_labels` surfaces the source map so PR 4 can layer it correctly against pack `shared_labels` / per-metric / override labels
- Pack entries' `pack:` and `overrides:` fields carried through untouched (pack expansion is PR 4)
- Required-field validation: missing `rate` identifies the offending entry by index + name/id/pack
- Rename `parse_v2 → parse` workspace-wide (module prefix carries the version)
- 34 normalize unit tests + 4 new fixtures (3 valid with golden snapshots, 1 invalid)
- Addresses validation matrix rows 5.8, 10.12, 10.13, 10.14, 10.15, 11.2, 11.3
- Reviewer NOTE (pack-label precedence collision) resolved inline via Option 2 — documented in `normalize.rs` module docs and in PR 4 Preparation Notes below
- Reviewer NITs addressed: stale `V2 AST types` comment renamed; no-op `serde(deny_unknown_fields)` dropped from `NormalizedFile`/`NormalizedEntry`; snapshot-harness `expect()` calls converted to `unwrap_or_else(panic!)` with OS error detail

## PR 5 Preparation Notes

These notes capture decisions and handoff context from PR 4 that PR 5 (the `after` compiler + dependency graph) must not re-litigate. A future session starting cold on PR 5 should read this section first, then the reviewer thread on PR 4 if deeper context is needed.

### What PR 4 already hands off

PR 4 produces `ExpandedFile { version, entries: Vec<ExpandedEntry> }` — a fully resolved, flat list of concrete signals. Every entry has:
- a **concrete `id`** for pack-expanded signals (`"{effective_entry_id}.{metric_name}"`); inline entries may still have `id: None` if the source YAML omitted it.
- a concrete `generator`, `rate`, `encoder`, `sink` (inherited from PR 3).
- labels already merged through the full spec §2.2 precedence chain.
- the raw `after: Option<AfterClause>` that the user wrote, after PR 4's propagation:
  - entry-level `after` on a pack entry is copied onto every expanded metric,
  - override-level `after` replaces the entry-level value for that specific metric.
- no `pack` or `overrides` field — the type itself doesn't carry them.

PR 5 can treat every `ExpandedEntry` uniformly; inline vs. pack-origin is no longer observable at this layer.

### What PR 5 must build

1. **Reference index.** Map every signal id (both user-declared on inline entries and auto-generated on pack entries) to the concrete `ExpandedEntry`. Spec §3.2: references target signal IDs, not metric names. Because `ExpandedEntry.id` is a flat `Option<String>`, the reference index is just a pass over the entries collecting `entry.id.clone()` as key → `&entry` as value. Reject entries with `after.ref` pointing to a missing id (matrix row 10.7).

2. **`after` resolution per signal.** For each signal with `after: Some(_)`:
   - validate the target generator supports the given operator (spec §3.3);
   - validate the threshold is in range for the target generator's output;
   - compute the crossing time on the target;
   - accumulate transitive offsets (walk the chain);
   - detect cycles with topological sort and report the full cycle path (matrix row 10.6).

3. **Clock group derivation.** For each connected component in the after-dependency graph, assign a shared `clock_group` when none is set. If users set `clock_group` explicitly on multiple entries in one chain, assert consistency (matrix row 11.16). Signals with no `after` and no explicit `clock_group` stay independent. PR 4 left `clock_group` untouched on `ExpandedEntry` — it is `Option<String>` exactly as the user wrote it.

4. **`phase_offset` application.** Set `phase_offset` on each signal to its computed total offset. If a signal already had an explicit `phase_offset`, add the computed offset to it (matrix row 11.14).

### Supported generators for `after` (per spec §3.3)

PR 5 must validate operator/threshold compatibility against the generator's analytical form. The generators that can participate as targets are: `sine`, `sawtooth`, `step`, `spike`, `flap`, `saturation`, `leak`, `degradation`, `steady`, `spike_event`, `sequence`. The aliases desugar to core generators before PR 5 runs — use the desugared form for math.

Generators that do not support `after` as targets (noise-dominated or constant, per matrix row 10.10): `constant`, `uniform`, `csv_replay`, and `jitter`-wrapped generators whose underlying type is `sine`/`steady` (spec §3.3 rejects `sine` and `steady` specifically as causal targets). Surface a clear error when an `after.ref` resolves to an unsupported generator.

### Data contract between PR 4 output and PR 5 input

- `ExpandedEntry.id` is the only identity PR 5 should key off. Do not reconstruct ids from `pack + metric` — that information is gone after PR 4 for a reason.
- `ExpandedEntry.after: Option<AfterClause>` is the source of truth for each signal. There is no parent-entry context to reach back to.
- `AfterClause` lives in `sonda-core::compiler` (`super::AfterClause` from `expand.rs`). `MetricOverride` in `sonda-core::packs` now also has an optional `after` field, but by the time PR 5 runs, overrides have been expanded into per-signal `AfterClause`s — PR 5 does not touch `MetricOverride`.
- `GeneratorConfig` aliases (`flap`, `saturation`, …) are still present in `ExpandedEntry.generator` because PR 4 does no desugaring. Run `config::aliases::desugar_*` (or equivalent) before generator-shape analysis.

### Scope for PR 5 (target matrix rows)

- 10.1–10.11 (`after` semantics and validation)
- 11.7 (dotted `after.ref` into pack sub-signals — works for free because PR 4 already assigns sub-signal ids of the form `{entry}.{metric}`)
- 11.9 (`delay` in after clause)
- 11.11 (cross-signal-type `after`)
- 11.14 (`after + phase_offset` sum)
- 11.15 (clock group auto-assignment)
- 11.16 (conflicting `clock_group` in a chain → error)
- 11.17, 11.18 (`after` with step and sequence generators)
- Promote 11.12 and 11.13 from "partial" to "Pass" once full resolution works.

## Active Risks
- Snapshot format stability — must be deterministic and survive refactor
- `deny_unknown_fields` on parse-time AST prevents forward-compatible parsing (deliberate); `NormalizedFile`/`NormalizedEntry` are Serialize-only projections and intentionally do not carry that attribute

## Process Notes
- All PRs target integration branch (`refactor/unified-scenarios-v2`), not `main`
- Integration branch merges to `main` only after full validation matrix passes (162/162)
- Progress file and validation status updated at end of every PR
- Every PR includes example YAML fixtures for reviewability
- Implementation plans get user approval before launching implementer
- Implementer gets requirements and constraints, not exact code blueprints
