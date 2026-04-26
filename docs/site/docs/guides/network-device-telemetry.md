# Network Device Telemetry

You inherit a snmp_exporter dashboard for a 200-router fleet. The PR that introduces
a new "Top Talkers" panel and an `InterfaceErrorBurst` alert needs to ship Monday.
The lab has two routers and a port-channel that never flaps. The interesting cases --
a 32-bit counter wrapping at peak traffic, a primary uplink dropping while the backup
saturates, BGP sessions toggling between Established and Idle -- are exactly the
shapes neither lab will produce on demand. Asking netops to break a production link
to test your dashboard is not a strategy.

Sonda models each interface as its own metric stream with the labels snmp_exporter
emits (`device`, `ifName`, `ifAlias`, `job=snmp`), then composes them into a scenario
that recreates the failure cascade you cannot trigger in the lab. PromQL written
against the synthetic data is the same PromQL you ship: `rate(interface_in_octets[1m])`
behaves the same way against a sawtooth-modeled counter as it does against a real
SNMP poll. The dashboard you tune against this scenario is the dashboard you ship.

This guide walks you through modeling a router with multiple uplinks, generating SNMP-style
metrics, simulating a link failure cascade, and validating your PromQL queries against the
synthetic data.

**What you need:**

- Sonda installed ([Getting Started](../getting-started.md))
- Familiarity with network monitoring concepts (interface counters, operational state, SNMP)

---

## Model a network device

A typical network device exposes several metric families per interface, plus system-level gauges.
Here is what we will model for a core router (`rtr-core-01`) with two uplinks:

| Metric | Type | Generator | Why |
|--------|------|-----------|-----|
| `interface_in_octets` | Counter | sawtooth | Monotonically increasing byte counter that resets at period boundary -- mimics SNMP ifInOctets |
| `interface_out_octets` | Counter | sawtooth | Same pattern for egress traffic |
| `interface_oper_state` | Gauge | constant / sequence | 1 = up, 0 = down -- toggles during failure scenarios |
| `interface_errors` | Counter | spike | Low baseline with periodic error bursts |
| `device_cpu_percent` | Gauge | sine | Smooth oscillation representing normal CPU load |
| `device_memory_percent` | Gauge | sine | Memory utilization with gentle oscillation |

Each interface gets its own set of labels (`device`, `ifName`, `ifAlias`, `job`) so the metrics
are distinguishable in PromQL, just like real SNMP-exported data.

### Why these generators?

**Sawtooth for counters.** SNMP interface counters are monotonically increasing values that reset
at a wrap point (32-bit or 64-bit max). The sawtooth generator ramps linearly from `min` to `max`
and resets -- exactly the shape you see from `ifInOctets` between polling intervals. Use
`rate()` in PromQL to derive throughput, just like you would with real SNMP data.

**Sine for system gauges.** CPU and memory utilization on a router fluctuates smoothly based on
traffic load and routing table churn. The sine generator produces that natural oscillation. Add
jitter for realism.

**Spike for error counters.** Interface errors are typically zero, with occasional bursts during
link instability or CRC failures. The spike generator holds at a baseline and periodically
injects a burst -- perfect for testing error-rate alerts.

**Sequence for state modeling.** When you need precise control over a timeline (interface goes
down at second 10, comes back at second 20), the sequence generator steps through an explicit
list of values. This is how you script failure scenarios.

---

## Generate baseline telemetry

The baseline scenario models `rtr-core-01` in a healthy state: both uplinks carrying traffic,
all interfaces up, steady CPU and memory.

```bash
sonda --dry-run run --scenario examples/network-device-baseline.yaml
```

```yaml title="examples/network-device-baseline.yaml (excerpt)"
version: 2

defaults:
  rate: 1
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  # Interface traffic counter (sawtooth = monotonic ramp)
  - signal_type: metrics
    name: interface_in_octets
    generator:
      type: sawtooth
      min: 0.0
      max: 500000000.0
      period_secs: 300
    jitter: 1000000.0
    jitter_seed: 10
    labels:
      device: rtr-core-01
      ifName: GigabitEthernet0/0/0
      ifAlias: uplink-isp-a
      job: snmp

  # Interface operational state (1 = up)
  - signal_type: metrics
    name: interface_oper_state
    generator:
      type: constant
      value: 1.0
    labels:
      device: rtr-core-01
      ifName: GigabitEthernet0/0/0
      ifAlias: uplink-isp-a
      job: snmp

  # ... more interfaces, CPU, memory (9 scenarios total)
```

The full file contains 9 concurrent scenarios: `interface_in_octets` and `interface_out_octets`
for both interfaces, `interface_oper_state` for both, `interface_errors` for the primary uplink,
plus `device_cpu_percent` and `device_memory_percent`.

