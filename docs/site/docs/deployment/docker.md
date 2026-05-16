# Docker

Sonda ships as a minimal Docker image built from scratch with statically linked musl binaries.
The image contains both the `sonda` CLI and the `sonda-server` HTTP API.

## Building the Image

The multi-stage Dockerfile compiles static musl binaries and copies them into a `scratch` base.
The final image is typically under 20 MB.

```bash
docker build -t sonda .
```

For multi-arch builds (linux/amd64 and linux/arm64) using Docker Buildx:

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t sonda .
```

Pre-built multi-arch images are published to GitHub Container Registry on each release.
Docker automatically pulls the correct architecture for your host.

```bash
docker pull ghcr.io/davidban77/sonda:latest
```

## Running with Docker

The default entrypoint is `sonda-server`, which starts the HTTP API on port 8080.
See [Server API](sonda-server.md) for the full endpoint reference.

```bash
# Start the server
docker run -p 8080:8080 ghcr.io/davidban77/sonda:latest

# Run the CLI against a mounted catalog (auto-detected as a sonda subcommand)
docker run --rm \
  -v "$PWD/my-catalog":/catalog \
  ghcr.io/davidban77/sonda:latest \
  --catalog /catalog run @cpu-spike

# Or run a single file directly
docker run --rm \
  -v "$PWD/scenarios":/work \
  ghcr.io/davidban77/sonda:latest \
  run /work/cpu-spike.yaml
