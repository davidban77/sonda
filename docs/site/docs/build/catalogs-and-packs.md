---
title: Catalogs and packs
description: Organize your scenarios into a catalog directory; reuse metric shapes across scenarios with packs.
---

# Catalogs and packs

A **catalog** is a directory of scenario YAML files Sonda discovers through `--catalog <dir>`. A **pack** is a reusable bundle of metric names and label schemas you reference from any runnable scenario with `pack: <name>`. The two work together — packs live alongside runnable scenarios in the same catalog — but catalogs can be used without packs if you do not need the reuse.

## Catalogs

Each file in a catalog declares a `kind:` — `runnable` for scenarios you can run, `composable` for [packs](#packs) other scenarios reference. Sonda does not include a built-in catalog. Yours lives in your own repository, versioned next to your alert rules, dashboards, and CI workflows. Scenarios become fully supported artifacts of the system they model instead of being pinned to a Sonda release.

### The minimum

```text
my-catalog/
├── cpu-spike.yaml          # kind: runnable
├── memory-leak.yaml        # kind: runnable
└── prom-text-stdout.yaml   # kind: composable  (a pack)
```

```bash
sonda --catalog ./my-catalog list
sonda --catalog ./my-catalog show @cpu-spike
sonda --catalog ./my-catalog run @cpu-spike
```

Files without a recognized `kind:` header are skipped silently. Files with an unparseable YAML body print a warning to stderr and are skipped. The listing continues.

Two files with the same logical name (`name:` field or filename) are a **hard error**. Discovery fails with the conflicting paths. Rename one to disambiguate.

### Browse the catalog

`sonda list` prints a tab-separated table of every entry in the catalog:

```bash
sonda --catalog ~/sonda-catalog list
```

```text title="Output"
KIND        NAME              TAGS                  DESCRIPTION
runnable    cpu-spike         cpu,infrastructure    CPU spike to 95% for 30 seconds
runnable    memory-leak       memory,leak           Slow memory leak from baseline to ceiling
composable  prom-text-stdout  defaults              Shared prometheus_text + stdout defaults
```

Filter by entry kind or tag:

```bash
sonda --catalog ~/sonda-catalog list --kind runnable
sonda --catalog ~/sonda-catalog list --tag cpu
```

For machine-readable output, add `--json` to get a stable array on stdout. Each element has `name`, `kind`, `description`, `tags`, and the resolved `source` path. Use it as the contract when you script catalog discovery.

### Run a scenario

`sonda run @name --catalog <dir>` resolves the name in the catalog and runs the entry:

```bash
sonda --catalog ~/sonda-catalog run @cpu-spike --rate 5 --duration 10s
```

```text title="Output"
▶ node_cpu_usage_percent  signal_type: metrics | rate: 5/s | encoder: prometheus_text | sink: stdout | duration: 10s
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686141
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686641
...
■ node_cpu_usage_percent  completed in 10.0s | events: 50 | bytes: 4350 B | errors: 0
```

`sonda run` also accepts a direct filesystem path (no `@`, no `--catalog`) when you want to run a single file:

```bash
sonda run examples/basic-metrics.yaml
```

CLI overrides (`--duration`, `--rate`, `--sink`, `--endpoint`, `--encoder`, `--label`) win over the values inside the file. Use them to pin a backend or speed up a long-running scenario without editing the YAML.

!!! tip "Validate without emitting"
    Add `--dry-run` to compile the scenario and print the resolved config. No events are written:

    ```bash
    sonda --catalog ~/sonda-catalog --dry-run run @cpu-spike
    ```

### Inspect the YAML

`sonda show @name --catalog <dir>` prints the file contents byte-for-byte:

```bash
sonda --catalog ~/sonda-catalog show @cpu-spike
```

```yaml title="Output"
# CPU spike: periodic CPU usage spikes above threshold.
version: 2
kind: runnable

name: cpu-spike
tags: [cpu, infrastructure]
description: "Periodic CPU usage spikes above threshold"

scenarios:
  - signal_type: metrics
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

Pipe the output to a file when you want to fork an entry and customize it:

```bash title="my-cpu-spike.yaml"
sonda --catalog ~/sonda-catalog show @cpu-spike > my-cpu-spike.yaml
# edit my-cpu-spike.yaml — change labels, generator params, etc.
sonda run my-cpu-spike.yaml
```

### Write your own entries

A catalog entry is a scenario YAML with a top-level `kind:` field. For runnable entries:

```yaml title="~/sonda-catalog/my-scenario.yaml"
version: 2
kind: runnable

name: my-scenario
tags: [application, custom]
description: "My custom scenario pattern"

scenarios:
  - id: my_metric
    signal_type: metrics
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
| `version` | yes | Must be `2`. |
| `kind` | yes | `runnable` for runnable scenarios; `composable` for packs (see [Packs](#packs)). |
| `name` | no | Catalog identifier. Defaults to the filename (without `.yaml`) if omitted. Used with `@name`. |
| `tags` | no | Optional list of strings. `sonda list --tag <t>` filters on this. |
| `description` | no | One-line summary shown in the `sonda list` table and JSON output. |

The compiler ignores `tags:` and `description:`. They only feed the catalog views. Strict unknown-field validation stays in force, so typos like `desc:` or `tag:` (singular) are rejected at parse time.

After you drop the file in your catalog directory, `sonda list` picks it up on the next run:

```bash
sonda --catalog ~/sonda-catalog list --tag application
```

## Packs

A metric pack is a reusable bundle of metric names and label schemas, expressed as a `kind: composable` YAML file in your catalog. Reference a pack from any runnable scenario with `pack: <name>` and Sonda expands it into one entry per metric: exact names, correct shared labels, and reasonable default generators per metric.

Sonda no longer includes any built-in packs. You write your own and check them into a catalog directory alongside your scenarios.

### Why metric packs

When you test dashboards or alert rules, the metric names and label keys matter as much as the values. A Grafana dashboard that queries `ifHCInOctets{device="...",ifName="..."}` breaks if the metric is called `interface_in_octets` or the label key is `interface` instead of `ifName`.

Packs solve this by encoding the exact schema your tooling expects. You provide the instance-specific labels (which device, which interface), and the pack fills in the metric names, shared labels, and default generators.

### Browse packs in the catalog

Packs live alongside scenarios in the catalog directory. Filter the listing to packs with `--kind composable`:

```bash
sonda --catalog ~/sonda-catalog list --kind composable
```

```text title="Output"
KIND        NAME                       TAGS                    DESCRIPTION
composable  telegraf_snmp_interface    network,snmp            Standard SNMP interface metrics (Telegraf-normalized)
composable  node_exporter_cpu          infrastructure,cpu      Per-CPU mode counters (node_exporter-compatible)
composable  node_exporter_memory       infrastructure,memory   Memory gauge metrics (node_exporter-compatible)
```

`sonda list` shows packs as `kind: composable`. Packs are not directly runnable. Reference them from a `kind: runnable` entry through `pack: <name>` and supply instance-specific labels.

For machine-readable output, add `--json` to get the same stable DTO that scenarios use.

### Use packs in YAML scenario files

The canonical pattern: a `kind: runnable` scenario references a pack inline, and the labels you set on the entry apply to every expanded metric.

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

Sonda expands the pack into five concurrent metric scenarios — one per metric in the pack — and runs them all with the same rate, duration, labels, and sink:

```text title="Output (stdout, interleaved)"
ifOperStatus{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 1 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 125000 1775684638121
ifHCOutOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifInErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifOutErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
...
```

The `pack: <name>` lookup requires `--catalog <dir>` on `sonda run`. To reference a pack file by path instead (useful for one-off runs), set `pack: ./my-pack.yaml` (anything containing `/` or starting with `.`).

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

Sometimes you need a different generator for one metric without changing the rest of the pack. The `overrides` map lets you replace the generator or add extra labels per metric:

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

In this example, `ifOperStatus` uses the [`flap`](generators.md#operational-aliases) alias to simulate an interface toggling up and down. The other metrics keep their pack defaults.

You can override any metric by name. Each override accepts:

| Field | Type | Description |
|-------|------|-------------|
| `generator` | object | Replacement generator (any [generator type](generators.md) or [operational alias](generators.md#operational-aliases)). |
| `labels` | map | Additional labels merged on top of all other label sources for this metric. |

!!! warning "Override key must match a metric name"
    If an override key does not match any metric in the pack, Sonda returns an error. This catches typos early. Use `sonda show <pack-name> --catalog <dir>` to check metric names.

### Label merge order

Labels are merged in this order, with later sources winning on key conflicts:

1. Pack `shared_labels` (e.g. `job: snmp`)
2. Per-metric `labels` in the pack definition (e.g. `mode: user` on `node_cpu_seconds_total`)
3. Your `labels` in the scenario file
4. Per-metric override `labels` (if any)

### Write a pack

A pack is a YAML file with `kind: composable`. The pack identity (`name`, `description`, `category`) and the metric set (`shared_labels`, `metrics`) sit flat at the top level of the file:

```yaml title="~/sonda-catalog/my-app-pack.yaml"
version: 2
kind: composable

name: my_app_metrics
description: "Core application metrics"
category: application
tags: [application]

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

Drop the file in your catalog directory and it appears immediately:

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

All pack fields are top-level keys in the file:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Snake_case identifier for the pack. This is the name a runnable scenario references with `pack: <name>`. |
| `description` | string | yes | One-line human-readable description, shown in `sonda list`. |
| `category` | string | yes | Broad grouping for the pack (e.g. `network`, `infrastructure`, `application`). |
| `shared_labels` | map | no | Labels applied to every metric in the pack. Empty values are placeholders for the user to fill. |
| `metrics` | list | yes | One or more metric specifications. |
| `metrics[].name` | string | yes | The metric name. |
| `metrics[].labels` | map | no | Per-metric labels (merged on top of `shared_labels`). |
| `metrics[].generator` | object | no | Default generator. Falls back to `constant { value: 0.0 }` when absent. |

## Pattern: catalog and packs together

The typical project structure uses both: runnable scenarios at the catalog root and packs in a `packs/` subdirectory.

```text
my-catalog/
├── packs/
│   ├── snmp-interfaces.yaml      # kind: composable
│   └── node-exporter-cpu.yaml    # kind: composable
├── cpu-spike.yaml                # kind: runnable
├── memory-leak.yaml              # kind: runnable
└── edge-router-snmp.yaml         # kind: runnable, references pack
```

```yaml title="edge-router-snmp.yaml"
version: 2
kind: runnable
name: edge-router-snmp

scenarios:
  - signal_type: metrics
    pack: snmp-interfaces
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
```

`sonda --catalog ./my-catalog list` discovers every file; `sonda run @edge-router-snmp` runs the pack-backed scenario. The catalog directory layout has no special meaning — Sonda walks it recursively for `kind:` headers — but a `packs/` subdirectory keeps the listing readable.

## Where to next

- [Generators](generators.md) — every generator type and operational alias.
- [Scenario file format](scenario-files.md#pack-backed-entries) — reference a pack inline from a `scenarios:` entry.
- [CLI flags](../reference/cli-flags.md) — full flag reference for `sonda list`, `sonda show`, `sonda run`.
- [Network device telemetry](../test/network-device-telemetry.md) — end-to-end walkthrough that uses SNMP-shaped metrics for dashboard testing.
