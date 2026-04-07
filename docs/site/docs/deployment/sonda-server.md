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
- **500 Internal Server Error** -- scenario thread could not be spawned, or internal state error.

!!! tip "Long-running scenarios"
    Omit the `duration` field from your scenario body to create a scenario that runs
    indefinitely. Stop it later with `DELETE /scenarios/{id}`. See the
    [tutorial](../guides/tutorial.md#long-running-scenarios)
    for a full start and stop example.

### Multi-scenario batch

You can launch multiple scenarios in a single request by wrapping them in a `scenarios` array.
This is the same format used by [`sonda run`](../configuration/scenario-file.md#multi-scenario-files),
so you can POST the exact same YAML files you use locally.

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @examples/multi-scenario.yaml \
      http://localhost:8080/scenarios
    ```

    ```yaml title="examples/multi-scenario.yaml"
    scenarios:
      - signal_type: metrics
        name: cpu_usage
        rate: 100
        duration: 30s
        generator:
          type: sine
          amplitude: 50
          period_secs: 60
          offset: 50
        encoder:
          type: prometheus_text
        sink:
          type: stdout

      - signal_type: logs
        name: app_logs
        rate: 10
        duration: 30s
        generator:
          type: template
          templates:
            - message: "Request from {ip} to {endpoint}"
              field_pools:
                ip: ["10.0.0.1", "10.0.0.2"]
                endpoint: ["/api/v1/health", "/api/v1/metrics"]
          seed: 42
        encoder:
          type: json_lines
        sink:
          type: stdout
    ```

=== "JSON"

    ```bash
    curl -X POST \
      -H "Content-Type: application/json" \
      -d @- http://localhost:8080/scenarios <<'EOF'
    {
      "scenarios": [
        {
          "signal_type": "metrics",
          "name": "cpu_usage",
          "rate": 10,
          "duration": "30s",
          "generator": { "type": "constant", "value": 42.0 },
          "encoder": { "type": "prometheus_text" },
          "sink": { "type": "stdout" }
        },
        {
          "signal_type": "metrics",
          "name": "memory_usage",
          "rate": 10,
          "duration": "30s",
          "generator": { "type": "constant", "value": 75.0 },
          "encoder": { "type": "prometheus_text" },
          "sink": { "type": "stdout" }
        }
      ]
    }
    EOF
    ```

The response wraps each launched scenario in a `scenarios` array:

```json
{
  "scenarios": [
    { "id": "a1b2c3d4-...", "name": "cpu_usage", "status": "running" },
    { "id": "e5f6a7b8-...", "name": "memory_usage", "status": "running" }
  ]
}
```

Each scenario gets its own ID and runs on a separate thread. You manage them
individually with `GET /scenarios/{id}`, `DELETE /scenarios/{id}`, etc.

!!! info "Single vs. multi response shape"
    The response format depends on the request body. A single-scenario body
    returns a flat object (`{"id", "name", "status"}`). A multi-scenario body
    returns `{"scenarios": [...]}`. Existing single-scenario clients are
    unaffected.

**Batch error handling** is atomic -- if any entry in the batch fails validation, the
entire request is rejected and nothing is launched:

| Condition | Status | Behavior |
|-----------|--------|----------|
| Empty `scenarios: []` | **400** | Bad request -- at least one scenario required |
| Any entry fails validation | **422** | Nothing launched, error detail identifies the failing entry |
| All entries valid | **201** | All scenarios launched and returned |

??? tip "Phase offsets in batch requests"
    Multi-scenario batches honor `phase_offset` and `clock_group` fields, just like
    `sonda run`. This lets you create time-correlated scenarios over the API:

    ```yaml
    scenarios:
      - signal_type: metrics
        name: cpu_usage
        phase_offset: "0s"
        clock_group: alert-test
        rate: 1
        duration: 120s
        generator:
          type: sequence
          values: [20, 20, 95, 95, 95, 20]
          repeat: true
        encoder:
          type: prometheus_text
        sink:
          type: stdout

      - signal_type: metrics
        name: memory_usage
        phase_offset: "3s"
        clock_group: alert-test
        rate: 1
        duration: 120s
        generator:
          type: sequence
          values: [40, 40, 88, 88, 88, 40]
          repeat: true
        encoder:
          type: prometheus_text
        sink:
          type: stdout
    ```

    The `memory_usage` scenario starts 3 seconds after `cpu_usage`, simulating a
    cascading failure for compound alert testing.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/scenarios` | Start one or more scenarios from YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect a scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats: rate, events, gap/burst state |
| GET | `/scenarios/{id}/metrics` | Latest metrics in Prometheus text format |

## Authentication

You can protect scenario endpoints with API key authentication. When enabled, all
`/scenarios/*` requests must include a bearer token. The `/health` endpoint is always
public, so health probes and load balancer checks work without credentials.

### Enabling authentication

Pass an API key via the `--api-key` flag or the `SONDA_API_KEY` environment variable:

=== "CLI flag"

    ```bash
    sonda-server --port 8080 --api-key my-secret-key
    ```

=== "Environment variable"

    ```bash
    SONDA_API_KEY=my-secret-key sonda-server --port 8080
    ```

When the server starts with a key configured, you will see:

```text
INFO sonda_server: API key authentication enabled for /scenarios/* endpoints
```

!!! info "No key = no auth"
    When neither `--api-key` nor `SONDA_API_KEY` is set, the server runs without
    authentication and all endpoints are publicly accessible. This preserves full
    backwards compatibility with existing deployments.

### Making authenticated requests

Include the key in the `Authorization: Bearer <key>` header:

```bash
# Start a scenario (requires auth)
curl -X POST \
  -H "Authorization: Bearer my-secret-key" \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/basic-metrics.yaml \
  http://localhost:8080/scenarios

# List scenarios (requires auth)
curl -H "Authorization: Bearer my-secret-key" \
  http://localhost:8080/scenarios

# Health check (always public, no header needed)
curl http://localhost:8080/health
```

### Error responses

Requests to protected endpoints without a valid key return **401 Unauthorized** with a
JSON error body:

| Condition | Response body |
|-----------|---------------|
| Missing or malformed header | `{"error": "unauthorized", "detail": "missing or malformed Authorization header"}` |
| Wrong key | `{"error": "unauthorized", "detail": "invalid API key"}` |

### Protected vs. public endpoints

| Endpoint | Auth required |
|----------|---------------|
| `GET /health` | No -- always public |
| `POST /scenarios` | Yes |
| `GET /scenarios` | Yes |
| `GET /scenarios/{id}` | Yes |
| `DELETE /scenarios/{id}` | Yes |
| `GET /scenarios/{id}/stats` | Yes |
| `GET /scenarios/{id}/metrics` | Yes |

!!! warning "Prometheus scraping with auth"
    If you enable authentication, your Prometheus scrape config must include the bearer
    token for `/scenarios/{id}/metrics`. Add a `bearer_token` or `bearer_token_file`
    field to your `scrape_configs` entry. See [Scrape Integration](#scrape-integration)
    below.

??? tip "Kubernetes Secrets"
    In Kubernetes deployments, store the API key in a Secret and reference it as an
    environment variable in your Deployment spec:

    ```yaml title="sonda-secret.yaml"
    apiVersion: v1
    kind: Secret
    metadata:
      name: sonda-api-key
    type: Opaque
    stringData:
      api-key: my-secret-key
    ```

    ```yaml title="deployment patch"
    env:
      - name: SONDA_API_KEY
        valueFrom:
          secretKeyRef:
            name: sonda-api-key
            key: api-key
    ```

    Apply the secret before deploying:

    ```bash
    kubectl apply -f sonda-secret.yaml
    ```

    See [API key authentication](kubernetes.md#api-key-authentication) in the Kubernetes
    deployment guide for the full Helm chart setup.

## Stopping a Scenario

`DELETE /scenarios/{id}` stops the scenario thread, collects final stats, and removes the
scenario from the server. After deletion, the scenario no longer appears in `GET /scenarios`
and its memory is freed.

```bash
curl -X DELETE http://localhost:8080/scenarios/<id>
# {"id":"<id>","status":"stopped","total_events":42}
```

Response codes:

| Status | Meaning |
|--------|---------|
| **200 OK** | Scenario stopped and removed. Body includes `id`, `status`, and `total_events`. |
| **404 Not Found** | No scenario with that ID exists (already deleted or never created). |

!!! warning "DELETE is not idempotent"
    A successful DELETE removes the scenario entirely. A second DELETE on the same ID
    returns **404**, not 200. If your automation retries deletes, treat 404 as success.

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
