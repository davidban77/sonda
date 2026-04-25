# Scheduling -- gaps and bursts

Real telemetry is messy. Networks drop packets, services restart, traffic spikes hit.
Gaps and bursts let you inject those irregularities into your synthetic data so you
can test how your pipeline and alerts behave under imperfect conditions.

## Gaps

Drop all events for a window within a recurring cycle. Useful for simulating network
partitions or scrape failures:

```bash
sonda metrics --name net_bytes --rate 2 --duration 20s \
  --gap-every 10s --gap-for 3s
```

This emits at 2/s, but goes silent for 3 seconds out of every 10-second cycle.

## Bursts

Temporarily multiply the emission rate. Useful for load spikes or log storms:

```bash
sonda metrics --name req_rate --rate 10 --duration 20s \
  --burst-every 10s --burst-for 2s --burst-multiplier 5
```

This emits at 10/s normally, but spikes to 50/s for 2 seconds every 10-second cycle.

YAML works the same way:

```bash
sonda metrics --scenario examples/burst-metrics.yaml --duration 20s
```

```yaml title="examples/burst-metrics.yaml"
version: 2

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