Run it:

```bash
sonda run --scenario examples/network-device-baseline.yaml
```

```text title="Sample output (interleaved from 9 threads)"
interface_in_octets{device="rtr-core-01",ifAlias="uplink-isp-a",ifName="GigabitEthernet0/0/0",job="snmp"} 0 1775265944249
interface_oper_state{device="rtr-core-01",ifAlias="uplink-isp-a",ifName="GigabitEthernet0/0/0",job="snmp"} 1 1775265944250
device_cpu_percent{device="rtr-core-01",job="snmp"} 36.42 1775265944251
```

Each scenario runs on its own thread at 1 event/second, matching a typical SNMP polling interval.
The output interleaves across all 9 metric streams.

!!! tip "Match your polling interval"
    Set `rate: 1` for 1-second resolution or `rate: 0.2` for a 5-second SNMP polling interval
    (one event every 5 seconds). The rate controls how many data points Sonda produces per
    second -- match it to your real collection interval for realistic dashboard testing.

---

## Which failure pattern to steal

Two example scenarios model a link failure, and they use different mechanics. Pick the one that
matches how you want to reason about time:

| Scenario | Mechanic | Best for |
|----------|----------|----------|
| `examples/network-link-failure.yaml` | `sequence` generator + `repeat: true`, aligned tick-by-tick across multiple entries | Tight, repeating cycles where every tick's value matters and failures recur on a fixed drumbeat |
| `scenarios/link-failover.yaml` | v2 `after:` chains -- each signal declares what it waits for, the compiler resolves phase offsets | Once-through causal chains where you care about ordering (primary drops -> backup saturates -> latency climbs) but not the exact tick |

Use `sequence + repeat` when you need hand-authored values at specific ticks and the same
pattern should loop indefinitely -- useful for soak testing and for dashboards that expect a
steady rhythm. Use `after:` when the signals form a cascade and you would rather declare
"latency starts degrading when the backup saturates" than count seconds across four entries.
Both patterns ship as runnable example files; you can mix them in the same scenario if you
have a repeating failure that also triggers a cascade.

---

## Simulate a link failover

The interesting part: what happens when a primary link drops? Traffic shifts to the backup path,
the backup saturates as it absorbs double the load, and latency climbs as the backup fills.
Testing your dashboards and alerts against that cascade is the whole point.

The built-in `link-failover` scenario models the cascade as a 3-signal causal chain. Each signal
uses a dedicated generator (not a hand-scripted sequence) and the `after:` field tells Sonda to
delay a signal until the one it depends on crosses a threshold:

| Signal | Generator | Starts when |
|--------|-----------|-------------|
| `interface_oper_state` (primary) | `flap` -- 60s up, 30s down, cycling | `t=0` |
| `backup_link_utilization` | `saturation` -- ramps 20% -> 85% over 2m | primary drops below 1 (first flap) |
| `latency_ms` | `degradation` -- climbs 5ms -> 150ms over 3m | backup utilization exceeds 70% |

