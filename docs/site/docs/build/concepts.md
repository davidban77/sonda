# Concepts

This page covers how Sonda's four parts nest, how packs let you reuse metric definitions, and what a multi-scenario run looks like. It assumes you have read [Your first scenario](../get-started/your-first-scenario.md), which introduces the scenario file, generator, encoder, and sink.

## How the parts nest

A catalog is a directory of scenario files. Each scenario file lists one or more entries. Each entry either declares its own generator, encoder, and sink, or references a pack. A pack is a separate file that holds a reusable bundle of metric definitions.

```
catalog/                       <-- a directory of YAML files you point sonda at
├── cpu-spike.yaml             <-- scenario file (kind: runnable)
│   └── scenarios:
│       └── - id: cpu          <-- entry (one signal you emit)
│           generator: ...       one of these per entry
│           encoder:   ...
│           sink:      ...
│
└── snmp-pack.yaml             <-- scenario file (kind: composable, i.e. a "pack")
    └── metrics:
        - name: ifHCInOctets     a reusable bundle of metric names
        - name: ifHCOutOctets    referenced from other scenarios by name
```

The next sections cover each part, starting from what `hello.yaml` in the get-started guide already showed.

## Scenario

A **scenario file** (see the [glossary](../reference/glossary.md#scenario)) is the YAML unit `sonda run` consumes. [Your first scenario](../get-started/your-first-scenario.md#scenario-file) covers the four top-level fields (`version`, `kind`, `defaults`, `scenarios`).

Two values for `kind:` exist. `kind: runnable` makes the file executable. `kind: composable` makes it a pack you reference from other files (see [Pack](#pack) below). For the full top-level field reference, including catalog metadata, environment-variable interpolation, and [sink-error policy](../reference/glossary.md#sink-error-policy), see [Scenario Files](scenario-files.md).

## Entry

An **entry** is one item under the `scenarios:` list. Each entry emits exactly one signal. The signal can be a metric series, a log stream, a histogram, or a summary. Histograms and summaries are different ways of representing distributions. See the glossary entries for [histogram](../reference/glossary.md#histogram) and [summary](../reference/glossary.md#summary) for the details.

A scenario file can hold one entry or many. Real systems emit many signals at once. A single process exposes CPU, memory, request rate, error rate, and queue depth in parallel. To model that, the scenario file declares one entry per metric and they all run together on shared defaults.

Each entry needs at minimum:

- `signal_type:` — the category: `metrics`, `logs`, `histogram`, or `summary`.
- `name:` (or a `pack:` reference).
- The [generator](../reference/glossary.md#generator) block for the signal type: `generator:` for metrics, `log_generator:` for logs, `distribution:` for histograms and summaries.

Everything else — `rate`, `duration`, [`encoder`](../reference/glossary.md#encoder), [`sink`](../reference/glossary.md#sink), `labels` — comes from `defaults:` unless the entry overrides it.

The example below declares four entries that imitate a node exporter. Node Exporter is the Prometheus agent that exposes host metrics like CPU, memory, disk, and network as a single HTTP endpoint.

```yaml title="node-exporter-style.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    instance: web-01
    job: node
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 30
      offset: 60
      period_secs: 60

  - id: mem
    signal_type: metrics
    name: memory_used_bytes
    generator:
      type: leak
      baseline: 2000000000
      ceiling: 6000000000
      time_to_ceiling: 5m

  - id: disk
    signal_type: metrics
    name: disk_io_bytes
    generator:
      type: sawtooth
      min: 1000000
      max: 50000000
      period_secs: 30

  - id: net
    signal_type: metrics
    name: network_throughput_bytes
    generator:
      type: spike
      baseline: 100000
      magnitude: 5000000
      duration_secs: 5
      interval_secs: 45
```

The four entries share one encoder, one sink, and one `labels` block. To a Prometheus scrape, the four series appear as if they came from a single endpoint. For the per-entry field reference covering generators, schedules, labels, encoders, sinks, `after:`, and `while:`, see [Scenario Fields](../reference/scenario-fields.md).

## Pack

The file above declared four entries by hand. That is fine for four metrics. Writing every metric by hand becomes tedious when a real exporter exposes thirty metrics. It is worse when you want twenty copies of that exporter across a fleet. Copy-pasting metric names causes typos and drift between files over time.

A **pack** (see the [glossary](../reference/glossary.md#pack)) is a reusable bundle of metric names, label schemas, and default generators per metric. You define a pack as a file with `kind: composable` and store it in the same directory as your runnable scenarios. A runnable entry references a pack with `pack: <name>`. Sonda then expands the reference into one entry per metric in the pack. Define the pack once; reference it from every scenario that needs that pattern.

The tabs below show the same five-metric SNMP interface entry written two ways. The "By hand" tab repeats the metric names, generators, and labels for every entry. The "With a pack" tab declares one entry that references a pack.

=== "By hand"

    ```yaml title="snmp-by-hand.yaml"
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
      - id: in_octets
        signal_type: metrics
        name: ifHCInOctets
        generator: { type: sawtooth, min: 0, max: 1000000000, period_secs: 60 }
        labels: { device: rtr-edge-01, ifName: Gi0/0/0 }
      - id: out_octets
        signal_type: metrics
        name: ifHCOutOctets
        generator: { type: sawtooth, min: 0, max: 1000000000, period_secs: 60 }
        labels: { device: rtr-edge-01, ifName: Gi0/0/0 }
      - id: in_errors
        signal_type: metrics
        name: ifInErrors
        generator: { type: constant, value: 0 }
        labels: { device: rtr-edge-01, ifName: Gi0/0/0 }
      - id: out_errors
        signal_type: metrics
        name: ifOutErrors
        generator: { type: constant, value: 0 }
        labels: { device: rtr-edge-01, ifName: Gi0/0/0 }
      - id: oper_status
        signal_type: metrics
        name: ifOperStatus
        generator: { type: constant, value: 1 }
        labels: { device: rtr-edge-01, ifName: Gi0/0/0 }
    ```

=== "With a pack"

    ```yaml title="snmp-with-pack.yaml"
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
      - id: edge_router_snmp
        signal_type: metrics
        pack: telegraf_snmp_interface
        labels:
          device: rtr-edge-01
          ifName: Gi0/0/0
    ```

The pack file sits in the same directory as the runnable file. When `sonda run` reads `pack: telegraf_snmp_interface`, it looks up the pack and produces one entry per metric. The metric names and shared labels match the by-hand version. To write your own pack and read the full field reference, see [Metric Packs](catalogs-and-packs.md).

## Catalog

Once you have more than one scenario file, Sonda needs to know where to look. A **catalog** (see the [glossary](../reference/glossary.md#catalog)) is a directory of scenario files you point `sonda` at with `--catalog <dir>`. Sonda walks the directory, indexes each file by its `name:` field, or by filename when `name:` is missing. You can then run any file with `sonda run @name`. Runnable files and packs live side by side; the `kind:` field tells Sonda which is which.

```
~/sonda-catalog/
├── cpu-spike.yaml          # kind: runnable,    name: cpu-spike
├── memory-leak.yaml        # kind: runnable,    name: memory-leak
└── snmp-interface.yaml     # kind: composable,  name: telegraf_snmp_interface
```

```bash
sonda --catalog ~/sonda-catalog list
sonda --catalog ~/sonda-catalog run @cpu-spike
```

Packs live in the catalog but you do not run them directly. They only take effect when a runnable entry references them by name. The catalog is yours: keep it in the same git repo as your alert rules and dashboards. Scenarios then version alongside the rules they test. The catalog can be flat or nested into subdirectories; Sonda walks the tree. For the discovery rules, `sonda list` and `sonda show` output, and the full directory contract, see [Catalogs](catalogs-and-packs.md).

## Defaults and overrides

The `defaults:` block factors out fields that would otherwise repeat on every entry: `rate`, `duration`, `encoder`, `sink`, `labels`, and `on_sink_error`. Each `scenarios:` entry then only declares what differs from the defaults.

```yaml title="defaults-and-overrides.yaml"
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    job: sonda
scenarios:
  - id: noisy
    signal_type: metrics
    name: noisy_metric
    generator: { type: sine, amplitude: 50, offset: 50, period_secs: 30 }

  - id: chatty
    signal_type: metrics
    name: chatty_metric
    generator: { type: constant, value: 1 }

  - id: slow
    signal_type: metrics
    name: slow_metric
    rate: 1                       # overrides the defaults: rate: 10
    generator: { type: constant, value: 1 }
```

Two entries inherit `rate: 10`. The third overrides to `rate: 1`. Every entry shares the same encoder, sink, and `job` label. This is the everyday convenience once a scenario file has more than one entry.

## Multi-scenario runs

A scenario file can mix signal types. The example below declares one metric, one histogram, and one log stream in the same file. The metric and histogram share a Prometheus remote-write sink. The log stream sends to Loki instead.

```yaml title="mixed-signals.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  sink:
    type: remote_write
    url: http://localhost:9090/api/v1/write
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    encoder: { type: remote_write }
    generator: { type: sine, amplitude: 30, offset: 60, period_secs: 60 }

  - id: latency
    signal_type: histogram
    name: http_request_duration_seconds
    encoder: { type: remote_write }
    distribution: { type: exponential, rate: 10.0 }
    observations_per_tick: 100

  - id: app_logs
    signal_type: logs
    name: app_logs
    encoder: { type: json_lines }
    sink: { type: loki, url: http://localhost:3100 }
    log_generator:
      type: template
      templates:
        - message: "request handled"
```

Three signal types and two sinks, all from one `sonda run`. The metric and histogram reach Prometheus; the logs reach Loki.

For entries that depend on each other in time, see `after:` and `while:` on the [Scenario Files](scenario-files.md#temporal-chains-with-after) page. The `after:` clause starts an entry once another crosses a threshold. The `while:` clause emits only while another entry is in a given state.

!!! info "Advanced: upstream lives in a different POST"
    When you run Sonda as an HTTP server, the upstream a `while:` clause depends on can arrive in a separate POST request. The clause then references the upstream by name across requests. See [Cross-POST `while:` refs](scenario-files.md#cross-post-while-refs).

## What next

- [**Scenario Files**](scenario-files.md) — full file-format reference, including `defaults:`, `after:` and `while:` chains, and [sink-error policy](../reference/glossary.md#sink-error-policy).
- [**Scenario Fields**](../reference/scenario-fields.md) — per-entry fields: generators, schedules, labels, encoders, sinks.
- [**Catalogs and packs**](catalogs-and-packs.md) — directory layout, `sonda list`, `sonda show`, and how to write your own packs.
</content>
</invoke>