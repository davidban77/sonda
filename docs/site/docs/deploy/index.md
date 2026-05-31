---
title: Deploy
description: Run Sonda as a CLI, a long-running HTTP server, or a containerized workload.
---

# Deploy

Sonda runs anywhere a Linux container can. Same binary on your laptop, on a CI runner, in Docker Compose, or as a Kubernetes Job. This section covers the three operating modes and the platforms each one supports.

<div class="grid cards" markdown>

-   :material-console: __[As a CLI](cli.md)__

    Run scenarios one-shot from the command line. Same binary on laptop and CI runner; flags override YAML; clean exit codes for scripting.

-   :material-server-network: __[As a server](server.md)__

    Run `sonda-server` as a long-running HTTP control plane. POST scenarios over REST, list and stop them, scrape Prometheus metrics off each running scenario. Great for CI and synthetic-monitoring fleets.

-   :material-api: __[HTTP API reference](http-api.md)__

    Every endpoint exposed by `sonda-server` — scenarios, events, metrics, health — with request shapes and response examples.

-   :material-docker: __[Docker](docker.md)__

    The published GHCR image, the bundled Compose stack (VictoriaMetrics + vmagent + Grafana + Loki), and `docker run` examples.

-   :material-kubernetes: __[Kubernetes](kubernetes.md)__

    The Helm chart, Service DNS layout, API-key secret wiring, and resource limits.

</div>
