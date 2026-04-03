# Sonda

Sonda is a synthetic telemetry generator that produces realistic metrics and logs for testing
observability pipelines. It models the failure patterns that actually break real systems: gaps,
micro-bursts, cardinality spikes, and shaped value sequences.

## What you can do with Sonda

- **Validate alert rules** -- generate exact metric shapes (sine waves, sequences, CSV replays) to
  trigger thresholds and verify `for:` duration behavior.
- **Smoke-test ingest pipelines** -- push Prometheus, InfluxDB, or JSON-encoded telemetry to any
  backend and confirm it arrives correctly.
- **Simulate failure modes** -- introduce intentional gaps, bursts, and cardinality spikes to test
  gap-fill logic, alert flap detection, buffer sizing, and cardinality-limiting rules.
- **Test recording rules** -- push known constant values and verify computed outputs.
- **Load-test backends** -- generate thousands of events per second in a static binary with zero
  runtime dependencies.

## Quick install

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

More installation options (Cargo, Docker, from source) in [Getting Started](getting-started.md#installation).

## A quick taste

```bash
sonda metrics --name cpu_usage --rate 2 --duration 2s \
  --value-mode sine --amplitude 50 --offset 50 --period-secs 4 \
  --label host=web-01
```

```text title="Output (Prometheus exposition format)"
cpu_usage{host="web-01"} 50 1774997347438
cpu_usage{host="web-01"} 85.35533905932738 1774997347943
cpu_usage{host="web-01"} 100 1774997348440
cpu_usage{host="web-01"} 85.35533905932738 1774997348943
```

One command, shaped values, labeled output. Define reusable scenarios in YAML for anything
beyond quick one-offs -- [Getting Started](getting-started.md#using-a-scenario-file) shows you how.

## Features at a glance

| Category | Options |
|----------|---------|
| **Generators** | constant, sine, sawtooth, uniform random, sequence, step counter, spike, CSV replay |
| **Encoders** | Prometheus text, InfluxDB line protocol, JSON lines, syslog, Prometheus remote write |
| **Sinks** | stdout, file, TCP, UDP, HTTP push, Prometheus remote write, Kafka, Loki |
| **Scheduling** | configurable rate, duration, gap windows, burst windows, cardinality spikes |
| **Signals** | metrics, logs (template and replay modes) |
| **Deployment** | static binary, Docker, Kubernetes (Helm chart) |

## What next

Ready to dive in? **[Get started in 5 minutes -->](getting-started.md)**

Or jump straight to what you need:

- [**Configuration**](configuration/scenario-file.md) -- scenario files, generators, encoders, sinks, CLI reference
- [**Deployment**](deployment/docker.md) -- Docker, Kubernetes, Server API
- [**Guides**](guides/tutorial.md) -- tutorial, alert testing, pipeline validation, example scenarios
