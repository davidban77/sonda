---
title: Run as a CLI
description: Run Sonda scenarios from the command line — laptop, CI runner, or ad-hoc.
---

# Run as a CLI

The `sonda` binary runs one scenario, prints lifecycle banners to stderr, writes data to stdout, and exits. The same binary works on your laptop and in CI. This page covers the common workflows. For the full flag list, see [CLI flags](../reference/cli-flags.md).

## Run a scenario file

The smallest workflow is to pass a YAML file to `sonda run`.

```bash
sonda run hello.yaml
```

Sonda prints a start banner to stderr, writes events to stdout, then prints a stop banner with totals. Redirect stdout to keep the banner text out of your data:

```bash
sonda run hello.yaml > metrics.txt
```

You can override any field for a single run without editing the file. `--rate`, `--duration`, `--sink`, `--endpoint`, `--encoder`, and `--label k=v` all override the `defaults:` block in the YAML:

```bash
sonda run hello.yaml \
  --rate 500 \
  --duration 10s \
  --sink http_push \
  --endpoint http://victoriametrics:8428/api/v1/import/prometheus
```

## Run a catalog entry with `@name`

When you have more than one scenario file, point `--catalog <dir>` at the directory and refer to entries by name. Sonda walks the directory and resolves `@cpu-spike` to the file with `name: cpu-spike` or filename `cpu-spike.yaml`.

```bash
sonda --catalog ~/sonda-catalog list
sonda --catalog ~/sonda-catalog run @cpu-spike
```

The `--catalog` flag also resolves `pack:` references inside a scenario file. See [Catalogs and packs](../build/catalogs-and-packs.md).

## Override URLs with environment variables

A hardcoded `url: http://localhost:8428` in your YAML breaks when you run the same scenario in a different network. Sonda's `${VAR:-default}` interpolation lets one file work from both your host CLI and a containerized `sonda-server`:

```yaml title="sink block"
sink:
  type: http_push
  url: "${VICTORIAMETRICS_URL:-http://localhost:8428/api/v1/import/prometheus}"
```

```bash
# Host CLI — VICTORIAMETRICS_URL is unset, the default applies
sonda run my-scenario.yaml

# Container override
VICTORIAMETRICS_URL=http://vm.example.com:8428/api/v1/import/prometheus \
  sonda run my-scenario.yaml
```

Seven built-in variable names cover the bundled Compose backends. For the full table see [Scenario file format — Environment variable interpolation](../build/scenario-files.md#environment-variable-interpolation).

## Validate before running

`--dry-run` parses and validates the scenario, prints the resolved config, and exits without writing events. Use it in CI before a long run, or to confirm a YAML edit is valid:

```bash
sonda --dry-run run my-scenario.yaml
```

JSON output is available for scripting:

```bash
sonda --dry-run --format json run my-scenario.yaml | jq '.scenarios[0].rate'
```

## Run in CI

In CI you want quiet output, machine-readable exit codes, and backend URLs from environment variables. `--quiet` hides banners and progress so stdout carries only the event stream:

```bash title=".github/workflows/sonda-smoke.yml (snippet)"
- name: Push synthetic baseline
  env:
    VICTORIAMETRICS_URL: ${{ secrets.VM_URL }}
  run: |
    sonda -q run examples/basic-metrics.yaml
```

Exit codes are scriptable:

| Code | Meaning |
|------|---------|
| `0` | Scenario completed without errors |
| `1` | Runtime failure: sink unreachable, YAML rejected, or scenario errored mid-run |
| `2` | Clap parse error: unknown flag or unrecognized subcommand |

For long CI jobs that should fail on the first sink error, set `on_sink_error: fail` in `defaults:` (or pass `--on-sink-error fail`). The scenario stops on the first failure instead of warning and continuing.

## Stop cleanly

A scenario runs to its `duration:` and exits on its own. To stop it early, send `SIGINT` (Ctrl+C) or `SIGTERM`. Sonda signals every running entry to stop, flushes pending sink buffers, prints the stop banners, and exits with code 0:

```bash
sonda run long-running.yaml
# ^C
# ■ cpu_usage  completed in 12.3s | events: 123 | bytes: 4.5 KB | errors: 0
```

For sinks with `retry:` configured, in-progress retries finish or time out per the retry policy before the binary exits. Scenarios sent through `sonda-server` follow the same shutdown path. See [As a server](server.md).

## Where to next

- [CLI flags](../reference/cli-flags.md) — every flag and every subcommand.
- [Scenario file format](../build/scenario-files.md) — full file shape, including `defaults:` and environment variable interpolation.
- [Run as a server](server.md) — for Sonda as a long-running HTTP service.
- [Docker](docker.md) — running the CLI from a container.
