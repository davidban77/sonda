# ADR — v2 scenario catalog metadata

**Status:** accepted (2026-04-15). Recommendation: **Option 1 — flat top-level metadata fields**.
**Blocks:** PR 8a built-ins migration (v1 → v2 YAML).

## Context

v1 scenario files carry catalog metadata at the top level:

```yaml
# scenarios/steady-state.yaml (v1)
scenario_name: steady-state
category: infrastructure
signal_type: metrics
description: "Normal oscillating baseline (sine + jitter)"

name: node_cpu_usage_idle_percent
rate: 1
duration: 60s
generator: { type: steady, ... }
encoder: { type: prometheus_text }
sink: { type: stdout }
```

Four consumers depend on these top-level fields:

1. **`sonda/src/scenarios.rs` `read_scenario_metadata`** — probes `scenario_name`,
   `category`, `signal_type`, `description`. Produces `BuiltinScenario`
   entries the CLI `scenarios list` and `catalog list` commands render.
2. **`sonda/src/catalog.rs` `CatalogRow`** — unified row shape
   `{name, type, category, signal, description, runnable}` that drives `catalog list`.
3. **`sonda/tests/scenario_yaml_validation.rs`** — CI validation:
   - every scenario has a non-empty description
   - every `category` is in `{infrastructure, network, application, observability}`
   - every `signal_type` is in `{metrics, logs, multi, histogram, summary}`
   - every file parses as the correct v1 config type for its signal type
4. **`--category` filter** on `scenarios list` / `catalog list`.

v2 `ScenarioFile` AST, by contrast:

```rust
pub struct ScenarioFile {
    pub version: u32,
    pub defaults: Option<Defaults>,
    pub scenarios: Vec<Entry>,
}
// with deny_unknown_fields
```

No `scenario_name`, `category`, or `description` at any level. `Entry` also
has no `category` / `description`. v2 was designed as a pure compile-input
format; catalog metadata was never modeled.

## The gap

| Field | v1 source | v2 source |
|---|---|---|
| `name` | `scenario_name` or filename | filename (already works) |
| `signal_type` | top-level field | derive from first `Entry.signal_type` |
| `category` | top-level field | **missing** |
| `description` | top-level field | **missing** |

Migrating any built-in to v2 silently breaks:

- `sonda scenarios list --category infrastructure` — migrated scenarios drop out of the filtered view.
- `sonda scenarios show steady-state` — description line is empty.
- `all_descriptions_are_non_empty` CI test — fails for every migrated scenario.
- `all_categories_are_known` CI test — fails (empty → `"uncategorized"` → not in known set).

## Options

### Option 1 — Flat optional metadata fields on `ScenarioFile`

```yaml
version: 2
scenario_name: steady-state
category: infrastructure
description: "Normal oscillating baseline (sine + jitter)"

scenarios:
  - signal_type: metrics
    name: node_cpu_usage_idle_percent
    ...
```

- Add three `Option<String>` fields to `ScenarioFile`: `scenario_name`, `category`, `description`.
- Compiler ignores them (metadata, not compile input).
- Catalog probe reads top-level fields; same code path works for v1 and v2.

### Option 2 — Nested `metadata:` wrapper on `ScenarioFile`

```yaml
version: 2
metadata:
  name: steady-state
  category: infrastructure
  description: "Normal oscillating baseline (sine + jitter)"

scenarios:
  - signal_type: metrics
    name: node_cpu_usage_idle_percent
    ...
```

- New `ScenarioMetadata` struct with `name`, `category`, `description` (all `Option<String>`, `deny_unknown_fields`).
- One optional field on `ScenarioFile`: `metadata: Option<ScenarioMetadata>`.
- Catalog probe reads the sub-struct for v2, falls back to top-level fields for v1.

### Option 3 — Lenient probe, metadata-less v2

- No v2 AST change.
- Catalog probe derives what it can for v2:
  - `name` from filename.
  - `signal_type` from first entry.
  - `category = "uncategorized"`.
  - `description = ""`.
- `scenarios list --category <x>` stops filtering migrated scenarios.
- `all_descriptions_are_non_empty` CI test relaxed to "v1 or non-empty."

### Option 4 — Sidecar `.meta.yaml` per scenario

```
scenarios/
├── steady-state.yaml        # v2 compile input
└── steady-state.meta.yaml   # {name, category, description}
```

- No AST change.
- New file type, sync burden, double source of truth.

### Option 5 — Leading-comment-derived description

- Parse the YAML file's leading `#` comment block as `description`.
- No AST change.
- Still no solution for `category` — option would need to pair with (1), (2), or (3).

## Evaluation

