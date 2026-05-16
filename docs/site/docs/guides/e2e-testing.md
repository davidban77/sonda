# E2E Testing

You changed an encoder, swapped a sink, or pointed at a new backend. Unit tests pass and
[Pipeline Validation](pipeline-validation.md) shows bytes leaving the wire — but did the
data actually land in the backend you query against? This guide shows the canonical
end-to-end loop: start a real backend, push a known value, query it back.

---

## The pattern

Every e2e check is the same three steps. The encoder, sink, and backend change; the
shape does not.

1. **Start the backend** — `docker compose up -d` against an `examples/docker-compose-*.yml` stack.
2. **Push a known value** — `sonda run examples/<scenario>.yaml` with a unique metric or log name.
3. **Query the backend** — `curl ... | jq ...` and assert the value arrived.

This is the heavier sibling of the [Pipeline Validation](pipeline-validation.md) smoke
check: same loop, but the backend is a real service container instead of `wc -l`.

---

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`).
- `sonda` on `PATH` — see [Installation](../getting-started.md#installation).
- `curl` and [`jq`](https://jqlang.github.io/jq/) for backend queries.

---

## Worked example: metrics into VictoriaMetrics

The fastest path from zero to a verified pipeline. Pushes a constant `99.0` to
VictoriaMetrics for ten seconds, queries the series, and tears down.

```bash title="Start the backend"
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

```bash title="Push a known value"
sonda run examples/e2e-scenario.yaml
```

```yaml title="examples/e2e-scenario.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"

scenarios:
  - signal_type: metrics
    name: e2e_pipeline_check
    generator:
      type: constant
      value: 99.0
    labels:
      test: pipeline
      env: ci
```

```bash title="Verify the data arrived"
sleep 5
curl -s "http://localhost:8428/api/v1/query?query=e2e_pipeline_check" \
  | jq '.data.result | length'
# Expected: 1 (one series with labels env=ci, test=pipeline)
```

