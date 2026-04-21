# Metric Packs

Metric packs are reusable bundles of metric names and label schemas for specific domains. Instead
of writing YAML for every individual metric, you reference a pack and get all the right metrics
pre-filled -- with correct names, labels, and generators that match real-world tooling.

Sonda ships with 3 packs covering common observability targets: Telegraf SNMP interface
metrics, Prometheus node_exporter CPU counters, and node_exporter memory gauges. Pack YAML files
live in the `packs/` directory and are discovered from the filesystem at runtime.

## Why metric packs

When you test dashboards or alert rules, the metric names and label keys matter as much as the
values. A Grafana dashboard that queries `ifHCInOctets{device="...",ifName="..."}` breaks if the
metric is called `interface_in_octets` or the label key is `interface` instead of `ifName`.

Packs solve this by encoding the exact schema your tooling expects. You provide the instance-specific
labels (which device, which interface), and the pack fills in the metric names, shared labels,
and default generators.

## Pack search path

Sonda discovers pack YAML files from the filesystem via a search path:

1. **`--pack-path <dir>`** CLI flag -- when present, only this directory is searched.
2. **`SONDA_PACK_PATH`** env var -- colon-separated list of directories.
3. **`./packs/`** relative to the current working directory.
4. **`~/.sonda/packs/`** in the user's home directory.

Non-existent directories are silently skipped. If the same pack name appears in multiple
directories, the first match wins (highest-priority path).

When running from the repo root, the included `packs/` directory is found automatically.
For Docker, the `SONDA_PACK_PATH` env var is set to `/packs` in the image.

## Browse the catalog

List every available pack with `sonda catalog list --type pack`:

```bash
sonda catalog list --type pack
```

```text title="Output"
NAME                             TYPE       CATEGORY         SIGNAL     RUNNABLE   DESCRIPTION
node_exporter_memory             pack       infrastructure   metrics    no         Memory gauge metrics (node_exporter-compatible)
telegraf_snmp_interface          pack       network          metrics    no         Standard SNMP interface metrics (Telegraf-normalized)
node_exporter_cpu                pack       infrastructure   metrics    no         Per-CPU mode counters (node_exporter-compatible)
3 entries
```

Packs are marked `RUNNABLE: no` -- they need instance-specific labels before they emit anything
useful, so a bare `catalog run <pack>` without `--label` flags will produce generic output.

Filter by category:

```bash
sonda catalog list --type pack --category infrastructure
```

