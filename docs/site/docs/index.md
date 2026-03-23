# Sonda

Sonda is a synthetic telemetry generator that produces realistic metrics and logs for testing
observability pipelines. It models the failure patterns that actually break real systems: gaps,
micro-bursts, cardinality spikes, and shaped value sequences.

## What you can do with Sonda

- **Validate alert rules** -- generate exact metric shapes (sine waves, sequences, CSV replays) to
  trigger thresholds and verify `for:` duration behavior.
- **Smoke-test ingest pipelines** -- push Prometheus, InfluxDB, or JSON-encoded telemetry to any
  backend and confirm it arrives correctly.
- **Simulate failure modes** -- introduce intentional gaps and bursts to test gap-fill logic, alert
  flap detection, and buffer sizing.
- **Test recording rules** -- push known constant values and verify computed outputs.
- **Load-test backends** -- generate thousands of events per second in a static binary with zero
  runtime dependencies.

## Quick install

=== "Cargo"

    ```bash
    cargo install sonda
    ```

=== "Docker"

    ```bash
    docker run --rm ghcr.io/davidban77/sonda:latest \
      /sonda metrics --name up --rate 5 --duration 10s
    ```

=== "From source"

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    cargo build --release -p sonda
    ```

## Your first metric

Generate a sine wave metric and print it to stdout:

```bash
sonda metrics \
  --name cpu_usage \
  --rate 2 \
  --duration 2s \
  --value-mode sine \
  --amplitude 50 \
  --period-secs 4 \
  --offset 50 \
  --label host=web-01
```

```text title="Output (Prometheus exposition format)"
cpu_usage{host="web-01"} 50 1711929600000
cpu_usage{host="web-01"} 85.355 1711929600500
cpu_usage{host="web-01"} 100 1711929601000
cpu_usage{host="web-01"} 85.355 1711929601500
cpu_usage{host="web-01"} 50 1711929602000
```

The sine wave oscillates between 0 (`offset - amplitude`) and 100 (`offset + amplitude`) with a
4-second period. Each line is a valid Prometheus text exposition sample.

## Using a scenario file

Define reusable scenarios in YAML:

```yaml title="scenario.yaml"
name: interface_oper_state
rate: 1000
duration: 30s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
gaps:
  every: 2m
  for: 20s
labels:
  hostname: t0-a1
  zone: eu1
encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
sonda metrics --scenario scenario.yaml
```

## Features at a glance

| Category | Options |
|----------|---------|
| **Generators** | constant, sine, sawtooth, uniform random, sequence, CSV replay |
| **Encoders** | Prometheus text, InfluxDB line protocol, JSON lines, syslog, Prometheus remote write |
| **Sinks** | stdout, file, TCP, UDP, HTTP push, Prometheus remote write, Kafka, Loki |
| **Scheduling** | configurable rate, duration, gap windows, burst windows |
| **Signals** | metrics, logs (template and replay modes) |
| **Deployment** | static binary, Docker, Kubernetes (Helm chart) |
