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
│                          ScenariosArgs/ScenariosAction for the `scenarios` subcommand
├── config.rs           ← config loading: YAML file or @builtin → merge CLI overrides → ScenarioConfig,
│                          resolve_scenario_source (@name shorthand), parse_builtin_scenario
├── progress.rs         ← live progress display during scenario execution (TTY/non-TTY aware,
│                          polls ScenarioHandle stats, all output to stderr)
└── status.rs           ← colored lifecycle banners (start/stop/config/summary) printed to stderr
```

This crate should stay small. Three to five files is the target. If it grows beyond five, something
is in the wrong crate.

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
```

The `--scenario` flag accepts either a filesystem path or a `@name` shorthand that resolves
a built-in scenario from the embedded catalog (see `sonda_core::scenarios`). Example:
`sonda metrics --scenario @cpu-spike`.

### Global Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress all status banners (start/stop/summary). Errors are still printed to stderr. |
| `--verbose` | `-v` | Show the resolved scenario config at startup, then run normally with start/stop banners. Mutually exclusive with `--quiet`. |
| `--dry-run` | | Parse and validate the scenario config, print it, then exit without emitting any events. Orthogonal to `--quiet`/`--verbose` — always prints the resolved config. |

The verbosity model is captured in the `Verbosity` enum (`Quiet`, `Normal`, `Verbose`), constructed
from the `--quiet` and `--verbose` flags via `Verbosity::from_flags()`. `--dry-run` is orthogonal.

The `metrics` subcommand is the MVP entry point. `logs` emits log events. `histogram` generates
Prometheus-style histogram data. `summary` generates Prometheus-style summary data. `run` runs
multiple scenarios concurrently from a single YAML file whose `scenarios:` list carries
`signal_type: metrics`, `logs`, `histogram`, or `summary` entries. `scenarios` provides access
to the built-in scenario library: `list` to browse, `show` to dump YAML, `run` to execute.

All subcommands go through the unified `sonda_core::prepare_entries` +
`sonda_core::launch_scenario` API introduced in Slice 3.0. No per-signal-type dispatch in main.rs.

The `run` subcommand prints an aggregate summary line after all scenarios complete, showing total
scenarios, events, bytes, errors, and elapsed time.

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

It should NOT depend on: `axum`, `tokio`, `hyper`, or any server-related crate.
