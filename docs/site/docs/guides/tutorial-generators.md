# Generators

A metric that always outputs zero is not very useful for testing. Generators let you
shape the values Sonda emits -- smooth waves for latency simulation, random noise for
jitter, or exact sequences to trigger alert thresholds.

Sonda ships eight generators:

| Generator | Description | Best for |
|-----------|-------------|----------|
| `constant` | Fixed value every tick | Up/down indicators, baselines |
| `sine` | Smooth sinusoidal wave | CPU, latency, cyclical load |
| `sawtooth` | Linear ramp, resets at period | Queue depth, buffer fill |
| `uniform` | Random value in `[min, max]` | Jitter, noisy signals |
| `sequence` | Cycles through an explicit list | Alert threshold testing |
| `step` | Monotonic counter with optional wrap | `rate()` and `increase()` testing |
| `spike` | Baseline with periodic spikes | Anomaly detection, alert thresholds |
| `csv_replay` | Replays values from a CSV file | Reproducing real incidents |

The generator picks the **shape** of the values; everything else (rate, duration, encoder, sink, labels) lives in the same scenario YAML. Scaffold a starter file with `sonda new --template`, swap the `generator:` block, and run with `sonda run <file>`.

## constant

A fixed value every tick — the simplest "is the pipeline alive?" probe:

```yaml title="up-constant.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 3s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: up
    signal_type: metrics
    name: up
    generator:
      type: constant
      value: 1.0
```

```bash
sonda run up-constant.yaml
```

## sine

Produces a smooth wave defined by amplitude, offset (midpoint), and period:

```yaml title="cpu-sine.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 40.0
      offset: 50.0
      period_secs: 30
```

```bash
sonda run cpu-sine.yaml
```

This oscillates between 10 and 90, centered on 50, completing one cycle every 30 seconds.

??? info "Sine wave math"
    The formula is `value = offset + amplitude * sin(2 * pi * elapsed / period)`. At t=0
    the value equals offset. It peaks at `offset + amplitude` after one quarter period.

## sequence

For testing alert thresholds, you often need values that cross a specific boundary at a
specific time. `sequence` gives you that exact control:

```bash
sonda run examples/sequence-alert-test.yaml --duration 10s
```

```yaml title="examples/sequence-alert-test.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 80s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: cpu_spike_test
    signal_type: metrics
    name: cpu_spike_test
    generator:
      type: sequence
      values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
      repeat: true
    labels:
      instance: server-01
      job: node
```

Each tick emits the next value in the list; `repeat: true` cycles back to the start. With
`rate: 1`, every value lands one second apart -- the spike crosses the threshold at t=5s
and clears at t=10s, deterministically, every run.

## The other four generators

??? tip "sawtooth, uniform, step, csv_replay"
    **sawtooth** -- A linear ramp from 0 to 1 that resets every period. Useful for
    simulating queue fill and drain cycles:

    ```yaml
    generator:
      type: sawtooth
      period_secs: 5
    ```

    **uniform** -- Random values drawn uniformly between `min` and `max`. Set `seed`
    for deterministic replay:

    ```yaml
    generator:
      type: uniform
      min: 1.0
      max: 100.0
      seed: 42
    ```

    **step** -- A monotonic counter that increments by `step_size` each tick. Set `max`
    to simulate counter resets, perfect for testing `rate()` and `increase()`:

    ```bash
    sonda run examples/step-counter.yaml --duration 5s
    ```

    **csv_replay** -- Replays recorded values from a CSV file. Point it at real
    incident data to reproduce production behavior:

    ```bash
    sonda run examples/csv-replay-metrics.yaml
    ```

    ```yaml title="examples/csv-replay-metrics.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 60s
      encoder:
        type: prometheus_text
      sink:
        type: stdout
      labels:
        instance: prod-server-42
        job: node

    scenarios:
      - id: cpu_replay
        signal_type: metrics
        name: cpu_replay
        generator:
          type: csv_replay
          file: examples/sample-cpu-values.csv
          columns:
            - index: 1
              name: cpu_replay
    ```

    For multi-column CSV files, add more entries to `columns` to emit multiple metrics
    from a single scenario -- see
    [Generators -- csv_replay](../configuration/generators.md#csv_replay).

For full configuration of every field on every generator (including `spike`), see the
[Generators reference](../configuration/generators.md).

## Add realism with jitter

Real metrics are never perfectly smooth. Add `jitter` to any generator to introduce
deterministic uniform noise:

```yaml title="examples/jitter-sine.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: cpu_usage_realistic
    generator:
      type: sine
      amplitude: 20
      period_secs: 120
      offset: 50
    jitter: 3.0
    jitter_seed: 42
    labels:
      instance: server-01
      job: node
```

Run it:

```bash
sonda run examples/jitter-sine.yaml
```

This adds noise in the range `[-3.0, +3.0]` to every value. Set `jitter_seed` for
reproducible noise across runs. See
[Generators -- Jitter](../configuration/generators.md#jitter) for details.

## Next

You have seen what values Sonda can generate. Next, see how those values are
formatted on the wire.

[Continue to **Encoders** -->](tutorial-encoders.md)
