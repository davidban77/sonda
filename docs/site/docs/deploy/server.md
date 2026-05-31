# Server API

`sonda-server` is the HTTP control plane for Sonda: a long-running process you POST scenarios to, then inspect or stop them over a REST API. Reach for it when you want Sonda running as a service rather than a one-shot CLI command — integrating into CI pipelines, test harnesses, or dashboards, or keeping a synthetic-telemetry baseline alive for hours or days.

The `sonda-server` binary ships alongside the `sonda` CLI: the [install script](../get-started/quickstart.md#installation) and release tarballs place both on your PATH, and the [Docker image](docker.md) runs `sonda-server` as its default entrypoint.

This page covers installing, configuring, and operating the server. For request and response shapes of every endpoint, see [HTTP API reference](http-api.md).

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

The scenario runs in a background thread inside the server until its `duration` expires, or until you stop it with `DELETE /scenarios/{id}`. See [HTTP API reference](http-api.md) for the full endpoint catalogue, request bodies, and response shapes.

## Server flags

`sonda-server` accepts `--port <PORT>` (default `8080`), `--bind <ADDR>` (default `0.0.0.0`), `--api-key <KEY>` (or `SONDA_API_KEY` env var), and `--catalog <DIR>` (or `SONDA_CATALOG` env var). Control log verbosity with the `RUST_LOG` environment variable (default `info`):

```bash
RUST_LOG=debug sonda-server --port 8080
```

| Flag | Env var | Default | Purpose |
|------|---------|---------|---------|
| `--port <PORT>` | -- | `8080` | Port the server listens on |
| `--bind <ADDR>` | -- | `0.0.0.0` | Bind address |
| `--api-key <KEY>` | `SONDA_API_KEY` | (unset) | Bearer token for `/scenarios/*`, `/metrics`, and `/events`. See [Authentication](#authentication). |
| `--catalog <DIR>` | `SONDA_CATALOG` | (unset) | Directory of scenario and pack YAML files. Lets `POST /scenarios` resolve `pack: <name>` references. See [Pack references over HTTP](http-api.md#pack-references-over-http). |

When you pass `--catalog`, point it at a directory that holds your `kind: composable` pack YAML files. The path must exist — a missing directory fails the server at startup with a clear error.

Press Ctrl+C for graceful shutdown — the server signals all running scenarios to stop before exiting.

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
    When neither `--api-key` nor `SONDA_API_KEY` is set, the server runs without authentication and all endpoints are publicly accessible. This preserves full backwards compatibility with existing deployments.

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

See [Authentication conventions on HTTP API reference](http-api.md#authentication) for request shapes, header format, and 401 response bodies.

!!! warning "Prometheus scraping with auth"
    If you enable authentication, your Prometheus scrape config must include the bearer token for both `/metrics` and `/scenarios/{id}/metrics`. Add a `bearer_token` or `bearer_token_file` field to your `scrape_configs` entry. See [Aggregate Prometheus scrape](http-api.md#aggregate-prometheus-scrape) for the scrape-config shape.

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

    Apply the secret before deploying:

    ```bash
    kubectl apply -f sonda-secret.yaml
    ```

    See [API key authentication](kubernetes.md#api-key-authentication) in the Kubernetes deployment guide for the full Helm chart setup.

## Networking

You wrote a scenario with `url: http://localhost:8428`, it worked from your laptop, you POSTed it to a containerized `sonda-server`, and now nothing arrives at your backend. That's the most common surprise when moving a YAML from one place to another, and it's the reason this section exists.

The rule that matters: Sonda resolves sink URLs in the process that **runs** the scenario, not in the process that **submits** it. `localhost` means "this container's loopback" inside a container, "your host" outside. This section gives you a table for every realistic combination — host, Compose, Kubernetes, external — plus the env-var pattern that lets one YAML work from all of them.

### Two invocation paths

Every sink URL is resolved inside the process that is about to write to it. Sonda has two invocation paths, and they resolve `localhost` very differently.

=== "Host CLI"

    You run `sonda run file.yaml` on your laptop or a bare host. The scenario runs **in the shell process on your host**. `http://localhost:8428` resolves to your host's loopback, which reaches whatever is listening on port 8428 there — typically a Compose-published port or a native install.

    ```bash
    sonda run examples/victoriametrics-metrics.yaml
    ```

=== "`sonda-server` in a container"

    You POST a scenario body to `sonda-server` running inside a container. The scenario is compiled and runs **inside that container's network namespace**. `http://localhost:8428` resolves to the container's own loopback — nothing is there, the request fails.

    ```bash
    curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @file.yaml \
      http://localhost:8080/scenarios
    ```

The host-side `curl` talks to the host's loopback (hitting the published `8080:8080` port), but the scenario it carries runs one level deeper, inside the server container.

!!! warning "The `localhost` trap"
    A scenario with `url: http://localhost:8428` works from the host CLI and silently fails when POSTed to a containerized `sonda-server` — inside the container, `localhost` is the container, not your host. The POST returns 201, the sink times out, no data lands.

    Two fixes: write the URL with [`${VAR:-default}`](#one-file-both-paths-var-default) so one file works from both paths, or hardcode the in-network address (Compose service name like `http://victoriametrics:8428`, or the Kubernetes Service DNS `http://vmsingle.monitoring.svc.cluster.local:8428`).

### Endpoint resolution reference

Pick the row that matches where `sonda` runs and where your backend lives.

| Process runs here | Backend runs here | Correct `url:` | Why |
|---|---|---|---|
| Host CLI | Backend on host (native install) | `http://localhost:<port>` | Host loopback reaches the native listener. |
| Host CLI | Backend in Compose (port-published) | `http://localhost:<published-port>` | The Compose-published port is exposed on the host. |
| `sonda-server` in Compose | Backend in same Compose network | `http://<service-name>:<port>` | Compose provides a DNS entry per service. `victoriametrics`, `loki`, `kafka`. |
| `sonda-server` in Compose | Backend on host (Docker Desktop) | `http://host.docker.internal:<port>` | Docker Desktop publishes a virtual DNS name that routes back to the host. |
| `sonda-server` in Kubernetes (same namespace) | Service in same namespace | `http://<svc>:<port>` | Kubernetes DNS resolves short names within a namespace. |
| `sonda-server` in Kubernetes (cross-namespace) | Service in another namespace | `http://<svc>.<ns>.svc.cluster.local:<port>` | FQDN is required for cross-namespace resolution. |
| `sonda-server` anywhere | External backend (SaaS, cloud) | `https://<public-dns>:<port>` | Fully qualified external DNS plus TLS. |

!!! note
    On Linux without Docker Desktop, `host.docker.internal` does not resolve by default. Either add `--add-host=host.docker.internal:host-gateway` to the `sonda-server` container or use the host's LAN IP.

### One file, both paths: `${VAR:-default}`

The first-class fix is `${VAR:-default}` interpolation in the YAML itself. The same file then runs from your host CLI on the defaults and from a containerized `sonda-server` on the overrides — no edit, no `sed`, no second copy.

```yaml title="A sink URL that works from both paths"
sink:
  type: http_push
  url: "${VICTORIAMETRICS_URL:-http://localhost:8428/api/v1/import/prometheus}"
```

The bundled `examples/docker-compose-victoriametrics.yml` exports the in-network overrides on the `sonda-server` container, so every scenario under `examples/` already works untouched in both places. See the [full reference](../build/scenario-files.md#environment-variable-interpolation) for syntax and the seven built-in variable names every example honours.

### Rewriting URLs before POSTing

If a YAML file hardcodes `http://localhost:<port>` and you would rather not add interpolation, rewrite the URL in flight:

```bash title="Swap localhost for Compose service names"
sed 's|http://localhost:8428|http://victoriametrics:8428|g; \
     s|http://localhost:3100|http://loki:3100|g; \
     s|http://localhost:9094|http://kafka:9092|g' \
  examples/http-push-retry.yaml \
  | curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @- \
      http://localhost:8080/scenarios
```

The swaps cover the Compose backends Sonda ships with:

| Backend | Host CLI URL (published port) | Compose URL (service name) |
|---|---|---|
| VictoriaMetrics | `http://localhost:8428` | `http://victoriametrics:8428` |
| Loki | `http://localhost:3100` | `http://loki:3100` |
| Kafka | `localhost:9094` (external listener) | `kafka:9092` (internal listener) |

Service names come from `examples/docker-compose-victoriametrics.yml`. Match the `sed` substitutions to your service names if you customize the compose file.

!!! tip "Diff before you POST"
    ```bash
    sed 's|http://localhost:8428|http://victoriametrics:8428|g' \
      examples/http-push-retry.yaml | diff examples/http-push-retry.yaml -
    ```

### Networking examples

=== "Host CLI to Compose VictoriaMetrics"

    `sonda` runs on your host. The Compose stack publishes VictoriaMetrics on `localhost:8428`. The scenario's `url:` uses host loopback.

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

## Where to next

- [HTTP API reference](http-api.md) — every endpoint, request body, and response shape.
- [Docker](docker.md) — Compose stacks and host-side `docker run` examples.
- [Kubernetes](kubernetes.md) — Helm chart, Service DNS, cross-namespace access.
- [Sinks](../build/sinks.md) — every sink type and its `url:` field.
- [Troubleshooting](../reference/troubleshooting.md) — diagnostics for connection-refused and empty-backend symptoms.
