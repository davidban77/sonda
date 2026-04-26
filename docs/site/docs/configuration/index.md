# Configuration

Every knob you can turn on a Sonda scenario. The CLI flags, the YAML fields, the
generators, encoders, and sinks -- this section is the reference you reach for when
you know what you want to change and need to confirm the exact field name or the
default value.

If you are looking for *which* generator to pick or *how* to wire a Loki sink into
your pipeline, start with the [Tutorial](../guides/tutorial.md) or jump to the
relevant guide under [Guides](../guides/index.md). This section answers "what does
this field do" and "what are the valid values."

!!! tip "Read this first"
    If you only open one page in this section, make it
    [**v2 Scenario Files**](v2-scenarios.md). Every other reference here -- generators,
    encoders, sinks -- plugs into that shape.

## Scenario file shape

<div class="grid cards" markdown>

-   :material-file-document-outline: __[v2 Scenario Files](v2-scenarios.md)__

    The canonical file format: `version: 2`, `defaults:`, `scenarios:`, packs, and
    `after:` temporal chains.

-   :material-format-list-bulleted-type: __[Scenario Fields](scenario-fields.md)__

    Per-entry field reference for everything inside a `scenarios:` entry --
    generators, schedules, labels, encoders, sinks.

</div>

## Building blocks

<div class="grid cards" markdown>

-   :material-sine-wave: __[Generators](generators.md)__

    Value shapes: `constant`, `sine`, `sawtooth`, `sequence`, `step`, `spike`,
    `csv_replay`, plus operational aliases and histogram/summary generators.

-   :material-code-braces: __[Encoders](encoders.md)__

    Wire formats: Prometheus text, InfluxDB line protocol, JSON lines, syslog,
    Prometheus remote write, OTLP.

-   :material-export-variant: __[Sinks](sinks.md)__

    Destinations: stdout, file, TCP, UDP, HTTP push, remote write, Kafka, Loki,
    OTLP/gRPC.

-   :material-package-variant: __[Sink Batching](sink-batching.md)__

    How each sink buffers, default thresholds, and the `batch_size` field where
    it applies.

</div>

## Command line

<div class="grid cards" markdown>

-   :material-console: __[CLI Reference](cli-reference.md)__

    Every subcommand, every flag, exit codes, and the `SONDA_*` environment
    variables that override flags.

</div>
