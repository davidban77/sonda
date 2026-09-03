---
title: Built-in packs
description: The metric packs compiled into the sonda binary — what each one emits, and how to reference, override or extend it.
---

# Built-in packs

Sonda ships a small curated catalog of [metric packs](catalogs-and-packs.md#packs) compiled into the binary. They need no `--catalog` and no files on disk:

```bash
sonda list
```

```text title="Output"
KIND        NAME                     TAGS  DESCRIPTION

[application]
composable  http_server_red                Request rate, error rate and latency quantiles (RED)

[infrastructure]
composable  node_exporter_cpu              Per-CPU mode counters (node_exporter-compatible)
composable  node_exporter_memory           Memory gauge metrics (node_exporter-compatible)

[kubernetes]
composable  kube_state_metrics             Pod phase, deployment replicas and restarts (kube-state-metrics)

[network]
composable  telegraf_snmp_interface        Standard SNMP interface metrics (Telegraf-normalized)
```

Reference one from any runnable scenario:

```yaml
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
    pack: http_server_red
    labels:
      service: checkout
      job: checkout
```

`sonda show <name>` prints any of them verbatim, which is the fastest way to see the metric names, ids and default generators a pack declares.

## What each pack is for

Every pack uses the real upstream metric names and label keys, so a dashboard query or alert rule written against the real exporter works against sonda unchanged. That is the point of them — they are not illustrative names.

| Pack | Category | Models |
|------|----------|--------|
| `http_server_red` | application | Rate, Errors, Duration for a request-serving service |
| `kube_state_metrics` | kubernetes | Pod phase, the deployment replica gap, container restarts, node readiness |
| `node_exporter_cpu` | infrastructure | `node_cpu_seconds_total` per CPU mode |
| `node_exporter_memory` | infrastructure | node_exporter memory gauges |
| `telegraf_snmp_interface` | network | SNMP interface counters and status, Telegraf-normalized |

### `http_server_red`

`http_requests_total` carries one series per status class (`200`, `404`, `500`), so `rate()` over the whole metric is your request rate and the `code="500"` series is your error rate. The defaults put roughly 0.4% of requests in the 5xx bucket, which is near a typical alert threshold without being over it.

`http_request_duration_seconds` is exposed as pre-computed quantile series — the shape a Prometheus **summary** has, not a histogram. A pack emits one value per spec, so it cannot produce `_bucket`/`_sum`/`_count`; for real buckets and `histogram_quantile()`, use a `signal_type: histogram` entry rather than a pack.

### `kube_state_metrics`

Enum metrics follow the upstream one-series-per-value convention: `kube_pod_status_phase` appears five times, one per phase, carrying 1 for the phase that holds and 0 for the rest. Those are five series of one metric, which is why each spec declares its phase as an `id:` — see [addressing a repeated metric name](catalogs-and-packs.md#addressing-a-repeated-metric-name).

`kube_deployment_status_replicas_available` cycles 60s at 3 replicas then 20s at 2, so an "unavailable replicas" rule has something to fire on. A run shorter than 60s only ever sees the healthy state.

## Change one metric without editing the pack

Use `overrides:`, keyed by selector — a metric name, or `name.id` where the pack repeats that name:

```yaml
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
    pack: http_server_red
    labels:
      service: checkout
      job: checkout
    overrides:
      http_request_duration_seconds.p99:
        generator:
          type: spike_event
          baseline: 0.310
          spike_height: 0.900
          spike_duration: 20s
          spike_interval: 180s
```

That turns the p99 series into a latency incident and leaves the other eight untouched. A bare `http_request_duration_seconds` would be an error, listing the three ids to choose from — the pack repeats that name, so a bare key addresses no single metric.

## Build on one instead of copying it

To model a specific platform, [extend](catalogs-and-packs.md#extend-a-pack) a built-in rather than forking it. The base stays where it is and you state only the difference:

```yaml
version: 2
kind: composable

name: checkout_api
description: "The checkout service's RED metrics"
category: application
extends: http_server_red

shared_labels:
  service: checkout

metrics:
  - name: http_request_queue_depth
    generator:
      type: steady
      center: 4.0
      amplitude: 2.0
      period: 120s
      noise: 0.4

deviations:
  - metric: http_requests_total.server_error
    replace:
      generator:
        type: step
        start: 0.0
        step_size: 0.4
```

Drop that in your `--catalog <dir>` and reference `pack: checkout_api`. Because `extends:` resolves through the same lookup as `pack:`, shadowing `http_server_red` with your own file in that directory would change every extension built on it.

!!! warning "Pack defaults are a starting point, not a fixture"
    The generators here are chosen to look plausible and to give alert rules something to evaluate. They are not a recording of any real system. When you need exact values — a specific incident, a captured production window — use [`sonda capture`](../import/index.md) or a `csv_replay` generator instead of tuning pack defaults.

## Adding to the built-in set

The built-in packs live in `packs/` at the repo root and are embedded with `include_str!`. Adding one means adding its entry to `sonda-core/src/catalog/builtin.rs` **and** moving `PACK_COUNT` — the count gate fails if you do only one, so a half-wired pack cannot ship quietly.

Two checks run over the whole set on every commit. Each pack must compile through the real pipeline with the real resolver, and no pack's default generator may emit a negative value: every metric these packs model is a count, a duration, a byte total or a 0/1 enum. That second gate exists because a latency series once shipped at -0.44 seconds — the `steady` alias defaults its noise to ±1.0 *absolute*, which is fine for a percentage and ruinous for a 32ms quantile. Set `noise:` explicitly on any small-magnitude generator.
