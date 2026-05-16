# Metric Packs

A metric pack is a reusable bundle of metric names and label schemas, expressed as a
`kind: composable` v2 YAML file in your [catalog](scenarios.md). Reference a pack from any
runnable scenario with `pack: <name>` and Sonda expands it into one entry per metric —
exact names, correct shared labels, and sensible default generators per metric.

Sonda no longer ships any built-in packs; you author your own and check them into a catalog
directory alongside your scenarios.

## Why metric packs

When you test dashboards or alert rules, the metric names and label keys matter as much as the
values. A Grafana dashboard that queries `ifHCInOctets{device="...",ifName="..."}` breaks if
the metric is called `interface_in_octets` or the label key is `interface` instead of `ifName`.

Packs solve this by encoding the exact schema your tooling expects. You provide the
instance-specific labels (which device, which interface), and the pack fills in the metric
names, shared labels, and default generators.

## Browse packs in the catalog

Packs live alongside scenarios in the catalog directory. Filter the listing to just packs with
`--kind composable`:

```bash
sonda --catalog ~/sonda-catalog list --kind composable
```

```text title="Output"
KIND        NAME                       TAGS                    DESCRIPTION
composable  telegraf_snmp_interface    network,snmp            Standard SNMP interface metrics (Telegraf-normalized)
composable  node_exporter_cpu          infrastructure,cpu      Per-CPU mode counters (node_exporter-compatible)
composable  node_exporter_memory       infrastructure,memory   Memory gauge metrics (node_exporter-compatible)
```

`sonda list` shows packs as `kind: composable`. They are not directly runnable — you reference
them from a `kind: runnable` entry via `pack: <name>` and supply instance-specific labels.

For machine-readable output, add `--json` to get the same stable DTO that scenarios use.

## Use packs in YAML scenario files

The canonical pattern: a `kind: runnable` scenario references a pack inline, and the labels you
set on the entry apply to every expanded metric.

```yaml title="snmp-edge.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifAlias: "Uplink to Core"
      ifIndex: "1"
```

```bash
sonda --catalog ~/sonda-catalog run snmp-edge.yaml
```

Sonda expands the pack into five concurrent metric scenarios — one per metric in the pack —
and runs them all with the same rate, duration, labels, and sink:

```text title="Output (stdout, interleaved)"
ifOperStatus{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 1 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 125000 1775684638121
ifHCOutOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifInErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifOutErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
...
```

The `pack: <name>` lookup requires `--catalog <dir>` on `sonda run`. To reference a pack file
by path instead (handy for ad-hoc one-offs), set `pack: ./my-pack.yaml` (anything containing
`/` or starting with `.`).

Use `--dry-run` to see the expanded config without emitting data:

```bash
sonda --catalog ~/sonda-catalog --dry-run run snmp-edge.yaml
```

```text title="Output (abridged)"
[config] [1/5] ifOperStatus
  name:          ifOperStatus
  signal:        metrics
  rate:          1/s
  duration:      10s
  generator:     constant (value: 1)
  encoder:       prometheus_text
  sink:          stdout
  labels:        device=rtr-edge-01, ifAlias=, ifIndex=1, ifName=GigabitEthernet0/0/0, job=snmp

[config] [2/5] ifHCInOctets
  name:          ifHCInOctets
  signal:        metrics
  rate:          1/s
  duration:      10s
  generator:     step (start: 0, step: 125000)
  encoder:       prometheus_text
  sink:          stdout
  labels:        device=rtr-edge-01, ifAlias=, ifIndex=1, ifName=GigabitEthernet0/0/0, job=snmp
...

Validation: OK (5 scenarios)
```

### Per-metric overrides

Sometimes you need a different generator for one metric without changing the rest of the pack.
The `overrides` map lets you replace the generator or add extra labels per metric:

```yaml title="snmp-with-overrides.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifAlias: "Uplink to Core"
      ifIndex: "1"
    overrides:
      ifOperStatus:
        generator:
          type: flap
```

