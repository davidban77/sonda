---
title: Encoders
description: Reference for every Sonda encoder — wire formats like prometheus_text and json_lines, their parameters, defaults, and example output.
---

# Encoders

An encoder serializes events into a wire format before a sink writes them. Set the format with the `encoder.type` field. Metric scenarios default to `prometheus_text`; log scenarios default to `json_lines`.

Switch the encoder below to see the same scenario — one sine metric with two labels — come out in each format. The bytes are produced by the real engine, in your browser, not pasted in.

<div class="sonda-livegen" data-gen="encoders" markdown="0"></div>

The three formats offered here are the ones compiled into the browser build. `remote_write`, `otlp`, `gnmi` and `snmp` are feature-gated and are documented below with static examples for that reason; the sections that follow cover every encoder, including those.

## prometheus_text

Prometheus text exposition format (v0.0.4). Each event becomes one line:

```
metric_name{label="value"} 42.0 1700000000000
```

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="Prometheus text encoder"
encoder:
  type: prometheus_text
```

```yaml title="With precision"
encoder:
  type: prometheus_text
  precision: 2
```

```text title="Wire format (precision: 2, value 99.60573)"
cpu_usage{host="web-01"} 99.61 1774279696105
```

Text formats keep trailing zeros. A value of `100.0` with `precision: 2` renders as `100.00`.

This encoder accepts metrics only. Log events are not supported.

## influx_lp

InfluxDB line protocol. Each event becomes one line with tags, a field, and a nanosecond timestamp:

```
metric_name,tag=value field_key=42.0 1700000000000000000
```

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `field_key` | string | no | `"value"` | The InfluxDB field key for the metric value. |
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="InfluxDB line protocol encoder"
encoder:
  type: influx_lp
  field_key: cpu_percent
```

```yaml title="With precision"
encoder:
  type: influx_lp
  precision: 4
```

```text title="Wire format (precision: 4, value 99.60573)"
test_influx,host=web-01 value=99.6057 1774279709667342000
```

This encoder accepts metrics only. Log events are not supported.

## json_lines

JSON Lines (NDJSON) format. Each event is one JSON object per line.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="JSON Lines encoder"
encoder:
  type: json_lines
```

```yaml title="With precision"
encoder:
  type: json_lines
  precision: 3
```

For metrics:

```json title="Metric wire format"
{"name":"cpu_usage","value":50.0,"labels":{"host":"web-01"},"timestamp":"2026-03-23T15:28:32.321Z"}
```

```json title="Metric wire format (precision: 3, value 99.60573)"
{"name":"cpu_usage","value":99.606,"labels":{"host":"web-01"},"timestamp":"2026-03-23T15:28:32.321Z"}
```

!!! note
    JSON has no trailing-zero concept. With `precision: 2`, a value of `100.0` still renders as `100.0` in JSON output, not `100.00`. The rounding still applies; JSON simply does not add trailing zeros.

For logs:

```json title="Log wire format"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"test log","fields":{}}
```

The `precision` field only affects metric values. Log events have no numeric value to format.

This is the default encoder for log scenarios. It supports both metrics and logs.

## syslog

RFC 5424 syslog format. Encodes log events as syslog lines.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `hostname` | string | no | `"sonda"` | The HOSTNAME field in the syslog header. |
| `app_name` | string | no | `"sonda"` | The APP-NAME field in the syslog header. |

```yaml title="Syslog encoder"
encoder:
  type: syslog
  hostname: web-01
  app_name: myapp
```

```text title="Wire format"
<14>1 2026-03-23T15:29:02.483Z sonda sonda - - - test log
```

This encoder accepts logs only. Metric events are not supported.

## remote_write

Prometheus remote write protobuf format. Encodes metrics as length-prefixed protobuf `TimeSeries` messages.

!!! note
    This encoder requires the `remote-write` Cargo feature flag. Pre-built release binaries include this feature. To build from source: `cargo build --features remote-write -p sonda`.

No additional parameters.

```yaml title="Remote write encoder"
encoder:
  type: remote_write
