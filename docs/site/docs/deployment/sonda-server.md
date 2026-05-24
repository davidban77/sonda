# Server API

`sonda-server` is the HTTP control plane for Sonda: a long-running process you POST scenarios to, then inspect or stop them over a REST API. Reach for it when you want Sonda running as a service rather than a one-shot CLI command — integrating into CI pipelines, test harnesses, or dashboards, or keeping a synthetic-telemetry baseline alive for hours or days.

The `sonda-server` binary ships alongside the `sonda` CLI: the [install script](../getting-started.md#installation) and release tarballs place both on your PATH, and the [Docker image](docker.md) runs `sonda-server` as its default entrypoint.

## Starting the Server

Start the server with the installed `sonda-server` binary. It listens on port `8080` by default:

=== "Installed binary"

    ```bash
    # Default port (8080), bind 0.0.0.0
    sonda-server

    # Custom port and bind address
    sonda-server --port 9090 --bind 127.0.0.1
    ```

=== "Docker"

    ```bash
    # The image's default entrypoint is sonda-server
    docker run -p 8080:8080 ghcr.io/davidban77/sonda:latest
    ```

=== "From source"

    ```bash
    # For contributors working from a checkout of the repo
    cargo run -p sonda-server
    ```

## Your first request loop

With the server running, you can drive its full lifecycle from `curl` in three steps — start it, POST a scenario, and confirm it is running:

```bash
# 1. Confirm the server is up
curl http://localhost:8080/health
# {"status":"ok"}

# 2. POST a scenario — the server compiles it and starts emitting
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @- http://localhost:8080/scenarios <<'EOF'
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 60s
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
# {"id":"a1b2c3d4-...","name":"up","state":"running"}

# 3. List running scenarios — your scenario appears with its live state
curl http://localhost:8080/scenarios
```

The scenario runs in a background thread inside the server until its `duration` expires, or until you stop it with `DELETE /scenarios/{id}`. Everything else on this page is the detail underneath that loop: the full flag table, every endpoint, batch submission, authentication, and the stats surface.

!!! tip "Two ways into the server"
    `POST /scenarios` runs a *sustained stream* of events over time — the loop above. To fire a **single** log or metric and block until the sink confirms delivery, use the [Single-Event API (`POST /events`)](events.md) instead. The [tutorial Server API page](../guides/tutorial-server.md) walks the same start-submit-stop loop step by step with more scenario shapes.

### Server flags

`sonda-server` accepts `--port <PORT>` (default `8080`), `--bind <ADDR>` (default `0.0.0.0`),
`--api-key <KEY>` (or `SONDA_API_KEY` env var), and `--catalog <DIR>` (or `SONDA_CATALOG` env
var). Control log verbosity with the `RUST_LOG` environment variable (default `info`):

```bash
RUST_LOG=debug sonda-server --port 8080
```

| Flag | Env var | Default | Purpose |
|------|---------|---------|---------|
| `--port <PORT>` | -- | `8080` | Port the server listens on |
| `--bind <ADDR>` | -- | `0.0.0.0` | Bind address |
| `--api-key <KEY>` | `SONDA_API_KEY` | (unset) | Bearer token for `/scenarios/*`, `/metrics`, and `/events`. See [Authentication](#authentication). |
| `--catalog <DIR>` | `SONDA_CATALOG` | (unset) | Directory of scenario and pack YAML files. Lets `POST /scenarios` resolve `pack: <name>` references. See [Pack references over HTTP](#pack-references-over-http). |

When you pass `--catalog`, point it at a directory that holds your `kind: composable` pack YAML
files. The path must exist -- a missing directory fails the server at startup with a clear error.

Press Ctrl+C for graceful shutdown -- the server signals all running scenarios to stop before
exiting.

## Health Check

```bash
curl http://localhost:8080/health
# {"status":"ok"}
```

## Start a Scenario

Post a [scenario](../configuration/scenario-files.md) YAML or JSON body to `POST /scenarios`. The server accepts both `text/yaml` (or `application/x-yaml`) and `application/json` content types.

!!! tip "Need just one event?"
    `POST /scenarios` is for sustained emission over time. To fire a single log or metric synchronously and block until the sink ACKs, use the [Single-Event API (`POST /events`)](events.md) instead.

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
    [`${VAR:-default}` interpolation](../configuration/scenario-files.md#environment-variable-interpolation)
    so one file works from both paths, and a `sed` one-liner for the rewrite-before-POST fallback.

### Single-scenario body

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @- http://localhost:8080/scenarios <<'EOF'
    version: 2
    kind: runnable

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
      "kind": "runnable",
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

    The JSON body is transcoded to YAML server-side and compiled through the same pipeline as the YAML path. Any valid scenario file can be posted as JSON by converting the YAML to its JSON equivalent.

The response shape depends on how many entries the compiler produces, not on the request format. A single-entry result returns the flat `{"id", "name", "state"}` body; anything that compiles to two or more entries (for example, a pack-backed entry that fans out) returns `{"scenarios": [...]}`. The `state` field reports the live lifecycle state at response time and takes one of `"pending"`, `"running"`, `"paused"`, or `"finished"` (see [`/scenarios/{id}/stats`](#scenariosidstats) for the full enum and the `pending -> paused` transition note).

### Multi-scenario body

Post a scenario file with two or more `scenarios:` entries to launch them atomically:

=== "YAML"

    ```bash
    curl -X POST \
      -H "Content-Type: text/yaml" \
      --data-binary @examples/multi-scenario.yaml \
      http://localhost:8080/scenarios
    ```

    ```yaml title="examples/multi-scenario.yaml"
    version: 2
    kind: runnable

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
      "kind": "runnable",
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
| Body is missing `version: 2` at the top level | **400** | Rejected with a pointer to the scenario file reference |
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
    kind: runnable

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

### Bodies missing `version: 2`

A body that does not declare `version: 2` at the top level is rejected with `400 Bad Request`. The `detail` field carries the parser's message and points at [Scenario Files](../configuration/scenario-files.md) for the file shape.

Bodies that do declare `version: 2` but fail to compile (unknown fields, unresolved pack references, malformed `after:` clauses) are also rejected with `400`; in that case the `detail` carries the compiler's error message instead.

### Pack references over HTTP

Start the server with `--catalog <DIR>` (or the `SONDA_CATALOG` env var) and `POST /scenarios` resolves `pack: <name>` references against the `kind: composable` pack YAML files in that directory. Post a body that names a pack -- `pack: telegraf_snmp_interface` -- and the server expands it the same way `sonda run --catalog <dir>` does. You no longer have to inline the pack's metrics into the posted body client-side.

```bash
sonda-server --port 8080 --catalog /scenarios
```

Without `--catalog`, a body that references a pack by name is rejected with `400 Bad Request`, and the `detail` field names the unresolved pack. Inlining the pack's metrics directly into the posted body still works as an alternative -- bodies that carry no `pack:` reference are unaffected either way.

### Error response reference

| Status | Condition | Detail field |
|--------|-----------|--------------|
| **400 Bad Request** | Body is not UTF-8, not valid JSON/YAML, missing `version: 2`, or fails compilation. | Parser or compiler error; v1 bodies include the migration hint. |
| **409 Conflict** | The posted body sets a top-level `scenario_name` that matches an active scenario already in the map. | Identifies the duplicate name and lists the conflicting scenarios. See [Duplicate scenario_name returns 409](#duplicate-scenario_name-returns-409). |
| **422 Unprocessable Entity** | Body compiles but fails runtime validation (`rate: 0`, zero `duration`, etc.). | Validation error identifying the failing entry. |
| **500 Internal Server Error** | Scenario thread could not be spawned, or internal state error. | Short internal error; check server logs. |

### Duplicate scenario_name returns 409

When a posted body sets a top-level `scenario_name`, the server scans the active scenario map for any handle that already carries the same `scenario_name` and is in `pending`, `running`, or `paused` state. If at least one match is found the POST is rejected with `409 Conflict`; nothing is launched. The contract is explicit: the operator must `DELETE` the conflicting scenarios first, then re-post. There is no `?force=true` override -- the explicit DELETE is the only way to free the name.

Anonymous bodies (no top-level `scenario_name`) bypass this check entirely — two consecutive POSTs of the same anonymous body both return 201. Finished handles are considered stale and never block a new POST — once every prior cascade with the same name reaches `finished` state, a new cascade with the same name returns 201.

The conflict check is best-effort: it acquires a read lock, scans the active scenarios, and releases the lock before launching. Two simultaneous POSTs of the same `scenario_name` can both pass the check if they race within the launch window -- both will register and their Prometheus streams will collide on duplicate timestamps. Workshop-scale and sequential-operator usage are unaffected; high-concurrency callers should serialize POSTs that share a `scenario_name`.

The 409 body lists every active scenario contributing to the conflict so the operator knows which IDs to DELETE:

```json title="Response (409 Conflict)"
{
  "error": "scenario_name 'flap-interface' is already running",
  "conflicting_scenarios": [
    {"id": "a1b2c3d4-...", "name": "link_status", "state": "running"}
  ],
  "hint": "DELETE the conflicting scenarios before posting a new cascade with the same scenario_name"
}
```

Each `conflicting_scenarios` entry carries the scenario `id` (use it with `DELETE /scenarios/{id}`), the per-entry `name` (the runtime-launched scenario name, not the file-level `scenario_name`), and the live `state` (one of `pending`, `running`, `paused`). When the body produced multiple entries (multi-entry POST or pack expansion), each launched handle inherits the same file-level `scenario_name` and contributes one item to the array.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/scenarios` | Start one or more scenarios from YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect a scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats: rate, events, gap/burst state, sink-failure counters. See [Self-observability via /stats](#self-observability-via-stats). |
| GET | `/scenarios/{id}/metrics` | Latest metrics in Prometheus text format. DRAIN semantics — one consumer. |
| GET | `/metrics` | Aggregate Prometheus scrape across all running scenarios. SNAPSHOT semantics — multi-consumer. Supports `?label=k:v` filtering. See [Aggregate Prometheus scrape](#aggregate-prometheus-scrape). |
| POST | `/events` | Emit one log or metric event synchronously. See [Single-Event API](events.md). |

## Authentication

You can protect scenario endpoints with API key authentication. When enabled, all `/scenarios/*` requests, `GET /metrics`, and `POST /events` must include a bearer token. The `/health` endpoint is always public, so health probes and load balancer checks work without credentials.

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
INFO sonda_server: API key authentication enabled for /scenarios/*, /events, and /metrics endpoints
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
| `GET /metrics` | Yes |
| `POST /events` | Yes |

!!! warning "Prometheus scraping with auth"
    If you enable authentication, your Prometheus scrape config must include the bearer token for both `/metrics` and `/scenarios/{id}/metrics`. Add a `bearer_token` or `bearer_token_file` field to your `scrape_configs` entry. See [Aggregate Prometheus scrape](#aggregate-prometheus-scrape) for the scrape-config shape.

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

External monitors — Kubernetes readiness probes, Prometheus alerts, ops dashboards — read these endpoints to answer one question: is the scenario actually delivering data, or is it silently wedged? `GET /scenarios` ships a precomputed `degraded` flag per scenario for at-a-glance checks, and `GET /scenarios/{id}/stats` returns the raw counters underneath so you can set your own thresholds.

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
      "elapsed_secs": 184.2,
      "degraded": false
    }
  ]
}
```

Each entry carries `id`, `name`, `state`, `elapsed_secs`, and `degraded`. The `state` field takes one of `pending`, `running`, `paused`, or `finished` (see the [`state` field reference](#scenariosidstats) below for what each value means and the transition note for `pending -> paused`).

#### The `degraded` field

`degraded` is the at-a-glance pipeline-health signal — one boolean per scenario that tells you whether its sink is delivering. It is `true` when the scenario has had sink failures (`total_sink_failures > 0`) **and** has not had a successful delivery within the last 30 seconds, or has never delivered at all. A healthy scenario, or one that failed earlier but is delivering again, reads `false`.

```text
curl /scenarios →
{
  "scenarios": [
    { "id": "abc", "name": "loki-prod",   "state": "running", "degraded": false },
    { "id": "xyz", "name": "loki-broken", "state": "running", "degraded": true  }
                                                                            ↑ wedged
  ]
}
```

`degraded = (total_sink_failures > 0) AND (no successful delivery in last 30s, or ever)`.

The win is operator ergonomics: one field replaces a multi-step threshold check. Before, you had to pull the raw counters from `/stats` and threshold them yourself:

```bash title="Threshold the raw stats yourself"
curl -sS http://localhost:8080/scenarios/$ID/stats |
  jq 'select(.total_sink_failures > 0)
      | select(.last_successful_write_at == null
               or (now * 1e9 - .last_successful_write_at) > 30e9)'
```

Now the server does that work for you, per scenario, on every list request:

```bash title="Scan the list for degraded scenarios"
curl -sS http://localhost:8080/scenarios |
  jq '.scenarios[] | select(.degraded)'
```

That same one-liner works as a Kubernetes readiness probe, a Prometheus alert input, or a Grafana panel query. If you need a different staleness window than the built-in 30 seconds, threshold the raw fields from `GET /scenarios/{id}/stats` yourself — `degraded` is a convenience over the same underlying counters.

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
  "last_successful_write_at": 1714694400000000000,
  "degraded": true
}
```

#### What a wedged batching sink looks like

Five sinks — `loki`, `http_push`, `remote_write`, `otlp_grpc`, `kafka` — pile events into an in-memory buffer and only deliver them in bursts ("flushes"). The other sinks (`stdout`, `file`, `tcp`, `udp`) deliver every event immediately. For the batching group, `total_events` climbs on every *buffered* write, but the delivery-health fields (`last_successful_write_at`, `consecutive_failures`, `total_sink_failures`) only move when a real flush succeeds or fails. That mismatch is the whole reason `/stats` exists: it tells you what's actually landing, not what's queued.

Picture a scenario writing to a Loki backend that has gone unreachable, running under the default [`on_sink_error: warn`](../configuration/scenario-files.md#sink-error-policy) policy. Six writes in:

```text
 write #1   buffer       Ok  →  /stats untouched (only buffered)
 write #2   buffer       Ok  →  /stats untouched
 write #3   buffer       Ok  →  /stats untouched
 write #4   buffer       Ok  →  /stats untouched
 write #5   buffer+FLUSH Err →  total_sink_failures += 1, consecutive_failures += 1
 write #6   buffer       Ok  →  /stats untouched
 ...
```

`total_events` keeps climbing the whole time — six successful tick results, six increments. But `/stats` tells the honest story:

```json title="curl http://localhost:8080/scenarios/$ID/stats"
{
  "total_events": 6,
  "last_successful_write_at": null,
  "consecutive_failures": 1,
  "total_sink_failures": 1,
  "last_sink_error": "connection refused: http://loki:3100/loki/api/v1/push"
}
```

`last_successful_write_at: null` says nothing has *ever* delivered. `consecutive_failures: 1` reflects the one failed flush in this window — buffered writes leave this counter alone; only a failed flush increments it, and only a *successful delivery* resets it to zero. `total_sink_failures: 1` is the same single failure counted as a lifetime total; until the first successful delivery, the two counters stay locked together. Run the scenario longer and both rise in step — once every `max_buffer_age` window (or whenever the batch fills), not on every tick.

This is the shape to look for: rising `total_events`, `last_successful_write_at` stuck at `null` (or stale), `consecutive_failures` non-zero. An operator who sees that pattern knows the backend is unreachable, no matter how high `total_events` climbs. Non-batching sinks deliver synchronously on every write, so for them the delivery-health fields and the event counter always advance together — the wedged-buffer trap doesn't apply.

| Field | Type | Meaning |
|---|---|---|
| `total_events` | integer | Total events emitted since the scenario started. |
| `current_rate` | float | Measured events per second from the runner's rate tracker. |
| `target_rate` | float | The rate configured in the scenario file. |
| `bytes_emitted` | integer | Total bytes written to the sink. |
| `errors` | integer | Encode or sink-write errors observed. |
| `uptime_secs` | float | Seconds since the scenario was launched. |
| `state` | string | One of `pending`, `running`, `paused`, `finished`. See the [`while:` lifecycle diagram](../configuration/scenario-files.md#lifecycle-states). |
| `in_gap` | bool | `true` while a [gap window](../configuration/scenario-fields.md#gap-window) is suppressing output. |
| `in_burst` | bool | `true` while a [burst window](../configuration/scenario-fields.md#burst-window) is elevating the rate. |
| `consecutive_failures` | integer | Sink errors observed since the most recent successful *delivery*. Resets to `0` on the next delivery. |
| `total_sink_failures` | integer | Lifetime sink-error count. Monotonic. |
| `last_sink_error` | string \| null | Text of the most recent sink error, or `null` if none has been observed. |
| `last_successful_write_at` | integer \| null | Wall-clock time of the most recent successful *delivery*, expressed as Unix nanoseconds. `null` until the first delivery succeeds. |
| `degraded` | bool | `true` when `total_sink_failures > 0` and no successful delivery in the last 30 seconds (or ever). Mirrors the field on [`GET /scenarios`](#scenarios-list). |

!!! info "Delivery-accurate, not buffer-accurate, for batching sinks"
    The batching sinks — `loki`, `http_push`, `remote_write`, `otlp_grpc`, `kafka` — buffer events and flush them to the backend in batches. `last_successful_write_at` and `consecutive_failures` track actual *delivery* to the destination, not buffering: `last_successful_write_at` advances only when a write triggers a successful flush, and a write that merely buffers neither advances it nor resets `consecutive_failures`. So a batching sink that is buffering but failing to reach its backend shows a *stale* `last_successful_write_at` and a *non-zero* `consecutive_failures` — the honest signal that nothing is landing. Non-batching sinks (`stdout`, `file`, `tcp`, `udp`) deliver synchronously on every write, so the two readings always reflect the latest write.

The four sink-failure fields are the runtime telemetry surface for the [`on_sink_error` policy](../configuration/scenario-files.md#sink-error-policy). When `on_sink_error: warn` (the default) is in effect, the runner stays alive on transient sink errors and these counters tell you what's happening; when `on_sink_error: fail` is set, the thread exits on the first error and `state` flips to `finished`.

!!! note "`pending -> paused` is a reachable direct transition"
    A scenario carrying both `after:` and `while:` whose `after:` fires while the gate is closed enters `paused` directly, skipping `running`. Clients building a state-machine assertion should not assume `pending` always precedes `running` -- watch for `paused` from the `pending` state too.

!!! warning "Upgrading from a release without `pending`?"
    Earlier Sonda releases reported only `running`, `paused`, and `finished` on `/scenarios/{id}/stats`. The `pending` value is new and arrives when a scenario is waiting on `after:` or on the first eligible upstream tick of a `while:` gate. Before rolling out, grep your Prometheus recording rules and Grafana dashboards for label matchers like `state=~"running|paused|finished"` -- exhaustive enumerations silently drop scenarios in `pending`. Either add `pending` to the alternation (`state=~"pending|running|paused|finished"`) or rewrite the matcher as a negation (`state!="finished"`) so new lifecycle values surface without another patch.

!!! tip "Detecting a wedged sink"
    To spot a scenario whose sink is wedged, read the `degraded` field on the [`GET /scenarios`](#scenarios-list) list response — it is `true` when a scenario has had sink failures and no successful delivery in the last 30 seconds:

    ```bash
    # List the IDs of every degraded scenario:
    curl -sS http://localhost:8080/scenarios |
      jq -r '.scenarios[] | select(.degraded) | .id'
    ```

    Wire that into a Kubernetes readiness probe, a Prometheus alert query, or a Grafana panel. If you need a different staleness window than the built-in 30 seconds, threshold `total_sink_failures` and the staleness of `last_successful_write_at` from this endpoint yourself.

## Aggregate Prometheus scrape

To a scraper, `sonda-server` presents itself the same way a Prometheus exporter on a real device does — one URL (`GET /metrics`), idempotent within a scrape window, with label selectors to slice the view. `GET /metrics` returns a snapshot of every running scenario's recent metric events fused into a single Prometheus text-format response, and `?label=k:v` filters that view by the labels the user attached when starting each scenario.

It is a **typed** exporter: each metric is fronted by `# TYPE` and (when configured) `# HELP` lines, so Prometheus, VictoriaMetrics, vmagent, and Telegraf consumers see the same exposition shape they would see scraping any real device. Set [`metric_type:` and `help:`](../configuration/scenario-fields.md#prometheus-exposition-fields) on a scenario to declare the type and description explicitly; omit them and the server picks a sensible default (`gauge` for most metric generators, `counter` for [`step`](../configuration/generators.md#step), `histogram` / `summary` for those signal types).

Why labels are the durable identity at scrape time: scenarios, multi-scenarios, and metric packs are three ways to *configure* Sonda, but only individual scenarios exist at runtime — packs and multi-scenarios fan out into independent scenarios at compile time. The labels you set on each scenario (`device: srl1`, `interface: eth0`, `region: us-east`) are the only cross-scenario grouping that survives, so `?label=k:v` is how you ask "give me one device's metrics" regardless of how the underlying scenarios got launched.

### Happy path

Post a scenario that declares `metric_type:` and `help:`, then scrape:

```yaml title="memory-utilization.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: mem_srl1
    signal_type: metrics
    name: memory_utilization
    metric_type: gauge
    help: "Memory usage percent on the device."
    generator:
      type: constant
      value: 41.528
    labels:
      device: srl1
  - id: mem_srl2
    signal_type: metrics
    name: memory_utilization
    generator:
      type: constant
      value: 67.812
    labels:
      device: srl2
```

```bash
curl http://localhost:8080/metrics
```

```text title="Response (text/plain; version=0.0.4; charset=utf-8)"
# HELP memory_utilization Memory usage percent on the device.
# TYPE memory_utilization gauge
memory_utilization{device="srl1"} 41.528 1779645380851
memory_utilization{device="srl2"} 67.812 1779645380851
```

Each sample line is `metric_name{labels} value timestamp_ms`. Per-sample wall-clock millisecond timestamps let Prometheus and VictoriaMetrics dedupe naturally on `(name, labels, timestamp_ms, value)` across overlapping scrape windows. The `# TYPE` line appears once per metric name, and `# HELP` appears when any contributing scenario set one. With no scenarios running, the response is `200 OK` with an empty body — exactly what Prometheus, vmagent, and Telegraf scrapers expect on a quiet target.

Histogram and summary scenarios surface the same way — one `# TYPE` block per base name covering every `_bucket{le="..."}`, `_sum`, `_count`, and quantile line:

```text title="Response (histogram entry)"
# HELP http_request_duration_seconds Request latency in seconds.
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{le="0.005",method="GET"} 3 1779645380851
http_request_duration_seconds_bucket{le="0.01",method="GET"} 11 1779645380851
http_request_duration_seconds_bucket{le="+Inf",method="GET"} 100 1779645380851
http_request_duration_seconds_sum{method="GET"} 9.505 1779645380851
http_request_duration_seconds_count{method="GET"} 100 1779645380851
```

This is the exposition shape `histogram_quantile()` expects, so PromQL percentile queries work end-to-end against the server.

!!! warning "Mixed-type collisions become `untyped`"
    Two scenarios that share a `name:` but declare different `metric_type` values (one `gauge`, one `counter`) collapse to a single `# TYPE <name> untyped` block in the aggregate response — Prometheus permits only one TYPE per metric name. The server logs a warning identifying both contributors. See [Prometheus exposition fields](../configuration/scenario-fields.md#prometheus-exposition-fields) for the full rule.

### Filter by label

`?label=k:v` narrows the response to scenarios whose configured `labels` contain that exact `k: v` pair. Repeat the parameter to AND-combine selectors — a scenario is included only when every filter matches:

```bash
# One device, all interfaces
curl 'http://localhost:8080/metrics?label=device:srl1'

# One device AND one interface — both must match
curl 'http://localhost:8080/metrics?label=device:srl1&label=interface:eth0'
```

A scenario started with no `labels:` block never matches any filter — there is nothing to match against. Drop the filter to see those events.

A malformed filter (missing `:`, empty key, empty value) returns `400 Bad Request`:

```json title="Response (400 Bad Request)"
{
  "error": "bad_request",
  "detail": "label filter 'invalid' is malformed: expected 'key:value'"
}
```

### `GET /metrics` vs `GET /scenarios/{id}/metrics`

Both endpoints emit Prometheus text. They serve different scrape models:

| Endpoint | Semantics | Use it for |
|---|---|---|
| `GET /metrics` | **Snapshot** — non-destructive. Two back-to-back calls return identical bytes. | Production scraping. Multiple scrapers can read it (Prometheus + vmagent + an ops dashboard) without stealing events from each other. |
| `GET /scenarios/{id}/metrics` | **Drain** — each call empties the per-scenario buffer. | Debugging a single scenario. Drives the per-event consumer pattern (one consumer pulls, observes, discards). |

Pick `GET /metrics` for any Prometheus / VictoriaMetrics / vmagent job — it is the endpoint that behaves like a normal exporter. Reach for the per-scenario drain when you are inspecting one scenario in isolation or wiring a one-off pull-based consumer.

### Scrape config

A Prometheus or vmagent scrape job targeting one device's metrics:

```yaml title="prometheus.yml"
scrape_configs:
  - job_name: sonda-srl1
    scrape_interval: 15s
    metrics_path: /metrics
    params:
      label: ["device:srl1"]
    static_configs:
      - targets: ["localhost:8080"]
```

Use one job per slice you want to scrape — different devices, different regions, different tenants — and add a `label` param to each. With no `params`, the job scrapes everything the server is running.

When [API key authentication](#authentication) is enabled, add the bearer token to the job:

```yaml title="prometheus.yml (with auth)"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /metrics
    bearer_token: my-secret-key
    static_configs:
      - targets: ["localhost:8080"]
```

## Per-scenario scrape

The `GET /scenarios/{id}/metrics` endpoint returns recent metric events for one scenario in Prometheus text exposition format. Each scrape **drains** the buffer — events appear once per cycle. Use it when you want a one-to-one consumer pulling from a single scenario; reach for [`GET /metrics`](#aggregate-prometheus-scrape) when more than one scraper needs the data or when you want a single job that covers every running scenario.

The response carries the same `# TYPE` and `# HELP` annotations as the aggregate endpoint, scoped to the single scenario:

```text title="GET /scenarios/<SCENARIO_ID>/metrics"
# HELP memory_utilization Memory usage percent on the device.
# TYPE memory_utilization gauge
memory_utilization{device="srl1"} 41.528 1779645380851
```

See [Prometheus exposition fields](../configuration/scenario-fields.md#prometheus-exposition-fields) for how `metric_type:` and `help:` control these lines and how defaults are derived.

```yaml title="prometheus.yml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /scenarios/<SCENARIO_ID>/metrics
    static_configs:
      - targets: ["localhost:8080"]
```

Replace `<SCENARIO_ID>` with the ID returned by `POST /scenarios`.

The endpoint accepts an optional `?limit=N` query parameter (default 100, max 1000) to control how many recent events are returned per scrape. If no metrics are available yet, you get `204 No Content`. Unknown scenario IDs return `404 Not Found`.

!!! note
    The server is also available as a [Docker image](docker.md) and
    [Helm chart](kubernetes.md) for containerized deployments.
