# Scenario Files

The scenario file format is Sonda's way to describe one or many signals in a single YAML file. One top-level block declares shared defaults; another lists the scenarios. Packs, `after:` temporal dependencies, and clock groups all compose inside the same file.

Every scenario file declares two top-level fields:

- **`version: 2`** — the format version. Always `2`.
- **`kind: runnable`** — a file you can run with `sonda run`. Use `kind: composable` for files that define a [metric pack](catalogs-and-packs.md) used by other scenarios.

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
- **Pack-backed entries** -- reference a [metric pack](catalogs-and-packs.md) inline, alongside regular metrics and logs.
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
| `name` | no | Catalog identifier (kebab-case). Defaults to the filename (without `.yaml`, hyphens preserved) if omitted. Used by `@name` shorthand and `sonda run @name`. When posted to a running `sonda-server`, this field also acts as a uniqueness key — POSTing two cascades that share an active `name` returns [`409 Conflict`](../deploy/http-api.md#duplicate-scenario_name-returns-409). |
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
[Endpoints & networking](../deploy/server.md) for the full
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
| `warn` (default) | The runner logs to stderr (rate-limited, one line per minute per scenario with a count), increments error counters in [`/stats`](../deploy/http-api.md#self-observability-via-stats), drops the failed batch, and keeps ticking. The scenario stays alive while the sink recovers. |
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
    Pick `fail` when sink delivery is itself the contract under test -- CI gates that compare metric counts against an expected total, or smoke tests that should abort the moment the backend goes away. Pick `warn` (the default) for everything else: long-running fleets, demo environments, or any scenario where you want runtime self-observability via [`/stats`](../deploy/http-api.md#self-observability-via-stats) instead of thread death.

`--on-sink-error <warn|fail>` on `sonda run` overrides `defaults.on_sink_error` for one-off invocations -- handy when you want to point a single CI run at a YAML that defaults to `warn`. See [CLI Reference](../reference/cli-flags.md#sonda-run).

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

`after:` is a one-shot trigger -- it fires once and the dependent scenario runs to completion. `while:` is the continuous-coupling counterpart: the gated scenario emits only while the upstream's latest value satisfies the predicate, pauses when the predicate fails, and resumes when the predicate becomes true again. A `while:`-gated entry must declare a `duration:` -- on the entry itself or via `defaults.duration` -- and is rejected at compile time without one, because the `paused` state needs a terminal point to bound the scenario's lifetime. Use `while:` when an event stream should track an upstream signal's lifecycle, not just its first crossing. When a `while:`-gated scenario pauses, downstream alerts on its metrics keep firing for ~5 minutes by default — see [Recovering Prometheus alerts on gate close](#recovering-prometheus-alerts-on-gate-close) for the stale-marker default that resolves them immediately. The upstream signal lives in the same scenario file by default; for [`sonda-server`](../deploy/server.md) deployments where the upstream arrives in a separate POST body, see [Cross-POST `while:` refs](#cross-post-while-refs).

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

A fifth state, `held`, replaces `paused` after a gate close when the scenario opts into the snap-to recovery shape via [`delay.close.snap_to`](#recovering-prometheus-alerts-on-gate-close) AND has emitted at least one sample. `held` differs from `paused` only on the pull-path: scrapers passing [`?include_state=...,held`](#aggregate-metrics-sees-paused-and-unresolved-scenarios) keep seeing the frozen value, while `paused` ghosts stay filtered out under that same allowlist. Push sinks behave identically in both states (the runner stops emitting). `held` applies to metric scenarios. See [Pattern C — Counter freeze-and-hold during outage](#pattern-c-counter-freeze-and-hold-during-outage) for the orchestration.

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

Gate-close emission is the active recovery primitive — Sonda writes a recovery sample (or stale marker) the moment a gate closes. For the passive counterpart, where Prometheus resolves the alert on its own once a metric stops emitting for the lookback-delta window, see [Alert resolution via gaps](../test/alert-testing.md). The two compose: gaps drive absence-based recovery on any sink, gate close drives marker-based recovery on `remote_write`.

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

`while:` accepts only the strict comparison operators `<` and `>`. Non-strict operators (`<=`, `>=`, `==`, `!=`) are rejected at compile time with the message `unsupported operator '<op>' on while: — only strict comparisons '<' and '>' are accepted`. Real-valued upstream signals make exact equality ambiguous; if you need an equality-like gate, pick a strict comparison with a small tolerance built into the upstream generator (for example, a `constant` upstream at `1.0` gated by `op: "<" value: 0.5`).

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

The `while:` clause shown above gates a scenario on another signal *in the same file*. When you drive Sonda via [`sonda-server`](../deploy/server.md), you can also gate on a signal from a **separately POSTed body** by qualifying the `ref:` with the upstream's `scenario_name:`. This lets one process drive multiple independent POSTs whose lifecycles are loosely coupled — a baseline counter you launch at boot, paused or resumed by a cascade body you POST on demand later — without restarting either side or rewriting the YAML when a new upstream arrives.

Cross-POST refs are a `sonda-server` feature. The CLI runs every scenario in a single process and has no cross-POST registry, so `sonda run` rejects a file with `while.scenario_name` at parse time and points here.

#### Two patterns, pick the one your scrape pipeline needs

Cross-POST `while:` is most often used to coordinate a long-running **baseline** with an on-demand **cascade**. Two shapes show up; the right one depends on whether the baseline and the cascade emit the *same* `(metric_name, label_set)` series or *different* series.

| Pattern | Use when | How it works |
|---|---|---|
| [**Pattern A — Cascade signals to pause a baseline**](#pattern-a-cascade-signals-to-pause-a-baseline) | Baseline and cascade emit **different series**. You want the baseline silent (no emissions, no scrape ghosts) only while the cascade is firing. | Baseline carries a cross-POST `while:` gated on a cascade signal, plus `if_unresolved: open` so it runs at the default rate when the cascade is absent. Push sinks honor the gate naturally; pull scrapers need `?include_state=running,unresolved` (see the callout under `?include_state=`). |
| [**Pattern B — Cascade overrides baseline emission**](#pattern-b-cascade-overrides-baseline-emission) | Baseline and cascade emit the **same series**. You want the cascade's values to replace the baseline's in the scrape during the outage window. | DELETE the baseline, POST the cascade for the outage window, DELETE the cascade, POST the baseline again. Or — for scrapers that can pass `?include_state=running` — keep both alive with inverse `while:` clauses so only one is `running` at a time. |
| [**Pattern C — Counter freeze-and-hold during outage**](#pattern-c-counter-freeze-and-hold-during-outage) | Single metric series (no separate baseline POST) whose value should freeze at the last sample during the outage window, then resume from the frozen value when the gate reopens. | A single scenario with `delay.close.snap_to: <value>` and a `while:` clause. Gate close transitions the scenario to `held`, the pull-path retains the frozen sample, and scrapers opt in with `?include_state=running,unresolved,held`. No DELETE-and-replace orchestration. |

All three worked examples are below. Read them before picking — the same-series vs different-series distinction governs A vs B, and the freeze-vs-replace distinction governs C. Picking the wrong one shows up as either duplicate samples in `/metrics`, an empty scrape body at steady state, or a counter that goes to zero during the outage when you wanted it held at its last value.

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

- **Initial POST**: the downstream lands in `unresolved`. Its [`pending_ref` field on `GET /scenarios/{id}`](../deploy/http-api.md#pending_ref-field-on-get-scenariosid) shows the upstream it is waiting on.
- **Upstream POSTs**: the downstream resolves automatically — the server's resolver wires the subscription and the downstream transitions to `running` (or to `paused` if the predicate is already false). No client orchestration needed.
- **Upstream is DELETEd, or finishes its duration**: the downstream transitions back to `unresolved` and applies the `if_unresolved:` mode again — `open` keeps it emitting, `closed` pauses, `pending` halts.
- **A new POST arrives with the same `scenario_name:`**: the downstream re-resolves to the new upstream automatically. The downstream keeps its accumulated state across the gap — counters preserve their value through pause/resume cycles.

`GET /scenarios/{id}` exposes the wait target as `pending_ref` whenever `state` is `unresolved`. The field is omitted in every other state.

```json title="GET /scenarios/{id} — unresolved downstream"
{
  "id": "01HX...",
  "name": "requests_total",
  "state": "unresolved",
  "pending_ref": {
    "scenario_name": "cascade_post",
    "entry_id": "link_state"
  }
}
```

`pending_ref.scenario_name` is the upstream POST body the downstream is waiting on; `pending_ref.entry_id` is the entry id inside that body. Both match the values written in the downstream's `while.scenario_name` and `while.ref`.

Re-resolution is event-driven, not polled. There is no periodic background sweep — the server attempts to resolve a downstream's `pending_ref` only when a POST lands carrying a matching `scenario_name:`. If no upstream POST ever arrives, the downstream stays `unresolved` indefinitely (or, with `if_unresolved: open`, keeps emitting at its default rate). The orchestration implication: POST the upstream first when the wiring is predictable, or accept that the downstream sits in `unresolved` until the upstream shows up.

#### Pattern A — Cascade signals to pause a baseline

A baseline counter that you want running at full rate by default, but paused whenever an on-demand cascade declares a "link state" of 0. The baseline and the cascade emit **different series** (`requests_total` and `link_state`); the cascade just signals the baseline to stop emitting while it is firing.

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

#### Failing fast on a misspelled ref — `?validate=strict`

By default `POST /scenarios` accepts a body whose `while.scenario_name` does not match any running scenario: the entry lands in `unresolved` and re-resolves whenever a matching upstream POSTs. That is the right behavior for predictable client-orchestrated wiring, but it can hide typos — a downstream gated on `bgp_peer_stat` (missing an `e`) sits in `unresolved` forever instead of failing the POST.

Append `?validate=strict` to reject the entire body if any cross-POST `while:` ref is unresolvable at POST time:

```bash
curl -X POST 'http://localhost:8080/scenarios?validate=strict' \
  -H 'Content-Type: application/x-yaml' \
  --data-binary @downstream.yaml
```

The check is atomic — either every entry passes or the whole body is rejected; no scenarios spawn on rejection. The response is `HTTP 422 Unprocessable Entity` with an `unresolved_refs` array listing the missing upstreams:

```json
{
  "error": "unresolved_refs",
  "unresolved_refs": [
    {
      "scenario_name": "bgp_peer_stat",
      "entry_id": "down",
      "referenced_by": "alert_route_flap"
    }
  ]
}
```

Use `?validate=strict` for pre-flight checks in CI pipelines and for one-shot POSTs where the upstream is meant to already exist. Leave it off (the default) when the downstream is intentionally launched ahead of its upstream — for instance the `if_unresolved: open` baseline-counter pattern above.

#### Re-POSTing under the same `scenario_name` is two operations

Every POST whose top-level `scenario_name:` matches an entry already in `pending` / `running` / `paused` / `unresolved` returns `HTTP 409 Conflict`. There is no in-place update — replacing an active cascade is always DELETE-then-POST.

```json title="409 response body"
{
  "error": "scenario_name 'cascade_post' is already running",
  "conflicting_scenarios": [
    {
      "id": "01HX...",
      "name": "link_state",
      "state": "running"
    }
  ],
  "hint": "DELETE the conflicting scenarios before posting a new cascade with the same scenario_name"
}
```

`conflicting_scenarios` lists every active entry that shares the posted `scenario_name`. DELETE each one (a multi-entry body produces multiple handles, all of which must go) before the next POST. Entries that have already transitioned to `finished` do not block — they are stale handles and the server ignores them.

#### Aggregate `/metrics` sees paused and unresolved scenarios

The aggregate [`GET /metrics`](../deploy/http-api.md#get-metrics-vs-get-scenariosidmetrics) endpoint walks the full scenario map without filtering on state. Every scenario the server currently knows about — `running`, `paused`, `unresolved`, even `pending` — contributes the most recent value per `(metric_name, label_set)` to the scrape output. The handle holds those last-seen samples until you DELETE the scenario; gate-pause alone does not clear them.

The practical consequence for cross-POST wiring: two scenarios that target the same `(metric_name, labels)` with mutually-exclusive gates both contribute samples to `/metrics`. The aggregate concatenates their points; it does not pick "the live one." If you want only one scenario to contribute a given series at a time, DELETE the inactive one rather than relying on the gate to silence it. This is load-bearing for the cascade-replaces-baseline pattern below.

Scrapers can opt into a state-aware view of the aggregate by passing `?include_state=<allowlist>` on the request. Series from scenarios whose state is outside the allowlist are skipped; with the parameter absent the response is unchanged (every scenario still contributes its last-known sample). The allowlist is comma-separated and accepts the six scenario states — `pending`, `running`, `paused`, `held`, `unresolved`, `finished` — in any combination:

```bash
# Only running scenarios — paused, held, and unresolved ghosts are filtered out
curl 'http://localhost:8080/metrics?include_state=running'

# Multiple states at once; whitespace after the comma is tolerated
curl 'http://localhost:8080/metrics?include_state=running,paused'

# Include held scenarios in the scrape (counter freeze-and-hold pattern)
curl 'http://localhost:8080/metrics?include_state=running,unresolved,held'

# Combine with the label filter; both are applied as intersection
curl 'http://localhost:8080/metrics?include_state=running&label=device:srl1'
```

An unknown state name or an empty value (`?include_state=`) returns `400 Bad Request` with a message listing the six valid options. This filter is pull-only — push sinks such as `remote_write`, `kafka`, and `otlp` already honor scenario state by construction, because the runner skips encoding and writing while the gate is closed.

A metric scenario configured with `delay.close.snap_to` reports `state: "held"` after gate close instead of `state: "paused"`, provided at least one sample emitted before the close. A scraper that matches on `?include_state=paused` to surface snap-to scenarios should switch to `?include_state=held` (or include both).

!!! warning "`?include_state=running` filters out `if_unresolved: open` baselines"

    A scenario that uses `if_unresolved: open` (Pattern A above) reports `state: "unresolved"`, not `state: "running"`, while no upstream cascade has resolved — the state machine treats "emitting at the open default" and "running normally as a resolved scenario" as distinct lifecycle states. A scraper hitting `?include_state=running` on a deployment that uses Pattern A will filter the baseline out and see an empty scrape body at steady state.

    For that combination, use `?include_state=running,unresolved`:

    ```bash
    # Keeps the if_unresolved: open baseline visible at rest, still filters paused ghosts during a cascade
    curl 'http://localhost:8080/metrics?include_state=running,unresolved'
    ```

    The baseline stays visible while the cascade is absent; once the cascade POSTs and the baseline resolves to `running`, both states still pass the filter, so the scrape never goes dark. During a gate-pause the baseline transitions to `paused` and drops out of the response, which is the desired behavior.

#### Pattern B — Cascade overrides baseline emission

Pattern A pauses the baseline whenever the cascade gates it shut, but the baseline's last value keeps appearing in the aggregate `/metrics` scrape (see the ghost-sample note just above) — which is fine when the baseline and cascade emit different series. When they emit the **same** `(metric_name, label_set)` series — a constant healthy value swapped out for a flapping outage value on the same series — gate-pause is not enough, because the baseline's last `1` would sit alongside the cascade's `0` in the scrape. Orchestrate with DELETE + POST instead.

```yaml title="baseline.yaml — steady-state value"
version: 2
kind: runnable
scenario_name: link_baseline
defaults:
  rate: 1
  duration: 1h
  encoder:
    type: prometheus_text
  sink:
    type: remote_write
    url: ${VICTORIAMETRICS_REMOTE_WRITE_URL:-http://localhost:8428/api/v1/write}
scenarios:
  - id: interface_oper_state
    signal_type: metrics
    name: interface_oper_state
    labels:
      device: rtr-edge-01
      interface: GigabitEthernet0/0/0
    generator:
      type: constant
      value: 1
```

```yaml title="outage.yaml — flapping value on the same series"
version: 2
kind: runnable
scenario_name: link_outage
defaults:
  rate: 1
  duration: 10m
  encoder:
    type: prometheus_text
  sink:
    type: remote_write
    url: ${VICTORIAMETRICS_REMOTE_WRITE_URL:-http://localhost:8428/api/v1/write}
scenarios:
  - id: interface_oper_state
    signal_type: metrics
    name: interface_oper_state
    labels:
      device: rtr-edge-01
      interface: GigabitEthernet0/0/0
    generator:
      type: flap
      up_duration: 30s
      down_duration: 60s
```

The orchestration sequence:

1. **Start the baseline.** POST `baseline.yaml`. The series `interface_oper_state{device="rtr-edge-01", interface="GigabitEthernet0/0/0"}` reads `1` (healthy).
2. **Begin the outage.** DELETE the baseline (`DELETE /scenarios/{baseline-id}`), then POST `outage.yaml`. Same series now flaps between `1` and `0` from the outage cascade.
3. **End the outage.** DELETE the outage cascade, then POST `baseline.yaml` again. Series returns to a steady `1`.

The inverse shape — keeping the baseline alive and adding a `while:` clause to pause it whenever the outage cascade fires — does NOT achieve the same effect on the aggregate scrape. The baseline's last `1` would still appear in `/metrics` alongside the outage's `0`, because gate-pause does not clear the handle's `current_values`. For replacement-of-emission, DELETE the baseline first.

If your scrapers can pass `?include_state=running`, the DELETE-and-replace dance collapses to a POST-and-leave-running flow: POST the baseline and the outage cascade with inverse `while:` clauses on the same upstream signal, so that exactly one of them is `running` at any moment. Scrapers request `GET /metrics?include_state=running` and see whichever scenario currently holds the gate open; the paused side is filtered out, so the target series never carries two simultaneous samples and no DELETE is needed. Keep the DELETE-and-replace flow above for scrapers that cannot set the query parameter — push-only sinks already see the right thing because the runner does not emit while a `while:` gate is closed.

#### Pattern C — Counter freeze-and-hold during outage

Some operational signals should freeze at their last value during an outage rather than go silent or drop to zero. A monotonic counter that represents "requests served" or a gauge that represents "last-known interface state" is more truthful held at the last sample than reset, and a scraper that sees the frozen value can keep alerting against the same threshold throughout the outage window.

A single metric scenario with a `while:` clause and `delay.close.snap_to: <value>` does this without a separate baseline POST and without DELETE-and-replace orchestration. When the gate closes, the runner fires the one-shot recovery sample, the scenario transitions to the `held` lifecycle state, and `current_values` retains the frozen sample. Scrapers that pass `?include_state=running,unresolved,held` continue to see the frozen value on every scrape; scrapers that omit `held` from the allowlist see the series drop out for the duration of the outage.

```yaml title="held-counter.yaml — single scenario, frozen during outage"
version: 2
kind: runnable
scenario_name: held_counter_post
defaults:
  rate: 1
  duration: 1h
  encoder:
    type: prometheus_text
  sink:
    type: remote_write
    url: ${VICTORIAMETRICS_REMOTE_WRITE_URL:-http://localhost:8428/api/v1/write}
scenarios:
  - id: bgp_oper_state
    signal_type: metrics
    name: bgp_oper_state
    labels:
      device: rtr-edge-01
      peer: 10.0.0.1
    generator:
      type: constant
      value: 1
    while:
      scenario_name: outage_signal_post
      ref: link_state
      op: ">"
      value: 0
      if_unresolved: open
    delay:
      close:
        duration: 0s
        snap_to: 1
```

The orchestration sequence:

1. **Steady state.** POST `held-counter.yaml`. Because the outage signal has not arrived, `if_unresolved: open` keeps the counter at `1`. The state reads `unresolved`. Scrapers using `?include_state=running,unresolved,held` see the value.
2. **Outage begins.** POST an outage cascade that publishes `link_state` going to `0` (the same shape used in Patterns A and B). The gate closes — the runner fires the `snap_to: 1` recovery sample, and the scenario transitions from `unresolved` (or `running`) to `held`. The frozen `1` stays visible on the pull-path. Push sinks (`remote_write`, `kafka`, `loki`, etc.) stop receiving new samples for the duration of the hold; the one recovery sample at the close edge is the last write they see.
3. **Outage ends.** The outage cascade reopens `link_state` (or is DELETEd). The downstream transitions from `held` back to `running` and resumes emission at the configured rate. The series carries forward without a gap on the pull-path; push sinks pick up new samples at the next tick.

`held` is reachable only from metric scenarios that have emitted at least one sample before the gate first closes. A snap-to-equipped scenario whose gate closes before its first emission lands in `paused` (not `held`) because there is no frozen value to retain — the next gate open then emits normally and a subsequent close can transition into `held`.

The choice between Patterns A, B, and C:

- **A** drops the baseline to zero contributions during the outage by gating its emission shut. Use when the baseline and the outage cascade emit different series, and "absent" is the right scrape behavior during the outage.
- **B** replaces the baseline series with the outage cascade's values during the outage window. Use when the two emit the same series and you need the outage values to overwrite the baseline values in the scrape.
- **C** holds a single series at its last value for the duration of the outage. Use when the right scrape behavior is "the value you last saw," with no separate baseline POST and no DELETE-and-replace orchestration.

For the HTTP surface — the strict-validation flag, the `pending_ref` field, the duplicate-name 409, and the new `/stats` fields — see [HTTP API reference — Cross-POST `while:` refs](../deploy/http-api.md#cross-post-while-refs).

## Pack-backed entries

Reference a [metric pack](catalogs-and-packs.md) directly from a scenarios entry. Sonda
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
[CLI Reference -- Status output](../reference/cli-flags.md#status-output) for what the banners look
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
existing CSV data. See [`sonda new`](../reference/cli-flags.md#sonda-new) for the full flag reference.

## What next

- [**CLI Reference -- sonda run**](../reference/cli-flags.md#sonda-run) -- flag reference for running scenario files
- [**CLI Reference -- dry run**](../reference/cli-flags.md#dry-run) -- validate and preview a scenario file before running
- [**Scenario Fields**](../reference/scenario-fields.md) -- per-entry field reference (generators, labels, schedules)
- [**Server API**](../deploy/server.md) -- `POST /scenarios` accepts a scenario file as YAML or JSON
- [**Metric Packs**](catalogs-and-packs.md) -- the pack catalog you can reference from scenario entries