```

Pair this encoder with the `remote_write` sink. The sink handles batching, snappy compression, and the HTTP POST with the correct protocol headers. See [Sinks — remote_write](sinks.md#remote_write) for details.

## otlp

OTLP protobuf format. Encodes metrics as OTLP `Gauge` data points and logs as OTLP `LogRecord` messages, using length-prefixed protobuf serialization.

!!! warning "Not included in the pre-built binaries"
    OTLP is an optional feature. The release binaries and the published Docker image do **not** include it. Two ways to get it:

    - **As a binary** — build from source: `cargo build --features otlp -p sonda`.
    - **As a container** — build the image yourself with OTLP compiled in. See [Building the image](../deploy/docker.md#building-the-image).

No additional parameters.

```yaml title="OTLP encoder"
encoder:
  type: otlp
```

Pair this encoder with the `otlp_grpc` sink. The sink handles batching and gRPC delivery to an OpenTelemetry Collector. See [Sinks — otlp_grpc](sinks.md#otlp_grpc) for details.

The encoder supports both metrics and logs. Set the sink's `signal_type` to match your scenario type (`metrics` or `logs`).

## gnmi

gNMI `Notification` protobuf. Each metric event becomes one `Notification` holding one `Update`, and the notification carries the event timestamp in nanoseconds:

- **Path:** rendered from a template for that series.
- **Value:** a `TypedValue` of the metric's value type.
- **Prefix:** the encoder's `origin` and a `target` taken from one label.

Pair this encoder with the `gnmi_target` sink, which serves the notifications to gNMI clients such as Telegraf. Each takes only the other. See [Sinks — gnmi_target](sinks.md#gnmi_target) for details.

!!! note
    This encoder requires the `gnmi` Cargo feature flag. Pre-built release binaries and the published Docker image include this feature. To build from source: `cargo build --features gnmi -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `path` | string | yes, unless every metric has a `paths:` entry | -- | Path template every series is rendered from. See [Path templates](#path-templates). |
| `paths` | map | no | `{}` | Per-metric templates keyed by exact metric name. A metric listed here is rendered from its own template instead of `path`. |
| `values` | map | no | `{}` | Per-metric value type keyed by exact metric name. Unlisted metrics are `double`. See [Value types](#value-types). |
| `origin` | string | no | `"openconfig"` | `Path.origin` on every notification's prefix. |
| `target_label` | string | no | `"device"` | Label whose value becomes `Path.target` on the prefix. |
| `drop_labels` | list of strings | no | `[]` | Labels deliberately left out of the path. |

```yaml title="gNMI encoder"
encoder:
  type: gnmi
  origin: openconfig-interfaces
  target_label: device
  path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
  values:
    in_octets: uint
    out_octets: uint
    oper_status:
      enum: { 1: UP, 2: DOWN }
  paths:
    oper_status: "/interfaces/interface[name={ifName}]/state/oper-status"
```

With that block, `in_octets{device="rtr-core-01", ifName="GigabitEthernet0/0/0"}` is sent with:

- **Path:** `/interfaces/interface[name=GigabitEthernet0/0/0]/state/counters/in-octets`.
- **Prefix:** origin `openconfig-interfaces` and target `rtr-core-01`.
- **Value:** a `uint_val`.

`oper_status` uses its own path from `paths:` and is sent as the string `UP` or `DOWN`.

### Paths and values from a pack

A pack's metrics can carry a `gnmi:` hint, a path template and a value type:

```yaml title="A pack metric with a gNMI hint"
# sonda:static - a pack fragment, not a scenario
metrics:
  - name: oper_status
    generator: { type: constant, value: 1.0 }
    gnmi:
      path: "/interfaces/interface[name={ifName}]/state/oper-status"
      value:
        enum: { 1: UP, 2: DOWN, 3: TESTING }
```

When a pack entry's encoder is `gnmi`, each hint fills `paths:` and `values:` for its metric. Your entries win: a `paths:` entry for a metric replaces its hint's path, a `values:` entry its hint's value type, each on its own. Metrics without a hint use `path` and the `double` default as usual. Under any other encoder the hints are not used. The built-in [`openconfig_interface`](builtin-packs.md#openconfig_interface) pack carries a hint on every metric, so its encoder block needs only `origin` and `target_label`.

### Path templates

- Segments are separated by `/`. A leading `/` is optional.
- A segment is an element name, optionally followed by one or more `[key=value]` groups. Key names are literal, and a key appears at most once per segment.
- An element name or a key value is either literal text or exactly one placeholder. A brace anywhere else is an error.
- A key value may be an optional placeholder, `{label?}`. The key is rendered when the event carries the label and left out when it does not. An element name cannot be optional: an element is never left out.
- `{name}` renders the metric name with `_` replaced by `-`, so `in_octets` becomes `in-octets`. Any other `{label}` renders that label's value as it is.
- A template has no `origin:` prefix; the origin comes from `origin`. A colon anywhere else, in a module-prefixed element name or in a key value, is literal text.
- A rendered element name must be non-empty and contain no `/`, `[` or `]`. Key values may contain `/`, as in `GigabitEthernet0/0/0`.

Every label must be accounted for, so that two series never share a path. This covers each key in an entry's `labels` and `dynamic_labels`, and every `cardinality_spikes` label. Each one must be either:

- referenced by the entry's template, as `{label}` or as an optional `{label?}`,
- the `target_label`, or
- listed in `drop_labels`.

A file that breaks this is refused with the label named. A `cardinality_spikes` label exists only inside its window, so it cannot be a required `{label}`. Reference it as an optional key, as in [Cardinality spikes as new series](#cardinality-spikes-as-new-series), or list it in `drop_labels`; it needs neither when it is the `target_label` or shares its key with a label the template references. An optional placeholder may name a label an entry never carries: its key is always left out, so one template can serve entries with and without the spike.

Histogram and summary entries take the encoder too:

- **Histogram:** emits `<name>_bucket` with an `le` label, plus `<name>_sum` and `<name>_count`. The bucket template must reference `{le}`, and `le` may not be dropped.
- **Summary:** emits `<name>` with a `quantile` label, plus `<name>_sum` and `<name>_count`. The template must reference `{quantile}`, and `quantile` may not be dropped.
- **`_sum` and `_count`:** these series carry no `le` or `quantile`, so they need a template without `{le}` or `{quantile}`. That is either their own `paths:` entries (`<name>_sum`, `<name>_count`), `path` when the bucket or quantile series has a `paths:` entry of its own, or one `path` for all three that writes the key as `[le={le?}]` or `[quantile={quantile?}]`. Validation names any series whose template uses a key it does not carry.

### Cardinality spikes as new series

A `cardinality_spikes` label referenced as an optional key gives every spike value a path of its own. Inside the window a gNMI client sees one new series per value beside the base path; outside it the key is left out and the metric is written to its base path. The complete scenario is [`examples/gnmi-spike.yaml`](https://github.com/davidban77/sonda/blob/main/examples/gnmi-spike.yaml):

```yaml title="examples/gnmi-spike.yaml (excerpt)"
# sonda:static - excerpt only; a gnmi_target listens for a client, which the browser playground cannot provide
version: 2
kind: runnable

defaults:
  rate: 1
  encoder:
    type: gnmi
    origin: openconfig-interfaces
    target_label: device
    path: "/interfaces/interface[name={ifName}][spike={spike?}]/state/counters/{name}"
    values:
      in_octets: uint
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400

scenarios:
  - signal_type: metrics
    name: in_octets
    generator: { type: step, start: 0, step_size: 125000 }
    labels: { device: rtr-edge-01, ifName: GigabitEthernet0/0/0 }
    cardinality_spikes:
      - label: spike
        every: 2m
        for: 30s
        cardinality: 20
```

For the first 30 seconds of every 2 minutes, `in_octets` is sent on 20 paths, `/interfaces/interface[name=GigabitEthernet0/0/0][spike=spike_0]/state/counters/in-octets` to `[spike=spike_19]`. For the rest of each cycle it is sent on `/interfaces/interface[name=GigabitEthernet0/0/0]/state/counters/in-octets`.

A series a spike creates keeps its last value until the scenario that publishes it stops. It is not removed when the window closes, so after the first window the target serves the base path and all 20 spike paths. [Sinks — gnmi_target](sinks.md#gnmi_target) describes when a series leaves the target.

Every path the metric's events can write is checked against every other metric's paths: here, the base path and one path per spike value. A metric whose path equals the base path is refused, just as two metrics on one path are.

### Value types

| `values:` entry | `TypedValue` field | Encoding |
|-----------------|--------------------|----------|
| `double` (default) | `double_val` | The value as it is. |
| `uint` | `uint_val` | Rounded to the nearest integer, halves away from zero. A non-finite value, or one that rounds below 0 or above the unsigned 64-bit range, is rejected. |
| `int` | `int_val` | Rounded the same way. A non-finite value or one outside the 64-bit range is rejected. |
| `bool` | `bool_val` | `true` when the rounded value is not 0. A non-finite value is rejected. |
| `string` | `string_val` | The shortest decimal text that reads back as the same value. |
| `enum: { 1: UP, 2: DOWN }` | `string_val` | The label for the rounded value. A value with no entry, or a non-finite value, is rejected. |

`enum` keys are integers (`1` or `"1"`). Generators that emit gNMI states, such as `flap` with `enum: oper_state` (UP = 1, DOWN = 2), pair with an `enum` map of the same codes.

A rejected event is skipped and counted in the scenario's errors, and the scenario keeps running.

This encoder accepts metrics only. Log events are not supported.

## snmp

SNMP publish records. Each metric event becomes one BER-encoded `VarBind`:

- **OID:** the instance OID, rendered from the metric's template.
- **Value:** the metric's value as its SNMP type.

A record carries no timestamp: an SNMP agent answers each poll with the latest value it holds.

Pair this encoder with the `snmp_agent` sink. Each takes only the other.

!!! note
    This encoder requires the `snmp` Cargo feature flag.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `oids` | map | yes, an entry for every metric | -- | OID template per metric, keyed by exact metric name. See [OID templates](#oid-templates). |
| `types` | map | no | `{}` | Per-metric value type keyed by exact metric name. Unlisted metrics are `gauge32`. See [SNMP value types](#snmp-value-types). |
| `label_columns` | map | no | `{}` | OID template per label whose value the agent serves as a string column, keyed by label. See [Label columns](#label-columns). |
| `agent_label` | string | no | `"device"` | Label that names the device. It needs no placeholder and is never part of an OID. |
| `drop_labels` | list of strings | no | `[]` | Labels deliberately left out of the OID. |

```yaml title="SNMP encoder"
encoder:
  type: snmp
  agent_label: device
  oids:
    ifHCInOctets: "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"
    ifHCOutOctets: "1.3.6.1.2.1.31.1.1.1.10.{ifIndex}"
    ifOperStatus: "1.3.6.1.2.1.2.2.1.8.{ifIndex}"
    ifInErrors: "1.3.6.1.2.1.2.2.1.14.{ifIndex}"
    ifOutErrors: "1.3.6.1.2.1.2.2.1.20.{ifIndex}"
  types:
    ifHCInOctets: counter64
    ifHCOutOctets: counter64
    ifOperStatus: integer
    ifInErrors: counter32
    ifOutErrors: counter32
  drop_labels: [job]
```

With that block, `ifHCInOctets{device="rtr-core-01", ifIndex="3", job="snmp"}` is the instance `1.3.6.1.2.1.31.1.1.1.6.3` with a Counter64 value. `device` is the agent label and `job` is dropped, so neither changes the OID.

### Label columns

A label can be served as a column of its own: its value, an OCTET STRING, at an OID rendered from the same index labels as the metrics, which is how IF-MIB gives each interface a name beside its counters:

```yaml title="ifName and ifAlias served beside the counters"
encoder:
  type: snmp
  oids:
    ifHCInOctets: "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"
  types:
    ifHCInOctets: counter64
  label_columns:
    ifName: "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}"
    ifAlias: "1.3.6.1.2.1.31.1.1.1.18.{ifIndex}"
  drop_labels: [job]
```

With that block, `ifHCInOctets{device="rtr-core-01", ifIndex="3", ifName="GigabitEthernet0/0/2", job="snmp"}` serves `1.3.6.1.2.1.31.1.1.1.6.3` as a Counter64 and `1.3.6.1.2.1.31.1.1.1.1.3` as the string `GigabitEthernet0/0/2`. A label named in `label_columns` needs no placeholder and no `drop_labels` entry, and must be a static label of the entry, set in `labels` or a pack's shared labels: series that differ only in a label-column label share the metric's OID, so a `dynamic_labels` or `cardinality_spikes` label, or a label the entry does not have, is refused. Each template is a string and follows the [OID template](#oid-templates) rules, and uses only placeholders that every `oids:` template uses. The agent holds a column's instance while any series carrying it is live (see [the `snmp_agent` sink](sinks.md#snmp_agent)).

### OIDs and types from a pack

A pack's metrics can carry an `snmp:` hint, an OID template and a value type:

```yaml title="A pack metric with an SNMP hint"
# sonda:static - a pack fragment, not a scenario
metrics:
  - name: ifHCInOctets
    generator: { type: step, start: 0.0, step_size: 125000.0 }
    snmp:
      oid: "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"
      type: counter64
```

When a pack entry's encoder is `snmp`, each hint fills `oids:` and `types:` for its metric, and the pack's `snmp_label_columns` map fills `label_columns:`. Your entries win: an `oids:` entry for a metric replaces its hint's OID, a `types:` entry its hint's type, a `label_columns:` entry the pack's template for that label, each on its own. Under any other encoder the hints are not used.

### OID templates

- A template is the object's OID in dotted decimal, without a leading dot, followed by its index: one or more placeholders, `{label}` or `{label:str}`: `1.3.6.1.2.1.2.2.1.8.{ifIndex}`.
- A template is a string. Quote it: an unquoted value such as `1.30` is a YAML number, which would read as `1.3`, and is refused with the field named.
- Placeholders are the last arcs. A literal arc after a placeholder is an error.
- The object's OID has at least two arcs. The first is 0, 1 or 2, and when it is 0 or 1 the second is below 40. A template has at most 128 arcs, counting a `{label:str}` placeholder as two, and so does every OID it renders.
- A template with no placeholder is a scalar and must end in `.0`, its one instance: `1.3.6.1.4.1.8072.9999.1.1.0` is the instance of the scalar `1.3.6.1.4.1.8072.9999.1.1`.
- A `{label}` placeholder renders its label's value as one arc. The value must be an unsigned 32-bit integer written in decimal, without a sign or leading zeros: `3`, not `03`.
- A `{label:str}` placeholder renders its label's value as a string index: its length in bytes, then one arc per byte. `GigabitEthernet0/0/0` is `20.71.105.103.97.98.105.116.69.116.104.101.114.110.101.116.48.47.48.47.48`. Any value but the empty string renders; an event with an empty one is rejected. Any other `{label:...}` form is refused.
- No template, in `oids:` or `label_columns:`, may render an OID under `1.3.6.1.2.1.1`, the system group, which the agent serves itself.

A scenario file is refused, with the metric and the label named, when any of these do not hold for an entry:

- Every metric the entry emits has an `oids:` entry, from the encoder block or a pack hint.
- Every placeholder names a key of the entry's `labels` or `dynamic_labels`. A `cardinality_spikes` label cannot be a placeholder: events carry it only while its window is open.
- Every placeholder of a `label_columns` template is a placeholder of every `oids:` template.
- Every `label_columns` label is a static label of the entry, not a `dynamic_labels` or `cardinality_spikes` label.
- Every value the label of a `{label}` placeholder can take is an arc: its static value, every `values:` entry of a dynamic label, and every value of a `counter` dynamic label or `counter` cardinality spike on it. A counter's values are its prefix followed by a number, and the default prefix (`<label>_`) is not a number, so set `prefix: ""`. A `random` cardinality spike on such a label is refused. A `{label:str}` placeholder takes any value.
- Every label key of the entry, from `labels`, `dynamic_labels` and `cardinality_spikes`, is a placeholder, a `label_columns` label, the `agent_label`, or listed in `drop_labels`. Otherwise series that differ only in that label would share one OID. Series that differ only in a `label_columns` label do share the metric's OID, which is why that label must be static.
- No two templates can render the same OID for the entry: its metric's template and each `label_columns` template are compared with each other and with every other `oids:` template. Two templates can when they have the same number of arcs and the arcs at every position can be equal:
    - two numbers when they are the same number;
    - a number and a placeholder when the number is one of the values the placeholder's label takes;
    - two placeholders when their labels share a value.

    A label's values are its static value, every entry of a `values` dynamic label, and the values of a `counter` dynamic label or `counter` cardinality spike: its prefix followed by `0` to `cardinality - 1`, with a spike's values outside its window as well. A label the entry does not have has none. `1.3.6.1.2.1.31.1.1.1.6.0` beside `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}` is refused when `ifIndex` is `0` or takes `0` among its values; the refusal names both templates, the OID and the label values that render it.

    A label used twice in one template takes the same value at both places: `1.3.6.1.4.1.8072.9999.2.1.{ifIndex}.{ifIndex}` renders `…2.1.0.0` and `…2.1.1.1` when `ifIndex` takes `0` and `1`, never `…2.1.1.0`, so it is accepted beside the scalar `1.3.6.1.4.1.8072.9999.2.1.1.0`. The same label in the other template can take another value, because the two render different series. The check is exact but in one case: where both positions hold `counter` labels whose values meet, it assumes they can be equal, so it may refuse two templates that never render the same OID; it never accepts two that do.

    When either template has a `{label:str}` placeholder, the two can render the same OID exactly when one's object OID equals or begins with the other's: `1.3.6.1.4.1.8072.9999.3.1.{ifName:str}` and `1.3.6.1.4.1.8072.9999.3.2.{ifName:str}` are accepted, `1.3.6.1.4.1.8072.9999.3.{ifName:str}` beside either is refused.

Histogram, summary and logs entries refuse both this encoder and the `snmp_agent` sink.

### SNMP value types

| `types:` entry | BER type | Range |
|----------------|----------|-------|
| `gauge32` (default) | Gauge32 | 0 to 4294967295 |
| `integer` | INTEGER | -2147483648 to 2147483647 |
| `counter32` | Counter32 | 0 to 4294967295 |
| `counter64` | Counter64 | 0 to 18446744073709551615 |
| `unsigned32` | Unsigned32, encoded as Gauge32 | 0 to 4294967295 |
| `timeticks` | TimeTicks, in hundredths of a second | 0 to 4294967295 |
| `string` | OCTET STRING | The shortest decimal text that reads back as the same value. |
| `opaque_float` | Opaque holding an IEEE 754 single, as net-snmp writes it | The value converted to a single; one beyond about ±3.4 × 10³⁸, which converts to an infinity, is rejected. |
| `opaque_double` | Opaque holding an IEEE 754 double, as net-snmp writes it | Any finite value, exactly. |
| `ipaddress` | IpAddress | Rounded, 0 to 4294967295, as four octets most significant first: `167772161` is `10.0.0.1`. |
| `{ enum: { 1: up, 2: down } }` | OCTET STRING | The label of the rounded value's entry: `1` is `up`. A value with no entry is rejected. |

The integer types, `ipaddress` and the enum round the value to the nearest integer, halves away from zero. Every type but `string` rejects a value that is not finite, and the integer types and `ipaddress` one that rounds outside the type's range. `string` writes a value that is not finite as its text: `NaN`, `inf` or `-inf`. The encoder does not wrap a counter: give the generator a `max`, as the `step` generator takes, for a counter that wraps.

The enum form gives the label of each code, as the `gnmi` encoder's `values:` enum does. Codes are integers, written bare or quoted (`1` or `"1"`); labels are text. The map needs at least one entry. A `flap` generator with `enum:` drives one, as `link_state` in [`examples/snmp-telegraf.yaml`](https://github.com/davidban77/sonda/blob/main/examples/snmp-telegraf.yaml) does:

```yaml title="An enum string"
encoder:
  type: snmp
  oids:
    link_state: "1.3.6.1.4.1.8072.9999.1.3.0"
  types:
    link_state: { enum: { 1: up, 2: down } }
```

net-snmp prints an Opaque float and an Opaque double alike, as `Opaque: Float:` with the value's digits. A pack's `snmp:` hint takes the same forms for its `type:`.

A rejected event is skipped and counted in the scenario's errors, and the scenario keeps running.

This encoder accepts metrics entries only.

## Value precision

The `prometheus_text`, `influx_lp`, and `json_lines` encoders accept an optional `precision` field that controls how many decimal places appear in metric values.

- **Range**: 0 to 17. An f64 carries roughly 15--17 significant digits.
- **Default**: omit the field to keep full f64 precision.
- **Effect**: values are rounded to the specified number of decimal places using standard rounding.

Set `precision` on the encoder block in your scenario YAML:

```yaml title="precision-formatting.yaml (encoder fragment)"
encoder:
  type: prometheus_text
  precision: 2    # 99.60573 becomes 99.61
```

The `syslog`, `remote_write`, `otlp`, `gnmi`, and `snmp` encoders do not support `precision`. Syslog encodes log events only, with no numeric values; remote write, OTLP and gNMI use binary protobuf encoding, and SNMP binary BER.

See [`examples/precision-formatting.yaml`](https://github.com/davidban77/sonda/blob/main/examples/precision-formatting.yaml) for a complete scenario that uses precision with several encoders.

## Encoder compatibility

| Encoder | Metrics | Logs | Precision | Feature flag |
|---------|---------|------|-----------|--------------|
| `prometheus_text` | yes | no | yes | -- |
| `influx_lp` | yes | no | yes | -- |
| `json_lines` | yes | yes | yes | -- |
| `syslog` | no | yes | -- | -- |
| `remote_write` | yes | no | -- | `remote-write` |
| `otlp` | yes | yes | -- | `otlp` |
| `gnmi` | yes | no | -- | `gnmi` |
| `snmp` | yes | no | -- | `snmp` |
