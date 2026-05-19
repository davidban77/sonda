# Dynamic Labels

Dynamic labels attach a rotating label value to every emitted event. Use them when the label
you care about -- `hostname`, `pod_name`, `region` -- belongs on every data point, and you need
the label values to cycle through a bounded, predictable set.

At a glance, a dynamic label lets a single scenario entry stand in for a fleet:

```yaml title="10-node fleet, one entry"
scenarios:
  - signal_type: metrics
    name: node_cpu_usage
    generator:
      type: sine
      amplitude: 40.0
      offset: 50.0
    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10
```

Every tick emits one event whose `hostname` cycles through `host-0`, `host-1`, ..., `host-9`
and wraps back to `host-0`. You did not have to copy the scenario ten times.

## When to reach for dynamic labels

Three situations call for dynamic labels:

- **Fleet simulation.** You want to test a dashboard that aggregates by hostname (`sum by
  (hostname)`), but running one scenario per host is tedious and hard to maintain. One dynamic
  label with `cardinality: 50` produces a 50-series dataset from a single entry.
- **Geographic or categorical rotation.** Metrics tagged by `region`, `az`, `tenant`, or
  `customer_id` where the set of values is meaningful (not just a counter). Use `values: [...]`
  to list the real identifiers.
- **High-cardinality query paths.** Exercise Prometheus or VictoriaMetrics index paths without
  pushing cardinality *spikes* -- the label is always present, so time-series count stays flat
  at `cardinality` for the full duration.

