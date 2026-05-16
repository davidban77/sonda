# sonda — The CLI

This is the binary crate. It is a **thin layer** over sonda-core. No business logic lives here.

## Responsibility

1. Parse CLI arguments using `clap` (derive API).
2. Load the YAML scenario file (file path or `@name` from a catalog directory).
3. Merge CLI flag overrides onto the loaded config.
4. Validate the merged config.
5. Instantiate the generator, encoder, and sink via sonda-core factories.
6. Hand control to the sonda-core scenario runner.
7. Handle graceful shutdown on SIGINT/SIGTERM.

If you are tempted to put signal generation, encoding, or scheduling logic here — stop. It belongs
in sonda-core.

## Module Layout

```
src/
├── main.rs             ← entrypoint, clap dispatch, orchestration
├── cli.rs              ← clap arg structs: Commands enum (Run / List / Show / New),
│                          RunArgs, ListArgs, ShowArgs, NewArgs, Verbosity enum,
│                          global --catalog and --dry-run / verbosity flags
├── config.rs           ← config loading: YAML file or @name → catalog peek → v2
│                          compile pipeline → merge CLI overrides → ScenarioConfig.
│                          Every scenario file is compiled through the v2 pipeline
│                          (`sonda_core::compile_scenario_file`); pre-v2 YAML shapes are
│                          rejected with a migration hint.
├── catalog_dir.rs      ← filesystem catalog discovery: enumerate(dir) walks YAML files
│                          and peeks frontmatter (kind / name / tags) without full
│                          deserialization; resolve(dir, name) does @name lookup.
│                          First-match-wins is a HARD ERROR on duplicate names —
│                          ambiguity is never silently resolved.
├── new/
│   ├── mod.rs          ← `sonda new` subcommand: dispatches between --template,
│   │                      --from <csv>, and interactive flow; writes to -o <path>
│   │                      or stdout
│   ├── prompts.rs      ← interactive prompt logic using dialoguer: signal type,
│   │                      scenario id, generator (metrics only), rate, duration, sink
│   ├── csv_reader.rs   ← CSV file reading for `--from <csv>`: header parsing,
│   │                      numeric column extraction
│   └── yaml_gen.rs     ← YAML rendering: minimal_template(), render_from_answers(),
│                          spec_from_pattern() — maps detected patterns to v2 YAML
│                          using operational vocabulary aliases (steady, spike_event,
│                          leak, flap, sawtooth, step)
├── dry_run.rs          ← compile-and-print path for `sonda --dry-run run`: renders
│                          either the resolved scenario text or JSON for tooling
├── progress.rs         ← live progress display during scenario execution (TTY/non-TTY
│                          aware, polls ScenarioStats via shared RwLock, all output to
│                          stderr)
└── status.rs           ← colored lifecycle banners (start/stop/config/summary) printed
                           to stderr
```

This crate stays small. Subdirectories (e.g., `new/`) are an accepted extension when a feature
requires multiple tightly-coupled files.

## CLI Surface

```
sonda [GLOBAL FLAGS] run <SCENARIO> [OPTIONS]
sonda [GLOBAL FLAGS] list --catalog <DIR> [--kind <runnable|composable>] [--tag <TAG>] [--json]
sonda [GLOBAL FLAGS] show <@NAME> --catalog <DIR>
sonda [GLOBAL FLAGS] new [--template | --from <CSV>] [-o <PATH>]
```

`<SCENARIO>` is either a path to a v2 YAML file or `@name` for catalog lookup. Every scenario file
must declare `version: 2` and `kind: runnable` (pack files declare `kind: composable`). Pre-v2
shapes are rejected with a migration hint. Pack references inside a v2 file (`pack: <name>` under
a `scenarios:` entry) are resolved by `CatalogPackResolver`, which reads the `--catalog <dir>`
directory.

