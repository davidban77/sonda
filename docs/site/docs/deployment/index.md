# Deployment

Sonda runs three ways: as a one-shot CLI binary you invoke from a shell, as a
long-lived `sonda-server` HTTP control plane, or as a containerized deployment in
Docker or Kubernetes. The shape you pick depends on whether scenarios are
human-driven (CLI), automation-driven (server), or part of an always-on
synthetic-monitoring fleet (Kubernetes).

This section covers the runtime side: where the process lives, how it reaches your
backends, and how scenarios are submitted. For *what* the scenarios contain, see
[Configuration](../configuration/index.md).

## Start here

- [**Endpoints & networking**](endpoints.md) -- the rules for `localhost`, Compose
  service names, Docker Desktop's `host.docker.internal`, and Kubernetes Service DNS.
  Read this before you change a sink URL -- the `localhost` trap catches most
  first-time `sonda-server` users.

## Runtimes

- [**Docker**](docker.md) -- pulling the image, dispatch between `sonda` and
  `sonda-server`, and the bundled Compose stacks for VictoriaMetrics, Loki, Kafka,
  and Grafana.
- [**Kubernetes**](kubernetes.md) -- the Helm chart, Deployment + Service shape,
  health probes, ConfigMap-injected scenarios, and cluster-DNS sink URLs.
- [**Server API**](sonda-server.md) -- the REST surface for submitting scenarios,
  inspecting live stats, scraping metrics, and stopping runs over HTTP.
