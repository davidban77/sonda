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

```bash
# Start the server
docker run -p 8080:8080 ghcr.io/davidban77/sonda:latest

# Run the CLI instead
docker run --entrypoint /sonda ghcr.io/davidban77/sonda:latest \
  metrics --name up --rate 10 --duration 5s

# Mount scenario files from the host
docker run -p 8080:8080 -v ./examples:/scenarios ghcr.io/davidban77/sonda:latest
```

## Docker Compose Stack

A `docker-compose.yml` ships with a full observability stack for demos and testing.

| Service | Port | Description |
|---------|------|-------------|
| `sonda-server` | 8080 | Sonda HTTP API |
| `prometheus` | 9090 | Prometheus (scrape or remote write) |
| `alertmanager` | 9093 | Alertmanager for alert routing |
| `grafana` | 3000 | Grafana dashboards (password: `admin`) |

Start, use, and tear down:

```bash
# Start the stack
docker compose up -d

# Verify the server
curl http://localhost:8080/health

# Post a metrics scenario
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/docker-metrics.yaml \
  http://localhost:8080/scenarios

# Post an alert-testing scenario
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/docker-alerts.yaml \
  http://localhost:8080/scenarios

# List running scenarios
curl http://localhost:8080/scenarios

# Tear down
docker compose down
```

Open Grafana at [http://localhost:3000](http://localhost:3000) and Prometheus at
[http://localhost:9090](http://localhost:9090) to explore your data.

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

You can also push from the host CLI using a pipe to VictoriaMetrics:

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
    The stack includes vmagent for remote write relay. If you build Sonda with
    `--features remote-write`, you can push protobuf metrics through vmagent using
    `examples/remote-write-vm.yaml`. See [Encoders](../configuration/encoders.md) for details.
