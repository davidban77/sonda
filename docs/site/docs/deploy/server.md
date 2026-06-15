# Server API

`sonda-server` is the HTTP control plane for Sonda. It is a long-running process. You send scenarios to it over REST, then inspect or stop them. Use it when you want Sonda as a service instead of a one-shot CLI command. Common cases: integrating Sonda into CI pipelines, test harnesses, or dashboards, or keeping a synthetic-telemetry baseline alive for hours or days.

The `sonda-server` binary is installed next to the `sonda` CLI. The [install script](../get-started/quickstart.md#installation) and release tarballs place both on your PATH. The [Docker image](docker.md) runs `sonda-server` as its default entrypoint.

This page covers installing, configuring, and operating the server. For the request and response shapes of every endpoint, see [HTTP API reference](http-api.md).

## Networking

You write a scenario with `url: http://localhost:8428`. It works from your laptop. You send it to a containerized `sonda-server` and nothing arrives at your backend. That is the most common surprise when moving a YAML from one place to another. This section exists to prevent it.

The rule that matters: Sonda resolves sink URLs in the process that **runs** the scenario, not the process that **submits** it. Inside a container, `localhost` means the container's loopback. Outside a container, it means your host. The next section covers every realistic combination: host, Compose, Kubernetes, and external. It also covers the environment variable pattern that lets one YAML work from all of them.

### Two invocation paths

Every sink URL is resolved inside the process that is about to write to it. Sonda has two invocation paths. They resolve `localhost` differently.

=== "Host CLI"

    You run `sonda run file.yaml` on your laptop or a bare host. The scenario runs **in the shell process on your host**. `http://localhost:8428` resolves to your host's loopback. It reaches whatever listens on port 8428 there. The usual target is a Compose-published port or a native install.

    ```bash
    sonda run examples/victoriametrics-metrics.yaml
    ```

=== "`sonda-server` in a container"

    You send a scenario body to `sonda-server` running inside a container. The scenario is validated and runs **inside that container's network namespace**. `http://localhost:8428` resolves to the container's own loopback. Nothing is there, so the request fails.

    ```bash
    curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @file.yaml \
      http://localhost:8080/scenarios
    ```

The host-side `curl` talks to the host's loopback (hitting the published `8080:8080` port). The scenario it carries runs one level deeper, inside the server container.

!!! warning "The `localhost` trap"
    A scenario with `url: http://localhost:8428` works from the host CLI and fails silently when sent to a containerized `sonda-server`. Inside the container, `localhost` is the container, not your host. The POST returns 201, the sink times out, no data arrives.

    Two fixes are available. Write the URL with [`${VAR:-default}`](#one-file-both-paths-var-default) so one file works from both paths. Or hardcode the in-network address: a Compose service name like `http://victoriametrics:8428`, or the Kubernetes Service DNS `http://vmsingle.monitoring.svc.cluster.local:8428`.

### Endpoint resolution reference

Pick the row that matches where `sonda` runs and where your backend lives.

| Process runs here | Backend runs here | Correct `url:` | Why |
|---|---|---|---|
| Host CLI | Backend on host (native install) | `http://localhost:<port>` | Host loopback reaches the native listener. |
| Host CLI | Backend in Compose (port-published) | `http://localhost:<published-port>` | The Compose-published port is exposed on the host. |
| `sonda-server` in Compose | Backend in same Compose network | `http://<service-name>:<port>` | Compose provides a DNS entry per service. Examples: `victoriametrics`, `loki`, `kafka`. |
| `sonda-server` in Compose | Backend on host (Docker Desktop) | `http://host.docker.internal:<port>` | Docker Desktop publishes a virtual DNS name that routes back to the host. |
| `sonda-server` in Kubernetes (same namespace) | Service in same namespace | `http://<svc>:<port>` | Kubernetes DNS resolves short names within a namespace. |
| `sonda-server` in Kubernetes (cross-namespace) | Service in another namespace | `http://<svc>.<ns>.svc.cluster.local:<port>` | The FQDN is required for cross-namespace resolution. |
| `sonda-server` anywhere | External backend (SaaS, cloud) | `https://<public-dns>:<port>` | Fully qualified external DNS plus TLS. |

!!! note
    On Linux without Docker Desktop, `host.docker.internal` does not resolve by default. Add `--add-host=host.docker.internal:host-gateway` to the `sonda-server` container, or use the host's LAN IP.

### One file, both paths: `${VAR:-default}`

The recommended fix is `${VAR:-default}` interpolation inside the YAML. The same file then runs from your host CLI on the defaults and from a containerized `sonda-server` on the overrides. No edit, no `sed`, no second copy.

```yaml title="A sink URL that works from both paths"
sink:
  type: http_push
  url: "${VICTORIAMETRICS_URL:-http://localhost:8428/api/v1/import/prometheus}"
```

The bundled `examples/docker-compose-victoriametrics.yml` sets the in-network overrides on the `sonda-server` container. Every scenario under `examples/` works untouched in both places. See the [full reference](../build/scenario-files.md#environment-variable-interpolation) for the syntax and the seven built-in variable names every example honours.

### Rewriting URLs before posting

If a YAML file hardcodes `http://localhost:<port>` and you prefer not to add interpolation, rewrite the URL before posting it:

```bash title="Swap localhost for Compose service names"
sed 's|http://localhost:8428|http://victoriametrics:8428|g; \
     s|http://localhost:3100|http://loki:3100|g; \
     s|http://localhost:9094|http://kafka:9092|g' \
  examples/http-push-retry.yaml \
  | curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @- \
      http://localhost:8080/scenarios
```

The substitutions cover the Compose backends bundled with Sonda:

| Backend | Host CLI URL (published port) | Compose URL (service name) |
|---|---|---|
| VictoriaMetrics | `http://localhost:8428` | `http://victoriametrics:8428` |
| Loki | `http://localhost:3100` | `http://loki:3100` |
| Kafka | `localhost:9094` (external listener) | `kafka:9092` (internal listener) |

Service names come from `examples/docker-compose-victoriametrics.yml`. Match the `sed` substitutions to your service names if you change the compose file.

!!! tip "Diff before you POST"
    ```bash
    sed 's|http://localhost:8428|http://victoriametrics:8428|g' \
      examples/http-push-retry.yaml | diff examples/http-push-retry.yaml -
    ```

### Networking examples

=== "Host CLI to Compose VictoriaMetrics"

    `sonda` runs on your host. The Compose stack exposes VictoriaMetrics on `localhost:8428`. The scenario's `url:` uses host loopback.

    ```yaml
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

    ```bash
    sonda run examples/vm-push-scenario.yaml
    ```

=== "sonda-server (Compose) to VictoriaMetrics (Compose)"

    Both services run in the same Compose network. The sink URL uses the VictoriaMetrics service name -- **not** `localhost`.

    ```yaml
    sink:
      type: http_push
      url: "http://victoriametrics:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

    ```bash
    curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @examples/victoriametrics-metrics.yaml \
      http://localhost:8080/scenarios
    ```

=== "sonda-server (Kubernetes) to VictoriaMetrics (Kubernetes)"

    Both workloads run in the same namespace. Use the Kubernetes Service short name.

    ```yaml
    sink:
      type: http_push
      url: "http://vmsingle:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

    For a Service in a different namespace, use the fully qualified name:

    ```yaml
    sink:
      type: http_push
      url: "http://vmsingle.monitoring.svc.cluster.local:8428/api/v1/import/prometheus"
    ```

## Starting the server

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

With the server running, you can drive its full lifecycle from `curl` in three steps. Start the server, send a scenario, then confirm it is running:

```bash
# 1. Confirm the server is up
curl http://localhost:8080/health
# {"status":"ok"}

# 2. POST a scenario — the server validates it and starts emitting
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

The scenario runs in the background inside the server until its `duration` expires, or until you stop it with `DELETE /scenarios/{id}`. See [HTTP API reference](http-api.md) for the full endpoint list, request bodies, and response shapes.

## Server flags

`sonda-server` accepts nine flags. The first four are the network and addressing surface; the remaining five control resource limits and runtime sizing. Use the `RUST_LOG` environment variable to control log verbosity (default `info`):

```bash
RUST_LOG=debug sonda-server --port 8080
```

| Flag | Env var | Default | Purpose |
|------|---------|---------|---------|
| `--port <PORT>` | -- | `8080` | Port the server listens on |
| `--bind <ADDR>` | -- | `0.0.0.0` | Bind address |
| `--api-key <KEY>` | `SONDA_API_KEY` | (unset) | Bearer token for `/scenarios/*`, `/events`, and metrics endpoints. See [Authentication](#authentication). |
| `--catalog <DIR>` | `SONDA_CATALOG` | (unset) | Directory of scenario and pack YAML files. Lets `POST /scenarios` resolve `pack: <name>` references. See [Pack references over HTTP](http-api.md#pack-references-over-http). |
| `--workers <N>` | -- | `available_parallelism()` | Tokio worker thread count. See [Tuning resource limits](#tuning-resource-limits). |
| `--max-scenarios <N>` | -- | `0` (unlimited) | Maximum concurrent scenario rows. POSTs beyond the cap return [`429 capacity_exceeded`](http-api.md#capacity-and-resource-errors). |
| `--max-inflight-requests <N>` | -- | `4 * workers` | Maximum concurrent in-flight control-plane HTTP requests. |
| `--request-timeout <SECS>` | -- | `30` | Per-request timeout on control-plane routes. Returns [`408 request_timeout`](http-api.md#capacity-and-resource-errors) on expiry. |
| `--max-body-bytes <N>` | -- | `1048576` (1 MB) | Maximum request body size on control-plane routes. Returns [`413 payload_too_large`](http-api.md#capacity-and-resource-errors) when exceeded. |

When you pass `--catalog`, point it at a directory that holds your `kind: composable` pack YAML files. The path must exist. A missing directory makes the server fail at startup with a clear error.

Press Ctrl+C for graceful shutdown. The server signals all running scenarios to stop before exiting.

### Tuning resource limits

The five resource-limit flags shape what the server accepts, how many scenarios it holds, and how it spends CPU. Their defaults work for a developer laptop. Production deployments should pick values per the guidance below.

#### `--workers <N>`

The size of the tokio multi-thread runtime. Bound: `N >= 1`. The default is `std::thread::available_parallelism()`, which respects cgroup CPU quotas — so the same binary picks 16 workers on a 16-core host and 2 workers on a Kubernetes pod with `cpu: 2000m` set. Override only when you want to deviate from the cgroup setting (for example, pin a workload to one worker to reduce context-switching noise during benchmarking).

The configured value is exported on [`sonda_server_worker_threads`](server-metrics.md#sonda_server_worker_threads) — use it to confirm the runtime sees the value you expect.

#### `--max-scenarios <N>`

A **row cap**, not a CPU-task cap. Every entry in the server's scenario map consumes one slot, including scenarios in `pending`, `running`, `paused`, `held`, `unresolved`, and `finished` state. Reaching the cap returns `429 capacity_exceeded` with a `by_state` breakdown of where the slots went.

Pick `N` from the memory you are willing to dedicate. Each scenario row holds runtime state plus a per-scenario stats buffer; an order-of-magnitude estimate is ~1 MB per scenario at moderate rates. Production deployments commonly use values in the `100` to `1000` range; CI runners often run with `--max-scenarios 50` so a runaway test cannot OOM the runner.

`0` disables the cap entirely. The server emits a `WARN` log line at startup so the choice is visible:

```text
WARN sonda_server: --max-scenarios 0 — scenario row cap disabled (unlimited)
```

The cap is exported on [`sonda_server_max_scenarios`](server-metrics.md#sonda_server_max_scenarios). Live row usage is in [`sonda_server_active_scenarios`](server-metrics.md#sonda_server_active_scenarios) plus [`sonda_server_scenarios_finished_total`](server-metrics.md#sonda_server_scenarios_finished_total).

!!! warning "Finished scenarios still occupy a slot"
    A scenario in `finished` state holds its slot until `DELETE /scenarios/{id}` removes it. Automation that forgets to delete completed scenarios will fill the cap and start serving 429s on every new POST. Either DELETE after a scenario completes, or set `--max-scenarios` high enough to absorb the backlog you expect between cleanups.

#### `--max-inflight-requests <N>`

A global concurrency cap across the control-plane sub-router — `POST /scenarios`, `DELETE /scenarios/{id}`, `POST /events`, and the list/inspect routes. The default `4 * workers` keeps a healthy queue depth without overcommitting. Raise it when the server has CPU and memory headroom and you see request latency climbing under load. Lower it when control-plane work is starving scenario runners.

The cap does **not** apply to the observability sub-router. `/metrics`, `/scenarios/metrics`, `/scenarios/{id}/metrics`, and `/scenarios/{id}/stats` stay scrape-able under saturation — exactly when your alerts need them.

Back-pressure is implicit. `tower::limit::ConcurrencyLimitLayer` does not return a 503; it back-pressures via the tokio runtime, and requests queue at the listener until a slot frees. The visible symptom is rising P99 on [`sonda_server_request_duration_seconds`](server-metrics.md#sonda_server_request_duration_seconds), not an error spike. Alert on the histogram, not on a status code.

#### `--request-timeout <SECS>`

Per-request handler bound on control-plane routes. Requests that exceed the limit return `408 request_timeout`. The default `30` covers the slowest legitimate POST (a large multi-scenario body with cross-POST `while:` resolution). Raise it when you regularly post bodies with many entries; lower it when you want a tighter SLO on the control plane.

This is a handler-execution timeout, not a TCP read timeout — slow clients shipping a body bit-by-bit are not the trigger. The flag does not apply to the observability sub-router.

#### `--max-body-bytes <N>`

The maximum request body size on control-plane routes. Bodies above the limit return `413 payload_too_large`. The default 1 MB fits any realistic scenario YAML and most multi-scenario batches. Raise it when you post very large catalogs by-value (rare — prefer `--catalog <DIR>` for that). Lower it as a hardening measure on internet-facing deployments.

The flag does not apply to the observability sub-router.

## Authentication

You can protect scenario endpoints with API key authentication. When enabled, all `/scenarios/*` requests, `GET /scenarios/metrics` (and its deprecated `/metrics` alias), and `POST /events` must include a bearer token. The `/health` endpoint is always public, so health probes and load balancer checks work without credentials.

### Enabling authentication

Pass an API key with the `--api-key` flag or the `SONDA_API_KEY` environment variable:

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
INFO sonda_server: API key authentication enabled for /scenarios/*, /events, /scenarios/metrics, and the deprecated /metrics alias
```

!!! info "No key = no auth"
    When neither `--api-key` nor `SONDA_API_KEY` is set, the server runs without authentication. All endpoints are public. This preserves backwards compatibility with existing deployments.

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
| `GET /scenarios/metrics` | Yes |
| `GET /metrics` | Yes |
| `POST /events` | Yes |

See [Authentication conventions on HTTP API reference](http-api.md#authentication) for request shapes, header format, and 401 response bodies.

!!! warning "Prometheus scraping with auth"
    If you enable authentication, your Prometheus scrape config must include the bearer token for `/metrics`, `/scenarios/metrics`, and `/scenarios/{id}/metrics`. Add a `bearer_token` or `bearer_token_file` field to your `scrape_configs` entry. See [Authentication on Server metrics](server-metrics.md#authentication) for the scrape-config shape.

??? tip "Kubernetes Secrets"
    In Kubernetes deployments, store the API key in a Secret and reference it as an environment variable in your Deployment spec:

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

    Apply the Secret before deploying:

    ```bash
    kubectl apply -f sonda-secret.yaml
    ```

    See [API key authentication](kubernetes.md#api-key-authentication) in the Kubernetes deployment guide for the full Helm chart setup.

## Where to next

- [HTTP API reference](http-api.md) — every endpoint, request body, and response shape.
- [Server metrics](server-metrics.md) — the nine `/metrics` series and the alerts that matter.
- [Docker](docker.md) — Compose stacks and host-side `docker run` examples.
- [Kubernetes](kubernetes.md) — Helm chart, Service DNS, cross-namespace access.
- [Sinks](../build/sinks.md) — every sink type and its `url:` field.
- [Troubleshooting](../reference/troubleshooting.md) — diagnostics for connection-refused and empty-backend symptoms.
