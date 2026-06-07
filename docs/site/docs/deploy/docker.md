# Docker

Use the Docker image when you want Sonda without a local Rust toolchain. Common cases are CI runners, a colleague's laptop, or running alongside the bundled observability stacks (Prometheus, VictoriaMetrics, Grafana) shown later on this page. The image is a single artifact. It carries both the `sonda` CLI and the `sonda-server` HTTP API. The same `docker run` works for a one-shot scenario or a long-running server.

## Run a scenario in three commands

Pre-built multi-arch images are published to GitHub Container Registry on each release. Pull the image, point it at a scenario file on your machine, and view the output:

```bash
# 1. Pull the published image (Docker picks the right architecture for your host)
docker pull ghcr.io/davidban77/sonda:latest

# 2. Generate a starter scenario file in the current directory
docker run --rm -v "$PWD":/work -w /work \
  ghcr.io/davidban77/sonda:latest \
  new --template -o hello.yaml

# 3. Run it — synthetic metrics stream to your terminal
docker run --rm -v "$PWD":/work -w /work \
  ghcr.io/davidban77/sonda:latest \
  run hello.yaml --duration 5s
```

That last command prints five Prometheus-format metric lines and a completion banner. The output matches a locally installed `sonda`. The `-v "$PWD":/work -w /work` flags mount your current directory into the container so `sonda` can read the YAML file and write the starter file back out.

