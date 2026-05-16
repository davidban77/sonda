# Scheduling -- gaps and bursts

Real telemetry is messy. Networks drop packets, services restart, traffic spikes hit.
Gaps and bursts let you inject those irregularities into your synthetic data so you
can test how your pipeline and alerts behave under imperfect conditions.

## Gaps

Drop all events for a window within a recurring cycle. Useful for simulating network
partitions or scrape failures:

```yaml title="net-bytes-gaps.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 20s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: net_bytes
    signal_type: metrics
    name: net_bytes
    generator:
      type: constant
      value: 1.0
    gaps:
      every: 10s
      for: 3s
```

```bash
sonda run net-bytes-gaps.yaml
```

This emits at 2/s, but goes silent for 3 seconds out of every 10-second cycle.

## Bursts

Temporarily multiply the emission rate. Useful for load spikes or log storms:

```yaml title="req-rate-bursts.yaml"
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 20s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: req_rate
    signal_type: metrics
    name: req_rate
    generator:
      type: constant
      value: 1.0
    bursts:
      every: 10s
      for: 2s
      multiplier: 5.0
```

```bash
sonda run req-rate-bursts.yaml
```

This emits at 10/s normally, but spikes to 50/s for 2 seconds every 10-second cycle.

A larger scenario with shared defaults and labels:

```bash
sonda run examples/burst-metrics.yaml --duration 20s
```

```yaml title="examples/burst-metrics.yaml"
version: 2
kind: runnable

defaults:
  rate: 100
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    host: web-01
    zone: us-east-1

scenarios:
  - id: cpu_burst
    signal_type: metrics
    name: cpu_burst
    generator:
      type: sine
      amplitude: 20.0
      period_secs: 60
      offset: 50.0
    bursts:
      every: 10s
      for: 2s
      multiplier: 5.0
```

## Real-world patterns

| Pattern | Real-world scenario |
|---------|---------------------|
| Gap 3s every 60s | Scrape target restarts |
| Gap 30s every 5m | Network partition |
| Burst 5x for 2s every 30s | Traffic spike |
| Burst 10x for 1s every 10s | Log storm during deploy |

!!! tip "Combine with generators"
    Gaps and bursts work with any generator. A sine wave with periodic gaps creates
    realistic "flapping service" patterns for alert testing.

## Next

Running one scenario at a time is great for exploration. Production systems emit
multiple signals simultaneously.

[Continue to **Multi-scenario runs** -->](tutorial-multi-scenario.md)
