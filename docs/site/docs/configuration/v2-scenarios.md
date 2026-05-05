# v2 Scenario Files

The v2 scenario format is Sonda's way to describe one or many signals in a single YAML file.
One top-level block declares shared defaults; another lists the scenarios. Packs, `after:`
temporal dependencies, and clock groups all compose inside the same file.

Every scenario file must declare `version: 2` at the top. Everything else you already know
about scenarios -- generators, encoders, sinks, labels -- still applies.

!!! warning "v1 YAML is no longer accepted"
    Sonda only accepts v2 scenario YAML. The CLI (`sonda run`, `sonda metrics`,
    `sonda catalog run`, every `--scenario` consumer) and the HTTP server (`POST /scenarios`)
    both refuse files or bodies without `version: 2` at the top and print a migration hint
    pointing at this page.

    If you are upgrading from a Sonda release before this change, jump straight to
    [Migrating from v1](#migrating-from-v1).

## Minimal example

```yaml title="hello-v2.yaml"
version: 2

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
sonda run --scenario hello-v2.yaml
```

```text title="Output"
▶ demo_cpu  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 30s
demo_cpu 42 1776090151609
demo_cpu 42 1776090152619
...
■ demo_cpu  completed in 30.0s | events: 30 | bytes: 680 B | errors: 0
━━ run complete  scenarios: 1 | events: 30 | bytes: 680 B | errors: 0 | elapsed: 30.1s
```

## What v2 buys you

- **Shared defaults** -- put `rate`, `duration`, `encoder`, `sink`, and `labels` in one place.
- **Temporal chains** -- link scenarios with `after:` clauses (`this starts when that crosses a threshold`).
- **Automatic clock groups** -- scenarios linked by `after:` share a clock so they stay in sync.
- **Pack-backed entries** -- reference a [metric pack](../guides/metric-packs.md) inline, alongside regular metrics and logs.
- **Richer validation** -- the compiler catches missing fields, cycles in `after:` chains, and
  pack references to unknown names at parse time.

## Catalog metadata

v2 files can carry optional top-level metadata that powers the unified catalog --
`sonda catalog list`, `sonda catalog show`, and the `--category` filter. The fields sit at
the root, alongside `version:` and `defaults:`:

```yaml title="scenarios/steady-state.yaml"
version: 2

scenario_name: steady-state
category: infrastructure
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
| `scenario_name` | no | Kebab-case identifier. Defaults to the filename (without `.yaml`, hyphens preserved) if omitted. Used by `@name` shorthand and `sonda catalog run`. When posted to a running `sonda-server`, this field also acts as a uniqueness key — POSTing two cascades that share an active `scenario_name` returns [`409 Conflict`](../deployment/sonda-server.md#duplicate-scenario_name-returns-409). |
| `category` | no | Catalog grouping. One of `infrastructure`, `network`, `application`, `observability`. Scenarios without a category render as `uncategorized` and drop out of `--category` filters. |
| `description` | no | One-line summary shown in the catalog table and JSON output. Keep it under ~60 characters so it fits the table column. |

The compiler ignores these fields -- they only feed the catalog. `deny_unknown_fields` stays
in force, so typos like `scenarioName:` or `desc:` are rejected at parse time.

!!! info "Same field names as legacy v1"
    The retired v1 format used the same top-level field names (`scenario_name`, `category`,
    `description`). Migrating a v1 file to v2 keeps the metadata as-is -- you add
    `version: 2` and reshape the body around `defaults:` + `scenarios:`.

Drop a v2 file into any directory on the
[scenario search path](../guides/scenarios.md#scenario-search-path) and it shows up
immediately:

```bash
sonda catalog list --category infrastructure
```

```text
NAME           TYPE      CATEGORY         SIGNAL    RUNNABLE   DESCRIPTION
steady-state   scenario  infrastructure   metrics   yes        Normal oscillating baseline (sine + jitter)
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
sonda run --scenario examples/loki-json-lines.yaml --duration 1s --dry-run

# Override -- every ${LOKI_URL} resolves to this value
LOKI_URL=http://loki:3100 sonda run --scenario examples/loki-json-lines.yaml --duration 1s --dry-run
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

`--on-sink-error <warn|fail>` on every CLI subcommand (`sonda run`, `sonda metrics`, `sonda logs`, `sonda histogram`, `sonda summary`) overrides `defaults.on_sink_error` for one-off invocations -- handy when you want to point a single CI run at a YAML that defaults to `warn`. See [CLI Reference](cli-reference.md#sonda-run).

## Temporal chains with `after:`

Use `after:` to express "this scenario starts when that one crosses a threshold". Sonda resolves
the timing at parse time -- no runtime reactivity -- by computing concrete `phase_offset` values
from each generator's shape.

The built-in `link-failover` scenario is a worked example: a primary interface flaps, a backup
link saturates once the primary drops, and latency degrades once the backup fills.

```yaml title="scenarios/link-failover.yaml"
version: 2

scenario_name: link-failover
category: network
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

Use `--dry-run` to see the resolved timing:

```bash
sonda run --scenario scenarios/link-failover.yaml --dry-run
```

```text
[config] file: scenarios/link-failover.yaml (version: 2, 3 scenarios)

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

The scenario also ships in the built-in catalog:

```bash
sonda catalog run link-failover
```

!!! tip "Supported generators in `after:`"
    The `after:` clause resolves against the target generator's trajectory. Supported shapes:
    `flap`, `saturation`, `leak`, `degradation`, `spike_event`. Using `steady` as the target is
    rejected -- sine crossings are ambiguous.

For continuous gating that pauses and resumes a downstream as the upstream's value oscillates above and below a threshold, see [Continuous coupling with `while:`](#continuous-coupling-with-while).

## Continuous coupling with `while:`

`after:` is a one-shot trigger -- it fires once and the dependent scenario runs to completion. `while:` is the continuous-coupling counterpart: the gated scenario emits only while the upstream's latest value satisfies the predicate, pauses when the predicate fails, and resumes when the predicate becomes true again. Use `while:` when an event stream should track an upstream signal's lifecycle, not just its first crossing. When a `while:`-gated scenario pauses, downstream alerts on its metrics keep firing for ~5 minutes by default — see [Recovering Prometheus alerts on gate close](#recovering-prometheus-alerts-on-gate-close) for the stale-marker default that resolves them immediately.

```yaml title="scenarios/link-traffic.yaml"
version: 2

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

`open` is the duration the upstream value must satisfy the predicate before the gate transitions from closed to open; `close` is the duration the value must violate the predicate before the gate transitions back to closed. Either field defaults to `0s` when omitted. `delay:` requires `while:` -- standalone `delay:` is rejected at compile time. The `close:` field also accepts an extended struct form for stale-marker control on `running → paused` transitions; see [Recovering Prometheus alerts on gate close](#recovering-prometheus-alerts-on-gate-close) below.

### Recovering Prometheus alerts on gate close

Prometheus retains the last-known sample for the lookback-delta window (default 5 minutes) when a series stops emitting. A `while:`-gated downstream that reports `bgp_oper_state=2` ("down") and then pauses keeps the alert firing for that window because Prometheus has no signal that the source is stale. Sonda emits a Prometheus stale-marker sample for every recently-active `(metric_name, label_set)` tuple on every committed `running → paused` transition when the sink is `remote_write`. Downstream alerts resolve immediately on the next scrape cycle.

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

`snap_to` replaces the stale marker with a normal sample carrying the supplied value. Use it when the recovery semantics call for an explicit recovered value (`bgp_oper_state=1` for "up") rather than a stale signal. When set, `snap_to` is honored on every sink the configured encoder can serialize to — including `stdout`, `file`, `loki`, and `kafka`. Without `snap_to`, only `remote_write` sinks emit a default close marker; the others stay silent on close.

To suppress the default emit entirely, set `stale_marker: false` instead:

```yaml title="Disable the default stale-marker emit"
    delay:
      open: 250ms
      close:
        duration: 5s
        stale_marker: false
```

Setting both `snap_to` and `stale_marker: false` is a config error — `snap_to` already replaces the stale marker, so the explicit `false` is redundant and likely a mistake. Setting `snap_to` alongside the implicit `stale_marker: true` default lets `snap_to` win silently.

The marker fires only on **committed** `running → paused` transitions. A brief close that gets cancelled by a fresh `WhileOpen` arriving inside the `delay.close.duration` debounce window emits nothing — the gate stayed open. A scenario that hits its `duration:` while paused goes `paused → finished` without an additional close-emit (see the lifecycle states above). The recently-active tuple set is sourced from a runtime buffer capped at 100 events; high-cardinality scenarios that exceed this ceiling under-emit on close. Track scenarios with more than ~100 distinct label sets via per-scenario `/stats` rather than relying on close-emit alone.

### Combining with `after:`

`after:` and `while:` compose on the same entry. `after:` defers the scenario's first emission until an upstream crosses a threshold; `while:` then continuously gates the entry on every later edge. Pair them when a downstream should wait for a triggering event AND track the upstream's state thereafter -- a BGP session that opens once a link drops, then pauses every time the link briefly recovers.

```yaml title="scenarios/bgp-session-cascade.yaml"
version: 2

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

`sonda run --scenario scenarios/link-traffic.yaml --dry-run` renders the gate plumbing alongside the existing layout:

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

```yaml title="scenarios/link-failover-recovery.yaml"
version: 2

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

## Pack-backed entries

Reference a [metric pack](../guides/metric-packs.md) directly from a scenarios entry. Sonda
expands the pack at compile time -- you get one prepared scenario per metric in the pack.

```yaml title="snmp-interface.v2.yaml"
version: 2

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
[Status output -- clock groups](cli-reference.md#clock-groups-in-status-output) for what the
banners look like at runtime.

## Scaffolding v2 files with `sonda init`

`sonda init` emits v2 YAML by default:

```bash
sonda init \
  --signal-type metrics \
  --domain infrastructure \
  --metric demo_cpu \
  --situation spike_event \
  --rate 5 --duration 30s \
  --encoder prometheus_text --sink stdout \
  -o ./scenarios/demo-cpu.yaml
```

```yaml title="./scenarios/demo-cpu.yaml"
# demo_cpu: infrastructure scenario using the 'spike_event' pattern.
#
# Generated by `sonda init`. Run with:
#   sonda run --scenario <this-file>

version: 2

# Defaults inherited by every entry in scenarios: below.
defaults:
  rate: 5
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: demo_cpu
    generator:
      type: spike_event
      baseline: 0.0
      spike_height: 100.0
      spike_duration: "10s"
      spike_interval: "30s"
```

Run the generated file with `sonda run --scenario`. For the full guided scaffolding flow
(signal types, packs, logs, histograms, summaries), see
[`sonda init`](cli-reference.md#sonda-init).

## Migrating from v1

Every legacy v1 shape maps cleanly to v2. Pick the tab that matches the file you have.

### How Sonda rejects v1

When Sonda encounters a file or request body without `version: 2`, it stops before running
anything and prints the error alongside a pointer to this guide.

=== "CLI (`sonda run`, `sonda metrics`, ...)"

    ```text title="stderr"
    error: scenario file /path/to/legacy.yaml is not a v2 scenario. Sonda only accepts v2 YAML (`version: 2` at the top level). Migrate this file to v2 — see docs/configuration/v2-scenarios.md for the migration guide.
    ```

    Exit code: `1`. Applies to `sonda run`, `sonda metrics`, `sonda logs`,
    `sonda histogram`, `sonda summary`, and `sonda catalog run` -- every `--scenario`
    consumer.

=== "Server (`POST /scenarios`)"

    ```http
    HTTP/1.1 400 Bad Request
    Content-Type: application/json

    {
      "error": "bad_request",
      "detail": "body is not a v2 scenario. Sonda only accepts v2 scenario bodies (`version: 2` at the top level). Migrate this body to v2 — see docs/configuration/v2-scenarios.md for the migration guide."
    }
    ```

    The server returns `400 Bad Request` (not `422`) because a non-v2 body is ill-formed by
    contract, not a semantic validation failure. See
    [Server API -- Start a Scenario](../deployment/sonda-server.md#start-a-scenario).

### Shape-by-shape migration

=== "Flat single-signal"

    The legacy "flat" layout put `name:`, `rate:`, `generator:`, `encoder:`, and `sink:` at the
    top level with no `scenarios:` wrapper. Wrap them in a single-entry v2 file:

    ```yaml title="Before (v1, flat single-signal)"
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
    labels:
      zone: us-east-1
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 100
      duration: 30s
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
          amplitude: 50.0
          period_secs: 60
          offset: 50.0
        labels:
          zone: us-east-1
    ```

    Two things changed:

    - `version: 2` at the top, with shared `rate` / `duration` / `encoder` / `sink` moved into
      `defaults:`.
    - One entry under `scenarios:` with an explicit `signal_type: metrics`. The `id:` is
      free-form; it's what `after:` clauses and `clock_group:` references use.

=== "Top-level `scenarios:` list"

    The legacy multi-scenario layout already had a `scenarios:` list, but each entry repeated
    `encoder:`, `sink:`, `rate:`, and `duration:`. Add `version: 2`, move the shared fields
    into `defaults:`, and drop the repetition:

    ```yaml title="Before (v1, multi-scenario)"
    scenarios:
      - signal_type: metrics
        name: cpu
        rate: 10
        duration: 60s
        generator: { type: constant, value: 1 }
        encoder: { type: prometheus_text }
        sink: { type: stdout }
      - signal_type: metrics
        name: mem
        rate: 10
        duration: 60s
        generator: { type: constant, value: 2 }
        encoder: { type: prometheus_text }
        sink: { type: stdout }
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 10
      duration: 60s
      encoder: { type: prometheus_text }
      sink: { type: stdout }

    scenarios:
      - id: cpu
        signal_type: metrics
        name: cpu
        generator: { type: constant, value: 1 }
      - id: mem
        signal_type: metrics
        name: mem
        generator: { type: constant, value: 2 }
    ```

    Any per-entry field (`rate`, `labels`, `gaps`, `encoder`, ...) still wins over `defaults:`.

=== "Logs"

    Log entries move into a `scenarios:` entry with `signal_type: logs` and `log_generator:`
    (note the `log_` prefix -- v2 log entries use `log_generator:` to keep the discriminated
    union unambiguous):

    ```yaml title="Before (v1, flat logs)"
    name: app_logs
    rate: 10
    duration: 60s
    generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2"]
            endpoint: ["/api", "/health"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
    labels:
      job: sonda
      env: dev
    encoder:
      type: json_lines
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 10
      duration: 60s
      encoder:
        type: json_lines
      sink:
        type: stdout
      labels:
        job: sonda
        env: dev

    scenarios:
      - id: app_logs
        signal_type: logs
        name: app_logs
        log_generator:
          type: template
          templates:
            - message: "Request from {ip} to {endpoint}"
              field_pools:
                ip: ["10.0.0.1", "10.0.0.2"]
                endpoint: ["/api", "/health"]
          severity_weights:
            info: 0.7
            warn: 0.2
            error: 0.1
          seed: 42
    ```

=== "Histogram / Summary"

    Histogram and summary entries do not use `generator:` at all -- they use
    `distribution:`, `observations_per_tick:`, and (optionally) `buckets:` or `quantiles:`.
    Wrap them in a v2 entry with `signal_type: histogram` or `signal_type: summary`:

    ```yaml title="Before (v1, flat histogram)"
    name: http_request_duration_seconds
    rate: 1
    duration: 60s
    distribution:
      type: exponential
      rate: 10.0
    observations_per_tick: 100
    seed: 42
    labels:
      job: api
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

    defaults:
      rate: 1
      duration: 60s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - id: http_request_duration_seconds
        signal_type: histogram
        name: http_request_duration_seconds
        distribution:
          type: exponential
          rate: 10.0
        observations_per_tick: 100
        seed: 42
        labels:
          job: api
    ```

    The `distribution`, `observations_per_tick`, `buckets`, and `quantiles` fields stay on the
    entry. See [Generators -- histogram and summary](generators.md#histogram-and-summary-generators)
    for the full field reference.

=== "`pack:` shorthand"

    The legacy `pack: <name>` shorthand file (a top-level `pack:` with `rate:` / `duration:` /
    `labels:`) becomes a single v2 entry whose body is `pack: <name>`:

    ```yaml title="Before (v1, pack shorthand)"
    pack: telegraf_snmp_interface
    rate: 1
    duration: 60s
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    ```yaml title="After (v2)"
    version: 2

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

    See [Pack-backed entries](#pack-backed-entries) above for how the compiler expands
    `pack:` into one entry per metric.

=== "Hand-tuned `phase_offset`"

    If a legacy file chained signals with hand-tuned `phase_offset` values, v2 expresses the
    same temporal relationships declaratively with `after:` clauses. Each entry says *when it
    starts relative to another entry's shape* and the compiler resolves the offsets at parse
    time. See the [`after:` section](#temporal-chains-with-after) above.

### Common gotchas

- **`signal_type` is per-entry in v2.** Legacy files let you put `signal_type:` at the top
  level; v2 reads it from the first entry (`metrics`, `logs`, `histogram`, `summary`). Every
  entry in a multi-signal file carries its own `signal_type:`.
- **Log entries use `log_generator:`, not `generator:`.** Metrics use `generator:`;
  histograms and summaries use `distribution:`; logs use `log_generator:`. Mismatched keys
  trigger a v2 compile error.
- **`deny_unknown_fields` is strict.** Typos like `scenarioName:` or `desc:` at the top of a
  v2 file are rejected at parse time with the offending field name in the error. Fix the
  typo and re-run.
- **`sonda import` already emits v2.** Regenerate any imported scenarios with
  [`sonda import`](cli-reference.md#sonda-import) if you kept older output around.

## What next

- [**CLI Reference -- sonda run**](cli-reference.md#sonda-run) -- flag reference for running v2 files
- [**CLI Reference -- dry run**](cli-reference.md#dry-run) -- validate and preview a v2 file before running
- [**Scenario Fields**](scenario-fields.md) -- per-entry field reference (generators, labels, schedules)
- [**Server API**](../deployment/sonda-server.md) -- `POST /scenarios` accepts v2 YAML or JSON
- [**Metric Packs**](../guides/metric-packs.md) -- the pack catalog you can reference from v2 entries
