---
title: Getting started with Sonda
description: Install Sonda, stream your first synthetic metric to stdout in under five minutes.
hide:
  - toc
---

<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">Quickstart · ~5 minutes</span>

<h1 class="sonda-section-hero__title">Get started with Sonda</h1>

<p class="sonda-section-hero__subtitle">Install Sonda, generate a starter YAML file with <code>sonda new</code>, and stream a synthetic metric to stdout. You do not need to write YAML by hand.</p>

</div>

## Installation

Pick the option that matches your environment. Each tab shows a single command.

=== "Install script (Linux/macOS)"

    ```bash
    curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
    ```

    Set a specific version with `SONDA_VERSION=v1.9.0` before the `| sh`.

=== "Cargo"

    ```bash
    cargo install sonda
    ```

=== "Docker"

    ```bash
    docker pull ghcr.io/davidban77/sonda:latest
    docker run --rm \
      -v "$PWD":/work -w /work \
      ghcr.io/davidban77/sonda:latest \
      run my-scenario.yaml
    ```

    ??? note "What the image runs"
        The image starts `sonda-server` by default. When the first argument is a CLI subcommand (`run`, `list`, `show`, `new`), the image runs the `sonda` CLI instead. The example above uses `run`, so it runs the CLI. See [Docker](../deploy/docker.md) for details.

=== "From source"

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    cargo build --release -p sonda
    ```

    The binary is at `target/release/sonda`.

Check the install: `sonda --version` should print the installed version.

## Your first metric

Sonda reads YAML files called **scenarios**. A scenario describes the telemetry you want to generate. You do not need to write one by hand — `sonda new --template` generates a starter file.

Generate the file:

```bash
sonda new --template -o hello.yaml
```

The file uses Sonda's YAML format. [Your first scenario](your-first-scenario.md) explains each field. For now, you can run the file as is.

```yaml title="hello.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: example
    signal_type: metrics
    name: example_metric
    generator:
      type: constant
      value: 1.0
```

Run it for five seconds:

```bash
sonda run hello.yaml --duration 5s
```

Sonda prints two kinds of output:

- **Status lines** on stderr. They start with `▶` when the scenario starts and `■` when it finishes. They report rate, encoder, sink, event count, and bytes sent.
- **Data lines** on stdout. They contain the actual metric samples.

```text title="stderr"
▶ example_metric  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 5s
```

```text title="stdout"
example_metric 1 1774277933018
example_metric 1 1774277934023
example_metric 1 1774277935023
example_metric 1 1774277936023
example_metric 1 1774277937023
■ example_metric  completed in 5.0s | events: 5 | bytes: 130 B | errors: 0
```

Each data line is in Prometheus exposition format: `metric_name value timestamp_ms`. See the [glossary](../reference/glossary.md#prometheus-exposition-format) for the format reference.

Because the status lines go to stderr, you can pipe stdout to another process and receive only the data.

!!! tip "Hide the status lines"
    `sonda -q run hello.yaml` (or `--quiet`) removes the status lines on stderr.

## Where to next

You have a metric streaming. The next pages take this further, one topic at a time.

- **[Your first scenario](your-first-scenario.md)** — the four parts of a scenario (file, generator, encoder, sink) with small YAML examples for each.
- **[Send to a real backend](send-to-a-backend.md)** — change the `sink:` block to reach Prometheus `remote_write` or Loki. Includes `docker run` commands for each backend.
- **[Generators](../build/generators.md)** — the eight value patterns (`sine`, `step`, `spike`, and others), with parameters for each.
- **[CLI Reference](../reference/cli-flags.md)** — every flag for `run`, `list`, `show`, and `new`.
