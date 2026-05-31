---
title: Build scenarios
description: Reference for the YAML scenario file format, generators, encoders, sinks, scheduling, and catalogs.
---

# Build scenarios

Sonda scenarios are YAML files you check into git alongside your alert rules and dashboards. This section covers the building blocks, the file format, the catalog of generators / encoders / sinks, scheduling primitives (gaps, bursts, dependencies), and how to organize multiple scenarios into a reusable catalog.

Start with **Concepts** if you've finished [Get started](../get-started/index.md) and want the names for the four moving parts. Jump straight to **Generators**, **Encoders**, or **Sinks** when you're looking up a specific type.

<div class="grid cards" markdown>

-   :material-shape-outline: __[Concepts](concepts.md)__

    The four nouns Sonda is built around — scenario, entry, pack, catalog — with a worked node-exporter-shaped example.

-   :material-file-document-outline: __[Scenario file format](scenario-files.md)__

    The canonical file shape: `version: 2`, `kind: runnable`, shared `defaults:`, `after:` chains, env-var interpolation, sink-error policy.

-   :material-chart-line-variant: __[Generators](generators.md)__

    Eight core metric generators (sine, sawtooth, step, spike, ...) plus operational aliases (`flap`, `saturation`, `leak`, ...) and log + histogram generators.

-   :material-translate-variant: __[Encoders](encoders.md)__

    Prometheus text, InfluxDB line protocol, JSON Lines, syslog, remote-write protobuf, OTLP. With precision rules and feature flags.

-   :material-cable-data: __[Sinks](sinks.md)__

    stdout, file, TCP/UDP, HTTP push, remote_write, Loki, Kafka, OTLP gRPC. Includes TLS, SASL, and retry-with-backoff.

-   :material-package-variant: __[Sink batching](sink-batching.md)__

    How network sinks buffer events before delivery, the size and time thresholds, and the trade-offs when tuning them.

-   :material-clock-time-four-outline: __[Scheduling](scheduling.md)__

    Gaps, bursts, dynamic labels, cardinality spikes, and dependencies (`after:` and `while:`) — everything that shapes the scenario over time.

-   :material-folder-multiple-outline: __[Catalogs and packs](catalogs-and-packs.md)__

    Organize a directory of scenarios with `--catalog <dir>`; reuse metric shapes across scenarios with composable packs.

</div>
