---
title: Get started
description: Install Sonda, run your first scenario, and send synthetic telemetry to a real backend.
---

# Get started

Three short pages get you from no Sonda installed to a synthetic metric landing in a real Prometheus, Loki, or OTLP backend. Plan on about fifteen minutes end to end.

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Quickstart](quickstart.md)__

    Install Sonda, scaffold a YAML scenario with `sonda new --template`, and watch a metric stream to stdout. Five minutes.

-   :material-shape-outline: __[Your first scenario](your-first-scenario.md)__

    The four moving parts of a Sonda scenario — scenario file, generator, encoder, sink — with small YAML examples for each.

-   :material-connection: __[Send to a real backend](send-to-a-backend.md)__

    Point your scenario at Prometheus (`remote_write`), Loki, or an OTLP collector. Includes the `docker run` commands to spin up each backend locally.

</div>

!!! tip "Where to next"
    Once you've finished here, jump to [Build scenarios](../build/index.md) to author YAML you can check into git, or [Test pipelines](../test/index.md) to start shaping data that exercises your alert rules.
