# The Server API

For long-running or programmatic use, Sonda includes an HTTP API that lets you submit,
monitor, and stop scenarios without touching the CLI. The API speaks the same v2 YAML
format you have been using throughout the tour.

## Start the server

=== "Docker (recommended)"

    ```bash
    docker run -p 8080:8080 ghcr.io/davidban77/sonda-server:latest
    ```

=== "From source"

    ```bash
    cargo run -p sonda-server
    ```

The server listens on `:8080` by default. Health-check it:

```bash
curl http://localhost:8080/health
```

## Submit a scenario

POST a v2 YAML body to `/scenarios`:

```bash
curl -X POST \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/simple-constant.yaml \
  http://localhost:8080/scenarios
```

```json
{"id":"a1b2c3d4-...","name":"up","status":"running"}
```

!!! tip "Capture the scenario ID"
    The `POST` response includes an `id` field (a UUID). Use this ID in every
    subsequent request to check status, scrape metrics, or stop the scenario.
    Pipe through `jq` to extract it:

    ```bash
    ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
      --data-binary @examples/simple-constant.yaml \
      http://localhost:8080/scenarios | jq -r '.id')
    ```

## Submit a multi-scenario batch

POST a v2 file with two or more `scenarios:` entries and the server launches them
atomically -- either every entry compiles and starts, or nothing does:

```bash
curl -X POST \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/multi-scenario.yaml \
  http://localhost:8080/scenarios
```

```json
{
  "scenarios": [
    { "id": "a1b2c3d4-...", "name": "cpu_usage", "status": "running" },
    { "id": "e5f6a7b8-...", "name": "app_logs", "status": "running" }
  ]
}
```

See [Multi-scenario body](../deployment/sonda-server.md#multi-scenario-body) for batch
error handling, `phase_offset`, and `after:` chains.

## Monitor a running scenario

```bash title="List all scenarios"
curl http://localhost:8080/scenarios
```

```bash title="Get scenario details"
curl http://localhost:8080/scenarios/$ID
```

```bash title="Get live stats"
curl http://localhost:8080/scenarios/$ID/stats
```

```bash title="Scrape Prometheus-formatted metrics for the run itself"
curl http://localhost:8080/scenarios/$ID/metrics
```

## Stop a scenario

```bash
curl -X DELETE http://localhost:8080/scenarios/$ID
```

## Long-running scenarios

Omit `duration` from `defaults:` (and from every entry) to run until stopped. This is
the operator-owned lifecycle pattern -- useful for soak tests, demo backdrops, or any
scenario you want running until you say otherwise.

```yaml title="examples/long-running-metrics.yaml"
version: 2

defaults:
  rate: 10
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    instance: api-server-01
    job: sonda

scenarios:
  - id: continuous_cpu
    signal_type: metrics
    name: continuous_cpu
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
```

Start and stop:

```bash
# Start
ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/long-running-metrics.yaml \
  http://localhost:8080/scenarios | jq -r '.id')

# Stop later
curl -X DELETE http://localhost:8080/scenarios/$ID
```

For the full API surface -- batch error codes, query parameters, response schemas --
see [Server API](../deployment/sonda-server.md).

## End of tour

You have walked through every concept Sonda exposes: generators, encoders, sinks, log
modes, scheduling, multi-signal runs, and the HTTP API. From here:

- [**E2E Testing**](e2e-testing.md) -- push your synthetic data into a real
  VictoriaMetrics, Loki, Kafka, or OTLP backend and assert it landed.
- [**Alert Testing**](alert-testing.md) -- shape metrics that cross thresholds on cue.
- [**Pipeline Validation**](pipeline-validation.md) -- fast smoke check after a pipeline change.
- [**Capacity Planning**](capacity-planning.md) -- sizing guidance for high-volume runs.
- [**Built-in Scenarios**](scenarios.md) and [**Example Scenarios**](examples.md) --
  ready-to-run patterns to start from.
- [**Troubleshooting**](troubleshooting.md) -- common issues and fixes.
