*[OTLP]: OpenTelemetry Protocol -- the wire format used by OpenTelemetry collectors and SDKs to ship traces, metrics, and logs.
*[OTel]: Short for OpenTelemetry, the CNCF observability framework for traces, metrics, and logs.
*[TSDB]: Time-series database -- storage engine optimised for timestamp-indexed numeric samples (e.g., Prometheus, VictoriaMetrics).
*[remote-write]: Prometheus protocol for pushing samples from a producer to a TSDB over HTTP, using protobuf + Snappy compression.
*[exposition format]: Prometheus text format for metrics scraped from an HTTP endpoint (`# HELP`, `# TYPE`, `name{label="v"} value`).
*[cardinality]: Number of unique label-value combinations for a metric. High cardinality blows up TSDB memory and index size.
*[scrape]: Prometheus pull model -- the server periodically GETs an exposition endpoint to ingest metrics.
*[line protocol]: InfluxDB text format: `measurement,tag=v field=v timestamp`. Used by Telegraf and InfluxDB ingest.
*[PromQL]: Prometheus Query Language -- the query syntax for selecting and aggregating time-series data.
*[LogQL]: Loki's query language -- combines label selectors with log line filters and metric extractors.
*[Loki]: Grafana's log aggregation system. Stores logs indexed by labels rather than full-text.
*[VictoriaMetrics]: High-performance Prometheus-compatible TSDB with native remote-write and import endpoints.
*[Alertmanager]: Prometheus component that handles alert routing, deduplication, grouping, and silencing.
*[recording rule]: Prometheus rule that pre-computes a query and stores the result as a new time series.
*[SLO]: Service Level Objective -- a target reliability level (e.g., 99.9% availability over 30 days).
*[SLI]: Service Level Indicator -- the measured value (latency, error rate) used to evaluate an SLO.
*[scenario]: Sonda's unit of work -- a YAML file describing what to generate, how, and where to send it.
*[generator]: Sonda component that produces synthetic events (metrics, logs) according to a pattern.
*[encoder]: Sonda component that serialises events into a wire format (Prometheus text, OTLP, JSON lines).
*[sink]: Sonda component that delivers encoded bytes to a destination (stdout, HTTP push, Kafka, OTLP gRPC).
*[metric pack]: Curated bundle of generators that simulate a known system (Linux node, NGINX, network device).
*[Telegraf]: InfluxData's plugin-driven agent for collecting and shipping telemetry.
*[Containerlab]: Tool for spinning up multi-vendor network topologies as containers, used in the netobs lab.
*[SASL]: Simple Authentication and Security Layer -- pluggable auth framework used by Kafka brokers.
*[mTLS]: Mutual TLS -- both client and server present certificates to authenticate each other.
