# Threshold and `for:` duration alerts

The two most common alert shapes are also the two easiest to get wrong: a `> threshold`
rule that never fires because the metric only breaches for 30 seconds, and a `for: 5m`
clause that fires three minutes early because the test data was lumpier than expected.
Sonda gives you three generators that cover both cases deterministically.

| Pattern | Generator | When to reach for it |
|---------|-----------|----------------------|
| Crosses threshold predictably | `sine` | Verifying that the rule fires at all |
| Stays above threshold for an exact duration | `sequence` | Validating short `for:` clauses (≤ 30s) |
| Holds above threshold indefinitely | `constant` | Validating long `for:` clauses (minutes) |

## Threshold crossings with sine

The sine generator produces a smooth wave that crosses your threshold predictably.
With `amplitude=50` and `offset=50` it oscillates between 0 and 100, crossing 90 for
about 12 seconds per 60-second cycle -- enough to trigger a bare `> 90` rule on every
period.

```bash
sonda run examples/sine-threshold-test.yaml
```

```yaml title="examples/sine-threshold-test.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 180s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
    labels:
      instance: server-01
      job: node
```

The metric crosses 90 around tick 9, stays above until tick 21, then drops -- giving you
roughly 12 seconds above threshold per cycle.

??? info "Sine wave math"
    The formula is: `value = offset + amplitude * sin(2 * pi * tick / period_ticks)`

    With `amplitude=50` and `offset=50`, the threshold at 90 is crossed when `sin(x) > 0.8`:

    - `sin(x) = 0.8` at `x = arcsin(0.8) = 0.927 radians`
    - The sine exceeds 0.8 from `x = 0.927` to `x = pi - 0.927 = 2.214`
    - That's `1.287 / 6.283 = 20.5%` of each cycle
    - With a 60-second period: **~12.3 seconds above 90 per cycle**

    | Tick (sec) | sin(2*pi*t/60) | Value | Above 90? |
    |------------|----------------|-------|-----------|
    | 0  | 0.000  | 50.0  | No  |
    | 5  | 0.500  | 75.0  | No  |
    | 10 | 0.866  | 93.3  | Yes |
    | 15 | 1.000  | 100.0 | Yes |
    | 20 | 0.866  | 93.3  | Yes |
    | 25 | 0.500  | 75.0  | No  |

Sine works for unbounded threshold rules. For a `for:` clause you need the breach to
last an exact, predictable number of seconds.

## Exact `for:` durations with sequence

Prometheus alerts with a `for:` clause require the condition to be true for a
**continuous** duration before firing. The [sequence generator](../configuration/generators.md#sequence)
steps through an explicit list of values, one per tick, so you control the breach
window down to the second:

```bash
sonda run examples/for-duration-test.yaml
```

```yaml title="examples/for-duration-test.yaml"
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
  - signal_type: metrics
    name: cpu_usage
    generator:
      type: sequence
      values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
      repeat: true
    labels:
      instance: server-01
      job: node
```

At `rate: 1`, ticks 5-9 are above 90 -- exactly 5 seconds continuous breach -- then
the pattern repeats. To match a `for: 30s` alert, extend the run of `95`s to 30 entries.

!!! tip "When sequence stops being practical"
    Typing 300 values to satisfy a `for: 5m` alert is no fun. Past about 30 values,
    switch to the constant generator below and let the runtime duration do the work.

## Constant generator shortcut

For sustained-breach tests longer than ~30 seconds, the
[constant generator](../configuration/generators.md#constant) is more practical:

```bash
sonda run examples/constant-threshold-test.yaml
```

```yaml title="examples/constant-threshold-test.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 360s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
    labels:
      instance: server-01
      job: node
```

Run this for 6 minutes to test a `for: 5m` alert. The value stays at 95 for the entire
duration -- the alert should fire after 5 minutes of continuous breach.

## Next

You can trigger an alert. Now make sure it resolves cleanly when the breach ends.

[Continue to **Resolution and recovery** -->](alert-testing-resolution.md)
