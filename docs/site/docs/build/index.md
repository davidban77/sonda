---
title: Build scenarios
description: Reference for the YAML scenario file format, generators, encoders, sinks, scheduling, and catalogs.
---

# Build scenarios

Sonda scenarios are YAML files you check into git alongside your alert rules and dashboards. This section is the reference for the scenario format and its parts:

- The file structure and shared defaults.
- The catalog of generators, encoders, and sinks.
- Scheduling options.
- Catalog and pack layout for many scenarios.

Start with **Concepts** if you have finished [Get started](../get-started/index.md) and want the names for each part. Go to **Generators**, **Encoders**, or **Sinks** when you need to look up a specific type.

### Start here

<div class="grid cards" markdown>

-   :material-shape-outline: __[Concepts](concepts.md)__

    The four nested parts of Sonda — scenario, entry, pack, catalog — with a worked example based on a Node Exporter.

-   :material-file-document-outline: __[Scenario file format](scenario-files.md)__

    The full file structure: `version: 2`, `kind: runnable`, shared `defaults:`, `after:` chains, environment-variable interpolation, and the [sink-error policy](../reference/glossary.md#sink-error-policy) for sink write failures.

-   :material-folder-multiple-outline: __[Catalogs and packs](catalogs-and-packs.md)__

    Organize a directory of scenarios with `--catalog <dir>`, and reuse metric definitions across scenarios with composable packs.

</div>

### Building blocks

<div class="grid cards" markdown>

-   :material-chart-line-variant: __[Generators](generators.md)__

    Eight core metric generators (sine, sawtooth, step, spike, and others). Plus shortcut names for common combinations (`flap`, `saturation`, `leak`) and generators for logs and histograms.

-   :material-translate-variant: __[Encoders](encoders.md)__

    Prometheus text, InfluxDB line protocol, JSON Lines, syslog, remote-write protobuf, and OTLP. Includes precision rules and feature flags.

-   :material-cable-data: __[Sinks](sinks.md)__

    stdout, file, TCP, UDP, HTTP push, remote_write, Loki, Kafka, and OTLP gRPC. Includes TLS, SASL, and retry-with-backoff.

</div>

### Advanced

<div class="grid cards" markdown>

-   :material-clock-time-four-outline: __[Scheduling](scheduling.md)__

    Gaps, bursts, dynamic labels, cardinality spikes, and dependencies (`after:` and `while:`) that drive a scenario's behavior over time.

-   :material-package-variant: __[Sink batching](sink-batching.md)__

    How network sinks buffer events before delivery, the size and time thresholds that flush the buffer, and the trade-offs when tuning them.

</div>
</content>
</invoke>