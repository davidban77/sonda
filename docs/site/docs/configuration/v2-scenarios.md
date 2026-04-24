# v2 Scenario Files

The v2 scenario format is Sonda's way to describe one or many signals in a single YAML file.
One top-level block declares shared defaults; another lists the scenarios. Packs, `after:`
temporal dependencies, and clock groups all compose inside the same file.

Every scenario file must declare `version: 2` at the top. Everything else you already know
about scenarios -- generators, encoders, sinks, labels -- still applies.

!!! warning "v1 YAML is no longer accepted"
    Sonda only accepts v2 scenario YAML. The CLI (`sonda run`, `sonda metrics`,
    `sonda catalog run`, every `--scenario` consumer) and the HTTP server (`POST /scenarios`)
    both refuse files or bodies without `version: 2` at the top and print a migration hint
    pointing at this page.

    If you are upgrading from a Sonda release before this change, jump straight to
    [Migrating from v1](#migrating-from-v1).

## Minimal example

```yaml title="hello-v2.yaml"
version: 2

defaults:
  rate: 1
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: cpu
    signal_type: metrics
    name: demo_cpu
    generator:
      type: constant
      value: 42.0
```

Run it like any other scenario file:

```bash
sonda run --scenario hello-v2.yaml
```

```text title="Output"
▶ demo_cpu  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 30s
demo_cpu 42 1776090151609
demo_cpu 42 1776090152619
...
■ demo_cpu  completed in 30.0s | events: 30 | bytes: 680 B | errors: 0
━━ run complete  scenarios: 1 | events: 30 | bytes: 680 B | errors: 0 | elapsed: 30.1s
```

## What v2 buys you

- **Shared defaults** -- put `rate`, `duration`, `encoder`, `sink`, and `labels` in one place.
- **Temporal chains** -- link scenarios with `after:` clauses (`this starts when that crosses a threshold`).
- **Automatic clock groups** -- scenarios linked by `after:` share a clock so they stay in sync.
- **Pack-backed entries** -- reference a [metric pack](../guides/metric-packs.md) inline, alongside regular metrics and logs.
- **Richer validation** -- the compiler catches missing fields, cycles in `after:` chains, and
  pack references to unknown names at parse time.

## Catalog metadata

v2 files can carry optional top-level metadata that powers the unified catalog --
`sonda catalog list`, `sonda catalog show`, and the `--category` filter. The fields sit at
the root, alongside `version:` and `defaults:`:

```yaml title="scenarios/steady-state.yaml"
version: 2

scenario_name: steady-state
category: infrastructure
description: "Normal oscillating baseline (sine + jitter)"

scenarios:
  - signal_type: metrics
    name: node_cpu_usage_idle_percent
    rate: 1
    duration: 60s
    generator:
      type: steady
      center: 75.0
      amplitude: 10.0
      period: "60s"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

| Field | Required | Description |
|-------|----------|-------------|
| `scenario_name` | no | Kebab-case identifier. Defaults to the filename (without `.yaml`, hyphens preserved) if omitted. Used by `@name` shorthand and `sonda catalog run`. |
| `category` | no | Catalog grouping. One of `infrastructure`, `network`, `application`, `observability`. Scenarios without a category render as `uncategorized` and drop out of `--category` filters. |
| `description` | no | One-line summary shown in the catalog table and JSON output. Keep it under ~60 characters so it fits the table column. |

The compiler ignores these fields -- they only feed the catalog. `deny_unknown_fields` stays
in force, so typos like `scenarioName:` or `desc:` are rejected at parse time.

!!! info "Same field names as legacy v1"
    The retired v1 format used the same top-level field names (`scenario_name`, `category`,
    `description`). Migrating a v1 file to v2 keeps the metadata as-is -- you add
    `version: 2` and reshape the body around `defaults:` + `scenarios:`.

Drop a v2 file into any directory on the
[scenario search path](../guides/scenarios.md#scenario-search-path) and it shows up
immediately:

```bash
sonda catalog list --category infrastructure
```

```text
NAME           TYPE      CATEGORY         SIGNAL    RUNNABLE   DESCRIPTION
steady-state   scenario  infrastructure   metrics   yes        Normal oscillating baseline (sine + jitter)
...
```

## The `defaults:` block

Every field in `defaults:` applies to every entry in `scenarios:` unless the entry overrides it.
This is the main ergonomic win over legacy multi-scenario files, where you typed the same
`encoder:`, `sink:`, and `rate:` for every entry.

```yaml title="defaults-example.yaml"
version: 2

defaults:
  rate: 10
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    job: sonda
    env: staging

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50

  - id: mem
    signal_type: metrics
    name: mem_usage
    rate: 1                       # override just the rate for this one
    generator:
      type: leak
      baseline: 40
      ceiling: 90
      time_to_ceiling: 2m
```

The `mem` entry overrides `rate` only; everything else comes from `defaults:`.

## Temporal chains with `after:`

Use `after:` to express "this scenario starts when that one crosses a threshold". Sonda resolves
the timing at parse time -- no runtime reactivity -- by computing concrete `phase_offset` values
from each generator's shape.

The built-in `link-failover` scenario is a worked example: a primary interface flaps, a backup
link saturates once the primary drops, and latency degrades once the backup fills.

```yaml title="scenarios/link-failover.yaml"
version: 2

scenario_name: link-failover
category: network
description: "Edge router link failure with traffic shift to backup"

defaults:
  rate: 1
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    device: rtr-edge-01
    job: network

scenarios:
  - id: interface_oper_state
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s
    labels:
      interface: GigabitEthernet0/0/0

  - id: backup_link_utilization
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    labels:
      interface: GigabitEthernet0/1/0
    after:
      ref: interface_oper_state
      op: "<"
      value: 1

  - id: latency_ms
    signal_type: metrics
    name: latency_ms
    generator:
      type: degradation
      baseline: 5
      ceiling: 150
      time_to_degrade: 3m
    labels:
      path: backup
    after:
      ref: backup_link_utilization
      op: ">"
      value: 70
```

The `interface_oper_state` flap signal drops below `1` at `t=60s` (its `up_duration`), so
`backup_link_utilization` starts at that same moment via an auto-computed `phase_offset`. The
backup ramp crosses 70% a little over two minutes later, which is when `latency_ms` begins to
degrade. Scenarios linked by `after:` get an auto-assigned `clock_group` so their timers share a
start reference.

Use `--dry-run` to see the resolved timing:

```bash
sonda run --scenario scenarios/link-failover.yaml --dry-run
```

```text
[config] file: scenarios/link-failover.yaml (version: 2, 3 scenarios)

[config] [1/3] interface_oper_state

    name:           interface_oper_state
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      flap (up_duration: 60s, down_duration: 30s, up_value: 1, down_value: 0)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, interface=GigabitEthernet0/0/0, job=network
    clock_group:    chain_backup_link_utilization (auto)
---

[config] [2/3] backup_link_utilization

    name:           backup_link_utilization
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      saturation (baseline: 20, ceiling: 85, time_to_saturate: 2m)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, interface=GigabitEthernet0/1/0, job=network
    phase_offset:   1m
    clock_group:    chain_backup_link_utilization (auto)
---

[config] [3/3] latency_ms

    name:           latency_ms
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      degradation (baseline: 5, ceiling: 150, time_to_degrade: 3m)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, job=network, path=backup
    phase_offset:   152.308s
    clock_group:    chain_backup_link_utilization (auto)

Validation: OK (3 scenarios)
```

The `phase_offset` lines show the concrete delays Sonda computed from the threshold-crossing
math. The `(auto)` suffix on `clock_group` indicates Sonda assigned the group automatically
because of the `after:` relationship. The scenario also ships in the built-in catalog:

```bash
sonda catalog run link-failover
```

!!! tip "Supported generators in `after:`"
    The `after:` clause resolves against the target generator's trajectory. Supported shapes:
    `flap`, `saturation`, `leak`, `degradation`, `spike_event`. Using `steady` as the target is
    rejected -- sine crossings are ambiguous.

## Pack-backed entries

Reference a [metric pack](../guides/metric-packs.md) directly from a scenarios entry. Sonda
expands the pack at compile time -- you get one prepared scenario per metric in the pack.

```yaml title="snmp-interface.v2.yaml"
version: 2

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: edge_router_snmp
    signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
```

The `pack:` field replaces the `name:` + `generator:` combo. Any fields you set on the entry
(`labels`, `rate`, `duration`, `encoder`, `sink`) apply to every expanded metric.

## Clock groups

Scenarios with the same `clock_group` share a start-time reference, which keeps multi-signal
scenarios phase-aligned. You can set `clock_group:` explicitly:

```yaml
scenarios:
  - id: a
    clock_group: incident-1
    ...
  - id: b
    clock_group: incident-1
    ...
```

Or let Sonda auto-assign one when you use `after:` -- every scenario in the same `after:` chain
ends up in the same auto-named group (`chain_<head>`).

The start banner and the grouped run summary both show the `clock_group`. See
[Status output -- clock groups](cli-reference.md#clock-groups-in-status-output) for what the
banners look like at runtime.

## Scaffolding v2 files with `sonda init`

`sonda init` emits v2 YAML by default:

```bash
sonda init \
  --signal-type metrics \
  --domain infrastructure \
  --metric demo_cpu \
  --situation spike_event \
  --rate 5 --duration 30s \
  --encoder prometheus_text --sink stdout \
  -o ./scenarios/demo-cpu.yaml
```

```yaml title="./scenarios/demo-cpu.yaml"
# demo_cpu: infrastructure scenario using the 'spike_event' pattern.
#
# Generated by `sonda init`. Run with:
#   sonda run --scenario <this-file>

version: 2

# Defaults inherited by every entry in scenarios: below.
defaults:
  rate: 5
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: demo_cpu
    generator:
      type: spike_event
      baseline: 0.0
      spike_height: 100.0
      spike_duration: "10s"
      spike_interval: "30s"
```

Run the generated file with `sonda run --scenario`. For the full guided scaffolding flow
(signal types, packs, logs, histograms, summaries), see
[`sonda init`](cli-reference.md#sonda-init).

## Migrating from v1

Every legacy v1 shape maps cleanly to v2. Pick the tab that matches the file you have.

### How Sonda rejects v1

When Sonda encounters a file or request body without `version: 2`, it stops before running
anything and prints the error alongside a pointer to this guide.

=== "CLI (`sonda run`, `sonda metrics`, ...)"

    ```text title="stderr"
    error: scenario file /path/to/legacy.yaml is not a v2 scenario. Sonda only accepts v2 YAML (`version: 2` at the top level). Migrate this file to v2 — see docs/configuration/v2-scenarios.md for the migration guide.
    ```

    Exit code: `1`. Applies to `sonda run`, `sonda metrics`, `sonda logs`,
    `sonda histogram`, `sonda summary`, and `sonda catalog run` -- every `--scenario`
    consumer.

=== "Server (`POST /scenarios`)"

    ```http
    HTTP/1.1 400 Bad Request
    Content-Type: application/json

    {
      "error": "bad_request",
      "detail": "body is not a v2 scenario. Sonda only accepts v2 scenario bodies (`version: 2` at the top level). Migrate this body to v2 — see docs/configuration/v2-scenarios.md for the migration guide."
    }
    ```

    The server returns `400 Bad Request` (not `422`) because a non-v2 body is ill-formed by
    contract, not a semantic validation failure. See
    [Server API -- Start a Scenario](../deployment/sonda-server.md#start-a-scenario).

### Shape-by-shape migration

=== "Flat single-signal"

    The legacy "flat" layout put `name:`, `rate:`, `generator:`, `encoder:`, and `sink:` at the
    top level with no `scenarios:` wrapper. Wrap them in a single-entry v2 file:

    ```yaml title="Before (v1, flat single-signal)"
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
    labels:
      zone: us-east-1
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 100
      duration: 30s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: cpu_usage
        signal_type: metrics
        name: cpu_usage
        generator:
          type: sine
          amplitude: 50.0
          period_secs: 60
          offset: 50.0
        labels:
          zone: us-east-1
    ```

    Two things changed:

    - `version: 2` at the top, with shared `rate` / `duration` / `encoder` / `sink` moved into
      `defaults:`.
    - One entry under `scenarios:` with an explicit `signal_type: metrics`. The `id:` is
      free-form; it's what `after:` clauses and `clock_group:` references use.

=== "Top-level `scenarios:` list"

    The legacy multi-scenario layout already had a `scenarios:` list, but each entry repeated
    `encoder:`, `sink:`, `rate:`, and `duration:`. Add `version: 2`, move the shared fields
    into `defaults:`, and drop the repetition:

    ```yaml title="Before (v1, multi-scenario)"
    scenarios:
      - signal_type: metrics
        name: cpu
        rate: 10
        duration: 60s
        generator: { type: constant, value: 1 }
        encoder: { type: prometheus_text }
        sink: { type: stdout }
      - signal_type: metrics
        name: mem
        rate: 10
        duration: 60s
        generator: { type: constant, value: 2 }
        encoder: { type: prometheus_text }
        sink: { type: stdout }
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 10
      duration: 60s
      encoder: { type: prometheus_text }
      sink: { type: stdout }

    scenarios:
      - id: cpu
        signal_type: metrics
        name: cpu
        generator: { type: constant, value: 1 }
      - id: mem
        signal_type: metrics
        name: mem
        generator: { type: constant, value: 2 }
    ```

    Any per-entry field (`rate`, `labels`, `gaps`, `encoder`, ...) still wins over `defaults:`.

=== "Logs"

    Log entries move into a `scenarios:` entry with `signal_type: logs` and `log_generator:`
    (note the `log_` prefix -- v2 log entries use `log_generator:` to keep the discriminated
    union unambiguous):

    ```yaml title="Before (v1, flat logs)"
    name: app_logs
    rate: 10
    duration: 60s
    generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2"]
            endpoint: ["/api", "/health"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
    labels:
      job: sonda
      env: dev
    encoder:
      type: json_lines
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 10
      duration: 60s
      encoder:
        type: json_lines
      sink:
        type: stdout
      labels:
        job: sonda
        env: dev

    scenarios:
      - id: app_logs
        signal_type: logs
        name: app_logs
        log_generator:
          type: template
          templates:
            - message: "Request from {ip} to {endpoint}"
              field_pools:
                ip: ["10.0.0.1", "10.0.0.2"]
                endpoint: ["/api", "/health"]
          severity_weights:
            info: 0.7
            warn: 0.2
            error: 0.1
          seed: 42
    ```

=== "Histogram / Summary"

    Histogram and summary entries do not use `generator:` at all -- they use
    `distribution:`, `observations_per_tick:`, and (optionally) `buckets:` or `quantiles:`.
    Wrap them in a v2 entry with `signal_type: histogram` or `signal_type: summary`:

    ```yaml title="Before (v1, flat histogram)"
    name: http_request_duration_seconds
    rate: 1
    duration: 60s
    distribution:
      type: exponential
      rate: 10.0
    observations_per_tick: 100
    seed: 42
    labels:
      job: api
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 1
      duration: 60s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: http_request_duration_seconds
        signal_type: histogram
        name: http_request_duration_seconds
        distribution:
          type: exponential
          rate: 10.0
        observations_per_tick: 100
        seed: 42
        labels:
          job: api
    ```

    The `distribution`, `observations_per_tick`, `buckets`, and `quantiles` fields stay on the
    entry. See [Generators -- histogram and summary](generators.md#histogram-and-summary-generators)
    for the full field reference.

=== "`pack:` shorthand"

    The legacy `pack: <name>` shorthand file (a top-level `pack:` with `rate:` / `duration:` /
    `labels:`) becomes a single v2 entry whose body is `pack: <name>`:

    ```yaml title="Before (v1, pack shorthand)"
    pack: telegraf_snmp_interface
    rate: 1
    duration: 60s
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 1
      duration: 60s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: edge_router_snmp
        signal_type: metrics
        pack: telegraf_snmp_interface
        labels:
          device: rtr-edge-01
          ifName: GigabitEthernet0/0/0
          ifIndex: "1"
    ```

    See [Pack-backed entries](#pack-backed-entries) above for how the compiler expands
    `pack:` into one entry per metric.

=== "Hand-tuned `phase_offset`"

    If a legacy file chained signals with hand-tuned `phase_offset` values, v2 expresses the
    same temporal relationships declaratively with `after:` clauses. Each entry says *when it
    starts relative to another entry's shape* and the compiler resolves the offsets at parse
    time. See the [`after:` section](#temporal-chains-with-after) above.

### Common gotchas

- **`signal_type` is per-entry in v2.** Legacy files let you put `signal_type:` at the top
  level; v2 reads it from the first entry (`metrics`, `logs`, `histogram`, `summary`). Every
  entry in a multi-signal file carries its own `signal_type:`.
- **Log entries use `log_generator:`, not `generator:`.** Metrics use `generator:`;
  histograms and summaries use `distribution:`; logs use `log_generator:`. Mismatched keys
  trigger a v2 compile error.
- **`deny_unknown_fields` is strict.** Typos like `scenarioName:` or `desc:` at the top of a
  v2 file are rejected at parse time with the offending field name in the error. Fix the
  typo and re-run.
- **`sonda import` already emits v2.** Regenerate any imported scenarios with
  [`sonda import`](cli-reference.md#sonda-import) if you kept older output around.

## What next

- [**CLI Reference -- sonda run**](cli-reference.md#sonda-run) -- flag reference for running v2 files
- [**CLI Reference -- dry run**](cli-reference.md#dry-run) -- validate and preview a v2 file before running
- [**Scenario Fields**](scenario-fields.md) -- per-entry field reference (generators, labels, schedules)
- [**Server API**](../deployment/sonda-server.md) -- `POST /scenarios` accepts v2 YAML or JSON
- [**Metric Packs**](../guides/metric-packs.md) -- the pack catalog you can reference from v2 entries
