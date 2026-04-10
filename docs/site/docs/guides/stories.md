# Stories

When real incidents happen, signals don't fire in isolation. A link goes down, traffic shifts
to a backup, and latency climbs as the backup saturates. Stories let you express that causal
chain in a single YAML file, with Sonda handling the timing math for you.

A story is a compilation layer on top of Sonda's existing scenario infrastructure. You write
signals with `after` clauses that describe temporal dependencies, and Sonda compiles them down
to concrete `phase_offset` values at parse time. No runtime reactivity -- just deterministic
timing based on each signal's behavior and parameters.

---

## Your first story

Here is a minimal story with two signals -- a flapping interface and a backup link that
saturates after the primary drops:

```yaml title="my-story.yaml"
story: link_failover
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }

signals:
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s

  - metric: backup_link_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 2m
    after: interface_oper_state < 1
```

Run it:

```bash
sonda story --file my-story.yaml
```

Sonda computes that the flap signal drops below 1 at `t=60s` (the `up_duration`), so
`backup_link_utilization` starts with a 60-second phase offset. You get correlated,
time-shifted signals from a single file.

Use `--dry-run` to inspect the compiled timing without emitting data:

```bash
sonda --dry-run story --file my-story.yaml
```

```text title="Output (excerpt)"
[config] [1/2] interface_oper_state

  name:          interface_oper_state
  signal:        metrics
  rate:          1/s
  duration:      5m
  generator:     sequence ([1, 1, ... 90 total], repeat)
  encoder:       prometheus_text
  sink:          stdout
  clock_group:   link_failover

---
[config] [2/2] backup_link_utilization

  name:          backup_link_utilization
  signal:        metrics
  rate:          1/s
  duration:      5m
  generator:     sawtooth (min: 20, max: 85, period: 120s)
  encoder:       prometheus_text
  sink:          stdout
  phase_offset:  1m
  clock_group:   link_failover
```

The `phase_offset: 1m` on the second signal is the compiled result of
`after: interface_oper_state < 1`.

---

## Story YAML format

### Top-level fields

| Field | Required | Description |
|-------|----------|-------------|
| `story` | yes | Identifier string. Used as the `clock_group` so all signals share a timeline. |
| `description` | no | Human-readable description (not displayed at runtime, useful for documentation). |
| `duration` | no | Shared duration for all signals (e.g., `5m`, `30s`). Per-signal overrides take precedence. |
| `rate` | no | Shared event rate in events/second. Default: `1`. |
| `encoder` | no | Shared encoder config. Default: `{ type: prometheus_text }`. |
| `sink` | no | Shared sink config. Default: `{ type: stdout }`. |
| `labels` | no | Labels applied to all signals. Per-signal labels merge in (and override on key conflict). |
| `signals` | yes | List of signal definitions. Must contain at least one signal. |

### Signal fields

