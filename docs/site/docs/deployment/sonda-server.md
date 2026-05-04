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

See [CLI Reference: sonda-server](../configuration/cli-reference.md#sonda-server) for all `sonda-server` flags.
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

Post a [v2 scenario](../configuration/v2-scenarios.md) YAML or JSON body to
`POST /scenarios`. The server accepts both `text/yaml` (or `application/x-yaml`) and
`application/json` content types.

!!! tip "Need just one event?"
    `POST /scenarios` is for sustained emission over time. To fire a single log or metric synchronously and block until the sink ACKs, use the [Single-Event API (`POST /events`)](events.md) instead.

!!! warning "v2 scenarios only"
    The server only accepts v2 bodies (`version: 2` at the top level). Legacy v1 bodies are
    rejected with `400 Bad Request` and a migration hint. See
    [Migrating v1 bodies](#migrating-v1-bodies) below.

!!! warning "Sink URLs resolve inside the server's network"
    POSTed scenarios compile and run inside the `sonda-server` process. A sink with
    `url: http://localhost:<port>` reaches the server container's loopback, not your
    host. Use the address the server can actually see:

    - In Docker Compose, use the service name -- `http://victoriametrics:8428`,
      `http://loki:3100`, `kafka:9092`.
    - In Kubernetes, use the in-cluster Service DNS --
      `http://vmsingle:8428` for same-namespace, or
      `http://vmsingle.monitoring.svc.cluster.local:8428` for cross-namespace.

    When a POSTed scenario targets `localhost`, `127.0.0.1`, or `[::1]`, the server
    still returns **201 Created** -- the trap is likely a mistake but sometimes
    legitimate, so the scenario launches regardless. A `warnings: [...]` field on
    the response identifies the offending sink and points here. The same message is
    emitted via `tracing::warn!` so operators can catch it in server logs:

    ```json title="Response (201 with loopback warning)"
    {
      "id": "a1b2c3d4-...",
      "name": "up",
      "state": "running",
      "warnings": [
        "scenario entry 'up' sink `http_push` targets `http://localhost:8428/api/v1/write` — this host resolves to the sonda-server container's own loopback, not your host. Use a Docker Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name instead. See docs/deployment/endpoints.md."
      ]
    }
    ```

    The `warnings` field is omitted entirely when no issues were detected, so existing
    clients that do not know about the field continue to parse the response unchanged.

    See [Endpoints & networking](endpoints.md) for the full reference,
    [`${VAR:-default}` interpolation](../configuration/v2-scenarios.md#environment-variable-interpolation)
    so one file works from both paths, and a `sed` one-liner for the rewrite-before-POST fallback.

### Single-scenario body

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @- http://localhost:8080/scenarios <<'EOF'
    version: 2

    defaults:
      rate: 10
      duration: 30s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: up
        signal_type: metrics
        name: up
        generator:
          type: constant
          value: 1.0
    EOF
    ```

    ```json title="Response"
    {"id":"a1b2c3d4-...","name":"up","state":"running"}
    ```

=== "JSON"

    ```bash
    curl -X POST \
      -H "Content-Type: application/json" \
      -d @- http://localhost:8080/scenarios <<'EOF'
    {
      "version": 2,
      "defaults": {
        "rate": 10,
        "duration": "30s",
        "encoder": { "type": "prometheus_text" },
        "sink": { "type": "stdout" }
      },
      "scenarios": [
        {
          "id": "up",
          "signal_type": "metrics",
          "name": "up",
          "generator": { "type": "constant", "value": 1.0 }
        }
      ]
    }
    EOF
    ```

    The JSON body is transcoded to YAML server-side and compiled through the same v2 pipeline
    as the YAML path. Any valid v2 scenario file can be posted as JSON by converting the YAML
    to its JSON equivalent.

