# E2E Testing

The `tests/e2e/` directory contains a Docker Compose-based test suite that validates Sonda
against real observability backends and message brokers.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`)
- [Task](https://taskfile.dev/) (optional, for convenient commands)
- `curl` and `python3` in PATH
- Rust toolchain (for `cargo build`)

## Services

The e2e stack runs these services:

| Service | Port | Purpose |
|---------|------|---------|
| VictoriaMetrics | 8428 | Push target and query endpoint |
| Prometheus | 9090 | Remote write receiver |
| vmagent | 8429 | Relay agent forwarding to VictoriaMetrics |
| Kafka | 9094 | Kafka broker (KRaft mode, no Zookeeper) |
| Kafka UI | 8080 | Browse topics and messages |
| Grafana | 3000 | Pre-configured datasources for VM, Prometheus, and Loki |
| Loki | 3100 | Log aggregation push target |

## Test Scenarios

**VictoriaMetrics** (verified by querying `/api/v1/series`):

| Scenario file | Encoder | Sink target | Metric verified |
|---------------|---------|-------------|-----------------|
| `vm-prometheus-text.yaml` | prometheus_text | VM `/api/v1/import/prometheus` | `sonda_e2e_vm_prom_text` |
| `vm-influx-lp.yaml` | influx_lp | VM `/write` | `sonda_e2e_vm_influx_lp_value` |

**Kafka** (verified by consuming from topic):

| Scenario file | Encoder | Kafka topic | Verification |
|---------------|---------|-------------|--------------|
| `kafka-prometheus-text.yaml` | prometheus_text | `sonda-e2e-metrics` | messages > 0 |
| `kafka-json-lines.yaml` | json_lines | `sonda-e2e-json` | messages > 0 |

## Using the Taskfile

The project Taskfile provides shortcuts for common operations:

```bash
task stack:up       # Start the full e2e stack
task stack:down     # Stop and remove everything
task stack:status   # Show service status
task stack:logs     # Tail all service logs

task e2e            # Run automated e2e tests (starts/stops stack)
task demo           # Start stack + send a 30s sine wave demo

task run:vm-prom    # Send Prometheus text metrics to VictoriaMetrics
task run:vm-influx  # Send InfluxDB LP metrics to VictoriaMetrics
task run:kafka      # Send metrics to Kafka
task run:loki       # Send log events to Loki
```

## Exploring Metrics Visually

Start the stack and generate some data:

```bash
task stack:up
task demo
```

Then open the dashboards:

- **Grafana** -- [http://localhost:3000](http://localhost:3000) (anonymous access). Go to Explore,
  select VictoriaMetrics, and query `demo_sine_wave`.
- **Kafka UI** -- [http://localhost:8080](http://localhost:8080). Browse topics `sonda-e2e-metrics`
  and `sonda-e2e-json`.
- **VictoriaMetrics** -- [http://localhost:8428/vmui](http://localhost:8428/vmui) for the built-in
  query UI.

## Running Automated Tests

```bash
# Via Taskfile
task e2e

# Or directly
./tests/e2e/run.sh
```

The script starts the Docker Compose stack, waits for all services to become healthy, builds
Sonda in release mode, runs each scenario, verifies data arrived (VictoriaMetrics via series
API, Kafka via consumer), and tears everything down. Exit code `0` means all passed.

## Running Scenarios Manually

```bash
# Start the stack
task stack:up

# Run individual scenarios
sonda metrics --scenario tests/e2e/scenarios/vm-prometheus-text.yaml
sonda metrics --scenario tests/e2e/scenarios/kafka-prometheus-text.yaml

# Verify VictoriaMetrics received data
curl "http://localhost:8428/api/v1/series?match[]={__name__=%22sonda_e2e_vm_prom_text%22}"

# Verify Kafka received messages
docker exec sonda-e2e-kafka kafka-console-consumer.sh \
    --bootstrap-server 127.0.0.1:9092 \
    --topic sonda-e2e-metrics \
    --from-beginning --timeout-ms 5000

# Tear down
task stack:down
```

!!! note
    The e2e tests build Sonda in release mode. Make sure you have sufficient disk space
    for the Rust target directory (~2 GB).