| Field | Required | Description |
|-------|----------|-------------|
| `metric` | yes | Metric name for this signal. Must be unique within the story. |
| `behavior` | yes | Behavior alias (see [Supported behaviors](#supported-behaviors)). |
| `after` | no | Temporal dependency clause (see [The after clause](#the-after-clause)). |
| `labels` | no | Per-signal labels. Merged with story-level labels; signal wins on conflict. |
| `rate` | no | Per-signal rate override. |
| `duration` | no | Per-signal duration override. |
| `encoder` | no | Per-signal encoder override. |
| `sink` | no | Per-signal sink override. |
| *anything else* | no | Passed through as behavior-specific parameters (e.g., `baseline`, `ceiling`, `up_duration`). |

Behavior-specific parameters are written as flat keys alongside `metric` and `behavior` -- no
nested `generator:` block needed. Sonda collects any key that isn't a reserved field and passes
it to the generator.

---

## Supported behaviors

Stories use Sonda's operational vocabulary aliases. Each alias maps to a generator with
preconfigured semantics:

| Alias | Generator | Parameters | What it models |
|-------|-----------|------------|----------------|
| `flap` | sequence | `up_duration`, `down_duration`, `up_value` (default 1), `down_value` (default 0) | Binary state toggling (link up/down, service healthy/unhealthy) |
| `saturation` | sawtooth | `baseline`, `ceiling`, `time_to_saturate` | Resource climbing to capacity and repeating (bandwidth, CPU) |
| `leak` | sawtooth | `baseline`, `ceiling`, `time_to_ceiling` | One-shot resource exhaustion (memory leak, disk fill) |
| `degradation` | sawtooth + jitter | `baseline`, `ceiling`, `time_to_degrade` | Gradual performance decay with noise (latency, error rate) |
| `spike_event` | spike | `baseline`, `spike_height`, `spike_duration`, `spike_interval` | Periodic spikes above a baseline (CPU bursts, traffic surges) |
| `steady` | sine + jitter | `center`, `amplitude`, `period_secs`, `jitter` | Normal oscillating baseline |

!!! warning "`steady` cannot be used in `after` clauses"
    A `steady` signal (sine wave) crosses any threshold twice per period, making the crossing
    time ambiguous. You can use `steady` as a signal *behavior*, but you cannot reference a
    `steady` signal in another signal's `after` clause. If you need to sequence after a steady
    signal, use explicit `phase_offset` in a regular
    [scenario file](../configuration/scenario-file.md) instead.

---

## The `after` clause

The `after` clause expresses "this signal starts after the referenced signal crosses a
threshold." The syntax is:

```
after: <metric_name> <operator> <threshold>
```

Two operators are supported:

| Operator | Meaning | Typical use |
|----------|---------|-------------|
| `<` | Signal drops below threshold | `interface_oper_state < 1` -- "after the link goes down" |
| `>` | Signal rises above threshold | `backup_utilization > 70` -- "after backup hits 70%" |

### How timing is computed

Sonda resolves `after` clauses at compile time using deterministic formulas based on each
behavior's math:

- **flap**: `< threshold` resolves to `up_duration` (the moment the signal transitions from
  up to down).
- **saturation / leak / degradation**: `> threshold` uses linear interpolation --
  `(threshold - baseline) / (ceiling - baseline) * period`.
- **spike_event**: `< threshold` resolves to `spike_duration` (when the spike ends and the
  signal returns to baseline).

For transitive chains (A -> B -> C), offsets accumulate. If B depends on A with an offset of
60s, and C depends on B with an offset of 92s, then C's total offset is 152s.

!!! info "Compile-time, not runtime"
    The `after` clause is resolved once when Sonda parses the story file. It does not watch
    signal values at runtime. This means the timing is deterministic and reproducible, but
    it assumes the referenced signal's behavior follows its mathematical model exactly.

??? tip "Understanding the timing formulas"
    Each behavior has a known mathematical shape. Sonda inverts that shape to find when a
    threshold crossing occurs:

    **Flap** (sequence of up/down values):

    - `< threshold`: the signal drops at `t = up_duration` (when it transitions to `down_value`)
    - `> threshold`: generally ambiguous (signal starts at `up_value`), so this is rejected

    **Saturation / leak / degradation** (linear ramp from baseline to ceiling):

    - `> threshold`: `t = (threshold - baseline) / (ceiling - baseline) * period_secs`
    - `< threshold`: rejected (the ramp only goes up)

    **Spike** (baseline with periodic pulses):

    - `< threshold`: `t = spike_duration` (when the first spike ends)
    - `> threshold`: rejected (spike starts immediately at t=0)

### Validation rules

Sonda rejects stories with:

- **Unknown metric references** -- `after: nonexistent_metric < 1` fails with a clear error.
- **Circular dependencies** -- `A after B` and `B after A` is detected and reported.
- **Out-of-range thresholds** -- `after: utilization > 150` when ceiling is 85 tells you
  the signal never reaches that value.
- **Ambiguous crossings** -- conditions satisfied at t=0 (e.g., `> 0` on a signal that
  starts at 1) are rejected.
- **Unsupported behaviors** -- referencing a `steady` signal in `after` is rejected with
  an explanation.

---

## CLI usage

```
sonda story --file <path> [--duration <d>] [--rate <r>] [--sink <type>] [--endpoint <url>] [--encoder <enc>]
```

| Flag | Description |
|------|-------------|
| `--file <path>` | **(required)** Path to the story YAML file. |
| `--duration <d>` | Override the story-level duration for all signals (e.g., `2m`, `30s`). |
| `--rate <r>` | Override the story-level event rate (events/second). |
| `--sink <type>` | Override the story-level sink (e.g., `stdout`, `http_push`, `remote_write`). |
| `--endpoint <url>` | Set the sink endpoint (required for network sinks like `http_push`). |
| `--encoder <enc>` | Override the story-level encoder (e.g., `prometheus_text`, `json_lines`). |

CLI flags override story-level shared fields. Per-signal overrides defined in the YAML
take precedence over both.

Global flags (`--dry-run`, `--quiet`, `--verbose`) work as usual:

```bash
sonda --dry-run story --file stories/link-failover.yaml
sonda -q story --file stories/link-failover.yaml
```

### Sending to a backend

Override the sink to push story output to Prometheus, VictoriaMetrics, or any HTTP endpoint:

```bash
sonda story --file stories/link-failover.yaml \
  --sink remote_write \
  --endpoint http://localhost:8428/api/v1/write \
  --encoder remote_write
```

---

## Worked example: link failover

The included `stories/link-failover.yaml` models an edge router primary link failure with
automatic traffic shift to a backup path. Here is the full file:

```yaml title="stories/link-failover.yaml"
story: link_failover
description: "Edge router link failure with traffic shift to backup"
duration: 5m
rate: 1
encoder: { type: prometheus_text }
sink: { type: stdout }
labels:
  device: rtr-edge-01
  job: network

signals:
  # Primary interface flaps: up for 60s, down for 30s, cycling.
  - metric: interface_oper_state
    behavior: flap
    up_duration: 60s
    down_duration: 30s
    labels:
      interface: GigabitEthernet0/0/0

  # Backup link saturates after primary drops below 1.
  # Ramps from 20% to 85% utilization over 2 minutes.
  - metric: backup_link_utilization
    behavior: saturation
    baseline: 20
    ceiling: 85
    time_to_saturate: 2m
    after: interface_oper_state < 1
    labels:
      interface: GigabitEthernet0/1/0

  # Latency degrades after backup utilization exceeds 70%.
  # Climbs from 5ms to 150ms over 3 minutes with noise.
  - metric: latency_ms
    behavior: degradation
    baseline: 5
    ceiling: 150
    time_to_degrade: 3m
    after: backup_link_utilization > 70
    labels:
      path: backup
```

### The causal chain

Three signals form a dependency chain:

1. **`interface_oper_state`** -- starts immediately. Flaps between 1 (up) and 0 (down) on a
   60s up / 30s down cycle.
2. **`backup_link_utilization`** -- starts after `interface_oper_state < 1`. The flap drops to
   0 at `t=60s`, so this signal begins at `phase_offset: 1m`. It ramps from 20% to 85% over 2
   minutes.
3. **`latency_ms`** -- starts after `backup_link_utilization > 70`. Linear interpolation:
   `(70 - 20) / (85 - 20) * 120s = 92.3s` after the backup starts. Total offset:
   `60s + 92.3s = 152.3s`. Latency climbs from 5ms to 150ms over 3 minutes with jitter.

The `--dry-run` output confirms these compiled offsets:

```text
[config] [2/3] backup_link_utilization
  phase_offset:  1m

[config] [3/3] latency_ms
  phase_offset:  152.308s
```

### Timeline visualization

```
t=0s         t=60s        t=152s       t=300s
|            |            |            |
|-- oper_state: up=1 ---->|-- down=0 --|-- up=1 --> ...
             |-- backup: ramps 20->85% over 2m ---->
                          |-- latency: ramps 5->150ms over 3m -->
```

---

## Limitations

- **Metrics only** -- stories currently compile to `signal_type: metrics`. Log, histogram, and
  summary signals are not yet supported.
- **No runtime reactivity** -- `after` clauses resolve at compile time. The timing is
  deterministic but does not react to actual signal values during execution.
- **No `steady` in `after`** -- sine waves cross thresholds twice per period, making the
  crossing time ambiguous. Use a different behavior or explicit `phase_offset` in a regular
  scenario file.
- **Single dependency per signal** -- each signal can have at most one `after` clause.
  Transitive chains (A -> B -> C) work, but a signal cannot depend on multiple predecessors.
- **Unique metric names** -- every signal in a story must have a unique `metric` name, since
  that name is used for `after` references.

## What next

- [**Built-in Scenarios**](scenarios.md) -- pre-built single-signal patterns you can run instantly
- [**Scenario Files**](../configuration/scenario-file.md) -- full YAML reference for scenario fields including `phase_offset` and `clock_group`
- [**CLI Reference**](../configuration/cli-reference.md) -- every flag for all subcommands
- [**Alert Testing**](alert-testing.md) -- use shaped signals to validate alert rules end-to-end
