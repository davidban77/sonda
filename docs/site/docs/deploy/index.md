---
title: Deploy
description: Run Sonda as a CLI, a long-running HTTP server, or a containerized workload.
---

# Deploy

Sonda runs anywhere a Linux container runs. The same binary works on your laptop, on a CI runner, in Docker Compose, and as a Kubernetes Job. This section covers the three operating modes and the platforms each one supports.

<div class="grid cards" markdown>

-   :material-console: __[As a CLI](cli.md)__

    Run scenarios one-shot from the command line. The same binary works on a laptop and a CI runner. Flags override the YAML file. Exit codes are clean for scripting.

-   :material-server-network: __[As a server](server.md)__

    Run `sonda-server` as a long-running HTTP control plane. Send scenarios over REST, list and stop them, and scrape Prometheus metrics per scenario. Useful for CI and synthetic-monitoring fleets.

-   :material-api: __[HTTP API reference](http-api.md)__

    Every endpoint of `sonda-server`: scenarios, events, metrics, and health. Each entry shows the request shape and response examples.

-   :material-docker: __[Docker](docker.md)__

    The published GHCR image, the bundled Compose stack (VictoriaMetrics, vmagent, Grafana, Loki), and `docker run` examples.

-   :material-kubernetes: __[Kubernetes](kubernetes.md)__

    The Helm chart, Service DNS layout, API-key Secret pattern, and resource limits.

</div>
