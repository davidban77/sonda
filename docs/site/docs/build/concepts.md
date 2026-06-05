# Concepts

## What Sonda is

Sonda is a synthetic telemetry generator. You write a YAML recipe that says *"pretend to be a CPU metric that oscillates between 40% and 80%"* or *"pretend to be a router emitting interface counters"* or *"pretend to be an application emitting JSON logs at 100/sec"* — Sonda produces realistic-looking data shaped exactly like the real thing and ships it to your sinks (stdout, a file, Prometheus remote write, Loki, Kafka, OTLP). You point your dashboards, alert rules, and ingestion pipelines at the synthetic stream and exercise them without needing real production traffic.

The mental model in one sentence: **Sonda turns YAML recipes into telemetry streams.**

## A first example

A scenario is a YAML file you hand to `sonda run`. The smallest useful one is six lines of substance:

```yaml title="hello.yaml"
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
  - id: cpu
    signal_type: metrics
    name: demo_cpu
    generator:
      type: constant
      value: 42
```

```bash
sonda run hello.yaml
```

That file emits one Prometheus-formatted metric named `demo_cpu` with a constant value of `42`, once per second, for thirty seconds, printed to stdout. Reading top to bottom: `version: 2` and `kind: runnable` mark this as a scenario file Sonda can run. The `defaults:` block sets the cadence (`rate: 1`), how long to run (`duration: 30s`), the wire format — Prometheus exposition format (Prometheus's plain-text metric format; see the [glossary](../reference/glossary.md#prometheus-exposition-format)) via `prometheus_text` — and the [sink](../reference/glossary.md#sink) (`stdout`). The `scenarios:` block lists what to emit — here, exactly one item, a constant-valued metric.

That one YAML file demonstrates two of the four concepts Sonda is built around. The whole picture is four nouns, each one solving a problem the previous one creates.

## The four pieces

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

The pieces nest. A catalog directory contains scenario files. Each scenario file contains one or more entries. An entry either declares its own generator/encoder/sink or references a pack. The four sections below introduce each concept in order, starting from what `hello.yaml` already showed.

## Scenario

A **scenario file** (see the [glossary](../reference/glossary.md#scenario)) is the YAML unit `sonda run` consumes. `hello.yaml` above is a complete one. Every scenario file declares two top-level fields:

- `version: 2` — the format version.
- `kind: runnable` — a file you can execute. `kind: composable` marks a file as a pack instead (see [Pack](#pack) below).

The other top-level fields are `defaults:` (shared settings) and `scenarios:` (the list of entries). The `kind:` distinction is the rule of thumb: `kind: runnable` makes the file executable; `kind: composable` makes it a pack you reference from other files. For the full top-level field reference — catalog metadata, environment-variable interpolation, sink-error policy — see [Scenario Files](scenario-files.md).

## Entry

An **entry** is one item under the `scenarios:` list. Each entry emits exactly one signal — one metric series, one log stream, one histogram (distribution across buckets — see the [glossary](../reference/glossary.md#histogram)), one summary (distribution observed via quantile sampling — see the [glossary](../reference/glossary.md#summary)). `hello.yaml` had one entry; that is the floor, not the ceiling.

Why scenario files have multiple entries: production systems emit many signals at once. A single process typically exposes dozens of metrics in parallel (CPU, memory, request rate, error rate, queue depth, ...). To model that realistically, the scenario file declares one entry per metric and they all run together on shared defaults. Each entry needs at minimum a `signal_type:` (the category — `metrics`, `logs`, `histogram`, or `summary`), a `name:` (or a `pack:` reference), and the [generator](../reference/glossary.md#generator) block that matches its signal type: `generator:` for metrics, `log_generator:` for logs, `distribution:` for histograms and summaries. Everything else — `rate`, `duration`, [`encoder`](../reference/glossary.md#encoder), [`sink`](../reference/glossary.md#sink), `labels` — inherits from `defaults:` unless the entry overrides it.

A four-entry node-exporter-shaped file:

```yaml title="node-exporter-shape.yaml"
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

Four metrics, four generators, one shared encoder + sink + labels block — all four series scrape together as if they came from a single exporter. For the per-entry field reference (generators, schedules, labels, encoders, sinks, `after:` / `while:`) see [Scenario Fields](../reference/scenario-fields.md).

## Pack

The node-exporter file above declared four entries by hand. That is fine for four metrics. It gets old when you want to simulate a real exporter that exposes thirty metrics, and worse when you want twenty instances of that exporter across a fleet. Copy-pasting metric names is a recipe for typos and drift.

A **pack** (see the [glossary](../reference/glossary.md#pack)) is a reusable bundle of metric names, label schemas, and sensible default generators per metric. You express a pack as a file with `kind: composable` and store it in the same directory as your runnable scenarios. A runnable entry references a pack with `pack: <name>`, and `sonda run` expands the reference at parse time into one entry per metric in the pack. Author the pack once; reference it from every scenario that uses that shape.

Side-by-side — writing five SNMP interface entries by hand versus referencing one pack:

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

The pack file sits in the same directory as the runnable file. At parse time, `sonda run` reads `pack: telegraf_snmp_interface`, looks it up, and produces one prepared entry per metric — same names, same shared labels, ready to scrape. To author your own pack and read the full field reference, see [Metric Packs](catalogs-and-packs.md).

## Catalog

Once you have more than one scenario file — and especially once packs enter the picture — Sonda needs to know where to look. A **catalog** (see the [glossary](../reference/glossary.md#catalog)) is a directory of scenario files you point `sonda` at with `--catalog <dir>`. Sonda walks the directory, indexes each file by its `name:` (or by filename if `name:` is omitted), and lets you run anything in it with `sonda run @name`. Runnable files and packs live side by side — the `kind:` field tells Sonda which is which.

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

Packs live in the catalog but you do not run them directly — they are only meaningful when a runnable entry references them by name. The catalog is yours: keep it in the same git repo as your alert rules and dashboards, so the scenarios that exercise those rules ship alongside them. You can keep the catalog flat or nest subdirectories — Sonda walks the tree. For the discovery rules, `sonda list` / `sonda show` output, and the full directory contract, see [Catalogs](catalogs-and-packs.md).

## Defaults & overrides

The `defaults:` block factors out fields that would otherwise repeat on every entry — `rate`, `duration`, `encoder`, `sink`, `labels`, `on_sink_error`. Each `scenarios:` entry only declares what differs.

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

Two entries inherit `rate: 10`; the third overrides to `rate: 1`. Every entry shares the same encoder, sink, and `job` label. This is the everyday convenience once a scenario file has more than one entry.

## Multi-scenario runs

When a scenario file has multiple entries, every entry runs on its own thread, concurrently, each on its own clock. They share `defaults:` by default but can override anything per entry. A common pattern: every entry pushes to the same Prometheus remote-write sink, so one `sonda run` populates a backend with a realistic mix of metrics + logs + histograms from one process.

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

Three signal types, three threads, two sinks — metrics + histogram go to Prometheus, logs go to Loki. For entries that depend on each other in time (one starts only after another crosses a threshold; one emits only while another is in a given state), see `after:` and `while:` on the [Scenario Files](scenario-files.md#temporal-chains-with-after) page. When the upstream signal is itself driven by a separate POST to a running [`sonda-server`](../deploy/server.md), the `while:` clause supports cross-POST refs — see [Cross-POST `while:` refs](scenario-files.md#cross-post-while-refs). For a hands-on walkthrough, see the [Multi-Scenario Runs](scenario-files.md) tutorial.

## What next

- [**Scenario Files**](scenario-files.md) — the full file-format reference: every top-level field, `defaults:`, `after:` / `while:` temporal chains, environment variable interpolation, sink-error policy.
- [**Scenario Fields**](../reference/scenario-fields.md) — per-entry field reference: generators, schedules, labels, encoders, sinks.
- [**Catalogs**](catalogs-and-packs.md) — directory layout, `sonda list`, `sonda show`.
- [**Metric Packs**](catalogs-and-packs.md) — authoring composable packs.
- [**Multi-Scenario Runs**](scenario-files.md) — walkthrough of a file with several signals running concurrently.
