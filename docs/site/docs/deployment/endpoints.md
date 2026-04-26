# Endpoints & networking

Sonda scenarios resolve sink URLs in the process that runs them, not in the process that
submits them. Running the same YAML from your host CLI and POSTing it to a containerized
`sonda-server` can reach very different backends -- or reach nothing at all.

This page explains how to pick the right `url:` for every realistic combination of where
`sonda` runs and where your backend lives.

## Two invocation paths

Every sink URL is resolved inside the process that is about to write to it. Sonda has two
invocation paths, and they resolve `localhost` very differently.

=== "Host CLI"

    You run `sonda metrics --scenario file.yaml` on your laptop or a bare host. The
    scenario runs **in the shell process on your host**. `http://localhost:8428` resolves
    to your host's loopback, which reaches whatever is listening on port 8428 there --
    typically a Compose-published port or a native install.

    ```bash
    sonda metrics --scenario examples/victoriametrics-metrics.yaml
    ```

=== "`sonda-server` in a container"

    You POST a scenario body to `sonda-server` running inside a container. The scenario
    is compiled and runs **inside that container's network namespace**.
    `http://localhost:8428` resolves to the container's own loopback -- nothing is there,
    the request fails.

    ```bash
    curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @file.yaml \
      http://localhost:8080/scenarios
    ```

The host-side `curl` talks to the host's loopback (hitting the published `8080:8080`
port), but the scenario it carries runs one level deeper, inside the server container.

!!! warning "The `localhost` trap"
    A scenario with `url: http://localhost:8428` works from the host CLI and silently
    fails when POSTed to a containerized `sonda-server`. From inside the container,
    `localhost` is the container, not your host or the Compose network. The server
    accepts the POST, the scenario starts, the sink tries to connect -- and the
    connection is refused or times out with no data in your backend.

    When you adapt a host-CLI example to run inside `sonda-server`, rewrite the URL to
    match the server's network. For Compose, that means using the service name
    (`http://victoriametrics:8428`). For Kubernetes, use the in-cluster Service DNS
    (`http://vmsingle.monitoring.svc.cluster.local:8428`).

## Endpoint resolution reference

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
    On Linux without Docker Desktop, `host.docker.internal` does not resolve by default.
    Either add `--add-host=host.docker.internal:host-gateway` to the `sonda-server`
    container or use the host's LAN IP.

## Rewriting URLs before POSTing

The first-class fix is environment-variable interpolation -- see
[URL interpolation with `${VAR:-default}`](#url-interpolation-with-var-default) below.
The bundled examples already use `${VAR:-default}` syntax so the same file works from
both invocation paths without edits.

If you have a YAML file that hardcodes `http://localhost:<port>` and you cannot or do not
want to add interpolation, rewrite the URL in flight when POSTing:

```bash title="Swap localhost for Compose service names"
sed 's|http://localhost:8428|http://victoriametrics:8428|g; \
     s|http://localhost:3100|http://loki:3100|g; \
     s|http://localhost:9094|http://kafka:9092|g' \
  examples/http-push-retry.yaml \
  | curl -X POST -H "Content-Type: text/yaml" \
      --data-binary @- \
      http://localhost:8080/scenarios
```

The three swaps cover the Compose backends Sonda ships with:

| Backend | Host CLI URL (published port) | Compose URL (service name) |
|---|---|---|
| VictoriaMetrics | `http://localhost:8428` | `http://victoriametrics:8428` |
| Loki | `http://localhost:3100` | `http://loki:3100` |
| Kafka | `localhost:9094` (external listener) | `kafka:9092` (internal listener) |

The service names come from `examples/docker-compose-victoriametrics.yml`. If you
customize the compose file, match the `sed` substitutions to your service names.

!!! tip "Verify the swap first"
    Pipe the rewritten YAML to `less` or `diff` before posting, so you can eyeball that
    every URL has been updated:

    ```bash
    sed 's|http://localhost:8428|http://victoriametrics:8428|g' \
      examples/http-push-retry.yaml | diff examples/http-push-retry.yaml -
    ```

## Examples

=== "Host CLI to Compose VictoriaMetrics"

    `sonda` runs on your host. The Compose stack publishes VictoriaMetrics on
    `localhost:8428`. The scenario's `url:` uses host loopback.

    ```yaml
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

    ```bash
    sonda metrics --scenario examples/vm-push-scenario.yaml
    ```

=== "sonda-server (Compose) to VictoriaMetrics (Compose)"

    Both services run in the same Compose network. The sink URL uses the VictoriaMetrics
    service name -- **not** `localhost`.

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

## URL interpolation with `${VAR:-default}`

The example scenarios in `examples/` use environment variable interpolation so the same
file works from both invocation paths. A sink URL like

```yaml
sink:
  type: http_push
  url: "${VICTORIAMETRICS_URL:-http://localhost:8428/api/v1/import/prometheus}"
```

resolves to the host-loopback default when no environment variable is set, and to the
in-network service hostname when `sonda-server` exports `VICTORIAMETRICS_URL=http://victoriametrics:8428/api/v1/import/prometheus`
in its container environment. The bundled `examples/docker-compose-victoriametrics.yml`
already wires up the right values for the Compose-network case, so POSTing an example
scenario to the containerized server "just works" out of the box.

See the full [Environment variable interpolation reference](../configuration/v2-scenarios.md#environment-variable-interpolation)
for the syntax, the variable name grammar, and the table of built-in variables every
example file honours.

When you cannot or do not want to set environment variables, the rewrite-before-POST
workflow above still works.

## See also

- [Docker](docker.md) -- Compose stacks and host-side `docker run` examples.
- [Server API](sonda-server.md) -- POSTing scenarios to `sonda-server`.
- [Kubernetes](kubernetes.md) -- Helm chart, Service DNS, cross-namespace access.
- [Sinks](../configuration/sinks.md) -- all sink types and their `url:` fields.
- [Troubleshooting](../guides/troubleshooting.md) -- diagnostics for connection-refused and empty-backend symptoms.
