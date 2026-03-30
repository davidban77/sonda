# Server API

`sonda-server` exposes a REST API for starting, inspecting, and stopping scenarios over HTTP.
Use it to integrate Sonda into CI pipelines, test harnesses, or dashboards without shell access.

## Starting the Server

```bash
# Default port (8080)
cargo run -p sonda-server

# Custom port and bind address
cargo run -p sonda-server -- --port 9090 --bind 127.0.0.1
```

See [CLI Reference](../configuration/cli-reference.md) for all `sonda-server` flags.
Control log verbosity with the `RUST_LOG` environment variable (default: `info`):

```bash
RUST_LOG=debug cargo run -p sonda-server -- --port 8080
```

Press Ctrl+C for graceful shutdown -- the server signals all running scenarios to stop before
exiting.

## Health Check

```bash
curl http://localhost:8080/health
# {"status":"ok"}
```

## Start a Scenario

Post a YAML or JSON scenario body to `POST /scenarios`. The server accepts both
`text/yaml` and `application/json` content types. See [Scenario Files](../configuration/scenario-file.md)
for the full YAML schema.

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @examples/basic-metrics.yaml \
      http://localhost:8080/scenarios
    # {"id":"<uuid>","name":"interface_oper_state","status":"running"}
    ```

=== "JSON"

    ```bash
    curl -X POST \
      -H "Content-Type: application/json" \
      -d '{"signal_type":"metrics","name":"up","rate":10,"generator":{"type":"constant","value":1},"encoder":{"type":"prometheus_text"},"sink":{"type":"stdout"}}' \
      http://localhost:8080/scenarios
    ```

Error responses:

- **400 Bad Request** -- body cannot be parsed as YAML or JSON.
- **422 Unprocessable Entity** -- valid YAML/JSON but fails validation (e.g., `rate: 0`).
- **500 Internal Server Error** -- scenario thread could not be spawned.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/scenarios` | Start a scenario from YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect a scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats: rate, events, gap/burst state |
| GET | `/scenarios/{id}/metrics` | Latest metrics in Prometheus text format |

## Scrape Integration

The `GET /scenarios/{id}/metrics` endpoint returns recent metric events in Prometheus text
exposition format. This enables pull-based integration: start a scenario via `POST /scenarios`,
then configure Prometheus or vmagent to scrape the endpoint directly.

```yaml title="prometheus.yml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /scenarios/<SCENARIO_ID>/metrics
    static_configs:
      - targets: ["localhost:8080"]
```

Replace `<SCENARIO_ID>` with the ID returned by `POST /scenarios`.

The endpoint accepts an optional `?limit=N` query parameter (default 100, max 1000)
to control how many recent events are returned per scrape. Each scrape drains the buffer,
so events appear once per cycle. If no metrics are available yet, you get `204 No Content`.
Unknown scenario IDs return `404 Not Found`.

!!! note
    The server is also available as a [Docker image](docker.md) and
    [Helm chart](kubernetes.md) for containerized deployments.
