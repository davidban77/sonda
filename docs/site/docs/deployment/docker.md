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

# Run the CLI instead
docker run --entrypoint /sonda ghcr.io/davidban77/sonda:latest \
  metrics --name up --rate 10 --duration 5s

# Mount scenario files from the host
docker run -p 8080:8080 -v ./examples:/scenarios ghcr.io/davidban77/sonda:latest
```

The image includes built-in [scenario](../guides/scenarios.md) and
[pack](../guides/metric-packs.md) YAML files at `/scenarios` and `/packs`, with
`SONDA_SCENARIO_PATH=/scenarios` and `SONDA_PACK_PATH=/packs` set so all built-in patterns
work out of the box. Mount a host directory to the same path to add or override scenarios.

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
    You can rewrite either scenario on the fly to push to Prometheus's remote-write
    receiver instead. See
    [Rewriting URLs before POSTing](endpoints.md#rewriting-urls-before-posting) --
    the Prometheus service name inside this Compose network is `prometheus`, and its
    remote-write path is `/api/v1/write`.

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

The scenario uses `url: http://victoriametrics:8428/...` -- the VictoriaMetrics service
name, not `localhost` -- because the sink runs inside the `sonda-server` container and
resolves DNS through the Compose network. If you adapt a host-CLI example that points at
`localhost:8428`, rewrite the URL before POSTing. See
[Endpoints & networking](endpoints.md) for the full explanation and a one-liner swap.

You can also push from the host CLI using a pipe to VictoriaMetrics.
See [Sinks](../configuration/sinks.md) for all available sink types (`http_push`, `remote_write`, `loki`, etc.).

```bash
sonda metrics \
  --name sonda_demo --rate 10 --duration 30s \
  --value-mode sine --amplitude 40 --period-secs 30 --offset 60 \
  --encoder prometheus_text --label job=sonda --label instance=local \
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
sonda metrics --scenario examples/alertmanager/alerting-scenario.yaml
```

See the [Alerting Pipeline](../guides/alerting-pipeline.md) guide for the full walkthrough.
