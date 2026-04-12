# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 2 — Defaults resolution (complete, pending merge)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 4 — Pack expansion inside `scenarios:`

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | Done | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Done | PR 3 | 2026-04-12 |
| 3 | Pack expansion in scenarios | Not Started | PR 4 | |
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
| 3 | Defaults resolution + `parse_v2 → parse` rename | `feat/defaults-resolution` | integration | Pending Review | 2026-04-12 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| Compiler parser unit tests | 49 | AST parsing, validation, shorthand, edge cases |
| Compiler normalize unit tests | 34 | Defaults inheritance, label merge (inline eager / pack deferred), built-in fallbacks, missing-rate error, defaults-labels surfacing |
| Compiler fixture integration tests | 15 | Valid/invalid YAML examples parsed + normalized from disk |
| Compile snapshot golden files | 12 | v1 parity baseline (6 fixtures x raw+prepared) |
| Normalize snapshot golden files | 3 | Resolved defaults snapshots (label merge, logs default encoder, pack entry) |
| **New in refactor** | **113** | |
| Workspace total | 2,585 | All existing + new |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 162-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 11 of 162 rows addressed so far.

| Section | Rows | Addressed | Notes |
|---------|------|-----------|-------|
| 1-10. Feature parity | 97 | 5 | 5.8 (PR 3), 10.12-10.15 (PR 3) — rest need runtime wiring |
| 11. New v2 features | 18 | 6 | 11.1, 11.2, 11.3, 11.4, 11.5, 11.10 |
| 12-15. CLI/Server/UX/Deploy | 47 | 0 | Later PRs (7-9) |
| **16. Scenario parity bridge** | **12** | **0** | **v1→v2 compile + runtime for all built-ins + story** |
| **17. Pack parity bridge** | **3** | **0** | **v1→v2 compile + runtime for all built-in packs** |

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

## PR 4 Preparation Notes

These notes capture decisions and handoff context from PR 3 that PR 4 (pack expansion) must not re-litigate. A future session starting cold on PR 4 should read this section first, then the full reviewer review thread if deeper context is needed.

### Label composition decision (locked)

PR 3 applies **two different label strategies** depending on entry kind. This is a deliberate choice to let PR 4 interleave spec §2.2 precedence levels 4–5 (pack `shared_labels`, pack per-metric labels) at the correct position between levels 2 (`defaults.labels`) and 6 (entry-level labels).

- **Inline entries** (`generator:` / `log_generator:`) → `NormalizedEntry.labels` is the eager merge `defaults.labels ∪ entry.labels`, entry wins on conflict. No downstream composition, so provenance doesn't matter.
- **Pack entries** (`pack:`) → `NormalizedEntry.labels` is exactly the entry's own `labels` field (unchanged, possibly `None`). The file-level `defaults.labels` is carried forward separately on `NormalizedFile.defaults_labels` for PR 4 to apply at the correct precedence slot.

Do not "fix" this asymmetry in PR 4 by eagerly merging. It exists because §2.2 places pack shared_labels (level 4) between defaults (2) and entry labels (6) — collapsing 2+6 loses the position where 4 and 5 need to slot in.

### PR 4 expansion sketch (implementer's handoff, confirmed)

1. **`NormalizedEntry` contract walk.** Before writing expansion code, walk every field of `NormalizedEntry` and classify it as either (a) propagates verbatim to each expanded child metric, or (b) participates in per-metric composition. `normalize.rs` groups fields with comments to help. Pack `overrides` definitely participates (per-metric generator/labels/after). Most schedule/delivery fields (rate, duration, encoder, sink, gaps, bursts, phase_offset, clock_group) propagate verbatim.

2. **Label precedence chain for pack-expanded signals** (spec §2.2, low → high — lowest number = lowest precedence, applied first; each subsequent level overwrites on key collision):
   1. Sonda built-in defaults (already resolved in PR 3 for non-label fields)
   2. `NormalizedFile.defaults_labels` (new source; not yet applied to pack entries)
   3. pack definition's top-level fields (shared rate/job, etc. — pack YAML)
   4. pack `shared_labels`
   5. pack per-metric `labels`
   6. entry-level `labels` on the pack entry (already preserved on `NormalizedEntry.labels`)
   7. override-level `labels` (from `NormalizedEntry.overrides[metric].labels`)
   8. CLI flags (PR 7 scope)

   A clean multi-level merge function with explicit precedence-named steps is preferable to nested merge calls.

3. **Override key validation (matrix row 9.7).** For every key in `NormalizedEntry.overrides`, assert it matches a metric name in the pack definition. Unknown keys → `NormalizeError` or a new `PackExpansionError` with a clear message. This is a mandatory merge blocker.

4. **Pack entry materialization.** One `NormalizedEntry`-equivalent (or a new `ExpandedEntry` type — PR 4's call) per metric in the pack. Synthesize:
   - `name = <pack_metric_name>`
   - `id = "{entry.id}.{metric}"` when `entry.id.is_some()`; otherwise an auto-generated ID from the pack name (see matrix row 11.8)
   - `generator` = override's `generator` if present, else pack's per-metric `generator`
   - `labels` = result of the full level-2-through-7 merge above
   - `after` = override's `after` if present, else entry-level `after` propagated (matrix row 11.13)
   - All other fields (rate, duration, encoder, sink, phase_offset, clock_group, gaps, bursts) copied from the parent pack entry verbatim

5. **Pack search path.** Pack resolution is not yet implemented in `sonda-core::compiler`. The existing v1 engine in `src/packs/mod.rs` handles pack YAML parsing and has a search path helper; reuse `MetricPackDef` and the discovery logic. PR 4 does not need to reshape the pack definition schema (spec §7 is explicit: pack YAML on disk is unchanged). Pack YAMLs live at the repo root in `packs/` — this crate doesn't embed them.

6. **Pack entries without `id`.** Spec matrix row 11.8 requires auto-generated IDs when `id` is absent. Pick a deterministic scheme (e.g., `"{pack_name}"` for the first anonymous pack entry, disambiguate subsequent ones). Decide before coding.

7. **`NormalizedEntry` field evolution.** Currently carries optional serde fields for pack metadata. If PR 4 introduces a new `ExpandedEntry` type (recommended), `NormalizedEntry` can stay narrow — don't bloat it with post-expansion fields.

### Target validation matrix rows for PR 4

- 9.1–9.12 (pack features — run by name, run from YAML, search path, file path, overrides, unknown override key error, label merge order, dry-run, list/show, custom pack definitions)
- 11.6 (pack inside scenarios: list)
- 11.8 (auto-generated pack IDs)
- 11.12 (after on pack override — partial; full `after` resolution is PR 5)
- 11.13 (pack entry-level after propagation — partial)

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