Sonda resolves the chain at parse time: the v2 compiler computes a concrete `phase_offset` for
each linked signal, so the signals still emit independently but start in the right order.
See the [v2 `after:` chain reference](../configuration/v2-scenarios.md#temporal-chains-with-after)
for the underlying mechanics.

```yaml title="scenarios/link-failover.yaml (excerpt)"
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

  # ... plus latency_ms, gated on backup_link_utilization > 70
```

Run the scenario from the catalog or directly from disk:

```bash
sonda catalog run link-failover
sonda run --scenario scenarios/link-failover.yaml
```

Use `--dry-run` to see the resolved `phase_offset` values that Sonda computed from the `after:`
clauses:

```bash
sonda run --scenario scenarios/link-failover.yaml --dry-run
```

```text title="Output (abridged)"
[config] file: scenarios/link-failover.yaml (version: 2, 3 scenarios)

[config] [1/3] interface_oper_state
    generator:      flap (up_duration: 60s, down_duration: 30s, up_value: 1, down_value: 0)
    clock_group:    chain_backup_link_utilization (auto)

[config] [2/3] backup_link_utilization
    generator:      saturation (baseline: 20, ceiling: 85, time_to_saturate: 2m)
    phase_offset:   1m
    clock_group:    chain_backup_link_utilization (auto)

[config] [3/3] latency_ms
    generator:      degradation (baseline: 5, ceiling: 150, time_to_degrade: 3m)
    phase_offset:   152.308s
    clock_group:    chain_backup_link_utilization (auto)

Validation: OK (3 scenarios)
```

The `phase_offset:` lines show the concrete delays Sonda derived from each `after:` threshold:
the backup saturates 1 minute in (when the primary first flaps down), and latency begins
degrading ~152 seconds in (when the backup ramp crosses 70%). All three signals share the same
auto-assigned `clock_group`, so their timers start from the same reference.

!!! info "Why `after:` instead of aligned sequences?"
    You can express a link failure with the `sequence` generator by hand-aligning values across
    scenarios -- that is what the built-in [interface-flap](scenarios.md#network) scenario does.
    `after:` is the v2 equivalent: instead of counting ticks, you declare the causal relationship
    once and let the compiler do the timing math. The [v2 scenarios guide](../configuration/v2-scenarios.md)
    covers the full surface.

---

## Label design for network metrics

Choosing the right labels determines whether your PromQL queries work naturally. The examples
in this guide use labels that mirror what real SNMP exporters produce:

| Label | Purpose | Example |
|-------|---------|---------|
| `device` | Hostname or FQDN of the network device | `rtr-core-01` |
| `ifName` | SNMP ifName (interface identifier) | `GigabitEthernet0/0/0` |
| `ifAlias` | Human-readable interface description | `uplink-isp-a` |
| `job` | Prometheus job label for scrape grouping | `snmp` |

This matches the label schema used by [snmp_exporter](https://github.com/prometheus/snmp_exporter)
and similar tools, so your dashboards and alert rules work the same way with synthetic data
as they do with real SNMP-exported metrics.

??? tip "Adding more interfaces"
    To model a device with more interfaces, duplicate a scenario entry and change the
    `ifName` and `ifAlias` labels. Each entry runs on its own thread, so adding interfaces
    is linear -- 10 interfaces with 4 metrics each means 40 concurrent scenarios. Sonda
    handles this comfortably at low rates (1/s per metric).

---

## PromQL queries for network monitoring

With synthetic data flowing, you can validate the PromQL queries that power your dashboards
and alerts. Here are the most common network monitoring queries, ready to use with the metrics
from the example scenarios.

### Interface throughput

Derive bits-per-second from the octets counter:

```promql
rate(interface_in_octets{device="rtr-core-01"}[1m]) * 8
```

This works because the sawtooth generator produces a monotonically increasing counter --
`rate()` computes the per-second derivative, and multiplying by 8 converts octets to bits.

### Interface state

Detect interfaces that are down:

```promql
interface_oper_state{device="rtr-core-01"} == 0
```

During the link failure scenario, this returns `GigabitEthernet0/0/0` for seconds 10--19
of each 30-second cycle.

### Error rate

Alert on sustained interface errors:

```promql
rate(interface_errors{device="rtr-core-01"}[5m]) > 0
```

### Traffic shift detection

Compare traffic ratios between interfaces to detect redistribution:

```promql
  rate(interface_in_octets{device="rtr-core-01",ifName="GigabitEthernet0/0/1"}[1m])
/
  (
    rate(interface_in_octets{device="rtr-core-01",ifName="GigabitEthernet0/0/0"}[1m])
    + rate(interface_in_octets{device="rtr-core-01",ifName="GigabitEthernet0/0/1"}[1m])
  )
```

Under normal conditions this ratio sits near 0.4 (ISP-B carries less traffic). During a
failure on Gi0/0/0, it jumps to 1.0 -- all traffic is on the backup link.

---

## Push to a monitoring backend

The example scenarios output to stdout for quick iteration. To push metrics into VictoriaMetrics
or Prometheus, change the sink in each scenario entry.

=== "VictoriaMetrics (HTTP push)"

    Replace the `sink` block in each scenario:

    ```yaml
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

    If you are using the project's Docker Compose stack:

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml up -d
    ```

    Then modify the scenario sinks to point at VictoriaMetrics and run:

    ```bash
    sonda run --scenario examples/network-device-baseline.yaml
    ```

    Verify data arrived:

    ```bash
    curl -s "http://localhost:8428/api/v1/query?query=interface_in_octets" | jq '.data.result | length'
    ```

=== "Prometheus (remote write)"

    Use the remote write encoder and sink for native Prometheus ingestion:

    ```yaml
    encoder:
      type: remote_write
    sink:
      type: remote_write
      url: "http://localhost:9090/api/v1/write"
      batch_size: 100
    ```

    Remote write works with Prometheus, Thanos Receive, Cortex, Mimir, Grafana Cloud, and
    VictoriaMetrics.

=== "File (offline analysis)"

    Write to a file for offline inspection or replay:

    ```yaml
    sink:
      type: file
      path: /tmp/network-metrics.txt
    ```

!!! tip "Change the sink in one place"
    In a v2 scenario file, the `defaults:` block holds the shared `sink` (and `encoder`,
    `rate`, `duration`, `labels`). Swap the sink there once and every entry in
    `scenarios:` picks it up. Per-entry overrides still win if you need a mixed setup.

---

## Alert rule examples

Here are Prometheus/VictoriaMetrics alert rules designed for network device monitoring. Test them
against the link failure scenario to verify they fire and resolve correctly.

```yaml title="network-alert-rules.yaml"
groups:
  - name: network-device-alerts
    interval: 10s
    rules:
      - alert: InterfaceDown
        expr: interface_oper_state{job="snmp"} == 0
        for: 30s
        labels:
          severity: critical
        annotations:
          summary: "Interface {{ $labels.ifName }} is down on {{ $labels.device }}"
          description: >
            {{ $labels.ifAlias }} ({{ $labels.ifName }}) on {{ $labels.device }}
            has been operationally down for more than 30 seconds.

      - alert: HighInterfaceErrorRate
        expr: rate(interface_errors{job="snmp"}[5m]) > 1
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "High error rate on {{ $labels.ifName }}"

      - alert: HighDeviceCPU
        expr: device_cpu_percent{job="snmp"} > 70
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "CPU above 70% on {{ $labels.device }}"
```

With the link failure scenario running, `InterfaceDown` fires during each 10-second failure
window (after the 30-second `for:` duration on the first cycle). `HighDeviceCPU` fires when
the CPU spike from rerouting sustains above 70%.

!!! tip "Validate alerts end-to-end"
    For a complete alerting pipeline test with vmalert and Alertmanager, see the
    [Alerting Pipeline](alerting-pipeline.md) guide. The network device scenarios work as
    drop-in replacements for the alert testing examples in that guide.

---

## Extend the model

The two example scenarios cover the most common network monitoring patterns. Here are ideas
for extending them to match your specific environment.

### More interfaces

Duplicate scenario entries with different `ifName` / `ifAlias` labels. For a 48-port switch,
you might only model the uplinks and a handful of access ports -- you don't need all 48 to
validate your dashboards.

### BGP session state

Use the sequence generator to model BGP session flaps:

```yaml
- signal_type: metrics
  name: bgp_session_state
  rate: 1
  duration: 120s
  generator:
    type: sequence
    # 1=Established, 0=Idle (flap at second 15, recovers at 25)
    values: [1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
             0,0,0,0,0,0,0,0,0,0,
             1,1,1,1,1]
    repeat: true
  labels:
    device: rtr-core-01
    bgp_peer: "192.168.1.1"
    bgp_asn: "65001"
    job: snmp
  encoder:
    type: prometheus_text
  sink:
    type: stdout
```

### SNMP counter wraps

Real 32-bit SNMP counters wrap at 2^32 (4,294,967,296). The sawtooth generator's `max`
parameter models this directly:

```yaml
generator:
  type: sawtooth
  min: 0.0
  max: 4294967296.0
  period_secs: 600
```

A 10-minute period with a high-traffic interface wrapping at the 32-bit boundary lets you
test whether your `rate()` queries handle counter resets correctly.

### Temperature and power

Model environmental sensors with sine waves:

```yaml
- signal_type: metrics
  name: device_temperature_celsius
  rate: 1
  duration: 120s
  generator:
    type: sine
    amplitude: 5.0
    period_secs: 3600
    offset: 45.0
  jitter: 0.5
  jitter_seed: 70
  labels:
    device: rtr-core-01
    sensor: intake
    job: snmp
  encoder:
    type: prometheus_text
  sink:
    type: stdout
```

---

## Quick reference

| Task | Command |
|------|---------|
| Validate baseline scenario | `sonda --dry-run run --scenario examples/network-device-baseline.yaml` |
| Run baseline (stdout) | `sonda run --scenario examples/network-device-baseline.yaml` |
| Validate failover scenario | `sonda --dry-run run --scenario scenarios/link-failover.yaml` |
| Run failover simulation | `sonda run --scenario scenarios/link-failover.yaml` |
| Run failover from catalog | `sonda catalog run link-failover` |

**Related pages:**

- [Generators](../configuration/generators.md) -- full reference for sawtooth, sequence, sine, spike, and jitter
- [Scenario Fields](../configuration/scenario-fields.md) -- multi-scenario YAML format and field reference
- [Alert Testing](alert-testing.md) -- threshold and compound alert testing patterns
- [Alerting Pipeline](alerting-pipeline.md) -- end-to-end alerting with vmalert and Alertmanager
- [Example Scenarios](examples.md) -- all example scenario files