```

!!! info "No `--entrypoint /sonda` needed"
    The default entrypoint inspects `argv[1]` and `exec`s the sibling `sonda` CLI when
    it matches a known subcommand (`run`, `list`, `show`, `new`). Recipes that pass
    `--entrypoint /sonda` still work.

The image ships no built-in catalog. Mount a directory of your own scenario and pack YAML
files (typically `kind: runnable` and `kind: composable` v2 files) at any path inside the
container and pass `--catalog <path>` to point `sonda` at it. See
[Catalogs](../guides/scenarios.md) for the directory layout.

## Authentication

You can protect the server's `/scenarios/*` endpoints with API key authentication.
Pass the key through the `SONDA_API_KEY` environment variable -- this works the same
way as the `--api-key` CLI flag.

=== "`docker run`"

    ```bash
    docker run -p 8080:8080 \
      -e SONDA_API_KEY=my-secret-key \
      ghcr.io/davidban77/sonda:latest
    ```

=== "Docker Compose"

    ```yaml title="docker-compose.yml"
    services:
      sonda-server:
        image: ghcr.io/davidban77/sonda:latest
        ports:
          - "8080:8080"
        environment:
          - SONDA_API_KEY=my-secret-key
    ```

Once enabled, all `/scenarios/*` requests require a `Bearer` token. The `/health`
endpoint stays public so health probes keep working.

```bash
# Authenticated request
curl -H "Authorization: Bearer my-secret-key" \
  http://localhost:8080/scenarios

# Health check (no auth needed)
curl http://localhost:8080/health
```

!!! warning "Don't embed secrets in plain text"
    The inline examples above are fine for local development. For production, use
    Docker secrets or a `.env` file instead of hardcoding the key in your compose file.

    ```bash title=".env"
    SONDA_API_KEY=your-production-key
    ```

    ```yaml title="docker-compose.yml"
    services:
      sonda-server:
        image: ghcr.io/davidban77/sonda:latest
        ports:
          - "8080:8080"
        env_file:
          - .env
    ```

See [Server API -- Authentication](sonda-server.md#authentication) for the full reference,
including error responses, protected vs. public endpoints, and Prometheus scrape configuration.

## Docker Compose Stack

A `docker-compose.yml` ships with `sonda-server`, Prometheus, Alertmanager, and Grafana
for smoke-testing scenario submission and exploring the control plane.

| Service | Port | Description |
|---------|------|-------------|
| `sonda-server` | 8080 | Sonda HTTP API |
| `prometheus` | 9090 | Prometheus (scrape or remote write) |
| `alertmanager` | 9093 | Alertmanager for alert routing |
| `grafana` | 3000 | Grafana dashboards (password: `admin`) |

!!! warning "Scenarios POSTed here write to container stdout"
    The two scenario files referenced below (`docker-metrics.yaml`,
    `docker-alerts.yaml`) use `sink: stdout`. When you POST them into `sonda-server`,
    the generated events land on the server container's stdout -- visible via
    `docker logs sonda-server` -- and nothing reaches Prometheus or Grafana in this
    stack.

    This stack is useful for verifying `sonda-server` accepts and runs your scenario
    body. **To see data flowing into Prometheus and Grafana, use the
    [VictoriaMetrics Stack](#victoriametrics-stack) below**, which ships scenarios
    that push to an HTTP backend reachable from inside the container.

    The [Endpoints & networking](endpoints.md) page explains why a sink URL resolves
    differently depending on where `sonda` runs.

Start, use, and tear down:

```bash
# Start the stack
docker compose up -d

# Verify the server
curl http://localhost:8080/health

# Post a metrics scenario (events go to sonda-server container stdout)
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/docker-metrics.yaml \
  http://localhost:8080/scenarios

# Post an alert-testing scenario
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/docker-alerts.yaml \
  http://localhost:8080/scenarios

# Inspect the generated events
docker logs -f sonda-server

# List running scenarios
curl http://localhost:8080/scenarios

# Tear down
docker compose down
```

Grafana is still provisioned at [http://localhost:3000](http://localhost:3000) and
Prometheus at [http://localhost:9090](http://localhost:9090) -- they just will not show
sonda data from the stdout scenarios above. Use the VictoriaMetrics Stack for that.

!!! info "Swapping `stdout` for `http_push` in this stack"
    Rewrite either scenario on the fly to push to Prometheus's remote-write receiver
    (`http://prometheus:9090/api/v1/write`). See
    [Endpoints & networking](endpoints.md#one-file-both-paths-var-default) -- use
    `${VAR:-default}` so one file works from both host CLI and inside the container,
    or `sed` the URL just before POSTing.

Two scenario files are provided for this stack:

- **`docker-metrics.yaml`** -- CPU sine wave (30--70%) with recurring gaps for testing gap-fill behavior.
- **`docker-alerts.yaml`** -- Sine wave (0--100) crossing warning/critical thresholds with burst windows.

See [Example Scenarios](../guides/examples.md) for the full catalog.

## VictoriaMetrics Stack

A dedicated compose file adds VictoriaMetrics, vmagent, and Grafana with a pre-provisioned datasource
and auto-provisioned **Sonda Overview** dashboard.

| Service | Port | Description |
|---------|------|-------------|
| `sonda-server` | 8080 | Sonda HTTP API |
| `victoriametrics` | 8428 | VictoriaMetrics single-node TSDB |
| `vmagent` | 8429 | Metrics relay agent |
| `grafana` | 3000 | Grafana with VictoriaMetrics datasource |

```bash
# Start the stack
docker compose -f examples/docker-compose-victoriametrics.yml up -d

# Push metrics via sonda-server
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/victoriametrics-metrics.yaml \
  http://localhost:8080/scenarios

# Verify data arrived
curl "http://localhost:8428/api/v1/series?match[]={__name__=~'sonda.*'}"

# Tear down
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

The scenario URL uses `${VICTORIAMETRICS_URL:-http://localhost:8428/...}`: the compose
file exports `VICTORIAMETRICS_URL` on the `sonda-server` container so the scenario
resolves the in-network service name when POSTed, and falls back to host loopback when
run from your host CLI. See
[Endpoints & networking](endpoints.md#one-file-both-paths-var-default) for the pattern.

You can also push from the host CLI using a pipe to VictoriaMetrics.
See [Sinks](../configuration/sinks.md) for all available sink types (`http_push`, `remote_write`, `loki`, etc.).

```yaml title="sonda-demo.yaml"
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    job: sonda
    instance: local
scenarios:
  - id: sonda_demo
    signal_type: metrics
    name: sonda_demo
    generator:
      type: sine
      amplitude: 40.0
      period_secs: 30
      offset: 60.0
```

```bash
sonda run sonda-demo.yaml \
  | curl -s --data-binary @- \
    -H "Content-Type: text/plain" \
    "http://localhost:8428/api/v1/import/prometheus"
```

Explore metrics in the VictoriaMetrics UI at [http://localhost:8428/vmui](http://localhost:8428/vmui),
or open Grafana and navigate to **Dashboards > Sonda > Sonda Overview**.

!!! tip
    The stack includes vmagent for remote write relay. You can push protobuf metrics
    through vmagent using `examples/remote-write-vm.yaml`.
    See [Encoders](../configuration/encoders.md) for details.

### Alerting Profile

Add vmalert, Alertmanager, and a webhook receiver to test the complete alerting loop:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting up -d
```

| Service | Port | Description |
|---------|------|-------------|
| `vmalert` | 8880 | Rule evaluation engine |
| `alertmanager` | 9093 | Alert routing and notification |
| `webhook-receiver` | 8090 | HTTP echo server (shows alert payloads) |

Push a threshold-crossing metric and watch alerts flow through to the webhook:

```bash
sonda run examples/alertmanager/alerting-scenario.yaml
```

See the [Alerting Pipeline](../guides/alerting-pipeline.md) guide for the full walkthrough.
