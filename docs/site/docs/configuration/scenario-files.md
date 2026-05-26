# Scenario Files

The scenario file format is Sonda's way to describe one or many signals in a single YAML file. One top-level block declares shared defaults; another lists the scenarios. Packs, `after:` temporal dependencies, and clock groups all compose inside the same file.

Every scenario file declares two top-level fields:

- **`version: 2`** — the format version. Always `2`.
- **`kind: runnable`** — a file you can run with `sonda run`. Use `kind: composable` for files that define a [metric pack](../guides/metric-packs.md) used by other scenarios.

Everything else you already know about scenarios — generators, encoders, sinks, labels — still applies.

## Minimal example

```yaml title="hello.yaml"
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
  - id: cpu
    signal_type: metrics
    name: demo_cpu
    generator:
      type: constant
      value: 42.0
```

Run it like any other scenario file:

```bash
sonda run hello.yaml
```

```text title="Output"
▶ demo_cpu  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 30s
demo_cpu 42 1776090151609
demo_cpu 42 1776090152619
...
■ demo_cpu  completed in 30.0s | events: 30 | bytes: 680 B | errors: 0
━━ run complete  scenarios: 1 | events: 30 | bytes: 680 B | errors: 0 | elapsed: 30.1s
```

## What this format gives you

- **Shared defaults** -- put `rate`, `duration`, `encoder`, `sink`, and `labels` in one place.
- **Temporal chains** -- link scenarios with `after:` clauses (`this starts when that crosses a threshold`).
- **Automatic clock groups** -- scenarios linked by `after:` share a clock so they stay in sync.
- **Pack-backed entries** -- reference a [metric pack](../guides/metric-packs.md) inline, alongside regular metrics and logs.
- **Richer validation** -- the compiler catches missing fields, cycles in `after:` chains, and
  pack references to unknown names at parse time.

## Catalog metadata

A scenario file can carry optional top-level metadata that powers the catalog views --
`sonda list` and `sonda show` against a `--catalog <dir>`. The fields sit at the root,
alongside `version:`, `kind:`, and `defaults:`:

```yaml title="my-catalog/steady-state.yaml"
version: 2
kind: runnable

name: steady-state
tags: [infrastructure, baseline]
description: "Normal oscillating baseline (sine + jitter)"

scenarios:
  - signal_type: metrics
    name: node_cpu_usage_idle_percent
    rate: 1
    duration: 60s
    generator:
      type: steady
      center: 75.0
      amplitude: 10.0
      period: "60s"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

| Field | Required | Description |
|-------|----------|-------------|
| `kind` | yes | `runnable` for runnable scenarios; `composable` for packs. Required at the top level of every scenario file. |
| `name` | no | Catalog identifier (kebab-case). Defaults to the filename (without `.yaml`, hyphens preserved) if omitted. Used by `@name` shorthand and `sonda run @name`. When posted to a running `sonda-server`, this field also acts as a uniqueness key — POSTing two cascades that share an active `name` returns [`409 Conflict`](../deployment/sonda-server.md#duplicate-scenario_name-returns-409). |
| `tags` | no | List of strings shown in the catalog table and filterable via `sonda list --tag <t>`. |
| `description` | no | One-line summary shown in the catalog table and JSON output. Keep it under ~60 characters so it fits the table column. |

The compiler ignores `tags:` and `description:` — they only feed the catalog. `deny_unknown_fields`
stays in force, so typos like `tag:` (singular) or `desc:` are rejected at parse time.

Drop a scenario file into your catalog directory and it shows up immediately:

```bash
sonda --catalog ./my-catalog list --tag infrastructure
```

```text
KIND        NAME           TAGS                      DESCRIPTION
runnable    steady-state   infrastructure,baseline   Normal oscillating baseline (sine + jitter)
...
```

## Environment variable interpolation

`${VAR}` and `${VAR:-default}` references in a scenario file are substituted from
the process environment before parsing. One file runs from your host CLI on the
defaults and from a containerized `sonda-server` on the overrides -- no edit, no
`sed` rewrite.

```yaml title="loki-json-lines.yaml (excerpt)"
defaults:
  sink:
    type: loki
    url: "${LOKI_URL:-http://localhost:3100}"