### Global Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress status banners. Errors still print to stderr. |
| `--verbose` | `-v` | Show the resolved scenario config at startup. Mutually exclusive with `--quiet`. |
| `--catalog` | | Directory containing scenario and pack YAML files for `@name` resolution. Mandatory whenever a `@name` reference appears. |
| `--dry-run` | | `run` subcommand only: parse and validate the scenario, print it, exit without emitting events. Use `--format json` for machine-readable output. |

There is no `SONDA_CATALOG` env var, no implicit search path, no built-in catalog. `--catalog` is
the single discovery surface. The verbosity model is captured in the `Verbosity` enum (`Quiet`,
`Normal`, `Verbose`), constructed from `--quiet` and `--verbose` via `Verbosity::from_flags()`.

### Subcommand summary

- **`sonda run <scenario>`** — launch a scenario. Accepts a file path or `@name`. Per-run override
  flags: `--rate`, `--duration`, `--encoder`, `--sink`, `--endpoint`, `--label`, `-o/--output`,
  `--on-sink-error`. `--dry-run` parses and prints without emitting. Multi-scenario v2 files
  (multiple entries under `scenarios:`) run concurrently and respect per-entry `phase_offset`
  and `after:` clauses; final summary line aggregates totals.
- **`sonda list --catalog <dir>`** — enumerate every YAML in the catalog. Filters: `--kind`,
  `--tag`. `--json` emits machine-readable output. No dry-run (the operation is purely
  observational).
- **`sonda show <@name> --catalog <dir>`** — print the raw source YAML for a catalog entry,
  round-trippable through `sonda run`.
- **`sonda new`** — scaffold a v2 scenario YAML. Default mode is interactive (signal type →
  scenario id → generator → rate → duration → sink). `--template` dumps a minimal valid YAML
  with no prompts. `--from <csv>` scaffolds from a CSV using `sonda_core::analysis::pattern`
  and maps detected patterns to operational vocabulary aliases. `-o <path>` writes to a file;
  otherwise the YAML is printed to stdout.

All subcommands route through the unified `sonda_core::prepare_entries` + `sonda_core::launch_scenario`
API. No per-signal-type dispatch in main.rs.

## Adding a New Subcommand

The CLI is intentionally restricted to four verbs. Adding a fifth verb is an architectural decision
that should be paired with a written rationale — most workflows are better expressed by adding flags
to `sonda run` or by extending the catalog metadata that `sonda list` / `sonda show` surface. If
a new verb is genuinely warranted:

1. Add a variant to the `Commands` enum in `cli.rs`.
2. Add the corresponding clap derive struct for its flags.
3. Add a match arm in `main.rs` that calls the appropriate sonda-core API.
4. Add the verb to `sonda-server/src/main.rs`'s `SONDA_SUBCOMMANDS` so the dispatch shim
   forwards it to the sibling `sonda` binary.

The actual logic stays in sonda-core.

## Error Handling

- Use `anyhow` for top-level error reporting. The CLI is the error presentation layer.
- Map sonda-core `SondaError` variants to user-friendly messages.
- Exit code 1 on any error. Print the error to stderr.
- Do not panic. Catch errors at the top level and format them.

## Config Precedence

From lowest to highest priority:
1. YAML scenario file (the `defaults:` block and per-entry fields).
2. CLI flags (`--rate`, `--duration`, `--encoder`, etc.).

There are no `SONDA_*` env-var overrides for scenario fields. The only env var read by the CLI is
`SONDA_API_KEY`, which is consumed by `sonda-server`, not the CLI itself.

Example: if the YAML says `rate: 100` and the CLI says `--rate 500`, the effective rate is 500.

## Dependencies

This crate depends on:
- `sonda-core` (workspace dependency)
- `clap` with derive feature
- `serde` + `serde_yaml_ng` for config loading
- `serde_json` for JSON output (`sonda list --json`, `sonda --dry-run run --format json`)
- `anyhow` for error handling
- `owo-colors` for colored terminal output (with `supports-colors` feature for auto-detection)
- `dialoguer` for interactive terminal prompts in `sonda new` (pure Rust, musl-compatible)

It should NOT depend on: `axum`, `tokio`, `hyper`, or any server-related crate.
