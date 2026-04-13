# Built-in Scenarios

Sonda ships with curated scenario patterns you can discover, inspect, and run without writing
any YAML. Scenario files live on the filesystem and load at runtime through a configurable
search path, alongside [metric packs](metric-packs.md) in the same unified catalog.

## Scenario search path

Sonda discovers scenario YAML files from the filesystem via a search path:

1. **`--scenario-path <dir>`** CLI flag -- when present, only this directory is searched.
2. **`SONDA_SCENARIO_PATH`** env var -- colon-separated list of directories.
3. **`./scenarios/`** relative to the current working directory.
4. **`~/.sonda/scenarios/`** in the user's home directory.

Non-existent directories are silently skipped. If the same scenario name appears in multiple
directories, the first match wins (highest-priority path).

When running from the repo root, the included `scenarios/` directory is found automatically.
For Docker, the `SONDA_SCENARIO_PATH` env var is set to `/scenarios` in the image.

## Browse the catalog

List every scenario and pack with `sonda catalog list`:

```bash
sonda catalog list
```

```text title="Output"
NAME                             TYPE       CATEGORY         SIGNAL     RUNNABLE   DESCRIPTION
cpu-spike                        scenario   infrastructure   metrics    yes        Periodic CPU usage spikes above threshold
memory-leak                      scenario   infrastructure   metrics    yes        Monotonically growing memory usage (sawtooth)
interface-flap                   scenario   network          multi      yes        Network interface toggling up/down with traffic shifts
log-storm                        scenario   application      logs       yes        Error-level log burst with template generation
histogram-latency                scenario   application      histogram  yes        Request latency histogram (normal distribution)
telegraf_snmp_interface          pack       network          metrics    no         Standard SNMP interface metrics (Telegraf-normalized)
node_exporter_cpu                pack       infrastructure   metrics    no         Per-CPU mode counters (node_exporter-compatible)
14 entries
```

Restrict to just scenarios:

```bash
sonda catalog list --type scenario
```

Filter by category:

```bash
sonda catalog list --category network
```

Available categories (scenarios and packs share the same set): `infrastructure`, `network`,
`application`, `observability`.

