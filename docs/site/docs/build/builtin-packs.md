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
composable  openconfig_interface           OpenConfig interface counters and status, with gNMI paths
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

!!! note "These examples have no \"Run in playground\" button"
    The playground runs the engine in your browser with no catalog, so it cannot resolve a `pack:` reference at all — every example on this page needs the CLI. They are compiled in CI like every other scenario on the site; it is only the in-page run button they cannot offer.

## What each pack is for

Every pack uses the real upstream metric names and label keys, so a dashboard query or alert rule written against the real exporter works against sonda unchanged. That is the point of them — they are not illustrative names.

| Pack | Category | Models |
|------|----------|--------|
| `http_server_red` | application | Rate, Errors, Duration for a request-serving service |
| `kube_state_metrics` | kubernetes | Pod phase, the deployment replica gap, container restarts, node readiness |
| `node_exporter_cpu` | infrastructure | `node_cpu_seconds_total` per CPU mode |
| `node_exporter_memory` | infrastructure | node_exporter memory gauges |
| `openconfig_interface` | network | OpenConfig interface counters, oper-status and admin-status, with the gNMI path and value type of each |
| `telegraf_snmp_interface` | network | SNMP interface counters and status, Telegraf-normalized, with the IF-MIB OID and SNMP type of each |

### `http_server_red`

`http_requests_total` carries one series per status class (`200`, `404`, `500`), so `rate()` over the whole metric is your request rate and the `code="500"` series is your error rate. The defaults put roughly 0.4% of requests in the 5xx bucket, which is near a typical alert threshold without being over it.

`http_request_duration_seconds` is exposed as pre-computed quantile series — the shape a Prometheus **summary** has, not a histogram. A pack emits one value per spec, so it cannot produce `_bucket`/`_sum`/`_count`; for real buckets and `histogram_quantile()`, use a `signal_type: histogram` entry rather than a pack.

### `kube_state_metrics`

Enum metrics follow the upstream one-series-per-value convention: `kube_pod_status_phase` appears five times, one per phase, carrying 1 for the phase that holds and 0 for the rest. Those are five series of one metric, which is why each spec declares its phase as an `id:` — see [addressing a repeated metric name](catalogs-and-packs.md#addressing-a-repeated-metric-name).

`kube_deployment_status_replicas_available` cycles 60s at 3 replicas then 20s at 2, so an "unavailable replicas" rule has something to fire on. A run shorter than 60s only ever sees the healthy state.

### `openconfig_interface`

The counters and status leaves of an OpenConfig interface (`/interfaces/interface/state` in the `openconfig-interfaces` model): `in_octets`, `out_octets`, `in_pkts`, `out_pkts`, `in_errors`, `out_errors`, `in_discards`, `out_discards`, `carrier_transitions`, `oper_status` and `admin_status`. The status metrics are enum codes, 1 UP, 2 DOWN, 3 TESTING, and default to UP.

Every metric carries a `gnmi:` hint: its OpenConfig path and value type. Under the [`gnmi` encoder](encoders.md#paths-and-values-from-a-pack) the hints fill the encoder's `paths:` and `values:`, so the encoder block needs no template:

```yaml
version: 2
kind: runnable

defaults:
  rate: 1
  encoder:
    type: gnmi
    origin: openconfig-interfaces
    target_label: device
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400

scenarios:
  - signal_type: metrics
    pack: openconfig_interface
    labels: { device: rtr-edge-01, ifName: Ethernet1 }
```

`in_octets` on `Ethernet1` is served at `/interfaces/interface[name=Ethernet1]/state/counters/in-octets` as a `uint_val`, and `oper_status` at `/interfaces/interface[name=Ethernet1]/state/oper-status` as the string `UP`. Under any other encoder the hints are not used and the pack emits plain metrics. [`examples/gnmi-pack.yaml`](https://github.com/davidban77/sonda/blob/main/examples/gnmi-pack.yaml) runs it for two interfaces.

### `telegraf_snmp_interface` { #snmp-hints }

The IF-MIB interface metrics Telegraf's `inputs.snmp` collects, under the names Telegraf gives them: `ifOperStatus`, `ifHCInOctets`, `ifHCOutOctets`, `ifInErrors` and `ifOutErrors`, labelled `device`, `ifName`, `ifAlias`, `ifIndex` and `job`. `ifOperStatus` is an enum code, 1 up, 2 down, and defaults to up.

Every metric carries an `snmp:` hint: its IF-MIB column, indexed by the `ifIndex` label, and its SNMP type. Under the [`snmp` encoder](encoders.md#snmp) the hints fill the encoder's `oids:` and `types:`, so the encoder block names no OIDs:

| Metric | OID | Type |
|--------|-----|------|
| `ifOperStatus` | `1.3.6.1.2.1.2.2.1.8.{ifIndex}` | `integer` |
| `ifHCInOctets` | `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}` | `counter64` |
| `ifHCOutOctets` | `1.3.6.1.2.1.31.1.1.1.10.{ifIndex}` | `counter64` |
| `ifInErrors` | `1.3.6.1.2.1.2.2.1.14.{ifIndex}` | `counter32` |
| `ifOutErrors` | `1.3.6.1.2.1.2.2.1.20.{ifIndex}` | `counter32` |

```yaml
version: 2
kind: runnable

defaults:
  rate: 1
  encoder:
    type: snmp
    drop_labels: [ifName, ifAlias, job]
  sink:
    type: snmp_agent
    listen: 0.0.0.0:1161

scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels: { device: rtr-edge-01, ifIndex: 1, ifName: Ethernet1 }
```

`ifHCInOctets` on interface 1 is served at `1.3.6.1.2.1.31.1.1.1.6.1` as a `Counter64`. Every label is either in the instance OID or dropped: `ifIndex` is the index arc and `device` the agent label, so `ifName`, `ifAlias` and `job` are dropped. An `oids:` or `types:` entry in the encoder block wins over the hint. Under any other encoder the hints are not used and the pack emits plain metrics. [`examples/snmp-telegraf.yaml`](https://github.com/davidban77/sonda/blob/main/examples/snmp-telegraf.yaml) runs it for four interfaces, polled by [Telegraf](sinks.md#snmp-telegraf).

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

That turns the p99 series into a latency incident and leaves the other six untouched — the pack emits seven series in total. A bare `http_request_duration_seconds` would be an error, listing the three ids to choose from — the pack repeats that name, so a bare key addresses no single metric.

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