```

```bash
# Host CLI -- LOKI_URL unset, default wins
sonda run examples/loki-json-lines.yaml --duration 1s --dry-run

# Override -- every ${LOKI_URL} resolves to this value
LOKI_URL=http://loki:3100 sonda run examples/loki-json-lines.yaml --duration 1s --dry-run
```

### Syntax

| Form | Meaning |
|---|---|
| `${VAR}` | Required. Compile fails if `VAR` is unset. |
| `${VAR:-default}` | Optional. Default text runs to the next unescaped `}`; may contain `:`, `/`, `?`, `&`, `=`. |
| `$$` | Literal `$`. The only escape. |

Variable names match `[A-Z_][A-Z0-9_]*`. Lowercase, leading digits, or other
characters are rejected at compile time so a typo cannot silently swallow a YAML field.

!!! note "Not supported"
    No recursion into substituted values, no nested defaults (`${A:-${B}}`), no
    bare `$VAR`, no `:?` / `:+` / `:=`.

### Built-in example variables

Every scenario under `examples/` honours these names. Defaults map to the
host-published Compose ports; the bundled `examples/docker-compose-victoriametrics.yml`
exports the in-network values so POSTing an example scenario to the
containerized `sonda-server` works untouched. See
[Endpoints & networking](../deployment/endpoints.md) for the full
host-vs-container resolution table.

| Variable | Default (host CLI) | In-network override |
|---|---|---|
| `VICTORIAMETRICS_URL` | `http://localhost:8428/api/v1/import/prometheus` | `http://victoriametrics:8428/api/v1/import/prometheus` |
| `VICTORIAMETRICS_REMOTE_WRITE_URL` | `http://localhost:8428/api/v1/write` | `http://victoriametrics:8428/api/v1/write` |
| `VMAGENT_URL` | `http://localhost:8429/api/v1/write` | `http://vmagent:8429/api/v1/write` |
| `PROMETHEUS_URL` | `http://localhost:9090/api/v1/write` | `http://prometheus:9090/api/v1/write` |
| `LOKI_URL` | `http://localhost:3100` | `http://loki:3100` |
| `KAFKA_BROKERS` | `localhost:9094` | `kafka:9092` |
| `OTLP_GRPC_ENDPOINT` | `http://localhost:4317` | `http://otel-collector:4317` |

## The `defaults:` block

Every field in `defaults:` applies to every entry in `scenarios:` unless the entry overrides it.
This is the main ergonomic win over legacy multi-scenario files, where you typed the same
`encoder:`, `sink:`, and `rate:` for every entry.

```yaml title="defaults-example.yaml"
version: 2
kind: runnable

defaults:
  rate: 10
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    job: sonda
    env: staging

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50

  - id: mem
    signal_type: metrics
    name: mem_usage
    rate: 1                       # override just the rate for this one
    generator:
      type: leak
      baseline: 40
      ceiling: 90
      time_to_ceiling: 2m
```

The `mem` entry overrides `rate` only; everything else comes from `defaults:`.

### Sink-error policy

`on_sink_error` controls what the runner does when the sink returns an error mid-run -- a Loki `500`, a TCP reset, an HTTP timeout. Two values:

