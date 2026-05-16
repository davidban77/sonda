# CLI Reference

The `sonda` binary has four verbs: `run`, `list`, `show`, and `new`. `run` executes a [v2 scenario YAML file](v2-scenarios.md), `list` and `show` browse a catalog directory of scenarios and composable packs, and `new` scaffolds a starter file. Anything that used to live behind a per-signal subcommand (`metrics`, `logs`, `histogram`, `summary`) is now a v2 scenario YAML — pick a generator in the file, point `sonda run` at it.

## Global options

```
sonda [OPTIONS] <run|list|show|new> [ARGS...]
```

| Flag | Short | Description |
|------|-------|-------------|
| `--catalog <DIR>` | -- | Directory holding scenario and pack YAML files. Required when resolving `@name` references with `run` / `show`, and required for `list`. No env-var fallback, no home-directory scan. |
| `--quiet` | `-q` | Suppress start/stop banners and live progress. Errors still print to stderr. Mutually exclusive with `--verbose`. |
| `--verbose` | `-v` | Print the resolved scenario config at startup, then run normally. Mutually exclusive with `--quiet`. |
| `--dry-run` | -- | Parse and validate the scenario, print the resolved config, exit without emitting events. |
| `--format <FORMAT>` | -- | Output format for `--dry-run`: `text` (default) or `json`. Only meaningful with `--dry-run run`. |
| `--help` | `-h` | Print help and exit. |
| `--version` | `-V` | Print the version and exit. |

Global flags go **before** the subcommand:

```bash
sonda --catalog ~/my-scenarios -q run @cpu-spike
sonda --catalog ~/my-scenarios --dry-run --format json run @cpu-spike
```

Subcommands from earlier releases (`metrics`, `logs`, `histogram`, `summary`, `scenarios`, `packs`, `catalog`, `import`, `init`) and the global flags `--scenario-path` / `--pack-path` (plus the `SONDA_SCENARIO_PATH` / `SONDA_PACK_PATH` env vars) were removed in 1.9. Clap rejects them with `unrecognized subcommand`.

## `sonda run`

Run a scenario from a YAML file or a `@name` catalog reference.

```
sonda [--catalog <DIR>] [--dry-run] [--format text|json] run <SCENARIO> [OVERRIDES...]
```

| Argument / Flag | Description |
|------|-------------|
| `<SCENARIO>` | Filesystem path to a v2 YAML file (`./my.yaml`, `examples/cpu.yaml`) **or** `@name` for a catalog entry (requires `--catalog`). |
| `--catalog <DIR>` | Required when `<SCENARIO>` starts with `@`. Also used to resolve `pack: <name>` references inside the file. |
| `--dry-run` | Compile and validate the scenario, print the resolved config, exit. |
| `--format text\|json` | Dry-run output format. Default `text`. |
| `--duration <D>` | Override `defaults.duration`. Accepts `30s`, `5m`, `1h`. |
| `--rate <R>` | Override `defaults.rate` (events per second). |
| `--sink <TYPE>` | Override the sink type (`stdout`, `file`, `tcp`, `udp`, `http_push`, `loki`, ...). |
| `--endpoint <URL>` | Override the sink endpoint. |
| `--encoder <TYPE>` | Override the encoder type. |
| `-o <PATH>` | Shortcut for `--sink file --endpoint <PATH>`. Conflicts with `--sink`. |
| `--label k=v` | Add a static label. Repeatable. |
| `--on-sink-error warn\|fail` | Override `defaults.on_sink_error`. |

### Run a file

```bash title="examples/cpu-spike.yaml"
sonda run examples/cpu-spike.yaml
```

Sonda prints a start banner, runs the scenario, and prints a stop banner with totals.

### Run a catalog entry

```bash
sonda --catalog ~/sonda-catalog run @cpu-spike
```

The `@cpu-spike` reference resolves to a YAML file in `~/sonda-catalog/` whose header has `kind: runnable` and either `name: cpu-spike` or a filename matching `cpu-spike.yaml`.

### Override at the command line

CLI flags win over `defaults:` inside the file. Useful for a one-off rate bump or pointing the same scenario at a different sink:

```bash title="examples/cpu-spike.yaml"
sonda run examples/cpu-spike.yaml \
  --rate 500 \
  --duration 10s \
  --sink http_push --endpoint http://victoriametrics:8428/api/v1/import/prometheus
```

### Dry-run

`--dry-run` compiles the scenario and prints the resolved configuration, then exits. Use it to validate a file before a long run:

=== "Text (default)"

    ```bash
    sonda --catalog ~/sonda-catalog --dry-run run @cpu-spike
    ```

    ```text title="Output"
    [config] file: @cpu-spike (version: 2, 1 scenario)

    [config] [1/1] cpu_usage_percent

        name:           cpu_usage_percent
        signal:         metrics
        rate:           10/s
        duration:       30s
        generator:      spike_event (baseline: 20, spike: 95, duration: 10s)
        encoder:        prometheus_text
        sink:           stdout

    Validation: OK (1 scenario)
    ```

=== "JSON"

    ```bash
    sonda --catalog ~/sonda-catalog --dry-run --format json run @cpu-spike
    ```

    ```json title="Output"
    {
      "file": "@cpu-spike",
      "version": 2,
      "scenarios": [
        {
          "index": 1,
          "name": "cpu_usage_percent",
          "signal": "metrics",
          "rate": 10.0,
          "duration": "30s",
          "generator": "spike_event (baseline: 20, spike: 95, duration: 10s)",
          "encoder": "prometheus_text",
          "sink": "stdout",
          "labels": {},
          "phase_offset": null,
          "clock_group": null,
          "clock_group_is_auto": false
        }
      ]
    }
    ```

