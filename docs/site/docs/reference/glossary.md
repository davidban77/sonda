---
title: Glossary
description: Definitions for Sonda-specific terms and the observability jargon used across the docs.
---

# Glossary

If you're new to observability or to Sonda, start here. Every term the docs assume you know is defined below, with a link to where it's used in depth. Skim the headings and come back when you find an unfamiliar word.

## A

### `after:` clause

A scenario field that starts the downstream scenario once the upstream scenario's value crosses a threshold. The downstream scenario waits (in the `pending` lifecycle state) until the trigger fires, then runs to completion. Use `after:` for one-shot triggers. For ongoing gating that pauses and resumes as the upstream value changes, use [`while:`](#while-clause). See [Scheduling — Dependencies](../build/scheduling.md#dependencies-after-and-while).

### Alert rule

A query expression that an alert evaluator re-checks on a fixed interval. When the expression returns a non-empty result for the rule's `for:` window, the alert fires. Sonda drives synthetic metrics to test these rules. The query language is [PromQL](#promql). The evaluator is [Alertmanager](#alertmanager) or [vmalert](#vmalert). The interval is the [evaluation tick](#evaluation-tick). See [Alert testing](../test/alert-testing.md).

### Alertmanager

The Prometheus component that handles alert routing, deduplication, grouping, silencing, and delivery to receivers (PagerDuty, Slack, webhooks). Sonda's end-to-end pipeline patterns include Alertmanager to validate that an alert fires *and* reaches its destination. See [End-to-end pipelines](../test/end-to-end-pipelines.md).

## B

### Burst