```bash title="Tear down"
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

That same shape — start, push, query — works for every signal × encoder × sink combo
below. Swap the scenario file and the verification command.

---

## Coverage matrix

Every row below is a real `examples/*.yaml` you can run today. Start the matching backend
profile from `examples/docker-compose-victoriametrics.yml` first.

| Signal | Encoder | Sink | Scenario | Verify |
|---|---|---|---|---|
| Metrics | `prometheus_text` | `http_push` (VictoriaMetrics) | `examples/e2e-scenario.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=e2e_pipeline_check' \| jq '.data.result \| length'` |
| Metrics | `prometheus_text` | `http_push` (VictoriaMetrics, sine) | `examples/vm-push-scenario.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage' \| jq '.data.result \| length'` |
| Metrics | `remote_write` | `remote_write` (VictoriaMetrics) | `examples/remote-write-vm.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage_rw' \| jq '.data.result \| length'` |
| Metrics | `remote_write` | `remote_write` (vmagent → VM) | `examples/remote-write-vmagent.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage_vmagent' \| jq '.data.result \| length'` |
| Metrics | `remote_write` | `remote_write` (Prometheus) | `examples/remote-write-prometheus.yaml` | `curl -s 'http://localhost:9090/api/v1/query?query=cpu_usage_prom' \| jq '.data.result \| length'` |
| Metrics | `otlp` | `otlp_grpc` (OTel Collector → VM) | `examples/otlp-metrics.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage' \| jq '.data.result \| length'` |
| Logs | `otlp` | `otlp_grpc` (OTel Collector → Loki) | `examples/otlp-logs.yaml` | `curl -sG 'http://localhost:3100/loki/api/v1/query_range' --data-urlencode 'query={service_name="sonda"}' \| jq '.data.result \| length'` |
| Metrics | `prometheus_text` | `kafka` | `examples/kafka-sink.yaml` | `docker exec <kafka> /opt/kafka/bin/kafka-console-consumer.sh --bootstrap-server kafka:9092 --topic sonda-metrics --from-beginning --timeout-ms 5000` |
| Logs | `json_lines` | `loki` | `examples/loki-json-lines.yaml` | `curl -sG 'http://localhost:3100/loki/api/v1/query_range' --data-urlencode 'query={job="sonda"}' \| jq '.data.result \| length'` |
| Logs | `json_lines` | `kafka` | `examples/kafka-json-logs.yaml` | `docker exec <kafka> /opt/kafka/bin/kafka-console-consumer.sh --bootstrap-server kafka:9092 --topic sonda-logs --from-beginning --timeout-ms 5000` |
| Metrics | `influx_lp` | `file` | `examples/influx-file.yaml` | `wc -l < /tmp/sonda-influx-output.txt` |

!!! info "Compose profiles"
    Loki, Kafka, Prometheus, and the OTel Collector are behind profiles to keep the base
    stack lean. Bring up only what each row needs. The vmagent row uses the default stack —
    no extra profile.
    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml \
      --profile loki --profile kafka --profile prometheus --profile otel-collector up -d
    ```
    The OTLP-logs row needs both `--profile otel-collector` and `--profile loki` so the
    collector has somewhere to forward log records.

!!! tip "Feature-gated sinks"
    `remote_write`, `kafka`, and `otlp_grpc` are compile-time features. Pre-built binaries
    and the Docker image include them; if you `cargo build` from source, add
    `--features remote-write,kafka,otlp` (or the subset you need). See
    [Sinks](../configuration/sinks.md) for the full feature flag list.

### Intentionally out of scope

The matrix covers sinks that talk to a queryable backend over HTTP, gRPC, or a broker.
A few sinks intentionally fall outside that pattern:

- **`tcp`, `udp`, `json-tcp`** — raw socket sinks. The fixtures (`examples/tcp-sink.yaml`,
  `examples/udp-sink.yaml`, `examples/json-tcp.yaml`) push to whatever process is listening
  on the configured port; verification is "did `nc -l 5000` print anything?", not a
  backend query. Use them when you're integrating with a custom collector or socket-based
  ingest path.
- **`stdout`** — pipes to the terminal. Already covered by [Pipeline Validation](pipeline-validation.md).

---

## The localhost trap

The matrix above runs `sonda` on your host, so `url: http://localhost:8428` reaches the
Compose-published port. POST the same scenario to a containerized `sonda-server` and the
URL resolves inside the server container — `localhost` is the container, and the push
silently fails.

Two ways to make one scenario file work from both paths:

- Use `${VAR:-default}` in the URL — the bundled examples already do this. See
  [Environment variable interpolation](../configuration/v2-scenarios.md#environment-variable-interpolation).
- Rewrite the URL with `sed` before POSTing — see
  [Endpoints & networking](../deployment/endpoints.md#rewriting-urls-before-posting).

---

## Visual exploration

Want to eyeball the data before bolting it into CI? The same Compose stack ships
Grafana with a pre-provisioned VictoriaMetrics datasource:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
sonda run examples/vm-push-scenario.yaml
open http://localhost:3000
```

For the full alert-flow loop (vmalert + Alertmanager + a webhook receiver), bring up the
alerting profile and walk through [Alerting Pipeline](alerting-pipeline.md):

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile alerting up -d
```

To verify the alert rules themselves cross thresholds correctly, see
[Alert Testing](alert-testing.md).

---

## CI integration

For a worked GitHub Actions workflow that runs this loop on every push, see the
[Pipeline Validation CI section](pipeline-validation.md#ci-integration). The same shape
extends to e2e: add a service container for VictoriaMetrics, run a scenario from
`examples/`, and assert with `curl` + `jq`.

For alert-rule validation in CI specifically — vmalert as a service, `for:` durations,
firing-state assertions — [CI Alert Validation](ci-alert-validation.md) is the worked example.

---

## Next steps

- [Pipeline Validation](pipeline-validation.md) — fast smoke check without a backend.
- [Alert Testing](alert-testing.md) — generate metric shapes that cross thresholds.
- [CI Alert Validation](ci-alert-validation.md) — assert rules fire in GitHub Actions.
- [Endpoints & networking](../deployment/endpoints.md) — pick the right `url:` per process.
- [Example Scenarios](examples.md) — browse every scenario in `examples/`.