The response shape depends on how many entries the compiler produces, not on the request format. A single-entry result returns the flat `{"id", "name", "state"}` body; anything that compiles to two or more entries (for example, a pack-backed entry that fans out) returns `{"scenarios": [...]}`. The `state` field reports the live lifecycle state at response time and takes one of `"pending"`, `"running"`, `"paused"`, or `"finished"` (see [`/scenarios/{id}/stats`](#scenariosidstats) for the full enum and the `pending -> paused` transition note).

### Multi-scenario body

Post a v2 file with two or more `scenarios:` entries to launch them atomically:

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @examples/multi-scenario.yaml \
      http://localhost:8080/scenarios
    ```

    ```yaml title="examples/multi-scenario.yaml"
    version: 2

    defaults:
      rate: 10
      duration: 30s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: cpu_usage
        signal_type: metrics
        name: cpu_usage
        generator:
          type: sine
          amplitude: 50
          period_secs: 60
          offset: 50

      - id: app_logs
        signal_type: logs
        name: app_logs
        encoder:
          type: json_lines
        log_generator:
          type: template
          templates:
            - message: "Request from {ip} to {endpoint}"
              field_pools:
                ip: ["10.0.0.1", "10.0.0.2"]
                endpoint: ["/api/v1/health", "/api/v1/metrics"]
          seed: 42
    ```

=== "JSON"

    ```bash
    curl -X POST \
      -H "Content-Type: application/json" \
      -d @- http://localhost:8080/scenarios <<'EOF'
    {
      "version": 2,
      "defaults": {
        "rate": 10,
        "duration": "30s",
        "encoder": { "type": "prometheus_text" },
        "sink": { "type": "stdout" }
      },
      "scenarios": [
        {
          "id": "cpu_usage",
          "signal_type": "metrics",
          "name": "cpu_usage",
          "generator": { "type": "constant", "value": 42.0 }
        },
        {
          "id": "memory_usage",
          "signal_type": "metrics",
          "name": "memory_usage",
          "generator": { "type": "constant", "value": 75.0 }
        }
      ]
    }
    EOF
    ```

The response wraps each launched scenario in a `scenarios` array:

```json
{
  "scenarios": [
    { "id": "a1b2c3d4-...", "name": "cpu_usage", "state": "running" },
    { "id": "e5f6a7b8-...", "name": "app_logs", "state": "running" }
  ]
}
```

Each scenario gets its own ID and runs on a separate thread. You manage them individually
with `GET /scenarios/{id}`, `DELETE /scenarios/{id}`, etc.

**Batch error handling** is atomic -- if any entry in the batch fails compilation or
validation, the entire request is rejected and nothing is launched:

| Condition | Status | Behavior |
|-----------|--------|----------|
| Body is not v2 (`version: 2` missing) | **400** | Rejected with migration hint |
| Body parses but compile fails (unknown field, unresolved pack, etc.) | **400** | Rejected with compiler error detail |
| Empty `scenarios: []` | **400** | At least one scenario required |
| Any entry fails runtime validation | **422** | Nothing launched, detail identifies the failing entry |
| All entries valid | **201** | All scenarios launched and returned |

!!! tip "Long-running scenarios"
    Omit the `duration` field from your scenario body (or put `duration:` only inside a
    single entry and omit it from `defaults:`) to create a scenario that runs indefinitely.
    Stop it later with `DELETE /scenarios/{id}`. The canonical run-until-stopped example is
    [`examples/long-running-metrics.yaml`](https://github.com/davidban77/sonda/blob/main/examples/long-running-metrics.yaml)
    -- POST it to start, DELETE to stop, operator owns the lifecycle. See the
    [tutorial Server API page](../guides/tutorial-server.md#long-running-scenarios) for
    a full start and stop example.

??? tip "Phase offsets and after: chains in batch requests"
    Multi-scenario batches honor `phase_offset`, `clock_group`, and `after:` fields, just
    like `sonda run`. This lets you create time-correlated scenarios over the API:

    ```yaml
    version: 2

    defaults:
      rate: 1
      duration: 120s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: cpu_usage
        signal_type: metrics
        name: cpu_usage
        phase_offset: "0s"
        clock_group: alert-test
        generator:
          type: sequence
          values: [20, 20, 95, 95, 95, 20]
          repeat: true

      - id: memory_usage
        signal_type: metrics
        name: memory_usage
        phase_offset: "3s"
        clock_group: alert-test
        generator:
          type: sequence
          values: [40, 40, 88, 88, 88, 40]
          repeat: true
    ```

    The `memory_usage` scenario starts 3 seconds after `cpu_usage`, simulating a cascading
    failure for compound alert testing.

### Migrating v1 bodies

When you POST a pre-v2 body, the server responds with `400 Bad Request` and a migration
hint in the detail field:

```json title="Response (400)"
{
  "error": "bad_request",
  "detail": "body is not a v2 scenario. Sonda only accepts v2 scenario bodies (`version: 2` at the top level). Migrate this body to v2 — see docs/configuration/v2-scenarios.md for the migration guide."
}
```

The same hint appears for bodies that do declare `version: 2` but fail to compile (unknown
fields, unresolved pack references, malformed `after:` clauses). In that case the `detail`
carries the compiler's error message instead. See
[Migrating from v1](../configuration/v2-scenarios.md#migrating-from-v1) for side-by-side
shape conversions.

!!! info "Pack references over HTTP"
    The server has no filesystem pack catalog. Bodies that reference a named pack
    (`pack: telegraf_snmp_interface`) compile against an empty in-memory resolver and fail
    with a pack-not-found error. For now, pack-backed scenarios must run via the CLI or be
    expanded into per-metric entries before posting.

### Error response reference

| Status | Condition | Detail field |
|--------|-----------|--------------|
| **400 Bad Request** | Body is not UTF-8, not valid JSON/YAML, missing `version: 2`, or fails compilation. | Parser or compiler error; v1 bodies include the migration hint. |
| **422 Unprocessable Entity** | Body compiles but fails runtime validation (`rate: 0`, zero `duration`, etc.). | Validation error identifying the failing entry. |
| **500 Internal Server Error** | Scenario thread could not be spawned, or internal state error. | Short internal error; check server logs. |

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/scenarios` | Start one or more scenarios from YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect a scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats: rate, events, gap/burst state, sink-failure counters. See [Self-observability via /stats](#self-observability-via-stats). |
| GET | `/scenarios/{id}/metrics` | Latest metrics in Prometheus text format |
| POST | `/events` | Emit one log or metric event synchronously. See [Single-Event API](events.md). |

## Authentication

You can protect scenario endpoints with API key authentication. When enabled, all `/scenarios/*` requests and `POST /events` must include a bearer token. The `/health` endpoint is always public, so health probes and load balancer checks work without credentials.

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
INFO sonda_server: API key authentication enabled for /scenarios/* and /events endpoints
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
| `POST /events` | Yes |

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

## Self-observability via /stats

`GET /scenarios/{id}/stats` returns live runner telemetry. The four sink-failure fields let external monitors spot a wedged runner without parsing logs, and you choose the threshold that counts as "degraded" for your environment.

### `/scenarios` list

```bash
curl -s http://localhost:8080/scenarios | jq .
```

```json title="Response"
{
  "scenarios": [
    {
      "id": "a1b2c3d4-...",
      "name": "noisy_logs",
      "state": "running",
      "elapsed_secs": 184.2
    }
  ]
}
```

Each entry carries `id`, `name`, `state`, and `elapsed_secs`. The `state` field takes one of `pending`, `running`, `paused`, or `finished` (see the [`state` field reference](#scenariosidstats) below for what each value means and the transition note for `pending -> paused`). To see sink health, follow up with `GET /scenarios/{id}/stats` for the scenario you care about.

### `/scenarios/{id}/stats`

```bash
curl -s http://localhost:8080/scenarios/$ID/stats | jq .
```

```json title="Response"
{
  "total_events": 3359,
  "current_rate": 100.4,
  "target_rate": 100.0,
  "bytes_emitted": 1048576,
  "errors": 12,
  "uptime_secs": 184.2,
  "state": "running",
  "in_gap": false,
  "in_burst": false,
  "consecutive_failures": 4,
  "total_sink_failures": 12,
  "last_sink_error": "HTTP 500 from 'http://loki:3100/loki/api/v1/push'",
  "last_successful_write_at": 1714694400000000000
}
```

| Field | Type | Meaning |
|---|---|---|
| `total_events` | integer | Total events emitted since the scenario started. |
| `current_rate` | float | Measured events per second from the runner's rate tracker. |
| `target_rate` | float | The rate configured in the scenario file. |
| `bytes_emitted` | integer | Total bytes written to the sink. |
| `errors` | integer | Encode or sink-write errors observed. |
| `uptime_secs` | float | Seconds since the scenario was launched. |
| `state` | string | One of `pending`, `running`, `paused`, `finished`. See the [`while:` lifecycle diagram](../configuration/v2-scenarios.md#lifecycle-states). |
| `in_gap` | bool | `true` while a [gap window](../configuration/scenario-fields.md#gap-window) is suppressing output. |
| `in_burst` | bool | `true` while a [burst window](../configuration/scenario-fields.md#burst-window) is elevating the rate. |
| `consecutive_failures` | integer | Sink errors observed since the most recent successful write. Resets to `0` on the next successful write. |
| `total_sink_failures` | integer | Lifetime sink-error count. Monotonic. |
| `last_sink_error` | string \| null | Text of the most recent sink error, or `null` if none has been observed. |
| `last_successful_write_at` | integer \| null | Wall-clock time of the most recent successful write, expressed as Unix nanoseconds. `null` until the first write succeeds. |

The four sink-failure fields are the runtime telemetry surface for the [`on_sink_error` policy](../configuration/v2-scenarios.md#sink-error-policy). When `on_sink_error: warn` (the default) is in effect, the runner stays alive on transient sink errors and these counters tell you what's happening; when `on_sink_error: fail` is set, the thread exits on the first error and `state` flips to `finished`.

!!! note "`pending -> paused` is a reachable direct transition"
    A scenario carrying both `after:` and `while:` whose `after:` fires while the gate is closed enters `paused` directly, skipping `running`. Clients building a state-machine assertion should not assume `pending` always precedes `running` -- watch for `paused` from the `pending` state too.

!!! warning "Upgrading from a release without `pending`?"
    Earlier Sonda releases reported only `running`, `paused`, and `finished` on `/scenarios/{id}/stats`. The `pending` value is new and arrives when a scenario is waiting on `after:` or on the first eligible upstream tick of a `while:` gate. Before rolling out, grep your Prometheus recording rules and Grafana dashboards for label matchers like `state=~"running|paused|finished"` -- exhaustive enumerations silently drop scenarios in `pending`. Either add `pending` to the alternation (`state=~"pending|running|paused|finished"`) or rewrite the matcher as a negation (`state!="finished"`) so new lifecycle values surface without another patch.

!!! tip "Detecting a wedged sink"
    Compute "degraded" yourself by thresholding `total_sink_failures` and the staleness of `last_successful_write_at`. Pick a staleness window that fits your scenario's rate and your tolerance for transient blips:

    ```bash
    # Flag a scenario as degraded when sink failures have happened and
    # no write has succeeded in the last 30 seconds:
    curl -sS http://localhost:8080/scenarios/$ID/stats |
      jq 'select(.total_sink_failures > 0 and (.last_successful_write_at == null or (now*1e9 - .last_successful_write_at) > 30e9))'
    ```

    A non-empty result means the scenario is degraded by your definition. Wire the same expression into a Kubernetes readiness probe, a Prometheus alert query, or a Grafana panel — the operator owns the threshold.

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
