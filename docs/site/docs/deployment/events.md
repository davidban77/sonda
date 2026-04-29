# Single-Event API

`POST /events` emits **one** log or metric event synchronously. The request blocks until the sink ACKs delivery, then returns the latency it took. Use it when you want a single signal to land *now* and you want to know it landed before continuing.

The flat JSON body is the easy alternative to the v2 scenario shape — paste the encoder and sink inline, no `defaults:`, no `version: 2`, no scenario IDs to track.

## When to use `/events` vs `/scenarios`

| Want… | Endpoint |
|------|----------|
| One signal, one moment, blocks until delivered | **`POST /events`** |
| A stream of signals at a sustained rate | [`POST /scenarios`](sonda-server.md#start-a-scenario) |

Both share the same encoders, sinks, auth, and loopback-warning behavior. They differ only in lifecycle:

- **`/events`** is synchronous. The handler encodes the event, pushes it through the sink, and returns `{sent, signal_type, latency_ms}` once the destination ACKs (typically 5–30 ms). There is no scenario ID — the call is fire-and-confirm.
- **`/scenarios`** is asynchronous. The server returns a scenario ID immediately and the scenario runs in the background until its `duration` expires or you call `DELETE /scenarios/{id}`.

!!! tip "Two real-world drivers"
    - **Workshop CLI** — fire teaching events without learning the v2 YAML shape.
    - **Live demos** — a single `curl` on stage produces a Loki log line that Grafana picks up as a panel annotation within ~5–15 ms.

## Request body

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

### Field reference

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `signal_type` | string | yes | `"logs"` or `"metrics"`. Anything else returns 400. |
| `labels` | object\<string,string\> | yes | Forwarded to the sink. Loki uses them as stream labels. May be `{}`. |
| `log` | object | when `signal_type=logs` | See **Log payload** below. |
| `metric` | object | when `signal_type=metrics` | See **Metric payload** below. |
| `encoder` | object | yes | Same shape as `/scenarios`. See [Encoders](../configuration/encoders.md). |
| `sink` | object | yes | Same shape as `/scenarios`. See [Sinks](../configuration/sinks.md). |

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

The on-wire metric shape (counter, gauge, histogram lines, TimeSeries protobuf, …) is determined by the `encoder` you pick — there is no separate `metric_type` field.

## Response

### Success (200)

```json
{
  "sent": true,
  "signal_type": "logs",
  "latency_ms": 7
}
```

`latency_ms` is the wall-clock time the handler spent encoding the event and waiting for the sink to ACK.

When pre-flight checks find advisories (e.g. a sink URL pointing at a loopback host), the response includes a `warnings` array. The field is **omitted entirely** when no warnings fire, so older clients parse responses unchanged:

```json title="Response with loopback warning"
{
  "sent": true,
  "signal_type": "logs",
  "latency_ms": 7,
  "warnings": [
    "scenario entry 'events.logs' sink `loki` targets `http://localhost:3100` — this host resolves to the sonda-server container's own loopback, not your host. Use a Docker Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name instead. See docs/deployment/endpoints.md."
  ]
}
```

Warnings are informational — they never block delivery. The same message is also written to the server log via `tracing::warn!`.

### Errors

All errors share the envelope `{"error": "<short_code>", "detail": "<message>"}`.

| Status | When | Example detail |
|--------|------|---------------|
| **400 Bad Request** | Malformed JSON; unknown `signal_type`; missing per-branch field; unknown encoder/sink type. | `unknown variant 'traces', expected 'logs' or 'metrics'` |
| **401 Unauthorized** | API key configured and `Authorization: Bearer <key>` missing or wrong. | `missing or malformed Authorization header` |
| **422 Unprocessable Entity** | Encoder/sink config validation failed (invalid metric name, `tcp` retry `max_attempts: 0`, etc.). | `invalid metric name "1bad": must match [a-zA-Z_:][a-zA-Z0-9_:]*` |
| **502 Bad Gateway** | Sink push or flush returned an error (Loki down, network unreachable, etc.). | `sink error: TCP connect to 127.0.0.1:1: Connection refused` |
| **500 Internal Server Error** | Unexpected — encoder error or panic in the blocking task. | `runtime error: <detail>` |

## Authentication

`/events` follows the same auth model as `/scenarios`. When the server starts with `--api-key <key>` (or `SONDA_API_KEY=<key>`), every request must include `Authorization: Bearer <key>`. When no key is configured, `/events` is publicly accessible — backwards compatible with existing deployments.

```bash title="Authenticated request"
curl -X POST http://localhost:8080/events \
  -H "Authorization: Bearer my-secret-key" \
  -H "Content-Type: application/json" \
  -d '{
    "signal_type": "logs",
    "labels": {"event": "x"},
    "log": {"severity": "info", "message": "hello"},
    "encoder": {"type": "json_lines"},
    "sink": {"type": "stdout"}
  }'
```

See [Authentication](sonda-server.md#authentication) on the Server API page for the full configuration reference.

## Demo: Grafana annotation from one curl

Fire a single log line and watch it appear as a Grafana panel annotation within seconds. This works on the **default `sonda-server` binary** — no feature flags required.

```bash title="Step 1 — fire the event"
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

```bash title="Step 2 — confirm it landed in Loki"
curl -s 'http://localhost:3100/loki/api/v1/query_range' \
  --data-urlencode 'query={event="deploy_start"}' \
  --data-urlencode 'limit=1'
```

End-to-end latency observed against a real Loki instance: **5–15 ms** from `curl` to ACK. Wire a Grafana annotation query against `{event="deploy_start"}` and the panel renders an overlay automatically.

## Build-time feature flags

The default `sonda-server` binary supports `loki`, `stdout`, `file`, `tcp`, `udp`, `http_push`, `json_lines`, `prometheus_text`, and `syslog`. The Loki annotation demo above works out of the box.

A few sinks and encoders are gated behind cargo features to keep the default binary small:

| Need | Build with |
|------|-----------|
| `remote_write` encoder, `remote_write` sink | `cargo build --release -p sonda-server -F remote-write` |
| `otlp` encoder, `otlp_grpc` sink | `cargo build --release -p sonda-server -F otlp` |
| `kafka` sink | `cargo build --release -p sonda-server -F kafka` |

If a request references a type that isn't compiled in, the server returns **422** with a clear hint, e.g. `encoder type 'remote_write' requires the 'remote-write' feature: cargo build -F remote-write`.

## Sink URL gotchas

When the server runs in a container, a `sink.url` of `http://localhost:<port>` resolves to the **server's own loopback**, not your host. The request still succeeds, but the `warnings` array calls out the misconfiguration. In Docker Compose use the service name (`http://loki:3100`); in Kubernetes use the in-cluster Service DNS. See [Endpoints & networking](endpoints.md) for the full reference.

## Not in this version

- **Burst path** — there is no `count` or `duration` field. For sustained emission, use [`POST /scenarios`](sonda-server.md#start-a-scenario).
- **Trace and flow signal types** — only `logs` and `metrics` are supported.
- **CLI subcommand** — there is no `sonda emit` yet. The endpoint is the only entry point.