In this example, `ifOperStatus` uses the [`flap`](../configuration/generators.md#operational-aliases)
alias to simulate an interface toggling up and down, while all other metrics keep their pack
defaults.

You can override any metric by name. Each override accepts:

| Field | Type | Description |
|-------|------|-------------|
| `generator` | object | Replacement generator (any [generator type](../configuration/generators.md) or [operational alias](../configuration/generators.md#operational-aliases)). |
| `labels` | map | Additional labels merged on top of all other label sources for this metric. |

!!! warning "Override key must match a metric name"
    If an override key does not match any metric in the pack, Sonda returns an error. This
    catches typos early. Use `sonda show <pack-name> --catalog <dir>` to check metric names.

### Label merge order

Labels are merged in this order, with later sources winning on key conflicts:

1. Pack `shared_labels` (e.g. `job: snmp`)
2. Per-metric `labels` in the pack definition (e.g. `mode: user` on `node_cpu_seconds_total`)
3. Your `labels` in the scenario file
4. Per-metric override `labels` (if any)

## Author a pack

A pack is a v2 YAML file with `kind: composable` and a top-level `pack:` block describing the
metric set:

```yaml title="~/sonda-catalog/my-app-pack.yaml"
version: 2
kind: composable

name: my_app_metrics
tags: [application]
description: "Core application metrics"

pack:
  shared_labels:
    service: ""
    env: ""

  metrics:
    - name: http_requests_total
      generator:
        type: step
        start: 0.0
        step_size: 10.0

    - name: http_request_duration_seconds
      generator:
        type: sine
        amplitude: 0.05
        period_secs: 60
        offset: 0.1

    - name: http_errors_total
      generator:
        type: constant
        value: 0.0
```

Drop the file in your catalog directory and it shows up immediately:

```bash
sonda --catalog ~/sonda-catalog list --kind composable
```

Reference it from a runnable entry:

```yaml title="run-my-pack.yaml"
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
  - signal_type: metrics
    pack: my_app_metrics
    labels:
      service: api-gateway
      env: staging
```

```bash
sonda --catalog ~/sonda-catalog run run-my-pack.yaml
```

### Pack definition fields

The fields under `pack:`:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `shared_labels` | map | no | Labels applied to every metric in the pack. Empty values are placeholders for the user to fill. |
| `metrics` | list | yes | One or more metric specifications. |
| `metrics[].name` | string | yes | The metric name. |
| `metrics[].labels` | map | no | Per-metric labels (merged on top of `shared_labels`). |
| `metrics[].generator` | object | no | Default generator. Falls back to `constant { value: 0.0 }` when absent. |

## How packs integrate with the pipeline

When Sonda encounters a `pack:` field in a YAML scenario, it:

1. Resolves the pack definition (catalog lookup by name, or file path).
2. Calls `expand_pack()` to produce one entry per metric in the pack.
3. Feeds those entries into the standard `prepare_entries()` pipeline.
4. Launches all metrics concurrently, just like a multi-scenario file.

This means every feature that works with multi-scenario runs — `--dry-run`, `--verbose`,
`--quiet`, live progress, aggregate summary — works with packs automatically.

## What next

- [**Catalogs**](scenarios.md) -- the directory layout that packs and runnable scenarios share.
- [**Network Device Telemetry**](network-device-telemetry.md) -- end-to-end walkthrough using
  SNMP-shaped metrics for dashboard testing.
- [**CLI Reference**](../configuration/cli-reference.md) -- full flag reference for `sonda list`,
  `sonda show`, `sonda run`.
- [**v2 Scenario Files -- Pack-backed entries**](../configuration/v2-scenarios.md#pack-backed-entries)
  -- reference a pack inline from a v2 `scenarios:` entry.
- [**Generators**](../configuration/generators.md) -- all generator types and operational aliases.