A recurring time window during which a scenario emits at an elevated rate, mimicking a traffic spike. Configured with the `bursts:` field. See [Scheduling — Gaps and bursts](../build/scheduling.md#gaps-and-bursts).

## C

### Cardinality

The number of unique label-value combinations a metric has. For example, `http_requests_total` with a `method` label of 5 values and a `status_code` label of 10 values has cardinality 50. High cardinality grows [TSDB](#tsdb) memory and index size. Low cardinality limits the dimensions you can slice on. See [Capacity planning](../test/capacity-planning.md).

### Cardinality spike

A sudden burst of new label combinations on a metric. Common causes are a buggy deployment, a runaway user-generated label, or a misbehaving scraper. The [TSDB](#tsdb) [ingester](#ingester) — the component that writes incoming samples to storage — can run out of memory when the burst is large enough. Sonda's `cardinality_spikes:` field reproduces the pattern. See [Scheduling — Cardinality spikes](../build/scheduling.md#cardinality-spikes).

### Catalog

A directory of scenario YAML files. Sonda walks it with `--catalog <dir>` and indexes each file by name. You can then run any entry with `sonda run @name`. Runnable scenarios and composable [packs](#pack) live side by side. See [Catalogs and packs](../build/catalogs-and-packs.md).

## D

### Dynamic labels

Labels whose values rotate per tick across a bounded, predictable set. Use them when one scenario entry needs to represent many sources: 10 hostnames, 3 regions, or 20 BGP peers. Dynamic labels are always on. [Cardinality spikes](#cardinality-spike) are limited to a configured time window. See [Scheduling — Dynamic labels](../build/scheduling.md#dynamic-labels).

## E

### Encoder

The Sonda component that serializes events into a wire format before a [sink](#sink) writes them out. Supported formats include Prometheus text, JSON Lines, InfluxDB line protocol, syslog, remote-write protobuf, and OTLP. Pick the encoder that matches what the receiving backend expects. See [Encoders](../build/encoders.md).

### Entry

One item under a scenario file's `scenarios:` list. Each entry emits exactly one signal — one metric series, one log stream, one histogram, one summary. A scenario file can have many entries running concurrently on shared `defaults:`. See [Concepts — Entry](../build/concepts.md#entry).

### Evaluation tick

The interval at which an alert evaluator re-checks every rule's PromQL expression. Usually 15–60 seconds, set per evaluator ([Alertmanager](#alertmanager) or [vmalert](#vmalert)). This is not the same as Sonda's emission rate. The evaluation tick is how often the evaluator queries samples; the scenario `rate:` is how often Sonda produces them.

### Exposition format

See [Prometheus exposition format](#prometheus-exposition-format).

## G

### Gap

A recurring time window during which a scenario suppresses emission entirely. The metric goes silent, Prometheus treats it as stale, downstream alerts resolve. Configured with the `gaps:` field. See [Scheduling — Gaps and bursts](../build/scheduling.md#gaps-and-bursts).

### Generator

The Sonda component that produces values for each tick of a scenario. For metrics, generators produce `f64` values (sine waves, sawtooths, constants, spikes). For logs, they produce structured log events. For histograms and summaries, they produce sampled distributions. See [Generators](../build/generators.md).

## H

### Histogram

A signal type that records the distribution of observations across pre-defined buckets. Each bucket carries a cumulative count, plus `_sum` and `_count` series. Use histograms for latency, request size, or any metric where you care about percentiles across the population. See [Generators — Histograms](../build/generators.md#histogram-and-summary-generators).

## I

### InfluxDB line protocol

InfluxDB's text format for metrics: `measurement,tag=v field=v timestamp`. Used by Telegraf, InfluxDB ingest, and many downstream consumers. Sonda's `influx_lp` encoder emits this format. See [Encoders — `influx_lp`](../build/encoders.md#influx_lp).

### Ingester

The component of a [TSDB](#tsdb) (Prometheus, VictoriaMetrics, etc.) that receives incoming samples and writes them to storage. Sonda's cardinality patterns are designed to stress-test ingesters before they fail in production. See [Capacity planning](../test/capacity-planning.md).

## K

### Kafka

A distributed event streaming platform. Sonda's `kafka` sink publishes encoded events to a Kafka topic over a pure-Rust client (no OpenSSL, no C dependencies). Common in observability pipelines as the buffer between producers and downstream consumers. See [Sinks — `kafka`](../build/sinks.md#kafka).

## L

### Label

A key-value tag attached to a metric or log event. Labels are the dimension you slice on in queries: `cpu_usage{host="web-01",region="eu1"}` has two labels. Each new label value increases [cardinality](#cardinality). When cardinality grows too large, the [TSDB](#tsdb) runs out of memory. See [Concepts](../build/concepts.md).

### Line protocol

See [InfluxDB line protocol](#influxdb-line-protocol).

### Loki

Grafana's log aggregation backend. Stores logs indexed by labels rather than full-text. Sonda's `loki` sink batches log lines and POSTs them to Loki over HTTP, with optional per-event dynamic labels that produce one Loki **stream** per rotating value. See [Sinks — `loki`](../build/sinks.md#loki).

### LogQL

Loki's query language. Combines label selectors with log line filters and metric extractors. See [Loki documentation](https://grafana.com/docs/loki/latest/logql/).

## M

### Metric

A numeric value emitted on a regular cadence, typically with one or more [labels](#label). Sonda emits metrics from a `signal_type: metrics` entry driven by a [generator](#generator).

## O

### OTel

Short for OpenTelemetry — the CNCF observability framework for traces, metrics, and logs.

### OTLP

OpenTelemetry Protocol. The wire format used by OpenTelemetry collectors and SDKs to send traces, metrics, and logs. Sonda's `otlp` encoder + `otlp_grpc` sink push to an OpenTelemetry Collector over gRPC. See [Sinks — `otlp_grpc`](../build/sinks.md#otlp_grpc).

## P

### Pack

A reusable bundle of metric names, label schemas, and useful default generators per metric. You write a pack as a `kind: composable` file in your catalog. Any runnable scenario can then reference it with `pack: <name>`. The compiler expands the reference at parse time into one prepared entry per metric in the pack. See [Catalogs and packs — Packs](../build/catalogs-and-packs.md#packs).

### PromQL

Prometheus's query language. Used to select, filter, and aggregate time-series data: `sum by (host) (rate(http_requests_total[5m]))`. Alert rules, recording rules, and Grafana panels all run PromQL. See [Prometheus documentation](https://prometheus.io/docs/prometheus/latest/querying/basics/).

### Prometheus exposition format

Prometheus's plain-text format for metrics scraped from an HTTP endpoint:

```
# HELP cpu_usage CPU usage percent
# TYPE cpu_usage gauge
cpu_usage{host="web-01"} 42.0 1700000000000
```

Sonda's `prometheus_text` encoder emits this format — the default for metric scenarios. See [Encoders — `prometheus_text`](../build/encoders.md#prometheus_text).

## R

### Recording rule

A precomputed PromQL metric. The Prometheus server (or vmalert) evaluates the rule on a schedule and stores the result as a new time series, so expensive queries don't re-execute on every dashboard refresh. See [Recording rules](../test/recording-rules.md).

### remote_write

Prometheus's protocol for pushing samples from a producer to a [TSDB](#tsdb) over HTTP, using protobuf + Snappy compression. Used by Sonda's `remote_write` encoder + sink to push metrics into Prometheus, VictoriaMetrics, vmagent, Thanos, Cortex, or Mimir. See [Sinks — `remote_write`](../build/sinks.md#remote_write).

## S

### SASL

Simple Authentication and Security Layer. A modular authentication framework used by Kafka brokers. Sonda's `kafka` sink supports SASL PLAIN, SCRAM-SHA-256, and SCRAM-SHA-512. See [Sinks — Kafka SASL](../build/sinks.md#kafka-sasl).

### Scenario

The unit of work Sonda runs — a YAML file describing what to generate, how, and where to send it. A scenario file has `version: 2`, a `kind:` (`runnable` or `composable`), shared `defaults:`, and a `scenarios:` list of [entries](#entry). Hand it to `sonda run`. See [Concepts](../build/concepts.md).

### Scrape endpoint

The HTTP URL Prometheus pulls metrics from on its scrape interval. `sonda-server` exposes one scrape endpoint per running scenario at `/scenarios/{id}/metrics`, so Prometheus can scrape Sonda's synthetic output without additional integration code. See [Server API](../deploy/server.md).

### Signal type

The category of telemetry a scenario entry emits: `metrics`, `logs`, `histogram`, or `summary`. Set per entry with the `signal_type:` field; determines which generator block applies (`generator:` for metrics and logs, `distribution:` for histograms and summaries).

### Sink

The Sonda component that delivers encoded bytes to a destination: stdout, file, TCP/UDP socket, HTTP POST, Prometheus remote_write, Loki, Kafka, OTLP gRPC. Configure with the `sink:` block. See [Sinks](../build/sinks.md).

### Sink-error policy

What the runner does when a sink write fails mid-run (a Loki `500`, a TCP reset, an HTTP timeout). Set with the `on_sink_error:` field at the `defaults:` or per-entry level: `warn` logs the error and keeps running; `fail` exits the runner. See [Scenario files — Sink-error policy](../build/scenario-files.md#sink-error-policy).

### SLI

Service Level Indicator. The measured value (latency, error rate, availability) used to evaluate a Service Level Objective.

### SLO

Service Level Objective. A target reliability level — e.g., "99.9% of requests under 200 ms over 30 days."

### Stream

In Loki, the index unit identified by a unique label set. Two log lines with the same labels go to the same stream. If one label value differs, they go to separate streams. Sonda's per-event `dynamic_labels` produce one Loki stream per rotating value. See [Sinks — Loki](../build/sinks.md#loki).

### Summary

A signal type that records the distribution of observations sampled at configured quantiles (`p50`, `p95`, `p99`). Unlike [histograms](#histogram), summaries are computed client-side and cannot be aggregated across instances. Useful for single-source latency reporting; histograms are preferred for fleets. See [Generators — Summary](../build/generators.md#histogram-and-summary-generators).

### Syslog

The RFC 5424 log format. Sonda's `syslog` encoder emits log events as syslog lines for delivery to syslog collectors over TCP/UDP. See [Encoders — `syslog`](../build/encoders.md#syslog).

## T

### Threshold

A numeric boundary that a value must cross to trigger an action. In alerting, an alert fires when the metric crosses the threshold. In Sonda's [`while:`](#while-clause) and [`after:`](#after-clause) clauses, crossing the threshold activates the downstream scenario. See [Scheduling — Dependencies](../build/scheduling.md#dependencies-after-and-while).

### Telegraf

InfluxData's plugin-driven agent for collecting and sending telemetry. Sonda's built-in SNMP and node packs match Telegraf's metric names and labels. Synthetic data then works with dashboards and alert rules built against Telegraf output.

### TSDB

Time-series database. A storage engine optimized for timestamp-indexed numeric samples — Prometheus, VictoriaMetrics, Thanos, Cortex, Mimir, InfluxDB. Sonda doesn't store data; it pushes to (or is scraped by) a TSDB.

## U

### Upstream scenario

In a `while:` or `after:` clause, the upstream scenario is the one whose value the clause references. The scenario carrying the `while:`/`after:` clause is the downstream — its emission depends on the upstream's value. See [Scheduling — Dependencies](../build/scheduling.md#dependencies-after-and-while).

## V

### VictoriaMetrics

A high-performance Prometheus-compatible TSDB with native remote-write and import endpoints. Sonda's bundled Compose stack uses VictoriaMetrics + vmagent + vmalert. See [Docker deployment](../deploy/docker.md).

### vmagent

VictoriaMetrics's lightweight metrics relay agent. Receives remote-write or scrape data and forwards it to one or more TSDB instances. See [VictoriaMetrics docs](https://docs.victoriametrics.com/vmagent.html).

### vmalert

VictoriaMetrics's alert rule and recording rule evaluator. PromQL-compatible. Used in Sonda's bundled Compose stack as the rule engine. See [End-to-end pipelines](../test/end-to-end-pipelines.md).

## W

### `while:` clause

A scenario field that gates emission on an upstream scenario's current value. The dependent scenario emits only while the predicate holds, pauses when it fails, and resumes when it becomes true again. Use `while:` for "the cascade reflects the upstream's lifecycle" patterns. See [Scheduling — Dependencies](../build/scheduling.md#dependencies-after-and-while).
