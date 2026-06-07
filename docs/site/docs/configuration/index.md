<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">Reference</span>

<h1 class="sonda-section-hero__title">Configuration</h1>

<p class="sonda-section-hero__subtitle">Every knob you can turn on a Sonda scenario — CLI flags, YAML fields, generators, encoders, sinks. This is the reference you reach for when you already know what you want to change and need to confirm the exact field name or the default value.</p>

</div>

If you are looking for *which* generator to pick or *how* to wire a Loki sink into
your pipeline, start with the [Tutorial](../get-started/quickstart.md) or jump to the
relevant guide under [Guides](../test/index.md). This section answers "what does
this field do" and "what are the valid values."

!!! tip "Read this first"
    If you only open one page in this section, make it
    [**Scenario Files**](../build/scenario-files.md). Every other reference here -- generators,
    encoders, sinks -- plugs into that shape.

## Scenario file shape

<div class="grid cards" markdown>

-   :material-lightbulb-outline: __[Concepts](../build/concepts.md)__

    The vocabulary: scenario, entry, pack, catalog, defaults inheritance, and
    multi-scenario runs. Start here if you are new.

-   :material-file-document-outline: __[Scenario Files](../build/scenario-files.md)__

    The canonical file format: `version: 2`, `defaults:`, `scenarios:`, packs, and
    `after:` temporal chains.

-   :material-format-list-bulleted-type: __[Scenario Fields](../reference/scenario-fields.md)__

    Per-entry field reference for everything inside a `scenarios:` entry --
    generators, schedules, labels, encoders, sinks.

</div>

## Building blocks

<div class="grid cards" markdown>

-   :material-sine-wave: __[Generators](../build/generators.md)__

    Value shapes: `constant`, `sine`, `sawtooth`, `sequence`, `step`, `spike`,
    `csv_replay`, plus shortcuts for common combinations and histogram/summary generators.

-   :material-code-braces: __[Encoders](../build/encoders.md)__

    Wire formats: Prometheus text, InfluxDB line protocol, JSON lines, syslog,
    Prometheus remote write, OTLP.

-   :material-export-variant: __[Sinks](../build/sinks.md)__

    Destinations: stdout, file, TCP, UDP, HTTP push, remote write, Kafka, Loki,
    OTLP/gRPC.

-   :material-package-variant: __[Sink Batching](../build/sink-batching.md)__

    How each sink buffers, default thresholds, and the `batch_size` field where
    it applies.

</div>

## Command line

<div class="grid cards" markdown>

-   :material-console: __[CLI Reference](../reference/cli-flags.md)__

    Every subcommand, every flag, exit codes, and the `SONDA_*` environment
    variables that override flags.

</div>
