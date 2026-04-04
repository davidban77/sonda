# Network Device Telemetry

You are building dashboards and alerts for network infrastructure -- routers, switches, firewalls --
and you need realistic test data. SNMP polling intervals, interface counter wraps, link flaps, and
traffic redistribution are hard to simulate in a lab. Sonda lets you model a network device with
multiple interfaces and generate correlated metrics that behave like the real thing.

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
scenarios:
  # Interface traffic counter (sawtooth = monotonic ramp)
  - signal_type: metrics
    name: interface_in_octets
    rate: 1
    duration: 120s
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
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  # Interface operational state (1 = up)
  - signal_type: metrics
    name: interface_oper_state
    rate: 1
    duration: 120s
    generator:
      type: constant
      value: 1.0
    labels:
      device: rtr-core-01
      ifName: GigabitEthernet0/0/0
      ifAlias: uplink-isp-a
      job: snmp
    encoder:
      type: prometheus_text
    sink:
      type: stdout

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

## Simulate a link failure

The interesting part: what happens when a link goes down? Traffic shifts to the backup path,
errors spike on the failing interface, and CPU jumps from the rerouting overhead. Testing your
dashboards and alerts against this pattern before it happens in production is the whole point.

The link failure scenario uses the sequence generator to script a precise timeline:

| Seconds | Event | Gi0/0/0 state | Gi0/0/0 traffic | Gi0/0/1 traffic | CPU |
|---------|-------|---------------|-----------------|-----------------|-----|
| 0--9 | Normal | 1 (up) | Normal | Normal | ~33% |
| 10--19 | Failure | 0 (down) | Drops to 0 | Doubles | ~80% |
| 20--29 | Recovery | 1 (up) | Resumes | Returns to normal | Settling |

This 30-second cycle repeats, giving you multiple failure/recovery events to test against.

```yaml title="examples/network-link-failure.yaml (excerpt)"
scenarios:
  # Gi0/0/0 operational state: up -> down -> up
  - signal_type: metrics
    name: interface_oper_state
    rate: 1
    duration: 120s
    generator:
      type: sequence
      values: [1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
               0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
               1, 1, 1, 1, 1, 1, 1, 1, 1, 1]
      repeat: true
    labels:
      device: rtr-core-01
      ifName: GigabitEthernet0/0/0
      ifAlias: uplink-isp-a
      job: snmp
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  # Gi0/0/1 traffic: absorbs load during failure
  - signal_type: metrics
    name: interface_in_octets
    rate: 1
    duration: 120s
    generator:
      type: sequence
      values: [100000000, 120000000, 140000000, 160000000, 180000000,
               200000000, 180000000, 160000000, 140000000, 120000000,
               300000000, 340000000, 380000000, 420000000, 460000000,
               500000000, 460000000, 420000000, 380000000, 340000000,
               100000000, 120000000, 140000000, 160000000, 180000000,
               200000000, 180000000, 160000000, 140000000, 120000000]
      repeat: true
    labels:
      device: rtr-core-01
      ifName: GigabitEthernet0/0/1
      ifAlias: uplink-isp-b
      job: snmp
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  # ... plus failing interface traffic, errors, CPU (6 scenarios total)
```

Run the failure scenario:

```bash
sonda run --scenario examples/network-link-failure.yaml
```

The full file includes 6 correlated scenarios: interface state for both links, traffic counters
for both (with the failing interface dropping to zero and the backup absorbing load), error
counters that spike during the failure window, and CPU that jumps from rerouting overhead.

!!! info "Correlation through sequence alignment"
    All scenarios in the file use the same 30-value sequence length at `rate: 1`. Because
    `sonda run` starts all threads at the same time, tick 10 in every scenario corresponds
    to the same wall-clock second. This is how you create correlated behavior across metrics
    without needing explicit synchronization.

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

!!! warning "Sink per scenario"
    In a multi-scenario file, each scenario entry has its own `sink` block. If you change
    the sink, update it in every entry. A quick way: use your editor's find-and-replace to
    swap `type: stdout` for your target sink across the file.

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
| Validate failure scenario | `sonda --dry-run run --scenario examples/network-link-failure.yaml` |
| Run failure simulation | `sonda run --scenario examples/network-link-failure.yaml` |

**Related pages:**

- [Generators](../configuration/generators.md) -- full reference for sawtooth, sequence, sine, spike, and jitter
- [Scenario Files](../configuration/scenario-file.md) -- multi-scenario YAML format and field reference
- [Alert Testing](alert-testing.md) -- threshold and compound alert testing patterns
- [Alerting Pipeline](alerting-pipeline.md) -- end-to-end alerting with vmalert and Alertmanager
- [Example Scenarios](examples.md) -- all example scenario files
