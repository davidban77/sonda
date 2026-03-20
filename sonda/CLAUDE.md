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
├── cli.rs              ← clap arg structs (#[derive(Parser)])
└── config.rs           ← config loading: YAML file → merge CLI overrides → ScenarioConfig
```

This crate should stay small. Three files is the target. If it grows beyond five, something is in the
wrong crate.

## CLI Surface

```
sonda metrics --scenario <file.yaml>
sonda metrics --name <n> --rate <r> --duration <d> [--encoder <enc>] [--label k=v]...
sonda logs --scenario <file.yaml>          # post-MVP
```

The `metrics` subcommand is the MVP entry point. `logs` comes later.

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
- `serde` + `serde_yaml` for config loading
- `anyhow` for error handling

It should NOT depend on: `axum`, `tokio`, `hyper`, or any server-related crate.
