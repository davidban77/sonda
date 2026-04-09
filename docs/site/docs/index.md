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
- **Import CSV data** -- analyze Grafana exports or plain CSVs, detect time-series patterns, and
  generate portable scenario YAML using generators instead of raw replay.
- **Scaffold scenarios interactively** -- `sonda init` walks you through building a scenario with
  guided prompts using operational language, no YAML knowledge required.
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

One command, shaped values, labeled output. Send that same metric straight to a backend --
no YAML needed:

```bash
# Push to Prometheus / VictoriaMetrics via remote write
sonda metrics --name cpu_usage --rate 10 --duration 30s \
  --value-mode sine --amplitude 50 --offset 50 --period-secs 60 \
  --label host=web-01 --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Define reusable scenarios in YAML for anything beyond quick one-offs --
[Getting Started](getting-started.md#using-a-scenario-file) shows you how.

## Features at a glance

| Category | Options |
|----------|---------|
| **Generators** | constant, sine, sawtooth, uniform random, sequence, step counter, spike, CSV replay |
| **Encoders** | Prometheus text, InfluxDB line protocol, JSON lines, syslog, Prometheus remote write, OTLP |
| **Sinks** | stdout, file, TCP, UDP, HTTP push, Prometheus remote write, Kafka, Loki, OTLP/gRPC |
| **Scheduling** | configurable rate, duration, gap windows, burst windows, cardinality spikes, jitter |
| **Signals** | metrics, logs (template and replay modes) |
| **CSV import** | Analyze CSVs, detect patterns, generate portable scenario YAML |
| **Interactive scaffolding** | `sonda init` -- guided wizard with operational vocabulary |
| **Built-in scenarios** | 11 curated patterns you can run instantly -- no YAML needed |
| **Deployment** | static binary, Docker, Kubernetes (Helm chart) |

## What next

Ready to dive in? **[Get started in 5 minutes -->](getting-started.md)**

Or jump straight to what you need:

- [**`sonda init`**](configuration/cli-reference.md#sonda-init) -- interactively scaffold a scenario YAML without writing any config by hand
- [**Built-in Scenarios**](guides/scenarios.md) -- run pre-built patterns instantly, customize from there
- [**CSV Import**](guides/csv-import.md) -- turn Grafana exports into portable, parameterized scenarios
- [**Configuration**](configuration/scenario-file.md) -- scenario files, generators, encoders, sinks, CLI reference
- [**Deployment**](deployment/docker.md) -- Docker, Kubernetes, Server API
- [**Guides**](guides/tutorial.md) -- tutorial, alert testing, pipeline validation, example scenarios
