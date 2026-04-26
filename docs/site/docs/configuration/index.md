# Configuration

Every knob you can turn on a Sonda scenario. The CLI flags, the YAML fields, the
generators, encoders, and sinks -- this section is the reference you reach for when
you know what you want to change and need to confirm the exact field name or the
default value.

If you are looking for *which* generator to pick or *how* to wire a Loki sink into
your pipeline, start with the [Tutorial](../guides/tutorial.md) or jump to the
relevant guide under [Guides](../guides/index.md). This section answers "what does
this field do" and "what are the valid values."

## Scenario file shape

- [**v2 Scenario Files**](v2-scenarios.md) -- the canonical file format: `version: 2`,
  `defaults:`, `scenarios:`, packs, and `after:` temporal chains.
- [**Scenario Fields**](scenario-fields.md) -- per-entry field reference for everything
  inside a `scenarios:` entry (generators, schedules, labels, encoders, sinks).

## Building blocks

- [**Generators**](generators.md) -- value shapes: `constant`, `sine`, `sawtooth`,
  `sequence`, `step`, `spike`, `csv_replay`, plus operational aliases (`steady`, `flap`,
  `saturation`, `leak`, `degradation`, `spike_event`) and histogram/summary generators.
- [**Encoders**](encoders.md) -- wire formats: Prometheus text, InfluxDB line protocol,
  JSON lines, syslog, Prometheus remote write, OTLP.
- [**Sinks**](sinks.md) -- destinations: stdout, file, TCP, UDP, HTTP push, remote
  write, Kafka, Loki, OTLP/gRPC.
- [**Sink Batching**](sink-batching.md) -- how each sink buffers, default thresholds,
  and the `batch_size` field where it applies.

## Command line

- [**CLI Reference**](cli-reference.md) -- every subcommand, every flag, exit codes,
  and the `SONDA_*` environment variables that override flags.