!!! info "Dynamic labels vs. cardinality spikes"
    Dynamic labels are **always on**: the label appears on every event. Cardinality spikes are
    **time-windowed**: the label appears only during recurring spike windows. Pick dynamic
    labels when you are modeling a stable fleet; pick
    [cardinality spikes](../configuration/scenario-fields.md#cardinality-spike-window) when you
    are modeling a traffic event that briefly explodes your label set.

## The two strategies

A dynamic label uses one of two strategies. Which one you pick depends entirely on whether
the label values carry meaning.

=== "Counter strategy"

    Provide `prefix` and `cardinality`. Values are generated as `{prefix}0`, `{prefix}1`, ...,
    `{prefix}{cardinality-1}`, then wrap.

    ```yaml
    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10
    ```

    Use this when the values are synthetic and their only job is to be distinct -- fleet
    simulation, load testing index performance at a chosen cardinality, generating N series
    for a dashboard panel. If you omit `prefix`, it defaults to `"{key}_"`
    (e.g., `hostname_0`, `hostname_1`).

=== "Values list strategy"

    Provide `values`. The label cycles through the list in order, wrapping at the end.

    ```yaml
    dynamic_labels:
      - key: region
        values: [us-east-1, us-west-2, eu-west-1]
    ```

    Use this when the values carry meaning -- AWS regions, environments (`prod`/`staging`/`dev`),
    named customer tenants. Cardinality is implicit: it equals `values.len()`.

### Choosing between them

| You want... | Use | Why |
|-------------|-----|-----|
| N synthetic hosts numbered 0..N-1 | `counter` | Deterministic, predictable, scales to any N. |
| Specific named regions, tenants, clusters | `values_list` | Real-world identifiers matter for dashboards. |
| A fixed cardinality without caring about names | `counter` | Only the label cardinality matters. |
| Reproducible cycle across runs | either | Both are deterministic for a given tick. |

## Worked example: simulating a 10-node fleet

You want to test a Grafana panel that shows `sum by (hostname)` of CPU utilization across a
10-node cluster. Without dynamic labels, you would write ten scenario entries that differ only
in one label. With dynamic labels, one entry does the job.

```yaml title="examples/dynamic-labels-fleet.yaml"
version: 2
kind: runnable

defaults:
  rate: 10
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: node_cpu_usage
    generator:
      type: sine
      amplitude: 40.0
      period_secs: 60
      offset: 50.0
    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10
    labels:
      env: production
      cluster: us-east-1
```

Run it:

```bash
sonda run examples/dynamic-labels-fleet.yaml
```

```text title="Output (abridged)"
node_cpu_usage{cluster="us-east-1",env="production",hostname="host-0"} 50.00 ...
node_cpu_usage{cluster="us-east-1",env="production",hostname="host-1"} 50.42 ...
node_cpu_usage{cluster="us-east-1",env="production",hostname="host-2"} 50.84 ...
...
node_cpu_usage{cluster="us-east-1",env="production",hostname="host-9"} 53.74 ...
node_cpu_usage{cluster="us-east-1",env="production",hostname="host-0"} 54.15 ...
```

Each event carries a `hostname` label. Across the full duration, the series count stays at
exactly 10 -- `sum by (hostname) (node_cpu_usage)` returns ten values in every scrape window.

!!! tip "The generator runs once per tick, the label rotates once per event"
    At `rate: 10` events/sec, the sine generator advances at 10 Hz. Each event in the tick gets
    the same generator value but a different `hostname` -- so host-0 and host-1 see the same
    CPU shape, offset by one sample. If you want truly independent generators per host, write
    ten entries (or a generator that is phase-shifted by `hostname`, via `phase_offset` on
    separate entries).

## Combining multiple dynamic labels

Two or more dynamic labels cycle independently on the same tick counter. The result is a
Cartesian product over time:

```yaml title="examples/dynamic-labels-multi.yaml"
scenarios:
  - signal_type: metrics
    name: request_count
    generator:
      type: step
      start: 0
      step_size: 1.0
      max: 10000
    dynamic_labels:
      - key: hostname
        prefix: "web-"
        cardinality: 3
      - key: region
        values: [us-east-1, eu-west-1]
    labels:
      service: frontend
```

```text title="Output"
request_count{hostname="web-0",region="us-east-1",service="frontend"} 0
request_count{hostname="web-1",region="eu-west-1",service="frontend"} 1
request_count{hostname="web-2",region="us-east-1",service="frontend"} 2
request_count{hostname="web-0",region="eu-west-1",service="frontend"} 3
```

Both labels advance every tick. `hostname` wraps every 3 ticks; `region` wraps every 2. The
full series count is `3 x 2 = 6` unique combinations, visited in a 6-tick cycle.

## Dynamic labels on log scenarios

Dynamic labels work identically on `logs:` entries. Swap `signal_type: metrics` for
`signal_type: logs`, and the rotating label attaches to every log event:

```yaml title="examples/dynamic-labels-logs.yaml"
scenarios:
  - signal_type: logs
    name: app_logs
    log_generator:
      type: template
      templates:
        - message: "Request handled successfully"
      severity_weights:
        info: 1.0
      seed: 42
    dynamic_labels:
      - key: pod_name
        prefix: "api-"
        cardinality: 3
    labels:
      app: sonda
```

Each emitted JSON log event carries `pod_name=api-0`, `api-1`, or `api-2` in rotation. Useful
for testing Loki label indexing or pod-level log aggregation panels.

## Dynamic labels with the Loki sink

Point a scenario with `dynamic_labels:` at a Loki sink and each rotating value becomes its own **Loki stream** — the smallest unit Loki indexes by, identified by its label set. The rotation values auto-promote into the stream label set alongside the scenario's static `labels:`, so a rotation through N values shows up in Grafana as N separate log streams, each queryable by that label.

This is how you build a realistic per-source feed from a single scenario entry: one entry stands in for a fleet of senders, but downstream looks like a fleet — per-source dashboards work, alerts can target a single source, and ingest behaviour exercises real per-stream paths instead of one fat stream.

### Worked example — 20 BGP peers from one scenario

Say you want to test a Grafana dashboard that breaks down BGP neighbor state per peer, or an alert that fires when a *specific* peer flaps. You need 20 distinct log streams that share most of their labels but differ in `peer_address`. One scenario with a 20-element `values` list does it:

```yaml title="examples/bgp-peer-logs.yaml"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 60s
  encoder:
    type: json_lines
  sink:
    type: loki
    url: http://localhost:3100
scenarios:
  - id: srl1_bgp_logs
    signal_type: logs
    name: srl1_bgp_logs
    labels:
      device: srl1
      vendor_facility_process: BGP
    dynamic_labels:
      - key: peer_address
        values:
          - "10.1.2.2"
          - "10.1.7.2"
          - "10.1.12.2"
          - "10.1.17.2"
          - "10.1.22.2"
          # ... up to 20 peers
    log_generator:
      type: template
      templates:
        - message: "BGP neighbor state changed to {state}"
          field_pools:
            state: ["established", "active", "open-confirm", "idle"]
```

The peer identity rides on the stream label (`peer_address`), not in the message text, so each Loki stream stays consistent with its label set.

### What lands in Loki

Loki sees one stream per peer. The label set on each stream is the merge of the scenario's `labels:` with the current `dynamic_labels` value:

```
{device="srl1", peer_address="10.1.2.2",  vendor_facility_process="BGP"}
{device="srl1", peer_address="10.1.7.2",  vendor_facility_process="BGP"}
{device="srl1", peer_address="10.1.12.2", vendor_facility_process="BGP"}
...
```

In Grafana's stream selector you'll see 20 distinct streams under `device="srl1"`, one per peer address.

### What queries become possible

Because each peer is its own stream, all the usual LogQL shapes work against the dataset:

```logql
{peer_address="10.1.2.2"}
```
Returns every log line for one specific peer — useful for drilling into a single neighbor.

```logql
{device="srl1"} |= "established"
```
Returns establishment events across all peers on the device — useful for spotting the global pattern.

```logql
sum by (peer_address) (count_over_time({device="srl1"}[5m]))
```
Returns a per-peer event count over the last 5 minutes — the shape you'd graph as "which peers are noisiest right now".

### Cardinality and the per-push cap

Loki indexes by stream, and unique stream count is the dimension that drives ingester memory and index cost. Pushing too many distinct streams in a single request is the classic way to overload an ingester, so the Sonda Loki sink caps unique streams **per push** at `max_streams_per_push` (default `128`). A flush that would exceed the cap fails with a message naming the offending count and the cap.

The cap is per-flush, not lifetime — a scenario that rotates through hundreds of values can still work if each flush stays under the cap (drop `batch_size` on the sink so each push carries fewer entries, hence fewer distinct streams).

If your Loki ingester is sized for higher cardinality, raise the cap on the [Loki sink](../configuration/sinks.md#loki):

```yaml
sink:
  type: loki
  url: http://localhost:3100
  max_streams_per_push: 512
```

### Stream-count preview when posting to `sonda-server`

When you POST a scenario to `sonda-server`, the response includes a registration-time preview naming the predicted stream count and the active cap — so high-cardinality misconfigurations surface at submission time, not the first time a flush fails:

```
scenario entry 'srl1_bgp_logs' will produce up to 20 distinct Loki streams
(dynamic_labels: peer_address). max_streams_per_push is 128.
```

See the [`dynamic_labels` field reference](../configuration/scenario-fields.md#dynamic-labels) for the full set of options on the rotating label itself.

## Runnable examples

| File | Signal | Strategy | What to look for |
|------|--------|----------|------------------|
| `examples/dynamic-labels-fleet.yaml` | metrics | counter (10) | 10 distinct `hostname` values on `node_cpu_usage` |
| `examples/dynamic-labels-regions.yaml` | metrics | values list | 3-element `region` cycle on `api_latency_seconds` |
| `examples/dynamic-labels-multi.yaml` | metrics | counter + values | Two rotating labels on a request counter |
| `examples/dynamic-labels-logs.yaml` | logs | counter (3) | Rotating `pod_name` on structured log events |

Run any of them:

```bash
sonda run examples/dynamic-labels-fleet.yaml
sonda run examples/dynamic-labels-logs.yaml
```

## Interaction with other fields

!!! info "Merge order: dynamic labels win on collision"
    Dynamic labels are merged on top of the scenario's static `labels:` on every tick. If a
    dynamic label key collides with a static label key, the dynamic value wins.

`dynamic_labels` composes cleanly with the rest of the scenario surface:

- **`cardinality_spikes`** can coexist with dynamic labels -- spike labels appear only during
  the spike window, while dynamic labels are always present.
- **`gaps`** take priority over both. During a gap, no events are emitted regardless of label
  strategy.
- **`after:` and `phase_offset`** do not interact with label rotation. The tick counter starts
  at 0 whenever the scenario starts emitting; phase-offsetting the start just delays when
  label rotation begins.
- **Packs** expand before dynamic labels apply. If you attach `dynamic_labels` to a
  pack-backed entry, every metric expanded from the pack gets the same rotating label.

## See also

- [**Scenario Fields -- Dynamic labels**](../configuration/scenario-fields.md#dynamic-labels)
  -- full field reference for `key`, `prefix`, `cardinality`, `values`.
- [**Scenario Fields -- Cardinality spike window**](../configuration/scenario-fields.md#cardinality-spike-window)
  -- use this instead when you want a label to appear only during recurring time windows.
- [**Capacity Planning**](capacity-planning.md) -- stress-testing ingest pipelines with high
  cardinality (includes an always-on fleet pattern).
- [**Example Scenarios -- Dynamic Labels**](examples.md#dynamic-labels) -- catalog entry for
  the four runnable YAML files.
