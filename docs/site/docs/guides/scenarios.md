# Built-in Scenarios

Sonda ships with 11 curated scenario patterns embedded in the binary. You can discover, inspect,
and run common observability patterns without writing any YAML.

## Browse the catalog

List every built-in scenario with `sonda scenarios list`:

```bash
sonda scenarios list
```

```text title="Output"
NAME                         CATEGORY           SIGNAL       DESCRIPTION
cpu-spike                    infrastructure     metrics      Periodic CPU usage spikes above threshold
memory-leak                  infrastructure     metrics      Monotonically growing memory usage (sawtooth)
disk-fill                    infrastructure     metrics      Constant-rate disk consumption (step counter)
interface-flap               network            multi        Network interface toggling up/down with traffic shifts
latency-degradation          application        metrics      Growing response latency with jitter (sawtooth)
error-rate-spike             application        metrics      Periodic HTTP error rate bursts
cardinality-explosion        observability      metrics      Pod label cardinality explosion with spike windows
log-storm                    application        logs         Error-level log burst with template generation
steady-state                 infrastructure     metrics      Normal oscillating baseline (sine + jitter)
network-link-failure         network            multi        Link down with traffic shift to backup path
histogram-latency            application        histogram    Request latency histogram (normal distribution)
11 scenarios
```

Filter by category to narrow the list:

```bash
sonda scenarios list --category network
```

```text title="Output"
NAME                         CATEGORY           SIGNAL       DESCRIPTION
interface-flap               network            multi        Network interface toggling up/down with traffic shifts
network-link-failure         network            multi        Link down with traffic shift to backup path
2 scenarios in category "network"
```

Available categories: `infrastructure`, `network`, `application`, `observability`.

For machine-readable output, add `--json` to get a JSON array:

```bash
sonda scenarios list --json
```

## Run a scenario

Pick any scenario and run it directly:

```bash
sonda scenarios run cpu-spike
```

```text title="Output"
▶ node_cpu_usage_percent  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 60s
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686141
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589687146
...
■ node_cpu_usage_percent  completed in 60.0s | events: 61 | bytes: 5307 B | errors: 0
```

Override duration, rate, encoder, or sink without editing any YAML:

```bash
sonda scenarios run cpu-spike --duration 10s --rate 5
```

```bash
sonda scenarios run cpu-spike --sink http_push --endpoint http://localhost:9090/api/v1/write
```

| Override | Description |
|----------|-------------|
| `--duration <d>` | Shorten or extend the run (e.g. `10s`, `2m`) |
| `--rate <r>` | Change events per second |
| `--encoder <enc>` | Switch output format (e.g. `influx_lp`, `json_lines`) |
| `--sink <type>` | Redirect output to a sink (e.g. `http_push`, `remote_write`) |
| `--endpoint <url>` | Set the sink endpoint (required for network sinks) |

!!! tip
    Use `--dry-run` to validate what a scenario *would* do without emitting any data:

    ```bash
    sonda --dry-run scenarios run cpu-spike
    ```

## Inspect the YAML

Every built-in scenario is a standard YAML file. View it with `sonda scenarios show`:

```bash
sonda scenarios show cpu-spike
```

```yaml title="Output"
# CPU spike: periodic CPU usage spikes above threshold.
#
# Models a server experiencing recurring CPU spikes, useful for testing
# alert rules that trigger on sustained high CPU usage.
#
# Pattern: baseline ~35% with periodic spikes to ~95%.

name: node_cpu_usage_percent
rate: 1
duration: 60s

generator:
  type: spike
  baseline: 35.0
  magnitude: 60.0
  duration_secs: 10
  interval_secs: 30

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
sonda scenarios show cpu-spike > my-cpu-spike.yaml
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
embedded YAML. For example, `--duration 10s` overrides whatever duration the built-in defines.

!!! info
    The `@name` shorthand is an alternative to `sonda scenarios run`. Both resolve the same
    embedded YAML. Use `scenarios run` when you want purpose-built override flags
    (`--sink`, `--encoder`, `--endpoint`). Use `@name` when you want the full set of flags
    available on `metrics`, `logs`, or `histogram`.

## Scenario catalog

### Infrastructure

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `cpu-spike` | metrics | spike | Server CPU surging from ~35% baseline to ~95% for 10s every 30s. Tests threshold alerts. |
| `memory-leak` | metrics | sawtooth | Memory ramping from 40% to 95% over 120s (linear growth). Tests growth-rate alerts. |
| `disk-fill` | metrics | step | Disk usage climbing in fixed increments. Tests capacity alerts. |
| `steady-state` | metrics | sine + jitter | Normal oscillating baseline (~50% +/- 20%) with jitter noise. Use as a healthy control signal. |

### Network

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `interface-flap` | multi | sequence | Router interface toggling up/down with matching traffic counters and error spikes. 3 correlated metrics. |
| `network-link-failure` | multi | sequence | Primary link going down with traffic shifting to a backup path. Multi-metric correlation. |

### Application

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `latency-degradation` | metrics | sawtooth | HTTP response latency growing over time (sawtooth with jitter). Tests latency SLO alerts. |
| `error-rate-spike` | metrics | spike | Periodic bursts of HTTP 5xx errors. Tests error-rate alerts. |
| `log-storm` | logs | template | Error-heavy log burst (60% error, 30% warn) with 10x volume spikes. Tests log pipeline backpressure. |
| `histogram-latency` | histogram | normal distribution | Request latency histogram (mean 100ms, stddev 30ms). Tests p99 SLO alerting and heatmap panels. |

### Observability

| Name | Signal | Generator | What it models |
|------|--------|-----------|----------------|
| `cardinality-explosion` | metrics | sine + cardinality spike | 500 unique pod labels injected for 20s every 60s. Tests TSDB cardinality limits. |

## What next

- [**Alert Testing**](alert-testing.md) -- end-to-end walkthrough using shaped signals to validate alert rules
- [**CLI Reference**](../configuration/cli-reference.md#sonda-scenarios) -- full flag reference for all `scenarios` subcommands
- [**Scenario Files**](../configuration/scenario-file.md) -- YAML reference for writing your own scenarios from scratch