From here you can run your own scenarios, start the HTTP server, or use one of the bundled stacks. See [Running with Docker](#running-with-docker) below for the server and [Docker Compose stacks](#docker-compose-stack) for the bundled options. Most readers use the published image and never build their own. The [build instructions](#building-the-image) are at the end of the page for when you do.

## Running with Docker

The default entrypoint is `sonda-server`. It starts the HTTP API on port 8080. See [Server API](server.md) for the full endpoint reference.

```bash
# Start the server (default behaviour)
docker run -p 8080:8080 ghcr.io/davidban77/sonda:latest

# Run the CLI against a mounted catalog by @name
docker run --rm \
  -v "$PWD/my-catalog":/catalog \
  ghcr.io/davidban77/sonda:latest \
  run @cpu-spike --catalog /catalog

# List what's in the mounted catalog
docker run --rm \
  -v "$PWD/my-catalog":/catalog \
  ghcr.io/davidban77/sonda:latest \
  list --catalog /catalog

# Or run a single file directly
docker run --rm \
  -v "$PWD/scenarios":/work \
  ghcr.io/davidban77/sonda:latest \
  run /work/cpu-spike.yaml
```

!!! info "`argv[1]` must be the subcommand"
    The default entrypoint is `sonda-server`. It inspects `argv[1]` and runs the sibling `sonda` CLI when it matches one of the four subcommands (`run`, `list`, `show`, `new`). That means **global flags like `--catalog` belong after the subcommand** when using the default entrypoint. Use `sonda run @x --catalog /catalog`, not `sonda --catalog /catalog run @x`. The host CLI accepts both orderings. The Docker constraint comes from the shim that runs the sibling binary, not clap.

    For invocations that don't start with a subcommand (`--help`, `--version`, or the global-flag-first style), override the entrypoint:

    ```bash
    # Inspect the 4-verb CLI surface
    docker run --rm --entrypoint /sonda \
      ghcr.io/davidban77/sonda:latest --help

    # Global-flag-first form, matching the host CLI
    docker run --rm --entrypoint /sonda \
      -v "$PWD/my-catalog":/catalog \
      ghcr.io/davidban77/sonda:latest \
      --catalog /catalog list
    ```

The image has no built-in catalog. Mount a directory of your own scenario and pack YAML files (typically `kind: runnable` and `kind: composable` files) at any path inside the container. Pass `--catalog <path>` to point `sonda` at it. See [Catalogs](../build/catalogs-and-packs.md) for the directory layout.

!!! warning "Pre-1.9 env vars are gone"
    Earlier releases let the image discover scenarios from `SONDA_PACK_PATH=/packs` and `SONDA_SCENARIO_PATH=/scenarios` environment variables. The image also included companion `/packs` and `/scenarios` directories. Both were removed in 1.9. Discovery is explicit through `--catalog <dir>`. There is no environment variable fallback and no implicit search path. Old recipes built around `docker run … run @scenario` fail with "catalog dir does not exist or is not a directory" or a `@name` resolution error. Add `--catalog /catalog` and mount your catalog volume there.

## Authentication

You can protect the server's `/scenarios/*` endpoints with API key authentication.
Pass the key through the `SONDA_API_KEY` environment variable. It works the same
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

See [Server API -- Authentication](server.md#authentication) for the full reference,
including error responses, protected vs. public endpoints, and Prometheus scrape configuration.

## Docker Compose Stack

A `docker-compose.yml` is provided with `sonda-server`, Prometheus, Alertmanager, and Grafana
for smoke-testing scenario submission and exploring the control plane.

| Service | Port | Description |
|---------|------|-------------|
| `sonda-server` | 8080 | Sonda HTTP API |
| `prometheus` | 9090 | Prometheus (scrape or remote write) |
| `alertmanager` | 9093 | Alertmanager for alert routing |
| `grafana` | 3000 | Grafana dashboards (password: `admin`) |

!!! warning "Scenarios sent here write to container stdout"
    The two scenario files referenced below (`docker-metrics.yaml`,
    `docker-alerts.yaml`) use `sink: stdout`. When you send them to `sonda-server`,
    the generated events arrive on the server container's stdout. View them with
    `docker logs sonda-server`. Nothing reaches Prometheus or Grafana in this stack.

    This stack is useful for verifying `sonda-server` accepts and runs your scenario
    body. **To see data flowing into Prometheus and Grafana, use the
    [VictoriaMetrics Stack](#victoriametrics-stack) below**. It includes scenarios
    that push to an HTTP backend reachable from inside the container.

    The [Endpoints & networking](server.md) page explains why a sink URL resolves
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
Prometheus at [http://localhost:9090](http://localhost:9090). They will not show
sonda data from the stdout scenarios above. Use the VictoriaMetrics Stack for that.

!!! info "Swapping `stdout` for `http_push` in this stack"
    Rewrite either scenario to push to Prometheus's remote-write receiver
    (`http://prometheus:9090/api/v1/write`). See
    [Endpoints & networking](server.md#one-file-both-paths-var-default). Use
    `${VAR:-default}` so one file works from both host CLI and inside the container,
    or `sed` the URL before posting.

Two scenario files are provided for this stack:

- **`docker-metrics.yaml`** -- CPU sine wave (30--70%) with recurring gaps for testing gap-fill behavior.
- **`docker-alerts.yaml`** -- Sine wave (0--100) crossing warning/critical thresholds with burst windows.

See [Example Scenarios](../test/examples.md) for the full catalog.

## VictoriaMetrics Stack

A dedicated compose file adds VictoriaMetrics, vmagent, and Grafana with a pre-provisioned datasource
and a pre-loaded **Sonda Overview** dashboard.

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

The scenario URL uses `${VICTORIAMETRICS_URL:-http://localhost:8428/...}`. The compose
file sets `VICTORIAMETRICS_URL` on the `sonda-server` container, so the scenario
resolves the in-network service name when sent over HTTP. The default applies when
you run the scenario from your host CLI. See
[Endpoints & networking](server.md#one-file-both-paths-var-default) for the pattern.

You can also push from the host CLI with a pipe to VictoriaMetrics.
See [Sinks](../build/sinks.md) for all available sink types (`http_push`, `remote_write`, `loki`, etc.).

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
or open Grafana and go to **Dashboards > Sonda > Sonda Overview**.

!!! tip
    The stack includes vmagent for remote write relay. You can push protobuf metrics
    through vmagent with `examples/remote-write-vm.yaml`.
    See [Encoders](../build/encoders.md) for details.

### Alerting Profile

Add vmalert, Alertmanager, and a webhook receiver to test the full alerting path:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting up -d
```

| Service | Port | Description |
|---------|------|-------------|
| `vmalert` | 8880 | Rule evaluation engine |
| `alertmanager` | 9093 | Alert routing and notification |
| `webhook-receiver` | 8090 | HTTP echo server (shows alert payloads) |

Push a threshold-crossing metric and observe alerts arrive at the webhook:

```bash
sonda run examples/alertmanager/alerting-scenario.yaml
```

See the [Alerting Pipeline](../test/end-to-end-pipelines.md) guide for the full walkthrough.

## Building the Image

Most readers use the published `ghcr.io/davidban77/sonda` image and never need this section. Build your own only when you want a local image from a working tree. One example is testing an unreleased change.

The multi-stage Dockerfile compiles static musl binaries and copies them into a `scratch` base. The final image is typically under 20 MB.

```bash
docker build -t sonda .
```

For multi-arch builds (linux/amd64 and linux/arm64) using Docker Buildx:

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t sonda .
```

The pre-built multi-arch images on GitHub Container Registry come from this same Dockerfile on each release. Docker pulls the correct architecture for your host automatically:

```bash
docker pull ghcr.io/davidban77/sonda:latest
```