## `sonda list`

Enumerate catalog entries — both `runnable` scenarios and `composable` packs — from a directory.

```
sonda --catalog <DIR> list [--kind runnable|composable] [--tag <TAG>] [--json]
```

| Flag | Description |
|------|-------------|
| `--catalog <DIR>` | **Required.** Directory to enumerate. |
| `--kind runnable\|composable` | Filter by entry kind. Default: both. |
| `--tag <TAG>` | Only include entries whose `tags:` array contains this value. |
| `--json` | Emit a stable JSON array on stdout instead of the default tab-separated table. |

The default output is a tab-separated table with four columns: `KIND`, `NAME`, `TAGS` (comma-joined), `DESCRIPTION`. Pipe it into `column -t -s$'\t'` for aligned reading.

```bash
sonda --catalog ~/sonda-catalog list
```

```text title="Output"
KIND	NAME	TAGS	DESCRIPTION
runnable	cpu-spike	cpu,infrastructure	CPU spike to 95% for 30 seconds
runnable	memory-leak	memory,leak	Slow memory leak from baseline to ceiling
composable	prom-text-stdout	defaults	Shared prometheus_text + stdout defaults
```

Filter to runnable entries tagged `cpu`:

```bash
sonda --catalog ~/sonda-catalog list --kind runnable --tag cpu
```

`--json` emits a stable array on stdout; each element has `name`, `kind`, `description`, `tags`, and the resolved `source` path. Use it as the contract when scripting catalog discovery.

Files without a recognized `kind:` header are silently skipped. Files with an unparseable YAML body print a warning to stderr and are skipped — the listing continues.

## `sonda show`

Print the raw source YAML for a catalog entry, exactly as it lives on disk.

```
sonda --catalog <DIR> show <@NAME>
```

| Argument / Flag | Description |
|------|-------------|
| `<@NAME>` | Catalog entry name. The leading `@` is optional — `show cpu-spike` and `show @cpu-spike` both work. |
| `--catalog <DIR>` | **Required.** Directory containing the entry. |

Works for both `kind: runnable` and `kind: composable` entries. Output is the file contents byte-for-byte. For runnable entries, it round-trips through `sonda --dry-run run`:

```bash title="/tmp/snap.yaml"
sonda --catalog ~/sonda-catalog show @cpu-spike > /tmp/snap.yaml
sonda --dry-run run /tmp/snap.yaml
```

## `sonda new`

Scaffold a new v2 scenario YAML. Three modes — pick the one that matches how much you already know about the scenario you want.

```
sonda new [--template] [--from <CSV>] [-o <PATH>]
```

| Flag | Description |
|------|-------------|
| (no flags) | Interactive [dialoguer](https://crates.io/crates/dialoguer) flow. Walks through signal type → generator → rate → duration → sink type → output path. Requires a TTY on stdin. |
| `--template` | Print a minimal valid YAML to stdout and exit. No prompts. |
| `--from <CSV>` | Seed the scaffold from a CSV file. Runs pattern detection on each numeric column and picks an operational alias (`steady`, `spike_event`, `leak`, `saturation`, `flap`) per column. |
| `-o <PATH>` | Write the result to a file instead of stdout. Parent directories are created if missing. |

### Minimal template

```bash
sonda new --template
```

```yaml title="Output"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: example
    signal_type: metrics
    name: example_metric
    generator:
      type: constant
      value: 1.0
```

Pipe it straight to a file:

```bash
sonda new --template -o my-scenario.yaml
```

### Seed from a CSV

```bash
sonda new --from cpu-30days.csv -o cpu-replay.yaml
```

Each numeric column in the CSV gets its own `scenarios:` entry. The generator alias is chosen by pattern detection (`steady`, `spike_event`, `leak`, `saturation`, `flap`), so you can edit the output and tune the parameters rather than starting from a blank file.

### Interactive flow

```bash
sonda new -o my-scenario.yaml
```

The prompts cover signal type, generator, rate, duration, sink, and output destination. Cancel with Ctrl+C at any point; nothing is written until you confirm the output path.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Runtime error (scenario failed, sink unreachable, validation rejected the YAML). |
| `2` | Clap parse error (unknown flag, unrecognized subcommand, missing required argument). |

## Status output

Sonda prints colored lifecycle banners to stderr while a scenario runs. Banners go to stderr, data goes to stdout — you can redirect stdout to a file or pipe it without mixing in banner text.

### Start and stop banners

```text
▶ cpu_usage_percent  signal_type: metrics | rate: 10/s | encoder: prometheus_text | sink: stdout | duration: 30s
...
■ cpu_usage_percent  completed in 30.0s | events: 300 | bytes: 12.3 KB | errors: 0
```

### Live progress

Between the start and stop banners, Sonda updates a progress line every 200ms on TTYs and every 5s on non-TTYs:

```text
  ~ cpu_usage_percent  events: 1,234 | rate: 9.8/s | bytes: 12.3 KB | elapsed: 5.2s
```

Colors are automatic. Sonda respects [`NO_COLOR`](https://no-color.org) and disables ANSI when stderr is not a terminal.

### Verbosity

`--quiet` suppresses banners and progress (errors still print). `--verbose` prints the resolved scenario config at startup, then runs normally. The two flags are mutually exclusive.

```bash title="examples/cpu-spike.yaml"
sonda -q run examples/cpu-spike.yaml > metrics.txt    # quiet for scripts
sonda -v run examples/cpu-spike.yaml                  # echo config first
```