| Value | Behavior |
|---|---|
| `warn` (default) | The runner logs to stderr (rate-limited, one line per minute per scenario with a count), increments error counters in [`/stats`](../deployment/sonda-server.md#self-observability-via-stats), drops the failed batch, and keeps ticking. The scenario stays alive while the sink recovers. |
| `fail` | The runner propagates the error and the scenario thread exits. Use this when any sink failure should hard-fail a CI run. |

Set the policy at `defaults:` to apply it to every entry, or per-entry to override one scenario in a mixed run:

```yaml title="sink-error-policy.yaml"
version: 2
kind: runnable

defaults:
  rate: 100
  duration: 30s
  on_sink_error: warn          # default; written here for clarity
  encoder:
    type: prometheus_text
  sink:
    type: loki
    url: ${LOKI_URL:-http://localhost:3100}

scenarios:
  - id: noisy_logs
    signal_type: logs
    name: noisy_logs
    log_generator:
      type: template
      templates:
        - message: "request handled"

  - id: canary
    signal_type: metrics
    name: canary
    on_sink_error: fail        # this one MUST hard-fail on sink errors
    generator:
      type: constant
      value: 1
```

The `noisy_logs` entry tolerates Loki blips. The `canary` entry treats any sink error as fatal -- useful when you want a CI run to fail the moment delivery breaks.

!!! tip "When to pick `fail`"
    Pick `fail` when sink delivery is itself the contract under test -- CI gates that compare metric counts against an expected total, or smoke tests that should abort the moment the backend goes away. Pick `warn` (the default) for everything else: long-running fleets, demo environments, or any scenario where you want runtime self-observability via [`/stats`](../deployment/sonda-server.md#self-observability-via-stats) instead of thread death.

`--on-sink-error <warn|fail>` on `sonda run` overrides `defaults.on_sink_error` for one-off invocations -- handy when you want to point a single CI run at a YAML that defaults to `warn`. See [CLI Reference](cli-reference.md#sonda-run).

## Temporal chains with `after:`

Use `after:` to express "this scenario starts when that one crosses a threshold". Sonda resolves
the timing at parse time -- no runtime reactivity -- by computing concrete `phase_offset` values
from each generator's shape.

The built-in `link-failover` scenario is a worked example: a primary interface flaps, a backup
link saturates once the primary drops, and latency degrades once the backup fills.

```yaml title="link-failover.yaml"
version: 2
kind: runnable

name: link-failover
tags: [network]
description: "Edge router link failure with traffic shift to backup"

defaults:
  rate: 1
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    device: rtr-edge-01
    job: network

scenarios:
  - id: interface_oper_state
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s
    labels:
      interface: GigabitEthernet0/0/0

  - id: backup_link_utilization
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    labels:
      interface: GigabitEthernet0/1/0
    after:
      ref: interface_oper_state
      op: "<"
      value: 1

  - id: latency_ms
    signal_type: metrics
    name: latency_ms
    generator:
      type: degradation
      baseline: 5
      ceiling: 150
      time_to_degrade: 3m
    labels:
      path: backup
    after:
      ref: backup_link_utilization
      op: ">"
      value: 70
```

The `interface_oper_state` flap signal drops below `1` at `t=60s` (its `up_duration`), so
`backup_link_utilization` starts at that same moment via an auto-computed `phase_offset`. The
backup ramp crosses 70% a little over two minutes later, which is when `latency_ms` begins to
degrade. Scenarios linked by `after:` get an auto-assigned `clock_group` so their timers share a
start reference.

An `after:`-gated entry must declare a `duration:` -- on the entry itself or via `defaults.duration` -- and is rejected at compile time without one. `after:` holds the scenario in `pending` until its upstream crosses the threshold; if that crossing never happens, a scenario with no `duration:` would have no terminal point and sit `pending` for the lifetime of the run.

Use `--dry-run` to see the resolved timing:

```bash
sonda --dry-run run link-failover.yaml
```

```text
[config] file: link-failover.yaml (version: 2, 3 scenarios)

[config] [1/3] interface_oper_state

    name:           interface_oper_state
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      flap (up_duration: 60s, down_duration: 30s, up_value: 1, down_value: 0)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, interface=GigabitEthernet0/0/0, job=network
    clock_group:    chain_backup_link_utilization (auto)
---

[config] [2/3] backup_link_utilization

    name:           backup_link_utilization
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      saturation (baseline: 20, ceiling: 85, time_to_saturate: 2m)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, interface=GigabitEthernet0/1/0, job=network
    phase_offset:   1m
    clock_group:    chain_backup_link_utilization (auto)
---

[config] [3/3] latency_ms

    name:           latency_ms
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      degradation (baseline: 5, ceiling: 150, time_to_degrade: 3m)
    encoder:        prometheus_text
    sink:           stdout
    labels:         device=rtr-edge-01, job=network, path=backup
    phase_offset:   152.308s
    clock_group:    chain_backup_link_utilization (auto)

Validation: OK (3 scenarios)
```

The `phase_offset` lines show the concrete delays Sonda computed from the threshold-crossing
math. The `(auto)` suffix on `clock_group` indicates Sonda assigned the group automatically
because of the `after:` relationship.

!!! info "Duration is per-entry, not per-cascade"
    Each entry's `duration:` runs from that entry's own resolved start time (`phase_offset`), not from cascade registration. The cascade's total wall-clock is therefore `max(phase_offset + duration)` across all entries, which can exceed `defaults.duration`.

    For the `link-failover` example above, `defaults.duration` is `5m` and the largest `phase_offset` is `152.308s` (on `latency_ms`), so the chain finishes at roughly `152.308s + 5m ≈ 7m32s` of wall-clock — not 5m.

    The CLI `--duration` flag (and the body-level `duration` field on `POST /scenarios`) shorten **every entry's `duration` individually**; they do not cap the cascade's total wall-clock. Running the same chain with `--duration 2m` produces `152.308s + 2m ≈ 4m32s`, because every entry now runs for 2m from its own start.

Run it from your catalog the same way as any other scenario — save the YAML under
`my-catalog/link-failover.yaml`, then:

```bash
sonda --catalog ./my-catalog run @link-failover
```

!!! tip "Supported generators in `after:`"
    The `after:` clause resolves against the target generator's trajectory. Supported shapes:
    `flap`, `saturation`, `leak`, `degradation`, `spike_event`. Using `steady` as the target is
    rejected -- sine crossings are ambiguous.

For continuous gating that pauses and resumes a downstream as the upstream's value oscillates above and below a threshold, see [Continuous coupling with `while:`](#continuous-coupling-with-while).

## Continuous coupling with `while:`

`after:` is a one-shot trigger -- it fires once and the dependent scenario runs to completion. `while:` is the continuous-coupling counterpart: the gated scenario emits only while the upstream's latest value satisfies the predicate, pauses when the predicate fails, and resumes when the predicate becomes true again. A `while:`-gated entry must declare a `duration:` -- on the entry itself or via `defaults.duration` -- and is rejected at compile time without one, because the `paused` state needs a terminal point to bound the scenario's lifetime. Use `while:` when an event stream should track an upstream signal's lifecycle, not just its first crossing. When a `while:`-gated scenario pauses, downstream alerts on its metrics keep firing for ~5 minutes by default — see [Recovering Prometheus alerts on gate close](#recovering-prometheus-alerts-on-gate-close) for the stale-marker default that resolves them immediately. The upstream signal lives in the same scenario file by default; for [`sonda-server`](../deployment/sonda-server.md) deployments where the upstream arrives in a separate POST body, see [Cross-POST `while:` refs](#cross-post-while-refs).

```yaml title="link-traffic.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: backup_traffic
    signal_type: metrics
    name: backup_link_throughput
    generator:
      type: constant
      value: 50.0
    while:
      ref: primary_link
      op: "<"
      value: 1
```

`backup_traffic` emits only while `primary_link` reports a value below `1` -- in other words, while the primary link is down. When the primary flaps back up, the gate closes and `backup_traffic` pauses; when the primary drops again, the gate reopens and emission resumes. The schedule is debounced via the optional `delay:` clause shown below.

### Lifecycle states

A scenario carrying a `while:` clause walks through four lifecycle states. The runtime exposes the live state on `GET /scenarios/{id}/stats` so monitors can react to gate transitions without polling the upstream signal.

```
            +-----------+
            |  pending  |
            +-----+-----+
                  | upstream's first eligible tick
                  v
       +------+--------+   close transition
       |              |    +------------+
       |   running    |--->|   paused   |
       |              |<---|            |
       +------+-------+    +------------+
              |  open transition
              | duration elapsed / shutdown
              v
        +-----+-----+
        |  finished |
        +-----------+
```

`pending` covers the wait for the upstream's first eligible tick. The downstream enters `running` when the gate first opens, oscillates between `running` and `paused` for the rest of the run, and ends in `finished` when its `duration:` elapses or shutdown is signaled. A scenario with both `after:` and `while:` whose `after:` fires while the gate is closed enters `paused` directly -- `pending` need not always precede `running`.

### Debouncing transitions with `delay:`

Pair `while:` with `delay:` to debounce noisy upstream signals.

```yaml
  - id: backup_traffic
    signal_type: metrics
    name: backup_link_throughput
    generator:
      type: constant
      value: 50.0
    while:
      ref: primary_link
      op: "<"
      value: 1
    delay:
      open: 250ms
      close: 1s
```

`open` is the duration the upstream value must satisfy the predicate before the gate transitions from closed to open; `close` is the duration the value must violate the predicate before the gate transitions back to closed. Either field defaults to `0s` when omitted. `delay:` requires `while:` -- standalone `delay:` is rejected at compile time. The `close:` field also accepts an extended struct form for stale-marker control; see [Recovering Prometheus alerts on gate close](#recovering-prometheus-alerts-on-gate-close) below.

### Recovering Prometheus alerts on gate close

Prometheus retains the last-known sample for the lookback-delta window (default 5 minutes) when a series stops emitting. A `while:`-gated downstream that reports `bgp_oper_state=2` ("down") and then pauses keeps the alert firing for that window because Prometheus has no signal that the source is stale. Sonda emits a Prometheus stale-marker sample for every recently-active `(metric_name, label_set)` tuple whenever a `while:`-gated entry exits the `running` state with the gate open — on committed `running → paused` gate-close transitions, and on `running → finished` exits via `duration:` expiry or user-initiated shutdown — when the sink is `remote_write`. Downstream alerts resolve immediately on the next scrape cycle.

The marker is on by default for `remote_write` sinks. The shorthand `close: 5s` keeps working unchanged. The extended form lets you override the stale marker with a literal recovery sample, or opt out of the default emit entirely. The two knobs are mutually exclusive — pick one.

```yaml title="Override the stale marker with a recovery value"
  - id: bgp_oper_state
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_link
      op: "<"
      value: 1
    delay:
      open: 250ms
      close:
        duration: 5s
        snap_to: 1            # emit one literal sample with this value before pausing
```

`snap_to` replaces the stale marker with a normal sample carrying the supplied value. Use it when the recovery semantics call for an explicit recovered value (`bgp_oper_state=1` for "up") rather than a stale signal. When set, `snap_to` is honored on every sink the configured encoder can serialize to — including `stdout`, `file`, `loki`, and `kafka`. Without `snap_to`, only `remote_write` sinks emit a default close marker; the others stay silent on close. Setting `stale_marker: true` on a non-`remote_write` sink is rejected at normalize time, because the marker is a Prometheus remote-write-specific signal that would otherwise no-op silently — either switch to `sink.type: remote_write`, replace it with `snap_to:` for a sink-agnostic recovery sample, or drop the field. The exception is when `snap_to` is set on the same `close:` block: an explicit `stale_marker: true` paired with `snap_to` is accepted because `snap_to` already wins.

To suppress the default emit entirely, set `stale_marker: false` instead:

```yaml title="Disable the default stale-marker emit"
    delay:
      open: 250ms
      close:
        duration: 5s
        stale_marker: false
```

Setting both `snap_to` and `stale_marker: false` is a config error — `snap_to` already replaces the stale marker, so the explicit `false` is redundant and likely a mistake. Setting `snap_to` alongside the implicit `stale_marker: true` default lets `snap_to` win silently.

A brief close that gets cancelled by a fresh `WhileOpen` arriving inside the `delay.close.duration` debounce window emits nothing — the gate stayed open. A scenario that hits its `duration:` while already paused goes `paused → finished` without an additional close-emit; the recovery markers were already written on the earlier `running → paused` transition. Every distinct `(name, labels)` series active since the last gate close receives exactly one recovery marker on close, with no cardinality ceiling — high-cardinality scenarios recover the same as any other.

Close-emit timestamps are strictly greater than the most recent active-emission timestamp for the same series. This avoids duplicate-timestamp rejection at the receiver — Prometheus and other TSDBs that dedup on `(series, timestamp)` would otherwise drop the recovery sample silently.

Gate-close emission is the active recovery primitive — Sonda writes a recovery sample (or stale marker) the moment a gate closes. For the passive counterpart, where Prometheus resolves the alert on its own once a metric stops emitting for the lookback-delta window, see [Alert resolution via gaps](../guides/alert-testing-resolution.md). The two compose: gaps drive absence-based recovery on any sink, gate close drives marker-based recovery on `remote_write`.

#### Prefer `snap_to:` for `remote_write` integrations

The default stale-marker behavior depends on how the receiving Prometheus handles stale-NaN samples ingested via remote-write. Some Prometheus configurations accept the marker into TSDB but do not propagate stale-marker semantics through the query engine for remote-write-ingested samples the way they do for scraped samples. The series stays "live" with the pre-pause value until natural `query.lookback-delta` expiry (around 5 minutes by default), and the alert clearance you expect on the next scrape cycle does not happen.

If your alerts are not clearing as expected on gate close, prefer `snap_to:` with an explicit recovery value over the default stale marker. The recovery sample is a normal Prometheus sample — stored and queried with consistent semantics across receiver versions and configurations — so the next evaluation sees the recovered value directly and clears the alert immediately.

```yaml title="Recommended for remote_write: explicit recovery value"
    delay:
      open: 250ms
      close:
        duration: 5s
        snap_to: 1            # bgp_oper_state=1 means "up"
```

The tradeoff is per-metric specificity: `snap_to:` requires you to choose a recovery value for each gated metric, which is reasonable for operator-facing signals where "healthy" has an obvious value (`bgp_oper_state=1`, `interface_oper_state=1`, error counters back to `0`). The default `stale_marker` is generic and needs no per-metric tuning, but its end-to-end effect is receiver-dependent. For pipelines that ingest into Prometheus via `remote_write` and drive alerting off the result, `snap_to:` is the more reliable default.

### Combining with `after:`

`after:` and `while:` compose on the same entry. `after:` defers the scenario's first emission until an upstream crosses a threshold; `while:` then continuously gates the entry on every later edge. Pair them when a downstream should wait for a triggering event AND track the upstream's state thereafter -- a BGP session that opens once a link drops, then pauses every time the link briefly recovers.

```yaml title="scenarios/bgp-session-cascade.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: bgp_session
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 1
    after:
      ref: primary_link
      op: "<"
      value: 1
    while:
      ref: primary_link
      op: "<"
      value: 1
```

`bgp_session` stays `pending` until `after:` fires the first time `primary_link` drops below `1`. From that moment on `while:` takes over: the gate opens whenever the link is down, pauses when the link flaps back up, and reopens on the next drop. A scenario that uses both clauses with the gate already closed when `after:` fires enters `paused` directly -- the lifecycle skips `running` until the next gate-open edge.

The two clauses may also reference different upstreams (e.g. `after:` on a link event, `while:` on a separate health signal); the compiler tracks the dependency graph for both edges independently.

### `--dry-run` preview

`sonda --dry-run run link-traffic.yaml` renders the gate plumbing alongside the existing layout:

```
[config] [2/2] backup_link_throughput

    name:           backup_link_throughput
    signal:         metrics
    rate:           1/s
    duration:       5m
    generator:      constant (value: 50)
    encoder:        prometheus_text
    sink:           stdout
    while:          upstream='primary_link' op='<' value=1
    first_open:     ~60s
```

The `first_open:` line shows the analytical time at which the upstream's value first satisfies the predicate, computed from the upstream generator's shape. When the upstream's generator is non-analytical (`sine`, `uniform`, `csv_replay`, `steady`), `first_open` renders as `<indeterminate -- non-analytical generator>` -- the gate still works at runtime, but no compile-time crossing time is available.

When an entry carries both `after:` and `while:` against different upstreams, both cues render side by side: `after_first_fire: <duration> (ref: <upstream_id>)` shows when the `after:` clause fires, while `first_open:` shows the time the `while:` gate first opens. Operators read `max(after_first_fire, first_open)` to know when the downstream first emits. When `after:` and `while:` share the same upstream they collapse into a single `phase_offset:` line.

### Supported operators

`while:` accepts only the strict comparison operators `<` and `>`. Non-strict operators (`<=`, `>=`, `==`, `!=`) are rejected at compile time -- equality on a continuous-valued upstream is numerically unsafe and forbidden by design.

### Value typing

`while.value` accepts either an integer or a float YAML scalar; both are stored as `f64`. `value: 1` and `value: 1.0` are equivalent. Prefer `1.0` (or any explicit decimal form) in scenario files so the YAML reader carries the operator's intent without relying on integer coercion. NaN and infinity (`.nan`, `.inf`, `-.inf`) are rejected at compile time because the strict comparison gate would never resolve deterministically; the same rejection applies to `delay.close.snap_to`.

### Migrating an `after:`-only cascade to `while:` with recovery

The `link-failover` scenario described above uses `after:` to start a `backup_link_utilization` saturation curve once the primary link drops below `1`. With `after:` the dependent scenario runs to completion regardless of what the primary does next -- if the primary recovers mid-cascade, the backup keeps emitting.

To make the backup track the primary's state continuously, swap `after:` for `while:` on the cascade members that should pause when the primary recovers:

```yaml title="link-failover-recovery.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 5m
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: backup_util
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    while:
      ref: primary_link
      op: "<"
      value: 1
    delay:
      close: 5s
```

The `delay.close: 5s` debounces flap transitions: a brief recovery on the primary does not immediately tear down `backup_util`, but a sustained recovery longer than 5s does.

### Cross-POST `while:` refs

The `while:` clause shown above gates a scenario on another signal *in the same file*. When you drive Sonda via [`sonda-server`](../deployment/sonda-server.md), you can also gate on a signal from a **separately POSTed body** by qualifying the `ref:` with the upstream's `scenario_name:`. This lets one process drive multiple independent POSTs whose lifecycles are loosely coupled — a baseline counter you launch at boot, paused or resumed by a cascade body you POST on demand later — without restarting either side or rewriting the YAML when a new upstream arrives.

Cross-POST refs are a `sonda-server` feature. The CLI runs every scenario in a single process and has no cross-POST registry, so `sonda run` rejects a file with `while.scenario_name` at parse time and points here.

#### Schema

A cross-POST `while:` clause is the same shape as the local form plus two fields:

```yaml
while:
  scenario_name: <upstream-body-name>     # which POST body owns the upstream signal
  ref: <entry-id>                         # entry id INSIDE that body
  op: ">"                                 # same operators as local while: (< or >)
  value: 1                                # threshold
  if_unresolved: open                     # behavior when the upstream is not running yet
```

Both POST bodies — the gated one and the gate source — MUST declare a top-level `scenario_name:` field. The upstream body's `scenario_name:` is the identifier the downstream's `while.scenario_name` references. A body without a top-level `scenario_name:` is anonymous and cannot be addressed by another body's `while:` clause; the compiler rejects a cross-POST `while:` on a body that does not name itself.

`ref:` must be the top-level entry `id:` of an entry in the upstream body. Pack sub-signal syntax (`ref: my_entry.metric_name`) is rejected at parse time on cross-POST refs — use the top-level entry id.

#### `if_unresolved:` modes

`if_unresolved:` controls what the downstream does when its named upstream has not yet POSTed. Three values:

| Value | Behavior |
|---|---|
| `open` | Emit at full rate as if the gate were true. Events flow at the configured rate; the scenario sits in `unresolved` (the resolution-status state) until the upstream POSTs and the resolver wires the subscription. Use this when the downstream is a baseline counter you want running unless the cascade gates it. |
| `closed` | Pause emission as if the gate were false. The scenario sits in `unresolved`, no events are emitted. |
| `pending` (default) | Neither emit nor advance state. The scenario sits in `unresolved` until the upstream POSTs. This is the conservative default — no traffic until the dependency is satisfied. |

Once the upstream IS running, the downstream behaves like a local `while:` clause: open when the predicate is true, paused (with `delay:` debounce, recovery markers, etc. — see [Continuous coupling with `while:`](#continuous-coupling-with-while)) when the predicate is false.

#### Lifecycle

A cross-POST-gated scenario adds one state to the `while:` lifecycle: **`unresolved`**, used while waiting for the named upstream to register. The scenario moves through the lifecycle like this:

- **Initial POST**: the downstream lands in `unresolved`. Its [`pending_ref` field on `GET /scenarios/{id}`](../deployment/sonda-server.md#pending_ref-field-on-get-scenariosid) shows the upstream it is waiting on.
- **Upstream POSTs**: the downstream resolves automatically — the server's resolver wires the subscription and the downstream transitions to `running` (or to `paused` if the predicate is already false). No client orchestration needed.
- **Upstream is DELETEd, or finishes its duration**: the downstream transitions back to `unresolved` and applies the `if_unresolved:` mode again — `open` keeps it emitting, `closed` pauses, `pending` halts.
- **A new POST arrives with the same `scenario_name:`**: the downstream re-resolves to the new upstream automatically. The downstream keeps its accumulated state across the gap — counters preserve their value through pause/resume cycles.

#### Example: baseline counter gated by a cascade

A baseline counter that you want running at full rate by default, but paused whenever an on-demand cascade declares a "link state" of 0:

```yaml title="baseline.yaml — POST first, runs immediately under if_unresolved: open"
version: 2
kind: runnable
scenario_name: baseline_post
defaults:
  rate: 100
  duration: 1h
  encoder:
    type: prometheus_text
  sink:
    type: remote_write
    url: ${VICTORIAMETRICS_REMOTE_WRITE_URL:-http://localhost:8428/api/v1/write}
scenarios:
  - id: requests_total
    signal_type: metrics
    name: requests_total
    generator:
      type: step
      start: 0
      step_size: 1
    while:
      scenario_name: cascade_post
      ref: link_state
      op: ">"
      value: 0
      if_unresolved: open
```

```yaml title="cascade.yaml — POST later to gate the baseline"
version: 2
kind: runnable
scenario_name: cascade_post
defaults:
  rate: 1
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: link_state
    signal_type: metrics
    name: link_state
    generator:
      type: flap
      up_duration: 10s
      down_duration: 5s
```

POST `baseline.yaml` first. Because the cascade has not arrived, `if_unresolved: open` keeps the counter emitting at the full 100/s rate. Later, POST `cascade.yaml` — the baseline resolves to the cascade automatically. The counter now emits only while `link_state > 0` and pauses while it is `0`. When the cascade finishes (30s) or you DELETE it, the baseline returns to `if_unresolved: open` and resumes the unpaused 100/s rate. POST the cascade again and the baseline re-resolves; the counter picks up from where it froze, so its values stay monotonic across pause/resume cycles.

For the HTTP surface — the strict-validation flag, the `pending_ref` field, the duplicate-name 409, and the new `/stats` fields — see [Server API](../deployment/sonda-server.md#cross-post-while-refs).

## Pack-backed entries

Reference a [metric pack](../guides/metric-packs.md) directly from a scenarios entry. Sonda
expands the pack at compile time -- you get one prepared scenario per metric in the pack.

```yaml title="snmp-interface.yaml"
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
  - id: edge_router_snmp
    signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
```

The `pack:` field replaces the `name:` + `generator:` combo. Any fields you set on the entry
(`labels`, `rate`, `duration`, `encoder`, `sink`) apply to every expanded metric.

## Clock groups

Scenarios with the same `clock_group` share a start-time reference, which keeps multi-signal
scenarios phase-aligned. You can set `clock_group:` explicitly:

```yaml
scenarios:
  - id: a
    clock_group: incident-1
    ...
  - id: b
    clock_group: incident-1
    ...
```

Or let Sonda auto-assign one when you use `after:` -- every scenario in the same `after:` chain
ends up in the same auto-named group (`chain_<head>`).

The start banner and the grouped run summary both show the `clock_group`. See
[CLI Reference -- Status output](cli-reference.md#status-output) for what the banners look
like at runtime.

## Scaffolding new files with `sonda new`

`sonda new` emits a scenario YAML file. The fastest path is `--template`, which prints a minimal runnable
file and exits with no prompts:

```bash
sonda new --template -o ./my-catalog/demo.yaml
```

```yaml title="./my-catalog/demo.yaml"
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

Drop the `--template` flag to walk the interactive flow (signal type → generator → rate →
duration → sink type → output path), or use `--from <csv>` to seed the scaffold from
existing CSV data. See [`sonda new`](cli-reference.md#sonda-new) for the full flag reference.

## What next

- [**CLI Reference -- sonda run**](cli-reference.md#sonda-run) -- flag reference for running scenario files
- [**CLI Reference -- dry run**](cli-reference.md#dry-run) -- validate and preview a scenario file before running
- [**Scenario Fields**](scenario-fields.md) -- per-entry field reference (generators, labels, schedules)
- [**Server API**](../deployment/sonda-server.md) -- `POST /scenarios` accepts a scenario file as YAML or JSON
- [**Metric Packs**](../guides/metric-packs.md) -- the pack catalog you can reference from scenario entries
