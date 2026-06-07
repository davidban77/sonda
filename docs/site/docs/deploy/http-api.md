---
title: HTTP API reference
description: REST endpoints exposed by sonda-server — scenarios, events, metrics, and health.
---

# HTTP API reference

`sonda-server` exposes a REST API over HTTP. This page lists every endpoint, its request shape, and the responses you should expect. For how to install, start, and operate the server, see [Deploy as a server](server.md).

## Conventions

### Authentication

When the server starts with `--api-key <key>` (or `SONDA_API_KEY=<key>`), every request to `/scenarios/*`, `/events`, and `/metrics` must include `Authorization: Bearer <key>`. The `/health` endpoint is always public. When no key is configured, all endpoints are public. This preserves backwards compatibility with existing deployments.

```bash title="Authenticated request"
curl -X POST http://localhost:8080/scenarios \
  -H "Authorization: Bearer my-secret-key" \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/basic-metrics.yaml
```

Requests to protected endpoints without a valid key return **401 Unauthorized**:

| Condition | Response body |
|-----------|---------------|
| Missing or malformed header | `{"error": "unauthorized", "detail": "missing or malformed Authorization header"}` |
| Wrong key | `{"error": "unauthorized", "detail": "invalid API key"}` |

For server-side setup (passing the flag, env var, Kubernetes Secret pattern), see [Authentication on Deploy as a server](server.md#authentication).

### Content types

- `POST /scenarios` accepts `text/yaml`, `application/x-yaml`, or `application/json`. JSON bodies are converted to YAML on the server and follow the same validation path.
- `POST /events` accepts `application/json` only.
- All response bodies are JSON unless the endpoint returns Prometheus text exposition (`GET /metrics`, `GET /scenarios/{id}/metrics`).

### Error response shape

All error responses share the format `{"error": "<short_code>", "detail": "<message>"}`. Common short codes:

| Status | Short code | When |
|--------|------------|------|
| 400 | (parser-specific) | Malformed body, missing `version: 2`, validation error |
| 401 | `unauthorized` | Missing or invalid `Authorization: Bearer <key>` |
| 404 | `not_found` | Unknown scenario ID |
| 409 | (conflict-specific) | Duplicate `scenario_name` already running |
| 422 | (validator-specific) | Runtime validation failure |
| 502 | (sink-specific) | Sink push or flush returned an error |
| 500 | (internal-specific) | Unexpected server error |

### Sink URL gotchas

When the server runs in a container, a `sink.url` of `http://localhost:<port>` resolves to the server's own loopback, not your host. POST responses include a `warnings` array when the server detects this misconfiguration. The field is omitted entirely when no warnings apply. See [Networking on Deploy as a server](server.md#networking) for the full address-resolution reference.

## Health and observability

### `GET /health`

Liveness probe. Always public. No `Authorization` header required.

```bash
curl http://localhost:8080/health
# {"status":"ok"}
```

Returns 200 OK with `{"status":"ok"}` when the server process is alive.

### `GET /scenarios/{id}/stats`

Live runtime telemetry for one scenario: rate, events, gap/burst state, sink-failure counters.

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
  "degraded": true,
  "current_state_secs": 12.7,
  "cumulative_resolution_attempts": 0
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
| `state` | string | One of `pending`, `running`, `paused`, `held`, `unresolved`, `finished`. See the [`while:` lifecycle diagram](../build/scenario-files.md#lifecycle-states) and the [cross-POST `unresolved` state](#unresolved-lifecycle-state). |
| `current_state_secs` | float | Seconds since the most recent state transition. See [Cross-POST `while:` refs](#cross-post-while-refs). |
| `cumulative_resolution_attempts` | integer | Lifetime count of cross-POST resolver attempts for this scenario. `0` for local-only scenarios. See [Cross-POST `while:` refs](#cross-post-while-refs). |
| `in_gap` | bool | `true` while a [gap window](../reference/scenario-fields.md#gap-window) is suppressing output. |
| `in_burst` | bool | `true` while a [burst window](../reference/scenario-fields.md#burst-window) is elevating the rate. |
| `consecutive_failures` | integer | Sink errors observed since the most recent successful *delivery*. Resets to `0` on the next delivery. |
| `total_sink_failures` | integer | Lifetime sink-error count. Monotonic. |
| `last_sink_error` | string \| null | Text of the most recent sink error, or `null` if none has been observed. |
| `last_successful_write_at` | integer \| null | Wall-clock time of the most recent successful *delivery*, expressed as Unix nanoseconds. `null` until the first delivery succeeds. |
| `degraded` | bool | `true` when `total_sink_failures > 0` and no successful delivery in the last 30 seconds (or ever). Mirrors the field on [`GET /scenarios`](#get-scenarios). |

#### Self-observability via /stats

External monitors read this endpoint to answer one question. Is the scenario delivering data, or is it stuck? Examples: Kubernetes readiness probes, Prometheus alerts, ops dashboards. `GET /scenarios` returns a precomputed `degraded` flag per scenario for quick checks. `GET /scenarios/{id}/stats` returns the raw counters so you can set your own thresholds.

The four sink-failure fields let external monitors detect a stuck runner without parsing logs. You choose the threshold that counts as "degraded" for your environment.

#### What a stuck batching sink looks like

Five sinks buffer events in memory and deliver them in bursts ("flushes"): `loki`, `http_push`, `remote_write`, `otlp_grpc`, and `kafka`. The other sinks (`stdout`, `file`, `tcp`, `udp`) deliver every event immediately. For the batching group, `total_events` increases on every *buffered* write. The delivery-health fields (`last_successful_write_at`, `consecutive_failures`, `total_sink_failures`) only move when a real flush succeeds or fails. That mismatch is the reason `/stats` exists. It tells you what is actually delivered, not what is queued.

Consider a scenario writing to a Loki backend that has gone unreachable. The scenario runs under the default [`on_sink_error: warn`](../build/scenario-files.md#sink-error-policy) policy. Six writes in:

```text
 write #1   buffer       Ok  →  /stats untouched (only buffered)
 write #2   buffer       Ok  →  /stats untouched
 write #3   buffer       Ok  →  /stats untouched
 write #4   buffer       Ok  →  /stats untouched
 write #5   buffer+FLUSH Err →  total_sink_failures += 1, consecutive_failures += 1
 write #6   buffer       Ok  →  /stats untouched
 ...
```

`total_events` keeps increasing the whole time. Six successful tick results, six increments. But `/stats` reports the delivery reality:

```json title="curl http://localhost:8080/scenarios/$ID/stats"
{
  "total_events": 6,
  "last_successful_write_at": null,
  "consecutive_failures": 1,
  "total_sink_failures": 1,
  "last_sink_error": "connection refused: http://loki:3100/loki/api/v1/push"
}
```

`last_successful_write_at: null` says nothing has *ever* been delivered. `consecutive_failures: 1` reflects the one failed flush in this window. Buffered writes leave this counter alone. Only a failed flush increments it. Only a *successful delivery* resets it to zero. `total_sink_failures: 1` is the same single failure counted as a lifetime total. Until the first successful delivery, the two counters stay locked together. Run the scenario longer and both rise in step. Each rise happens once every `max_buffer_age` window, or whenever the batch fills, not on every tick.

This is the pattern to look for: rising `total_events`, `last_successful_write_at` stuck at `null` or stale, and a non-zero `consecutive_failures`. An operator who sees that pattern knows the backend is unreachable, no matter how high `total_events` rises. Non-batching sinks deliver synchronously on every write. For them the delivery-health fields and the event counter always advance together. This stuck-buffer pattern does not apply.

!!! info "Delivery-accurate, not buffer-accurate, for batching sinks"
    The batching sinks (`loki`, `http_push`, `remote_write`, `otlp_grpc`, `kafka`) buffer events and flush them to the backend in batches. `last_successful_write_at` and `consecutive_failures` track actual *delivery* to the destination, not buffering. `last_successful_write_at` advances only when a write triggers a successful flush. A write that only buffers neither advances it nor resets `consecutive_failures`. A batching sink that is buffering but failing to reach its backend shows a *stale* `last_successful_write_at` and a *non-zero* `consecutive_failures`. That is the signal that nothing is delivered. Non-batching sinks (`stdout`, `file`, `tcp`, `udp`) deliver synchronously on every write, so the two readings always reflect the latest write.

The four sink-failure fields are the runtime telemetry surface for the [`on_sink_error` policy](../build/scenario-files.md#sink-error-policy). When `on_sink_error: warn` (the default) is in effect, the runner stays alive on transient sink errors and these counters tell you what is happening. When `on_sink_error: fail` is set, the scenario exits on the first error and `state` flips to `finished`.

!!! note "`pending -> paused` is a reachable direct transition"
    A scenario carrying both `after:` and `while:` whose `after:` triggers while the gate is closed enters `paused` directly, skipping `running`. Clients building a state-machine assertion should not assume `pending` always precedes `running`. Allow `paused` to follow the `pending` state too.

!!! warning "Upgrading from a release without `pending`?"
    Earlier Sonda releases reported only `running`, `paused`, and `finished` on `/scenarios/{id}/stats`. The `pending` value is new. It applies when a scenario is waiting on `after:` or on the first eligible upstream tick of a `while:` gate. Before rolling out, grep your Prometheus recording rules and Grafana dashboards for label matchers like `state=~"running|paused|finished"`. A matcher that lists every known state drops scenarios in `pending` silently. Either add `pending` to the alternation (`state=~"pending|running|paused|finished"`) or rewrite the matcher as a negation (`state!="finished"`). The negation form surfaces new lifecycle values without another patch.

!!! tip "Detecting a stuck sink"
    To detect a scenario whose sink is stuck, read the `degraded` field on the [`GET /scenarios`](#get-scenarios) list response. It is `true` when a scenario has had sink failures and no successful delivery in the last 30 seconds:

    ```bash
    # List the IDs of every degraded scenario:
    curl -sS http://localhost:8080/scenarios |
      jq -r '.scenarios[] | select(.degraded) | .id'
    ```

    Use that query as a Kubernetes readiness probe, a Prometheus alert query, or a Grafana panel. If you need a different staleness window than the built-in 30 seconds, threshold `total_sink_failures` and the age of `last_successful_write_at` from this endpoint yourself.

## Scenarios

| Method | Path | Description |
|--------|------|-------------|
| POST | `/scenarios` | Start one or more scenarios from a YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect one scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats (see [above](#get-scenariosidstats)) |
| GET | `/scenarios/{id}/metrics` | Per-scenario Prometheus snapshot |

### `POST /scenarios`

Send a [scenario](../build/scenario-files.md) YAML or JSON body. The server validates it and launches it. The endpoint returns the scenario IDs immediately. The scenario runs in the background until its `duration` expires or you call `DELETE /scenarios/{id}`.

!!! tip "Need one event only?"
    `POST /scenarios` is for sustained emission over time. To send a single log or metric synchronously and block until the sink acknowledges, use [`POST /events`](#post-events) instead.

!!! warning "Sink URLs resolve inside the server's network"
    Scenarios sent over HTTP run inside the `sonda-server` process. A sink with `url: http://localhost:<port>` reaches the server container's loopback, not your host. Use the address the server can reach:

    - In Docker Compose, use the service name -- `http://victoriametrics:8428`, `http://loki:3100`, `kafka:9092`.
    - In Kubernetes, use the in-cluster Service DNS -- `http://vmsingle:8428` for same-namespace, or `http://vmsingle.monitoring.svc.cluster.local:8428` for cross-namespace.

    When a scenario targets `localhost`, `127.0.0.1`, or `[::1]`, the server still returns **201 Created**. The address is usually a mistake but sometimes legitimate, so the scenario launches regardless. A `warnings: [...]` field on the response identifies the offending sink and points at [Networking](server.md#networking). The same message is written to the server log as a warning so operators can find it there:

    ```json title="Response (201 with loopback warning)"
    {
      "id": "a1b2c3d4-...",
      "name": "up",
      "state": "running",
      "warnings": [
        "scenario entry 'up' sink `http_push` targets `http://localhost:8428/api/v1/write` — this host resolves to the sonda-server container's own loopback, not your host. Use a Docker Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name instead."
      ]
    }
    ```

    The `warnings` field is omitted entirely when no issues were detected. Existing clients that do not know the field continue to parse the response unchanged.

#### Single-scenario body

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

    The JSON body is converted to YAML on the server and follows the same validation path as the YAML body. Any valid scenario file can be sent as JSON by converting the YAML to its JSON equivalent.

The response shape depends on how many entries the request produces, not on the request format. A single-entry result returns the flat `{"id", "name", "state"}` body. A request that produces two or more entries (for example, a pack-backed entry that expands) returns `{"scenarios": [...]}`. The `state` field reports the live lifecycle state at response time. It takes one of `"pending"`, `"running"`, `"paused"`, `"held"`, `"unresolved"`, or `"finished"`.

#### Multi-scenario body

Send a scenario file with two or more `scenarios:` entries to launch them atomically:

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

Each scenario gets its own ID and runs independently. You manage them individually with `GET /scenarios/{id}`, `DELETE /scenarios/{id}`, and the rest.

**Batch error handling** is atomic. If any entry in the batch fails compilation or validation, the entire request is rejected and nothing is launched:

| Condition | Status | Behavior |
|-----------|--------|----------|
| Body is missing `version: 2` at the top level | **400** | Rejected with a pointer to the scenario file reference |
| Body parses but validation fails (unknown field, unresolved pack, etc.) | **400** | Rejected with the validation error detail |
| Empty `scenarios: []` | **400** | At least one scenario required |
| Any entry fails runtime validation | **422** | Nothing launched, detail identifies the failing entry |
| All entries valid | **201** | All scenarios launched and returned |

!!! tip "Long-running scenarios"
    Omit the `duration` field from your scenario body, or put `duration:` inside a single entry and omit it from `defaults:`, to create a scenario that runs indefinitely. Stop it later with `DELETE /scenarios/{id}`. The reference run-until-stopped example is [`examples/long-running-metrics.yaml`](https://github.com/davidban77/sonda/blob/main/examples/long-running-metrics.yaml). Send it to start, DELETE to stop. The operator controls the lifecycle.

??? tip "Phase offsets and after: chains in batch requests"
    Multi-scenario batches honor `phase_offset`, `clock_group`, and `after:` fields, the same as `sonda run`. This lets you create time-correlated scenarios over the API:

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

    The `memory_usage` scenario starts 3 seconds after `cpu_usage`, simulating a cascading failure for compound alert testing.

#### Pack references over HTTP

Start the server with `--catalog <DIR>` (or the `SONDA_CATALOG` environment variable) and `POST /scenarios` resolves `pack: <name>` references against the `kind: composable` pack YAML files in that directory. Send a body that names a pack (for example `pack: telegraf_snmp_interface`) and the server expands it the same way `sonda run --catalog <dir>` does. You no longer have to inline the pack's metrics into the request body on the client side.

```bash
sonda-server --port 8080 --catalog /scenarios
```

Without `--catalog`, a body that references a pack by name is rejected with `400 Bad Request`. The `detail` field names the unresolved pack. Inlining the pack's metrics directly into the request body still works as an alternative. Bodies that carry no `pack:` reference are unaffected either way.

#### Error response reference

| Status | Condition | Detail field |
|--------|-----------|--------------|
| **400 Bad Request** | Body is not UTF-8, not valid JSON/YAML, missing `version: 2`, or fails validation. | Parser or validation error; v1 bodies include the migration hint. |
| **409 Conflict** | The posted body sets a top-level `scenario_name` that matches an active scenario already in the map. | Identifies the duplicate name and lists the conflicting scenarios. See [Duplicate scenario_name returns 409](#duplicate-scenario_name-returns-409). |
| **422 Unprocessable Entity** | Body is valid YAML but fails runtime checks (`rate: 0`, zero `duration`, etc.), or — with `?validate=strict` — at least one cross-POST `while:` reference does not resolve at submission time. | Validation error identifying the failing entry, or `{error: "unresolved_refs", unresolved_refs: [...]}`. See [Cross-POST `while:` refs](#cross-post-while-refs). |
| **500 Internal Server Error** | Scenario could not be launched, or internal state error. | Short internal error; check server logs. |

#### Duplicate scenario_name returns 409

When a request body sets a top-level `scenario_name`, the server scans the active scenario map for a matching handle. A match is any handle with the same `scenario_name` in `pending`, `running`, `paused`, `held`, or `unresolved` state. If at least one match is found, the POST is rejected with `409 Conflict`. Nothing is launched. The rule is explicit: the operator must `DELETE` the conflicting scenarios first, then re-send the body. There is no `?force=true` override. The explicit DELETE is the only way to free the name.

Anonymous bodies (no top-level `scenario_name`) skip this check entirely. Two consecutive POSTs of the same anonymous body both return 201. Finished handles do not block a new POST. Once every prior cascade with the same name reaches `finished` state, a new cascade with the same name returns 201.

The conflict check is best-effort. The server scans the active scenarios before launching the new one. Two simultaneous POSTs of the same `scenario_name` can both pass the check if they race within the launch window. Both register and their Prometheus streams will collide on duplicate timestamps. Sequential operator use is unaffected. High-concurrency callers should serialize POSTs that share a `scenario_name`.

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

Each `conflicting_scenarios` entry carries three fields:

- `id` — use it with `DELETE /scenarios/{id}`.
- `name` — the runtime-launched scenario name, not the file-level `scenario_name`.
- `state` — one of `pending`, `running`, `paused`, `held`, or `unresolved`.

When the body produces multiple entries through a multi-entry POST or pack expansion, each launched handle inherits the same file-level `scenario_name` and contributes one item to the array.

### Cross-POST `while:` refs

A scenario sent over HTTP can gate itself with `while:` on a signal in a **separate** POST body. Qualify the `ref:` with the upstream's `scenario_name:`. The HTTP surface adds four things on top of the [YAML schema](../build/scenario-files.md#cross-post-while-refs):

- A `?validate=strict` flag on `POST /scenarios`.
- A `pending_ref` field on `GET /scenarios/{id}`.
- An `unresolved` lifecycle state.
- Two new fields on `GET /scenarios/{id}/stats`.

#### Deferred vs strict validation

By default `POST /scenarios` accepts a body whose cross-POST refs have not been registered yet. The scenario enters the `unresolved` state and resolves automatically once a matching upstream is sent. This is the loose-coupling pattern. It lets you launch a baseline body without coordinating with whatever drives it later.

Pass `?validate=strict` to change that behavior. The server rejects the whole body with `422 Unprocessable Entity` if any cross-POST `while:` reference does not resolve when the request arrives. Nothing is launched. Use it when the dependency order is part of your contract and a missing upstream should fail loudly.

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @baseline.yaml \
  'http://localhost:8080/scenarios?validate=strict'
```

```json title="Response (422 Unprocessable Entity) — strict rejection"
{
  "error": "unresolved_refs",
  "unresolved_refs": [
    {
      "scenario_name": "cascade_post",
      "entry_id": "link_state",
      "referenced_by": "requests_total"
    }
  ]
}
```

Each `unresolved_refs` entry identifies the missing upstream by three fields:

- `scenario_name`: the upstream POST body's name.
- `entry_id`: the entry inside that body the `while:` clause references.
- `referenced_by`: the id of the downstream entry whose `while:` clause could not be connected.

#### `unresolved` lifecycle state

A scenario with a cross-POST `while:` clause may have no registered upstream yet. When that scenario's `if_unresolved:` mode is `pending` (the default), it sits in the `unresolved` state. The wire string is `"state": "unresolved"`. The full set of lifecycle states reported on `GET /scenarios/{id}` and `GET /scenarios/{id}/stats` is:

| State | When |
|---|---|
| `pending` | Waiting for `after:` to trigger, or the first eligible tick of a local `while:` gate. |
| `running` | Emitting events. |
| `paused` | Local `while:` gate is closed, or a cross-POST gate's `if_unresolved: closed` is in effect. |
| `held` | Metric scenario configured with [`delay.close.snap_to`](../build/scenario-files.md#recovering-prometheus-alerts-on-gate-close) whose gate has closed after at least one emission. The frozen value is retained for pull-path scrapers that opt in through `?include_state=...,held`. |
| `unresolved` | Cross-POST `while.scenario_name:` has not been received yet (with `if_unresolved: pending`), or its upstream was deleted. |
| `finished` | `duration:` elapsed or shutdown signalled. Terminal. |

A scenario can transition `unresolved → pending → running` once the upstream registers. It returns to `unresolved` when the upstream is deleted or finishes its own duration. Re-sending the same `scenario_name:` re-resolves the downstream automatically. No client orchestration is required. See [Cross-POST `while:` refs](../build/scenario-files.md#cross-post-while-refs) for the YAML schema and the `if_unresolved:` mode reference.

!!! warning "Add `unresolved` and `held` to your dashboards"
    If you maintain Prometheus recording rules or Grafana dashboards that enumerate `state=~"pending|running|paused|finished"`, add `unresolved` and `held` to the alternation (`state=~"pending|running|paused|held|unresolved|finished"`). Or rewrite the matcher as a negation (`state!="finished"`). A matcher that lists every known state drops scenarios in `unresolved` or `held` silently.

#### `pending_ref` field on `GET /scenarios/{id}`

When a scenario is in the `unresolved` state, `GET /scenarios/{id}` includes a `pending_ref` object identifying the upstream it is waiting on. The field is omitted from the response for any other state.

```bash
curl -s http://localhost:8080/scenarios/$ID | jq .
```

```json title="Response (state == unresolved)"
{
  "id": "a1b2c3d4-...",
  "name": "requests_total",
  "state": "unresolved",
  "elapsed_secs": 2.4,
  "degraded": false,
  "stats": { "total_events": 0, "current_rate": 0.0, "bytes_emitted": 0, "...": "..." },
  "pending_ref": {
    "scenario_name": "cascade_post",
    "entry_id": "link_state",
    "if_unresolved": "pending",
    "registered_at": "2026-05-26T14:32:08Z",
    "attempts": 3
  }
}
```

`scenario_name` and `entry_id` are the upstream the downstream is waiting for. `if_unresolved` is the mode that applies until the upstream resolves (`open`, `closed`, or `pending`). `registered_at` is the ISO-8601 wall-clock time the downstream entered the resolver queue. `attempts` counts how many times the resolver has tried to connect this subscription. It increases on every promotion attempt and persists across `unresolved → running → unresolved` cycles. Use it to detect a downstream that has bounced between states.

#### New stats fields

`GET /scenarios/{id}/stats` adds two fields to the existing stats payload:

| Field | Type | Meaning |
|---|---|---|
| `current_state_secs` | float | Seconds since the most recent state transition. Resets to `0.0` every time `state` changes, including the `unresolved → running` resolution edge. Use it to alert on a scenario stuck in one state for too long. For example, `current_state_secs > 60 and state == "unresolved"` flags a cross-POST dependency that has not arrived. |
| `cumulative_resolution_attempts` | integer | Lifetime count of how many times the cross-POST resolver has tried to connect this scenario's subscription. Increments on each promotion attempt. Persists across `unresolved → running → unresolved` cycles. Only `DELETE /scenarios/{id}` resets it. Use it to detect a downstream bouncing between states because its upstream keeps coming and going. |

Both fields are present on every response, not only on `unresolved` scenarios. `cumulative_resolution_attempts` is `0` for any scenario whose `while:` clause does not carry `scenario_name:`. `current_state_secs` measures the time since the last lifecycle edge for every state.

#### Duplicate name across POSTs

The [`409 Conflict` rule](#duplicate-scenario_name-returns-409) extends to cross-POST scenarios. A body whose top-level `scenario_name:` collides with one already registered in the resolver is rejected. This applies whether that earlier scenario is `pending`, `running`, `paused`, `held`, or `unresolved`. The `conflicting_scenarios` array on the 409 response identifies which IDs to DELETE before re-sending. This applies to the upstream (the body that publishes the gate signal) and to any downstream POST whose top-level `scenario_name:` is already in use.

### `GET /scenarios`

Lists all scenarios the server is currently tracking. Returns one entry per scenario with the `degraded` flag included.

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

Each entry carries `id`, `name`, `state`, `elapsed_secs`, and `degraded`. The `state` field takes one of `pending`, `running`, `paused`, `held`, `unresolved`, or `finished`.

#### The `degraded` field

`degraded` is the quick pipeline-health signal. One boolean per scenario tells you whether its sink is delivering. It is `true` when the scenario has had sink failures (`total_sink_failures > 0`) **and** has not had a successful delivery in the last 30 seconds, or has never delivered. A healthy scenario, or one that failed earlier but is delivering again, reads `false`.

```text
curl /scenarios →
{
  "scenarios": [
    { "id": "abc", "name": "loki-prod",   "state": "running", "degraded": false },
    { "id": "xyz", "name": "loki-broken", "state": "running", "degraded": true  }
                                                                            ↑ stuck
  ]
}
```

`degraded = (total_sink_failures > 0) AND (no successful delivery in last 30s, or ever)`.

The benefit is operator ergonomics. One field replaces a multi-step threshold check. Before, you had to read the raw counters from `/stats` and threshold them yourself:

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

The same one-liner works as a Kubernetes readiness probe, a Prometheus alert input, or a Grafana panel query. If you need a different staleness window than the built-in 30 seconds, threshold the raw fields from `GET /scenarios/{id}/stats` yourself. `degraded` is a shortcut over the same underlying counters.

### `GET /scenarios/{id}`

Inspects a single scenario: config, stats, elapsed time, and the `pending_ref` object identifying the upstream it is waiting on when `state == unresolved`. See [Cross-POST `while:` refs](#cross-post-while-refs) for the unresolved-state response shape.

### `DELETE /scenarios/{id}`

Stops the scenario, collects final stats, and removes the scenario from the server. After deletion, the scenario no longer appears in `GET /scenarios` and its memory is freed.

```bash
curl -X DELETE http://localhost:8080/scenarios/<id>
# {"id":"<id>","status":"stopped","total_events":42}
```

| Status | Meaning |
|--------|---------|
| **200 OK** | Scenario stopped and removed. Body includes `id`, `status`, and `total_events`. |
| **404 Not Found** | No scenario with that ID exists (already deleted or never created). |

!!! warning "DELETE is not idempotent"
    A successful DELETE removes the scenario entirely. A second DELETE on the same ID returns **404**, not 200. If your automation retries deletes, treat 404 as success.

### `GET /scenarios/{id}/metrics`

Returns the current value of every series one scenario is emitting, in Prometheus text exposition format. It is the per-scenario counterpart to [`GET /metrics`](#get-metrics). It has the same one-sample-per-series shape and the same idempotent snapshot semantics, but scoped to a single scenario. Use it when each scenario is its own logical target and you have a stable ID to point at.

The response carries the same `# TYPE` and `# HELP` annotations as the aggregate endpoint, scoped to the single scenario:

```text title="GET /scenarios/<SCENARIO_ID>/metrics"
# HELP memory_utilization Memory usage percent on the device.
# TYPE memory_utilization gauge
memory_utilization{device="srl1"} 41.528
```

See [Prometheus exposition fields](../reference/scenario-fields.md#prometheus-exposition-fields) for how `metric_type:` and `help:` control these lines and how defaults are derived.

```yaml title="prometheus.yml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /scenarios/<SCENARIO_ID>/metrics
    static_configs:
      - targets: ["localhost:8080"]
```

Replace `<SCENARIO_ID>` with the ID returned by `POST /scenarios`. Unknown scenario IDs return `404 Not Found`.

## Events

### `POST /events`

`POST /events` emits **one** log or metric event synchronously. The request blocks until the sink acknowledges delivery, then returns the latency it took. Use it when you want a single signal to arrive *now* and you want confirmation before continuing.

The flat JSON body is the simple alternative to the scenario file shape. You set the encoder and sink inline, with no `defaults:`, no `version: 2`, and no scenario IDs to track.

#### When to use `/events` vs `/scenarios`

| Want… | Endpoint |
|------|----------|
| One signal, one moment, blocks until delivered | **`POST /events`** |
| A stream of signals at a sustained rate | [`POST /scenarios`](#post-scenarios) |

Both share the same encoders, sinks, auth, and loopback-warning behavior. They differ only in lifecycle:

- **`/events`** is synchronous. The handler encodes the event, pushes it through the sink, and returns `{sent, signal_type, latency_ms}` once the destination acknowledges (typically 5 to 30 ms). There is no scenario ID. The call is send-and-confirm.
- **`/scenarios`** is asynchronous. The server returns a scenario ID immediately. The scenario runs in the background until its `duration` expires or you call `DELETE /scenarios/{id}`.

!!! tip "Two common uses"
    - **Teaching and demos** — send individual events without writing a full scenario YAML.
    - **Live demos** — a single `curl` produces a Loki log line that Grafana picks up as a panel annotation within 5 to 15 ms.

#### Request body

The body is a JSON object tagged by `signal_type`. The discriminator selects which per-branch field is required (`log` for logs, `metric` for metrics).

=== "Logs"

    ```bash
    curl -X POST http://localhost:8080/events \
      -H "Content-Type: application/json" \
      -d '{
        "signal_type": "logs",
        "labels": {"event": "deploy_start", "env": "prod"},
        "log": {
          "severity": "info",
          "message": "Deploy started",
          "fields": {"version": "1.2.2"}
        },
        "encoder": {"type": "json_lines"},
        "sink": {"type": "loki", "url": "http://loki:3100"}
      }'
    ```

    ```json title="Response (200)"
    {"sent":true,"signal_type":"logs","latency_ms":7}
    ```

=== "Metrics"

    ```bash
    curl -X POST http://localhost:8080/events \
      -H "Content-Type: application/json" \
      -d '{
        "signal_type": "metrics",
        "labels": {"event": "deploy_start", "job": "sonda"},
        "metric": {"name": "deploy_events_total", "value": 1.0},
        "encoder": {"type": "remote_write"},
        "sink": {"type": "remote_write", "url": "http://prom:9090/api/v1/write"}
      }'
    ```

    ```json title="Response (200)"
    {"sent":true,"signal_type":"metrics","latency_ms":12}
    ```

#### Field reference

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `signal_type` | string | yes | `"logs"` or `"metrics"`. Anything else returns 400. |
| `labels` | object\<string,string\> | yes | Forwarded to the sink. Loki uses them as stream labels. May be `{}`. |
| `log` | object | when `signal_type=logs` | See **Log payload** below. |
| `metric` | object | when `signal_type=metrics` | See **Metric payload** below. |
| `encoder` | object | yes | Same shape as `/scenarios`. See [Encoders](../build/encoders.md). |
| `sink` | object | yes | Same shape as `/scenarios`. See [Sinks](../build/sinks.md). |

**Log payload** (`signal_type: "logs"`):

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `severity` | string | yes | `trace` / `debug` / `info` / `warn` / `error` / `fatal` (lowercase). |
| `message` | string | yes | Human-readable log message. |
| `fields` | object\<string,string\> | no | Flat structured fields. Defaults to `{}`. |

**Metric payload** (`signal_type: "metrics"`):

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `name` | string | yes | Metric name. Must match `[a-zA-Z_:][a-zA-Z0-9_:]*`. |
| `value` | number (f64) | yes | Sample value. |

The on-wire metric shape (counter, gauge, histogram lines, TimeSeries protobuf, and so on) is determined by the `encoder` you pick. There is no separate `metric_type` field.

#### Response

##### Success (200)

```json
{
  "sent": true,
  "signal_type": "logs",
  "latency_ms": 7
}
```

`latency_ms` is the wall-clock time the handler spent encoding the event and waiting for the sink to acknowledge.

When pre-flight checks find advisories (for example, a sink URL pointing at a loopback host), the response includes a `warnings` array. The field is **omitted entirely** when no warnings apply, so older clients parse responses unchanged:

```json title="Response with loopback warning"
{
  "sent": true,
  "signal_type": "logs",
  "latency_ms": 7,
  "warnings": [
    "scenario entry 'events.logs' sink `loki` targets `http://localhost:3100` — this host resolves to the sonda-server container's own loopback, not your host. Use a Docker Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name instead."
  ]
}
```

Warnings are informational. They never block delivery. The same message is also written to the server log as a warning.

#### Demo: Grafana annotation from one curl

Send a single log line and observe it appear as a Grafana panel annotation within seconds. This works on the **default `sonda-server` binary**. No feature flags are required.

```bash title="Step 1 — send the event"
curl -s -X POST http://127.0.0.1:8080/events \
  -H "Content-Type: application/json" \
  -d '{
    "signal_type":"logs",
    "labels":{"event":"deploy_start","env":"prod"},
    "log":{"severity":"info","message":"Deploy started","fields":{"version":"1.2.2"}},
    "encoder":{"type":"json_lines"},
    "sink":{"type":"loki","url":"http://loki:3100"}
  }'
# {"sent":true,"signal_type":"logs","latency_ms":7}
```

```bash title="Step 2 — confirm it arrived in Loki"
curl -s 'http://localhost:3100/loki/api/v1/query_range' \
  --data-urlencode 'query={event="deploy_start"}' \
  --data-urlencode 'limit=1'
```

End-to-end latency observed against a real Loki instance: **5 to 15 ms** from `curl` to acknowledge. Configure a Grafana annotation query against `{event="deploy_start"}` and the panel renders the overlay automatically.

##### Errors

All errors share the format `{"error": "<short_code>", "detail": "<message>"}`.

| Status | When | Example detail |
|--------|------|---------------|
| **400 Bad Request** | Malformed JSON; unknown `signal_type`; missing per-branch field; unknown encoder/sink type. | `unknown variant 'traces', expected 'logs' or 'metrics'` |
| **401 Unauthorized** | API key configured and `Authorization: Bearer <key>` missing or wrong. | `missing or malformed Authorization header` |
| **422 Unprocessable Entity** | Encoder/sink config validation failed (invalid metric name, `tcp` retry `max_attempts: 0`, and similar). | `invalid metric name "1bad": must match [a-zA-Z_:][a-zA-Z0-9_:]*` |
| **502 Bad Gateway** | Sink push or flush returned an error (Loki down, network unreachable, and similar). | `sink error: TCP connect to 127.0.0.1:1: Connection refused` |
| **500 Internal Server Error** | Unexpected: encoder error or internal failure while handling the request. | `runtime error: <detail>` |

#### Build-time feature flags

The default `sonda-server` binary supports `loki`, `stdout`, `file`, `tcp`, `udp`, `http_push`, `json_lines`, `prometheus_text`, and `syslog`. A few sinks and encoders are behind cargo feature flags to keep the default binary small:

| Need | Build with |
|------|-----------|
| `remote_write` encoder, `remote_write` sink | `cargo build --release -p sonda-server -F remote-write` |
| `otlp` encoder, `otlp_grpc` sink | `cargo build --release -p sonda-server -F otlp` |
| `kafka` sink | `cargo build --release -p sonda-server -F kafka` |

If a request references a type that is not compiled in, the server returns **422** with a clear hint. For example: `encoder type 'remote_write' requires the 'remote-write' feature: cargo build -F remote-write`.

#### Not in this version

- **Burst path** — there is no `count` or `duration` field. For sustained emission, use [`POST /scenarios`](#post-scenarios).
- **Trace and flow signal types** — only `logs` and `metrics` are supported.
- **CLI subcommand** — there is no `sonda emit` yet. The endpoint is the only entry point.

## Aggregate Prometheus scrape

### `GET /metrics`

To a scraper, `sonda-server` looks like a Prometheus exporter on a real device. One URL, idempotent within a scrape window, with label selectors to slice the view. `GET /metrics` returns the current value of every series across every running scenario. One sample per `(name, labels)` series, with no per-sample timestamp, fused into a single Prometheus text-format response. `?label=k:v` filters that view by the labels you attached when starting each scenario.

It is a **typed** exporter. Each metric is prefixed by `# TYPE` and (when configured) `# HELP` lines. Prometheus, VictoriaMetrics, vmagent, and Telegraf consumers see the same exposition shape they would see scraping any real device. Set [`metric_type:` and `help:`](../reference/scenario-fields.md#prometheus-exposition-fields) on a scenario to declare the type and description. Omit them and the server picks a default: `gauge` for most metric generators, `counter` for [`step`](../build/generators.md#step), and `histogram` or `summary` for those signal types.

Why labels are the durable identity at scrape time: scenarios, multi-scenarios, and metric packs are three ways to *configure* Sonda. Only individual scenarios exist at runtime. Packs and multi-scenarios expand into independent scenarios when the server loads them. The labels you set on each scenario (`device: srl1`, `interface: eth0`, `region: us-east`) are the only cross-scenario grouping that survives. So `?label=k:v` is how you ask "give me one device's metrics" regardless of how the underlying scenarios were launched.

#### Typical use

Send a scenario that declares `metric_type:` and `help:`, then scrape:

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
memory_utilization{device="srl1"} 41.528
memory_utilization{device="srl2"} 67.812
```

One line per `(name, labels)` series, carrying the current value with no per-sample timestamp. This matches the shape `node_exporter` and Prometheus self-scrape produce. The scraper stamps every sample with its own scrape wall-clock at ingest time. The server emitting its own timestamps would conflict with that. The `# TYPE` line appears once per metric name. `# HELP` appears when any contributing scenario set one. With no scenarios running, the response is `200 OK` with an empty body. This is what Prometheus, vmagent, and Telegraf scrapers expect on a quiet target.

!!! info "Idempotent within a scrape window"
    `GET /metrics` is non-destructive and stable. Two scrapes back-to-back return byte-identical bodies. A Prometheus job, a vmagent job, and an ops dashboard can all scrape the same server without taking events from each other. The CLI streaming sinks (`stdout`, `file`, `tcp`, `udp`) still emit a timestamp per event. They encode a stream over time, so the timestamp is what gives each line its identity. The HTTP scrape and the CLI stream serve different consumer models, so the encoder is configured differently for each path.

Histogram and summary scenarios behave the same way. One `# TYPE` block per base name covers every `_bucket{le="..."}`, `_sum`, `_count`, and quantile line:

```text title="Response (histogram entry)"
# HELP http_request_duration_seconds Request latency in seconds.
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{le="0.005",method="GET"} 3
http_request_duration_seconds_bucket{le="0.01",method="GET"} 11
http_request_duration_seconds_bucket{le="+Inf",method="GET"} 100
http_request_duration_seconds_sum{method="GET"} 9.505
http_request_duration_seconds_count{method="GET"} 100
```

This is the exposition shape `histogram_quantile()` expects. PromQL percentile queries work end-to-end against the server.

!!! warning "Mixed-type collisions become `untyped`"
    Two scenarios that share a `name:` but declare different `metric_type` values (one `gauge`, one `counter`) collapse to a single `# TYPE <name> untyped` block in the aggregate response. Prometheus permits only one TYPE per metric name. The server logs a warning identifying both contributors. See [Prometheus exposition fields](../reference/scenario-fields.md#prometheus-exposition-fields) for the full rule.

#### Filter by label

`?label=k:v` narrows the response to scenarios whose configured `labels` contain that exact `k: v` pair. Repeat the parameter to AND-combine selectors. A scenario is included only when every filter matches:

```bash
# One device, all interfaces
curl 'http://localhost:8080/metrics?label=device:srl1'

# One device AND one interface — both must match
curl 'http://localhost:8080/metrics?label=device:srl1&label=interface:eth0'
```

A scenario started with no `labels:` block never matches any filter. There is nothing to match against. Drop the filter to see those events.

A malformed filter (missing `:`, empty key, empty value) returns `400 Bad Request`:

```json title="Response (400 Bad Request)"
{
  "error": "bad_request",
  "detail": "label filter 'invalid' is malformed: expected 'key:value'"
}
```

#### `GET /metrics` vs `GET /scenarios/{id}/metrics`

Both endpoints emit Prometheus text and both are idempotent snapshots. Two back-to-back calls return byte-identical bodies. They differ only in scope:

| Endpoint | Scope | Use it for |
|---|---|---|
| `GET /metrics` | Every running scenario fused into one response. Supports `?label=k:v` to slice the view. | Production scraping. One job covers every scenario, with no need to know IDs in advance. |
| `GET /scenarios/{id}/metrics` | One scenario. | Debugging or setting up a per-scenario route when each scenario is its own logical target. |

Pick `GET /metrics` for any Prometheus, VictoriaMetrics, or vmagent job. It is the endpoint that behaves like a normal exporter. Use the per-scenario endpoint when you are inspecting one scenario in isolation, or when you want a stable URL per scenario.

#### Scrape config

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

Use one job per slice you want to scrape, and add a `label` param to each. Slices include different devices, different regions, or different tenants. With no `params`, the job scrapes everything the server is running.

When [authentication](#authentication) is enabled, add the bearer token to the job:

```yaml title="prometheus.yml (with auth)"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /metrics
    bearer_token: my-secret-key
    static_configs:
      - targets: ["localhost:8080"]
```

## Endpoint summary

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/scenarios` | Start one or more scenarios from YAML/JSON body |
| GET | `/scenarios` | List all running scenarios |
| GET | `/scenarios/{id}` | Inspect a scenario: config, stats, elapsed |
| DELETE | `/scenarios/{id}` | Stop and remove a running scenario |
| GET | `/scenarios/{id}/stats` | Live stats: rate, events, gap/burst state, sink-failure counters |
| GET | `/scenarios/{id}/metrics` | Current per-series values for one scenario in Prometheus text format |
| GET | `/metrics` | Aggregate Prometheus scrape across all running scenarios. Supports `?label=k:v` filtering |
| POST | `/events` | Emit one log or metric event synchronously |

## Where to next

- [Deploy as a server](server.md) — install, configure, network, and operate the server itself.
- [Scenario file format](../build/scenario-files.md) — what to put in the body of `POST /scenarios`.
- [Encoders](../build/encoders.md) and [Sinks](../build/sinks.md) — every encoder/sink option you can declare in a posted body.
- [Cross-POST `while:` refs (YAML schema)](../build/scenario-files.md#cross-post-while-refs) — the file-side counterpart to the HTTP cross-POST surface.