| Option | UX preserved | AST surface | v1↔v2 probe | Effort | Reads naturally |
|---|---|---|---|---|---|
| 1. Flat optional fields | full | +3 fields on `ScenarioFile` | **same struct works for both** | small | yes — metadata at root |
| 2. `metadata:` wrapper | full | +1 field + sub-struct | version-branched probe | small | extra nesting noise |
| 3. Lenient probe | degraded | none | same struct (probe-only) | trivial | n/a — no metadata |
| 4. Sidecar | full | none | two files per scenario | small-but-forever | no — hidden second file |
| 5. Comment-derived | partial | none | comment parser | small | no — metadata in `#` prefix |

## Recommendation

**Option 1 — flat optional metadata fields at the top level.**

Reasoning:

1. **v1↔v2 parity in the catalog probe.** v1 already carries `scenario_name`,
   `category`, `description` at the root. Using the same field names at the
   v2 root makes the catalog probe one struct, one code path, no version
   branching. The current `read_scenario_metadata` implementation needs
   essentially no change — it already works.

2. **Reads naturally.** Built-in YAMLs are read by humans as often as by the
   compiler. Metadata at the top is immediately visible when scanning;
   nesting under `metadata:` adds structural noise for a minor hygiene gain.

3. **`deny_unknown_fields` coexists fine.** Three optional fields on
   `ScenarioFile` don't collide with anything in the compiler path — the
   compiler reads `version`, `defaults`, `scenarios` and ignores the rest of
   the (now-known) optional fields.

4. **Extensibility concerns are speculative.** Three fields today do not
   justify preemptive nesting for hypothetical future growth. If the
   metadata set grows to ten fields later, refactoring to a wrapper is a
   mechanical change. Early-bound nesting is the kind of design that ages
   poorly.

5. **Consistency with conventions.** Top-level metadata matches how other
   declarative YAML ecosystems (k8s manifests, containerlab, many CI
   configs) surface identity fields.

## Options considered and rejected

- **Option 2 (`metadata:` wrapper).** The only concrete advantage is
  schema hygiene — metadata segregated from compile input. Abstract benefit;
  doesn't outweigh the v1 probe-code-path consistency or the YAML
  readability cost. Reconsidered if the metadata set grows past ~six fields.

- **Option 3 (lenient probe).** Degrades `--category` filter UX and empties
  `description` for every migrated built-in. Regressing a shipped feature is
  not a transparent cost, even for a transitional migration.

- **Option 4 (sidecar `.meta.yaml`).** Permanent maintenance tax, no
  precedent in the codebase, hidden from readers who only see the scenario
  file. No justification over Option 1.

- **Option 5 (comment-derived description).** Doesn't solve `category`; has
  to combine with Option 1, 2, or 3 anyway. Comment parsing is a lossy,
  fragile channel.

## Consequences (Option 1)

**AST change:**
- `sonda-core/src/compiler/mod.rs`: add three `Option<String>` fields to
  `ScenarioFile`: `scenario_name`, `category`, `description`.
- `deny_unknown_fields` stays.
- Parser roundtrip test covering the new fields (present / absent / mixed).
- Normalize / expand / compile_after passes ignore the metadata fields —
  they do not propagate to `CompiledFile` or `PreparedEntry`.
- Prepare pass does not consume metadata.

**Catalog integration:**
- `sonda/src/scenarios.rs::read_scenario_metadata`: no change. The existing
  probe struct (`scenario_name`, `category`, `signal_type`, `description`
  all `Option<String>`) works for both v1 and v2 files because v2 puts them
  at the same root level.
- `signal_type` on v2 files remains inferable from the first entry's
  `signal_type` if not present at root — or we include it at root too for
  symmetry with v1.
- `sonda/tests/scenario_yaml_validation.rs`: parse dispatch branches on
  version (v1 → existing `ScenarioConfig`-style targets, v2 →
  `compile_scenario_file` or a lightweight v2 probe); metadata asserts (non-empty
  description, known category) work unchanged because the probe struct is the same.

**Classification:**
- Adding a field to `ScenarioFile` AST is an **architectural** change per our rubric (touches compiler AST). Routes to Opus `@reviewer` direct, plus `@uat` + `@doc`.
- Once landed, the PR 8a sub-slices are:
  - **Sub-slice 1** (this ADR's implementation): add `metadata:` AST + catalog probe + migrate one scenario (e.g. `steady-state.yaml`) as first consumer. Architectural class.
  - **Sub-slice 2**: batch-migrate remaining 10 scenarios. Pure YAML. Mechanical class.
  - **Sub-slice 3**: story + dedup + `docs/site` updates. User-visible class.

Each sub-slice dogfoods a different Plan B tier — best possible dogfood structure.

**Forward compat:**
- v2 AST change only adds an optional field — existing v2 files and tests continue to parse.
- `metadata:` sub-struct can grow without breaking callers.

## Status

accepted 2026-04-15. Sub-slice 1 implements Option 1: add three optional
top-level metadata fields (`scenario_name`, `category`, `description`) to
`ScenarioFile`, migrate `scenarios/steady-state.yaml` as the first consumer.
