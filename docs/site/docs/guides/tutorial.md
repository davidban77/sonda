# Tutorial

A guided tour through every part of Sonda you will reach for in real testing work.
Each page below adds one concept on top of the last; by the end, you can build
multi-signal scenarios, push them to a real backend, and drive them from the HTTP API.

This tutorial picks up where [Getting Started](../getting-started.md) leaves off. You
have Sonda installed and have run your first metric and log; now you want to know
what every knob does.

**What you need:**

- Sonda installed -- see [Getting Started -- Installation](../getting-started.md#installation).
- Docker -- only for the [Server API](tutorial-server.md) page.

## The tour

1. [**Generators**](tutorial-generators.md) -- the eight value shapes Sonda can produce, when to reach for each, and how `jitter` adds realism.
2. [**Encoders**](tutorial-encoders.md) -- wire formats your backend speaks: Prometheus text, InfluxDB line protocol, JSON lines, syslog, OTLP, remote write.
3. [**Sinks**](tutorial-sinks.md) -- where the encoded bytes go: stdout, files, HTTP, TCP/UDP, Loki, Kafka, OTLP/gRPC.
4. [**Generating logs**](tutorial-logs.md) -- template mode with field pools, replay mode, and pairing with the syslog encoder.
5. [**Scheduling -- gaps and bursts**](tutorial-scheduling.md) -- inject the irregularities real telemetry has, so your alerts and pipeline see real-shaped data.
6. [**Multi-scenario runs**](tutorial-multi-scenario.md) -- `sonda run`, phase offsets, and `clock_group` for compound-alert testing.
7. [**The Server API**](tutorial-server.md) -- submit scenarios over HTTP, scrape live stats, manage long-running runs.

## After the tour

When you have walked through all seven pages you have everything you need to:

- [**Push synthetic data into a real backend end-to-end**](e2e-testing.md) -- the canonical
  start-the-stack, push, query loop with VictoriaMetrics, Loki, Kafka, and OTLP.
- [**Test alert rules with predictable threshold crossings**](alert-testing.md).
- [**Validate a pipeline change end-to-end**](pipeline-validation.md).
- [**Plan capacity for high-volume runs**](capacity-planning.md).

If you would rather not write YAML by hand, run **`sonda init`** for an interactive
wizard, or browse [**Built-in Scenarios**](scenarios.md) for ready-to-run patterns.