For machine-readable output, add `--json` to get a stable JSON array. See
[`catalog list`](../configuration/cli-reference.md#catalog-list) for the DTO schema.

## Run a scenario

Use `sonda run --scenario @<name>` with the `@name` shorthand to execute a built-in scenario:

```bash
sonda run --scenario @interface-flap --duration 30s
```

For flat single-signal built-ins like `cpu-spike`, use the matching signal subcommand:

```bash
sonda metrics --scenario @cpu-spike --duration 10s --rate 5
sonda logs    --scenario @log-storm --duration 20s
sonda histogram --scenario @histogram-latency
```

```text title="Output"
▶ node_cpu_usage_percent  signal_type: metrics | rate: 5/s | encoder: prometheus_text | sink: stdout | duration: 10s
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686141
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686641
...
■ node_cpu_usage_percent  completed in 10.0s | events: 50 | bytes: 4350 B | errors: 0
```

Any flag available on the signal subcommand (`--label`, `--precision`, `--sink`, `--output`,
etc.) composes with `@name`:

```bash
sonda metrics --scenario @cpu-spike \
  --duration 30s --rate 5 \
  --sink http_push --endpoint http://localhost:9090/api/v1/write \
  --label env=staging
```

!!! tip
    Use `--dry-run` to validate what a scenario *would* do without emitting any data:

    ```bash
    sonda --dry-run metrics --scenario @cpu-spike
    ```

!!! info "Why `sonda run --scenario @name` does not always work"
    `sonda run` expects a multi-scenario file (top-level `scenarios:` list), a `pack:`
    shorthand, or a [v2 file](../configuration/v2-scenarios.md). Most built-in single-signal
    scenarios use the flat v1 format, so they run through the signal subcommand
    (`sonda metrics`, `sonda logs`, `sonda histogram`) instead. Multi-signal built-ins like
    `interface-flap` work with `sonda run` because they already use the `scenarios:` list form.

## Inspect the YAML

Every scenario in the catalog is a standard YAML file on disk. View it with
`sonda catalog show`:

```bash
sonda catalog show cpu-spike
```

```yaml title="Output"
# CPU spike: periodic CPU usage spikes above threshold.
#
# Models a server experiencing recurring CPU spikes using the
# `spike_event` alias. Useful for testing alert rules that trigger
# on sustained high CPU usage.
#
# Pattern: baseline ~35% with periodic spikes to ~95%.

scenario_name: cpu-spike
category: infrastructure
signal_type: metrics
description: "Periodic CPU usage spikes above threshold"

name: node_cpu_usage_percent
rate: 1
duration: 60s

generator:
  type: spike_event
  baseline: 35.0
  spike_height: 60.0
  spike_duration: "10s"
  spike_interval: "30s"

labels:
  instance: web-01
  job: node_exporter
  cpu: "0"

encoder:
  type: prometheus_text

sink:
  type: stdout
```

## Customize a built-in

Built-in scenarios are a starting point. To customize one beyond what the `--duration` / `--rate`
overrides offer, save the YAML to a file and edit it:

```bash
sonda catalog show cpu-spike > my-cpu-spike.yaml
# Edit my-cpu-spike.yaml to change labels, generator params, etc.
sonda metrics --scenario my-cpu-spike.yaml
```

This workflow lets you use built-in patterns as templates without starting YAML from scratch.

## The @name shorthand

Every subcommand that accepts `--scenario` also supports a `@name` shorthand. Instead of
pointing to a file on disk, prefix the scenario name with `@` to load a built-in:

=== "metrics"

    ```bash
    sonda metrics --scenario @cpu-spike --duration 10s
    ```

=== "logs"

    ```bash
    sonda logs --scenario @log-storm --duration 10s
    ```

=== "histogram"

    ```bash
    sonda histogram --scenario @histogram-latency
    ```

=== "run"

    ```bash
    sonda run --scenario @interface-flap
    ```

The `@name` shorthand works exactly like a file path -- CLI flags still override values in the
scenario YAML. For example, `--duration 10s` overrides whatever duration the built-in defines.

!!! info
    `@name` and `sonda catalog run <name>` both resolve the same scenario from the search path.
    Use `@name` on a signal subcommand when you want the full set of per-signal flags
    (`--label`, `--precision`, `--value`). Use `catalog run` for a focused, cross-type surface
    (scenarios and packs) with a small override set. See
    [`catalog run`](../configuration/cli-reference.md#catalog-run) for the limitations
    on flat v1 scenarios.

## Scenario catalog

### Infrastructure

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `cpu-spike` | metrics | `spike_event` | Server CPU surging from ~35% baseline to ~95% for 10s every 30s. Tests threshold alerts. |
| `memory-leak` | metrics | `leak` | Memory ramping from 40% to 95% over 120s (linear growth). Tests growth-rate alerts. |
| `disk-fill` | metrics | `step` | Disk usage climbing in fixed increments. Tests capacity alerts. |
| `steady-state` | metrics | `steady` | Normal oscillating baseline (~75% +/- 10) with noise. Use as a healthy control signal. |

### Network

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `interface-flap` | multi | sequence | Router interface toggling up/down with matching traffic counters and error spikes. 3 correlated metrics. |
| `network-link-failure` | multi | sequence | Primary link going down with traffic shifting to a backup path. Multi-metric correlation. |

### Application

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `latency-degradation` | metrics | `degradation` | HTTP response latency growing over time with noise. Tests latency SLO alerts. |
| `error-rate-spike` | metrics | `spike` | Periodic bursts of HTTP 5xx errors. Tests error-rate alerts. |
| `log-storm` | logs | template | Error-heavy log burst (60% error, 30% warn) with 10x volume spikes. Tests log pipeline backpressure. |
| `histogram-latency` | histogram | normal distribution | Request latency histogram (mean 100ms, stddev 30ms). Tests p99 SLO alerting and heatmap panels. |

### Observability

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `cardinality-explosion` | metrics | sine + cardinality spike | 500 unique pod labels injected for 20s every 60s. Tests TSDB cardinality limits. |

## Custom scenarios

You can add your own scenario YAML files to any directory on the search path. For example,
create `~/.sonda/scenarios/my-scenario.yaml` and it will be discovered automatically:

```bash
sonda catalog list                          # shows your custom scenario
sonda metrics --scenario @my-scenario       # @name shorthand on the signal subcommand
```

Or use `--scenario-path` to point to a custom directory:

```bash
sonda --scenario-path ./my-scenarios catalog list
```

Custom scenario files use the same YAML format as any scenario file. The only addition is
metadata fields at the top for catalog display:

```yaml title="~/.sonda/scenarios/my-scenario.yaml"
scenario_name: my-scenario
category: application
signal_type: metrics
description: "My custom scenario pattern"

name: my_metric
rate: 1
duration: 30s
generator:
  type: sine
  amplitude: 50.0
  period_secs: 60
  offset: 50.0
encoder:
  type: prometheus_text
sink:
  type: stdout
```

| Field | Required | Description |
|-------|----------|-------------|
| `scenario_name` | yes | Kebab-case identifier used with `@name` and `sonda catalog run` |
| `category` | yes | Grouping for `--category` filter |
| `signal_type` | yes | Signal type: `metrics`, `logs`, `histogram`, `multi` |
| `description` | yes | One-line description shown in `sonda catalog list` |

## What next

- [**Metric Packs**](metric-packs.md) -- pre-built metric bundles for Telegraf SNMP and node_exporter with correct schemas
- [**Alert Testing**](alert-testing.md) -- end-to-end walkthrough using shaped signals to validate alert rules
- [**CLI Reference**](../configuration/cli-reference.md#sonda-catalog) -- full flag reference for `sonda catalog`
- [**Scenario Files**](../configuration/scenario-file.md) -- YAML reference for writing your own scenarios from scratch
- [**v2 Scenario Files**](../configuration/v2-scenarios.md) -- the forward-compatible format with defaults, `after:`, and inline packs
