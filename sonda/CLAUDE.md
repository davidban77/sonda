# sonda — The CLI

This is the binary crate. It is a **thin layer** over sonda-core. No business logic lives here.

## Responsibility

1. Parse CLI arguments using `clap` (derive API).
2. Load the YAML scenario file (if provided).
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
├── main.rs             ← entrypoint, clap setup, orchestration
├── cli.rs              ← clap arg structs (#[derive(Parser)]), Verbosity enum,
│                          ScenariosArgs/ScenariosAction for the `scenarios` subcommand,
│                          PacksArgs/PacksAction for the `packs` subcommand,
│                          --pack-path and --scenario-path global flags
├── config.rs           ← config loading: YAML file or @name → merge CLI overrides → ScenarioConfig,
│                          resolve_scenario_source (@name shorthand via ScenarioCatalog),
│                          parse_builtin_scenario, load_pack_from_catalog, resolve_pack_source,
│                          is_pack_config, load_pack_from_yaml
├── packs.rs            ← filesystem-based metric pack discovery: PackCatalog, PackEntry,
│                          build_search_path(). Scans directories for pack YAML files and
│                          caches results for the CLI invocation.
│                          Search path (priority): --pack-path > SONDA_PACK_PATH > ./packs/ >
│                          ~/.sonda/packs/
├── scenarios.rs        ← filesystem-based scenario discovery: ScenarioCatalog,
│                          build_search_path(). Scans directories for scenario YAML files
│                          with metadata (scenario_name, category, signal_type, description).
│                          Search path (priority): --scenario-path > SONDA_SCENARIO_PATH >
│                          ./scenarios/ > ~/.sonda/scenarios/
├── import/
│   ├── mod.rs          ← `sonda import` subcommand: top-level orchestration (analyze, generate,
│   │                      run modes), parse_column_list()
│   ├── csv_reader.rs   ← CSV file reading: header detection via sonda-core csv_header,
│   │                      numeric data extraction, column selection, ColumnMeta, CsvData
│   ├── pattern.rs      ← time-series pattern detection (statistical analysis):
│   │                      detect_pattern() → Pattern enum (Steady, Spike, Climb, Sawtooth,
│   │                      Flap, Step). All heuristics are in the CLI crate, not sonda-core.
│   └── yaml_gen.rs     ← YAML scenario generation from detected patterns: pattern_to_spec(),
│                          render_yaml(). Maps patterns to operational vocabulary aliases
│                          (steady, spike_event, leak, flap) or base generators (sawtooth, step).
├── init/
│   ├── mod.rs          ← `sonda init` subcommand: top-level orchestration (run_init),
│   │                      InitResult (yaml + run_now flag + InitScenarioType for dispatch),
│   │                      build_prefill() merges --from data with CLI flags into Prefill,
│   │                      prefill_from_scenario() extracts fields from @name scenario YAML
│   │                      (including labels), prefill_from_csv() detects pattern from CSV
│   │                      and maps to alias (gracefully handles no-numeric-columns),
│   │                      welcome banner, YAML preview, success summary, run-now prompt
│   │                      (bypassed by --run-now flag or non-TTY stdin),
│   │                      print_prefill_summary() shows pre-filled values before prompts
│   ├── prompts.rs      ← interactive prompt logic using dialoguer: signal type, domain,
│   │                      approach (single metric vs pack), situation selection,
│   │                      situation-specific parameters (bypassed with defaults when
│   │                      situation is prefilled), labels, rate (validated > 0), duration
│   │                      (validated via parse_duration), encoder, sink.
│   │                      Histogram and summary prompt flows: distribution model selection,
│   │                      distribution-specific parameters, observations per tick,
│   │                      bucket/quantile boundaries, and seed. All distribution-related
│   │                      prompts are bypassed with sensible defaults when signal_type
│   │                      is prefilled (non-interactive mode).
│   │                      default_distribution_params() returns defaults matching the
│   │                      interactive prompts for each distribution model.
│   │                      Prefill struct carries optional pre-filled values for each prompt
│   │                      including log-specific (message_template, severity) and
│   │                      sink-specific (kafka_brokers, kafka_topic, otlp_signal_type).
│   │                      Each prompt fn checks prefill — valid value skips the prompt,
│   │                      invalid value warns and falls through to interactive.
│   │                      Two-tier sink menu (primary: stdout/http_push/file; advanced:
│   │                      remote_write/loki/otlp_grpc/kafka/tcp/udp behind "Advanced...").
│   │                      Prefilled advanced sinks populate extra fields from prefill.
│   │                      Pack domain filtering (list_by_category, fallback to all).
│   │                      enforce_encoder_for_sink() auto-overrides encoder for protocol sinks.
│   │                      prompt_run_now() offers immediate execution after file write.
│   └── yaml_gen.rs     ← YAML rendering from collected answers: ScenarioKind, MetricAnswers,
│                          PackAnswers, LogAnswers, HistogramAnswers, SummaryAnswers,
│                          DeliveryAnswers. InitScenarioType enum
│                          (SingleMetric/Pack/Logs/Histogram/Summary) for typed dispatch
│                          in run-now path.
│                          required_encoder_for_sink() maps sink→encoder constraints.
│                          render_sink() handles all sink types incl. advanced YAML fields.
├── yaml_helpers.rs     ← shared YAML formatting and quoting utilities: ParamValue, needs_quoting(),
│                          escape_yaml_double_quoted(), format_float(), format_rate().
│                          Used by both init/yaml_gen and import/yaml_gen.
├── story/
│   ├── mod.rs          ← `sonda story` subcommand: StoryConfig, SignalConfig, compile_story(),
│   │                      signal→ScenarioEntry expansion. Stories are a concise YAML format
│   │                      for multi-signal temporal scenarios that compiles to
│   │                      Vec<ScenarioEntry> + phase_offset at parse time.
│   └── after_resolve.rs ← AfterClause parsing, dependency graph, topological sort (Kahn's
│                          algorithm), cycle detection, and phase_offset computation.
│                          Threshold-crossing math lives in `sonda_core::compiler::timing`
│                          (shared with the v2 compiler's Phase 4 `after` resolution).
├── progress.rs         ← live progress display during scenario execution (TTY/non-TTY aware,
│                          polls ScenarioStats via shared RwLock, all output to stderr)
└── status.rs           ← colored lifecycle banners (start/stop/config/summary) printed to stderr
```

This crate should stay small. Seven files plus subdirectory modules for complex features is the
target. Subdirectories (e.g., `import/`) are an accepted extension when a feature requires
multiple tightly-coupled files. If top-level file count grows beyond seven or a subdirectory
exceeds four files, something may belong in sonda-core.

## CLI Surface

```
sonda [--quiet | --verbose] [--dry-run] metrics --scenario <file.yaml | @builtin-name>
sonda [--quiet | --verbose] [--dry-run] metrics --name <n> --rate <r> --duration <d> [--encoder <enc>] [--precision <0-17>] [--label k=v]... [--sink <type> --endpoint <url> ...]
sonda [--quiet | --verbose] [--dry-run] logs --scenario <file.yaml | @builtin-name>
sonda [--quiet | --verbose] [--dry-run] logs --mode <mode> [--sink <type> --endpoint <url> ...]
sonda [--quiet | --verbose] [--dry-run] histogram --scenario <file.yaml | @builtin-name>
sonda [--quiet | --verbose] [--dry-run] run --scenario <multi-scenario.yaml | @builtin-name>
sonda scenarios list [--category <cat>] [--json]
sonda scenarios show <name>
sonda [--quiet | --verbose] [--dry-run] scenarios run <name> [--duration <d>] [--rate <r>] [--sink <type>] [--endpoint <url>] [--encoder <enc>]
sonda [--pack-path <dir>] packs list [--category <cat>] [--json]
sonda [--pack-path <dir>] packs show <name>
sonda [--quiet | --verbose] [--dry-run] packs run <name> [--duration <d>] [--rate <r>] [--sink <type>] [--endpoint <url>] [--encoder <enc>] [--label k=v]...
sonda import <file.csv> --analyze
sonda import <file.csv> -o <output.yaml> [--columns <1,3,5>] [--rate <r>] [--duration <d>]
sonda [--quiet | --verbose] import <file.csv> --run [--columns <1,3,5>] [--rate <r>] [--duration <d>]
sonda init [--from <@name | path.csv>] [--signal-type <metrics|logs|histogram|summary>] [--domain <cat>] [--situation <alias>] [--metric <name>] [--pack <name>] [--rate <r>] [--duration <d>] [--encoder <enc>] [--sink <type>] [--endpoint <url>] [-o <path>] [--label k=v]... [--run-now] [--message-template <tpl>] [--severity <preset>] [--kafka-brokers <addrs>] [--kafka-topic <topic>] [--otlp-signal-type <type>]
sonda [--quiet | --verbose] [--dry-run] story --file <story.yaml> [--duration <d>] [--rate <r>] [--sink <type>] [--endpoint <url>] [--encoder <enc>]
```

The `--scenario` flag accepts either a filesystem path or a `@name` shorthand that resolves
a scenario from the filesystem catalog discovered via the scenario search path. Example:
`sonda metrics --scenario @cpu-spike`.

The `run --scenario` path also detects YAML files with a `pack:` field and expands them
via `sonda_core::packs::expand_pack` before feeding into `prepare_entries()`.

### Global Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress all status banners (start/stop/summary). Errors are still printed to stderr. |
| `--verbose` | `-v` | Show the resolved scenario config at startup, then run normally with start/stop banners. Mutually exclusive with `--quiet`. |
| `--dry-run` | | Parse and validate the scenario config, print it, then exit without emitting any events. Orthogonal to `--quiet`/`--verbose` — always prints the resolved config. |
| `--pack-path` | | Directory containing metric pack YAML files. When set, this is the sole search path for packs -- `SONDA_PACK_PATH`, `./packs/`, and `~/.sonda/packs/` are not consulted. |
| `--scenario-path` | | Directory containing scenario YAML files. When set, this is the sole search path for scenarios -- `SONDA_SCENARIO_PATH`, `./scenarios/`, and `~/.sonda/scenarios/` are not consulted. |

The verbosity model is captured in the `Verbosity` enum (`Quiet`, `Normal`, `Verbose`), constructed
from the `--quiet` and `--verbose` flags via `Verbosity::from_flags()`. `--dry-run` is orthogonal.

The `metrics` subcommand is the MVP entry point. `logs` emits log events. `histogram` generates
Prometheus-style histogram data. `summary` generates Prometheus-style summary data. `run` runs
multiple scenarios concurrently from a single YAML file whose `scenarios:` list carries
`signal_type: metrics`, `logs`, `histogram`, or `summary` entries -- or from a YAML file with a
`pack:` field that references a metric pack. `scenarios` discovers scenario YAML files from the
search path (`--scenario-path`, `SONDA_SCENARIO_PATH`, `./scenarios/`, `~/.sonda/scenarios/`):
`list` to browse, `show` to dump YAML, `run` to execute. `packs` provides access to metric pack
files: `list` to browse, `show` to dump YAML, `run` to execute with rate/duration/sink/encoder
overrides. `import` analyzes a CSV file, detects time-series patterns (steady, spike, climb,
flap, sawtooth, step), and generates a portable scenario YAML using generators instead of
csv_replay. Three modes: `--analyze` (read-only), `-o` (write YAML), `--run` (generate + execute).
`init` walks through an interactive prompt flow and generates a commented, runnable scenario YAML.
Uses operational vocabulary aliases (steady, spike_event, flap, etc.) and supports metric packs
with domain-filtered selection. Two-tier sink menu (primary + advanced behind "Advanced...") with
automatic encoder override for protocol sinks (remote_write, otlp_grpc). After writing the file,
offers immediate execution via the run-now prompt (dispatched by InitScenarioType).
Supports fully non-interactive mode via CLI flags (`--signal-type`, `--domain`, `--situation`,
`--metric`, `--rate`, `--duration`, `--encoder`, `--sink`, `-o`, `--run-now`, etc.) and
pre-filling from built-in scenarios (`--from @name`) or CSV files (`--from path.csv`).
CLI flags override `--from` values. Pre-filled values skip their interactive prompts; missing
values prompt as usual (partial non-interactive mode). Situation-specific parameters use
defaults when the situation is prefilled. Log-specific prompts (message template, severity)
and sink-specific extra fields (kafka brokers/topic, OTLP signal type) are also prefillable.
Rate and duration are validated; invalid values warn and fall through.

`story` runs a story file -- a multi-signal format with temporal causality. Stories
compile down to `Vec<ScenarioEntry>` + `phase_offset` at parse time (no runtime reactivity).
Signals use `after` clauses (e.g., `after: metric_name < 1`) that resolve to concrete
`phase_offset` values via deterministic timing math. Supported behaviors for `after`
resolution: `flap`, `saturation`, `leak`, `degradation`, `spike_event`. `steady` is
rejected (ambiguous sine crossings). Story files live in `stories/` at the repo root.
CLI flags (`--duration`, `--rate`, `--sink`, `--endpoint`, `--encoder`) override story-level
shared fields but not per-signal overrides.

All subcommands go through the unified `sonda_core::prepare_entries` +
`sonda_core::launch_scenario` API introduced in Slice 3.0. No per-signal-type dispatch in main.rs.

The `run` subcommand prints an aggregate summary line after all scenarios complete, showing total
scenarios, events, bytes, errors, and elapsed time.

### Discovery Search Paths

Both packs and scenarios use the same priority: CLI flag (sole path) > env var (colon-separated) >
`./packs/` or `./scenarios/` > `~/.sonda/packs/` or `~/.sonda/scenarios/`. Non-existent dirs
silently skipped. First-match-wins on name collisions. See `packs.rs` and `scenarios.rs` for details.

## Adding a New Subcommand

1. Add a variant to the `Commands` enum in `cli.rs`.
2. Add the corresponding clap derive struct for its flags.
3. Add a match arm in `main.rs` that:
   - Loads config.
   - Calls the appropriate sonda-core runner.
4. That's it. The actual logic stays in sonda-core.

## Error Handling

- Use `anyhow` for top-level error reporting. The CLI is the error presentation layer.
- Map sonda-core `SondaError` variants to user-friendly messages.
- Exit code 1 on any error. Print the error to stderr.
- Do not panic. Catch errors at the top level and format them.

## Config Precedence

From lowest to highest priority:
1. YAML scenario file
2. `SONDA_*` environment variables
3. CLI flags

Example: if the YAML says `rate: 100` and the CLI says `--rate 500`, the effective rate is 500.

## Dependencies

This crate depends on:
- `sonda-core` (workspace dependency)
- `clap` with derive feature
- `serde` + `serde_yaml_ng` for config loading
- `serde_json` for JSON output in `scenarios list`
- `anyhow` for error handling
- `owo-colors` for colored terminal output (with `supports-colors` feature for auto-detection)
- `dialoguer` for interactive terminal prompts in `sonda init` (pure Rust, musl-compatible)

It should NOT depend on: `axum`, `tokio`, `hyper`, or any server-related crate.