For machine-readable output, add `--json` to get the same
[stable DTO](../configuration/cli-reference.md#json-output) that scenarios use.

## Run a pack

Pick any pack and run it directly with `sonda catalog run`. You must provide `--rate` and
typically `--duration`, plus any labels your scenario needs:

```bash
sonda catalog run telegraf_snmp_interface \
  --rate 1 --duration 10s \
  --label device=rtr-edge-01 \
  --label ifName=GigabitEthernet0/0/0 \
  --label ifIndex=1
```

Sonda expands the pack into 5 concurrent metric scenarios -- one per metric in the pack -- and
runs them all with the same rate, duration, labels, and sink:

```text title="Output (stdout, interleaved)"
ifOperStatus{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 1 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifHCInOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 125000 1775684638121
ifHCOutOctets{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifInErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
ifOutErrors{device="rtr-edge-01",ifAlias="",ifIndex="1",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775684637116
...
```

Override the encoder or sink:

```bash
sonda catalog run node_exporter_memory \
  --rate 1 --duration 30s \
  --label instance=web-01 \
  --encoder json_lines

sonda catalog run telegraf_snmp_interface \
  --rate 1 --duration 60s \
  --label device=rtr-core-01 \
  --label ifName=Ethernet1 \
  --label ifIndex=1 \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Capture the expanded output to a file with `-o` (shorthand for `--sink file --endpoint <path>`).
Every metric in the pack writes to the same file:

```bash
sonda catalog run telegraf_snmp_interface \
  --rate 1 --duration 10s \
  --label device=rtr-edge-01 \
  --label ifName=Gi0/0/0 \
  --label ifIndex=1 \
  -o /tmp/snmp.prom
```

Use `--dry-run` to see the expanded config without emitting data:

```bash
sonda --dry-run catalog run telegraf_snmp_interface \
  --rate 1 --duration 10s \
  --label device=rtr-edge-01 \
  --label ifName=Gi0/0/0 \
  --label ifIndex=1
```

```text title="Output"
[config] [1/5] ifOperStatus

  name:          ifOperStatus
  signal:        metrics
  rate:          1/s
  duration:      10s
  generator:     constant (value: 1)
  encoder:       prometheus_text
  sink:          stdout
  labels:        device=rtr-edge-01, ifAlias=, ifIndex=1, ifName=Gi0/0/0, job=snmp

───
[config] [2/5] ifHCInOctets

  name:          ifHCInOctets
  signal:        metrics
  rate:          1/s
  duration:      10s
  generator:     step (start: 0, step: 125000)
  encoder:       prometheus_text
  sink:          stdout
  labels:        device=rtr-edge-01, ifAlias=, ifIndex=1, ifName=Gi0/0/0, job=snmp

...

Validation: OK (5 scenarios)
```

## Inspect a pack

View the raw YAML for any built-in pack:

```bash
sonda catalog show telegraf_snmp_interface
```

```yaml title="Output"
name: telegraf_snmp_interface
description: "Standard SNMP interface metrics (Telegraf-normalized)"
category: network

shared_labels:
  device: ""
  ifName: ""
  ifAlias: ""
  ifIndex: ""
  job: snmp

metrics:
  - name: ifOperStatus
    generator:
      type: constant
      value: 1.0

  - name: ifHCInOctets
    generator:
      type: step
      start: 0.0
      step_size: 125000.0

  - name: ifHCOutOctets
    generator:
      type: step
      start: 0.0
      step_size: 62500.0

  - name: ifInErrors
    generator:
      type: constant
      value: 0.0

  - name: ifOutErrors
    generator:
      type: constant
      value: 0.0
```

Save a pack to a file to use as a starting point for a custom pack:

```bash
sonda catalog show telegraf_snmp_interface > my-snmp-pack.yaml
```

## Use packs in YAML scenario files

For repeatable setups, reference a pack in a YAML file and pass it to `sonda run`:

```yaml title="pack-scenario.yaml"
version: 2

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
sonda run --scenario pack-scenario.yaml
```

The `pack:` field tells Sonda to expand the referenced pack before running. The result is
identical to running `sonda catalog run` with the same parameters. For a v2 file, set the pack
inline on a `scenarios:` entry -- see
[v2 Scenario Files -- Pack-backed entries](../configuration/v2-scenarios.md#pack-backed-entries).

### Per-metric overrides

Sometimes you need a different generator for one metric without changing the rest of the pack.
The `overrides` map lets you replace the generator or add extra labels per metric:

```yaml title="pack-with-overrides.yaml"
version: 2

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
alias to simulate an interface toggling up and down, while all other metrics keep their pack defaults.

You can override any metric by name. Each override accepts:

| Field | Type | Description |
|-------|------|-------------|
| `generator` | object | Replacement generator (any [generator type](../configuration/generators.md) or [operational alias](../configuration/generators.md#operational-aliases)). |
| `labels` | map | Additional labels merged on top of all other label sources for this metric. |

!!! warning "Override key must match a metric name"
    If an override key does not match any metric in the pack, Sonda returns an error. This catches
    typos early. Use `sonda catalog show <name>` to check metric names.

### Label merge order

Labels are merged in this order, with later sources winning on key conflicts:

1. Pack `shared_labels` (e.g. `job: snmp`)
2. Per-metric `labels` in the pack definition (e.g. `mode: user` on `node_cpu_seconds_total`)
3. Your `labels` in the scenario file or `--label` on the CLI
4. Per-metric override `labels` (if any)

## Built-in pack reference

### telegraf_snmp_interface

**Category:** network -- **Metrics:** 5

Models the standard interface metrics collected by the Telegraf SNMP input plugin. Metric names
and label keys match the Telegraf-normalized schema exactly, so Sonda output can replace real
device telemetry in dashboards and alert rules.

| Metric | Generator | Description |
|--------|-----------|-------------|
| `ifOperStatus` | constant (1.0) | Interface operational state (1 = up) |
| `ifHCInOctets` | step (+125,000/tick) | Ingress byte counter (monotonic) |
| `ifHCOutOctets` | step (+62,500/tick) | Egress byte counter (monotonic) |
| `ifInErrors` | constant (0.0) | Ingress error counter |
| `ifOutErrors` | constant (0.0) | Egress error counter |

**Shared labels:** `device`, `ifName`, `ifAlias`, `ifIndex`, `job` (default: `snmp`)

You should set `device`, `ifName`, and `ifIndex` via `--label` or the `labels:` block in your
scenario file. Leave `ifAlias` empty or set it to a description string.

### node_exporter_cpu

**Category:** infrastructure -- **Metrics:** 8

Models the per-CPU mode counters exposed by Prometheus node_exporter. Each metric spec represents
one mode of `node_cpu_seconds_total` with a step counter whose rate reflects typical CPU
utilization proportions.

| Metric | Labels | Generator | Rate (step/tick) |
|--------|--------|-----------|-----------------|
| `node_cpu_seconds_total` | `mode=user` | step | 0.25 |
| `node_cpu_seconds_total` | `mode=system` | step | 0.10 |
| `node_cpu_seconds_total` | `mode=idle` | step | 0.55 |
| `node_cpu_seconds_total` | `mode=iowait` | step | 0.03 |
| `node_cpu_seconds_total` | `mode=irq` | step | 0.01 |
| `node_cpu_seconds_total` | `mode=softirq` | step | 0.02 |
| `node_cpu_seconds_total` | `mode=nice` | step | 0.01 |
| `node_cpu_seconds_total` | `mode=steal` | step | 0.03 |

**Shared labels:** `instance`, `job` (default: `node_exporter`), `cpu` (default: `"0"`)

Set `instance` to identify the target host. Override `cpu` to simulate multi-core systems.

!!! tip "Testing `rate()` queries"
    The step sizes sum to 1.0 per tick -- at a rate of 1 event/sec, `rate(node_cpu_seconds_total[1m])`
    returns the expected per-mode utilization fractions. This makes the pack useful for testing
    PromQL recording rules and Grafana CPU panels.

### node_exporter_memory

**Category:** infrastructure -- **Metrics:** 5

Models the memory gauge metrics exposed by Prometheus node_exporter. Default values approximate a
server with 16 GiB total memory under moderate load.

| Metric | Generator | Default value |
|--------|-----------|---------------|
| `node_memory_MemTotal_bytes` | constant | 17,179,869,184 (16 GiB) |
| `node_memory_MemFree_bytes` | constant | 2,147,483,648 (2 GiB) |
| `node_memory_MemAvailable_bytes` | constant | 8,589,934,592 (8 GiB) |
| `node_memory_Buffers_bytes` | constant | 536,870,912 (512 MiB) |
| `node_memory_Cached_bytes` | constant | 5,368,709,120 (5 GiB) |

**Shared labels:** `instance`, `job` (default: `node_exporter`)

Set `instance` to identify the target host.

??? tip "Simulating memory pressure"
    Override `node_memory_MemFree_bytes` and `node_memory_MemAvailable_bytes` with a `leak` or
    `degradation` generator to simulate memory pressure over time:

    ```yaml
    overrides:
      node_memory_MemAvailable_bytes:
        generator:
          type: degradation
          start: 8589934592.0
          end: 1073741824.0
          duration: "300s"
    ```

## Custom packs

You can create your own pack YAML files and place them on the search path, or reference them
by file path in a scenario file.

### Pack definition format

A pack definition has the same structure as the built-in packs:

```yaml title="my-app-pack.yaml"
name: my_app_metrics
description: "Core application metrics"
category: application

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

### Place on the search path

Drop your pack YAML file into any directory on the search path. For example, create
`~/.sonda/packs/my-app-pack.yaml` and it will be discovered automatically:

```bash
sonda catalog list --type pack           # shows my_app_metrics
sonda catalog run my_app_metrics --rate 1 --duration 60s --label service=api-gateway
```

Or use `--pack-path` to point to a custom directory:

```bash
sonda --pack-path ./my-packs catalog list --type pack
```

### Reference by file path

In a scenario file, use a path (containing `/` or starting with `.`) instead of a pack name:

```yaml title="run-my-pack.yaml"
version: 2

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    pack: ./my-app-pack.yaml
    labels:
      service: api-gateway
      env: staging
```

```bash
sonda run --scenario run-my-pack.yaml
```

Sonda detects that the `pack:` value is a file path (because it contains `/`) and reads the
pack definition from disk.

!!! info "Name vs. file path detection"
    If the `pack:` value contains `/` or starts with `.`, it is treated as a file path.
    Otherwise, it is looked up by name on the search path. For example,
    `pack: telegraf_snmp_interface` resolves via the search path, while
    `pack: ./telegraf-snmp-interface.yaml` reads from a specific file path.

### Pack definition fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Snake_case identifier for the pack. |
| `description` | string | yes | One-line human-readable description. |
| `category` | string | yes | Broad grouping (e.g. `network`, `infrastructure`, `application`). |
| `shared_labels` | map | no | Labels applied to every metric in the pack. Empty values are placeholders for the user to fill. |
| `metrics` | list | yes | One or more metric specifications. |
| `metrics[].name` | string | yes | The metric name. |
| `metrics[].labels` | map | no | Per-metric labels (merged on top of `shared_labels`). |
| `metrics[].generator` | object | no | Default generator. Falls back to `constant { value: 0.0 }` when absent. |

## How packs integrate with the pipeline

When Sonda encounters a `pack:` field in a YAML file (via `sonda run --scenario`) or a pack
name (via `sonda catalog run`), it:

1. Resolves the pack definition (built-in catalog or file path).
2. Calls `expand_pack()` to produce one `ScenarioEntry` per metric in the pack.
3. Feeds those entries into the standard `prepare_entries()` pipeline.
4. Launches all metrics concurrently, just like a multi-scenario file.

This means every feature that works with multi-scenario runs -- `--dry-run`, `--verbose`,
`--quiet`, live progress, aggregate summary -- works with packs automatically.

## What next

- [**Network Device Telemetry**](network-device-telemetry.md) -- end-to-end walkthrough using SNMP metrics for dashboard testing
- [**CLI Reference**](../configuration/cli-reference.md#sonda-catalog) -- full flag reference for `catalog list`, `catalog show`, `catalog run`
- [**v2 Scenario Files**](../configuration/v2-scenarios.md#pack-backed-entries) -- reference a pack inline from a v2 `scenarios:` entry
- [**Scenario Files**](../configuration/scenario-file.md) -- YAML reference for all scenario fields
- [**Generators**](../configuration/generators.md) -- all generator types and operational aliases
