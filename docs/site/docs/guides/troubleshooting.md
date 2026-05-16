# Troubleshooting

When Sonda isn't behaving as expected, start here. This guide covers the most common issues
and how to resolve them, organized from general diagnostics to specific sink and deployment
problems.

---

## First steps

Before diving into specific issues, run these quick checks.

### Validate your configuration

Use `--dry-run` to parse and validate a scenario without emitting any events:

=== "Scenario file"

    ```bash title="my-scenario.yaml"
    sonda --dry-run run my-scenario.yaml
    ```

=== "Catalog entry"

    ```bash title="./my-catalog"
    sonda --dry-run --catalog ./my-catalog run @cpu-spike
    ```

If the config is valid, Sonda prints the resolved settings and exits with code `0`. If there's
an error, it prints the problem to stderr and exits with code `1`.

### Get diagnostic output

Use `--verbose` to print the resolved config at startup, then run normally. This shows exactly
what Sonda parsed before it starts emitting events:

```bash title="my-scenario.yaml"
sonda --verbose run my-scenario.yaml \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus
```

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success -- scenario completed or `--dry-run` validation passed |
| `1` | Runtime error -- invalid config, sink unreachable, scenario validation failure |
| `2` | Argument parse error -- unknown flag, unrecognized subcommand |

### A scenario stopped emitting silently

A scenario looks alive (the process is running, no error in the foreground) but no data is reaching the backend. This typically means the sink is failing on every write. Sonda's default [`on_sink_error: warn`](../configuration/v2-scenarios.md#sink-error-policy) policy keeps the runner alive through transient sink errors, so the symptom is degradation rather than a crash. Confirm the diagnosis from three places:

- **Stderr `[progress]` banner** -- when a runner exits, the progress reporter prints a one-shot `STOPPED` line that includes the last sink error:

    ```text
    [progress] my_scenario  STOPPED (sink: HTTP 500 from 'http://loki:3100/loki/api/v1/push') | events: 3359 | bytes: 1.0 MB | elapsed: 18h59m
    ```

    No parenthetical means the scenario stopped cleanly (duration expired, Ctrl+C, etc.) and the sink was healthy.

- **`GET /scenarios/{id}/stats`** -- a non-zero `consecutive_failures` with a stale `last_successful_write_at` confirms the sink is wedged. `last_sink_error` carries the message:

    ```bash
    curl -s http://localhost:8080/scenarios/$ID/stats | jq '.consecutive_failures, .last_sink_error'
    ```

    See [Self-observability via /stats](../deployment/sonda-server.md#self-observability-via-stats).

    !!! warning "`total_events` is not a delivery signal for batching sinks"
        Batching sinks (`loki`, `http_push`, `remote_write`, `otlp_grpc`, `kafka`) buffer events and only deliver in bursts. `total_events` increments on every *buffered* write, so a rising counter is **not** proof that anything is reaching the backend. Read the delivery-health fields instead: `last_successful_write_at` (stale or `null` means nothing has landed) and `consecutive_failures` (non-zero means a wedged buffer). See [What a wedged batching sink looks like](../deployment/sonda-server.md#what-a-wedged-batching-sink-looks-like) for the full timeline.

- **Use the `degraded` field for monitoring** -- `GET /scenarios` ships a precomputed `degraded: bool` per scenario, true when the scenario has had sink failures and has not delivered in the last 30 seconds. Use it directly for a readiness probe or alert:

    ```bash
    curl -sS http://localhost:8080/scenarios | jq '.scenarios[] | select(.degraded)'
    ```

    A non-empty result means at least one scenario has stopped delivering. The same expression works as a Kubernetes readiness probe or a Prometheus alert input. If you need a different staleness window than 30 seconds, you can still threshold the raw fields from the per-scenario `/stats` endpoint -- combine `total_sink_failures > 0` with your own staleness check on `last_successful_write_at`.

The recovery is the default behavior: under `on_sink_error: warn`, the runner keeps ticking while you fix the sink (restart Loki, repair DNS, restore the network path). Once the sink accepts a write, `consecutive_failures` resets to `0` and `last_successful_write_at` advances, so any threshold you set clears automatically. If you want a sink failure to hard-fail the run instead, set `on_sink_error: fail` -- see [Sink-error policy](../configuration/v2-scenarios.md#sink-error-policy).

---

## Connection and delivery issues

### Connection refused

You configured a network sink but Sonda reports a connection error.

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `connection refused` on HTTP/TCP sink | Backend is not running or not listening on expected port | Verify the backend is up: `curl -s http://host:port/health` |
| `connection refused` on gRPC (OTLP) | Collector not running, or wrong port (HTTP vs gRPC) | OTLP gRPC uses port `4317`, not `4318` (HTTP). Check collector status |
| DNS resolution failure | Hostname typo or DNS not configured | Test with `dig` or `nslookup`. Use IP address to isolate DNS |
| Timeout with no error | Firewall blocking the port | Check firewall rules. Try `nc -zv host port` to test connectivity |

!!! tip
    Test connectivity to your backend *before* running Sonda. A quick
    `curl -s http://localhost:8428/health` for VictoriaMetrics or
    `curl -s http://localhost:3100/ready` for Loki confirms the backend is reachable.

### Data not appearing at the destination

Sonda runs without errors but you don't see data in your backend.

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| No data in VictoriaMetrics | Wrong endpoint path | Use `/api/v1/import/prometheus` for `http_push`, `/api/v1/write` for `remote_write` |
| No data in Prometheus | Prometheus needs remote write receiver enabled | Start Prometheus with `--web.enable-remote-write-receiver` |
| Encoder/sink mismatch | Using `prometheus_text` encoder with `remote_write` sink (or vice versa) | Match encoder to sink: `remote_write` encoder with `remote_write` sink, `otlp` encoder with `otlp_grpc` sink |
| HTTP 400 Bad Request | Wrong `content_type` for the endpoint | Use `text/plain` for VictoriaMetrics import endpoint |
| POST to `sonda-server` succeeds but no data in backend | Sink `url: http://localhost:<port>` resolves inside the server container | Use the in-network address (Compose service name `http://victoriametrics:8428`, or Kubernetes Service DNS), or write the URL with [`${VAR:-default}`](../configuration/v2-scenarios.md#environment-variable-interpolation) so one file works from both paths. See [Endpoints & networking](../deployment/endpoints.md) |

### Batching delays

Data arrives in chunks or only appears when the scenario ends.

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Stdout output appears in bursts | Normal OS-level buffering (~8 KB) | Expected behavior. Data flushes when the buffer fills or the scenario ends |
| No HTTP POST until scenario ends | Batch threshold not reached at low rates | Lower `batch_size` (e.g., `512` for `http_push`) or increase the rate. See [Sink Batching](../configuration/sink-batching.md) |
| Short scenario sends only one batch | Total data smaller than batch threshold | All data flushes on exit. This is correct behavior for short runs |

!!! info
    At 10 events/sec with `http_push` at the default 4 KiB threshold, ~40 events
    (~4 seconds) must accumulate before the first POST. Set `batch_size: 512` for
    faster feedback. Time-based flushing is tracked in
    [#266](https://github.com/davidban77/sonda/issues/266).

---

## Sink-specific issues

### Loki

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `400 Bad Request` from Loki | Label names contain invalid characters | Loki labels must match `[a-zA-Z_][a-zA-Z0-9_]*`. Avoid dots, dashes, or spaces in label keys |
| Logs rejected in multi-tenant Loki | Missing tenant header | Add `X-Scope-OrgID` via custom headers on an `http_push` sink, or use the default tenant if Loki is in single-tenant mode |
| No logs visible in Grafana | Wrong label selector in Explore | Check that your Grafana query matches the labels you set in the scenario |

!!! tip
    Sonda sends logs to `{url}/loki/api/v1/push`. You only configure the base URL
    (e.g., `http://localhost:3100`), not the full push path.

### Kafka

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Broker connection timeout | Wrong broker address or port | Verify broker is reachable: `nc -zv broker-host 9092`. Check for TLS port (`9093`) vs plaintext (`9092`) |
| `UnknownTopicOrPartition` | Topic doesn't exist and auto-creation is off | Set `auto.create.topics.enable=true` on the broker, or create the topic before running Sonda |
| Authentication failure with SASL | Wrong mechanism, username, or password | Double-check `sasl.mechanism` matches your broker config. Confluent Cloud uses `PLAIN`, AWS MSK uses `SCRAM-SHA-256` |
| Data sent but unreadable | Consumer expects a different encoding | Ensure the consumer's deserializer matches Sonda's encoder (e.g., `prometheus_text` produces plain text) |

!!! warning
    SASL credentials are sent in plaintext if TLS is not enabled. Sonda warns about this at
    startup, but always enable `tls.enabled: true` alongside SASL in production.

### Remote write

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| HTTP 400 from backend | Wrong endpoint URL for the backend | Each backend has a specific path. See the [compatible endpoints table](../configuration/sinks.md#remote_write) |
| HTTP 403 or 401 | Backend requires authentication headers | Add auth headers via `http_push` with custom `headers` instead |

### OTLP gRPC

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| gRPC `INVALID_ARGUMENT` | Signal type mismatch between encoder and sink | Set `signal_type` in the sink to match your scenario: `metrics` for metric scenarios, `logs` for log scenarios |
| Connection refused on port 4318 | Using the HTTP port instead of gRPC | OTLP gRPC uses port `4317`. Port `4318` is for OTLP HTTP |
| `UNAUTHENTICATED` | Collector requires auth token | Configure the collector to accept unauthenticated connections, or use an `http_push` sink with auth headers instead |

---

## Resource issues

### High memory usage

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Memory grows during cardinality spikes | Each unique label combination creates a new series in memory | Reduce `cardinality` in spike config, or use shorter `for` windows |
| Memory spikes during CSV replay | Large CSV file loaded into memory | Use smaller CSV files, or split large files into chunks |
| Steady memory growth over long runs | Large label sets with many static labels | Reduce the number of labels per metric. Each label adds memory per series |

!!! info
    Sonda's baseline memory footprint is roughly 5 MB. Memory scales with the number of
    unique series being generated simultaneously. For sizing guidance, see
    [Capacity Planning -- Performance baselines](capacity-planning.md#performance-baselines).

---

## Configuration mistakes

### YAML parsing errors

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `invalid type` error on a numeric field | Value is quoted as a string in YAML (e.g., `rate: "10"`) | Remove quotes from numeric fields: `rate: 10` |
| `unknown field` error | Typo in a field name, or field placed at the wrong nesting level | Check indentation. `labels` goes at the scenario level, not inside `sink` |
| `missing field` error | Required field omitted | Run `sonda --dry-run` to see which field is missing |

### Feature flag errors

Some sinks and encoders require Cargo feature flags when building from source. Pre-built
release binaries include all features.

| Feature | Required for | Build command |
|---------|-------------|---------------|
| `http` | `http_push`, `loki` sinks | `cargo build --features http -p sonda` |
| `remote-write` | `remote_write` encoder and sink | `cargo build --features remote-write -p sonda` |
| `otlp` | `otlp` encoder, `otlp_grpc` sink | `cargo build --features otlp -p sonda` |
| `kafka` | `kafka` sink | `cargo build --features kafka -p sonda` |

!!! tip
    Build with all features at once: `cargo build --features http,remote-write,otlp,kafka -p sonda`

---

## Container and signal handling

Sonda flushes all buffered data on clean shutdown (SIGTERM or SIGINT). If the process is killed
with SIGKILL, any data still in the buffer is lost.

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Partial data loss in Docker | Container stopped with `docker kill` (sends SIGKILL) | Use `docker stop` instead, which sends SIGTERM and waits for graceful shutdown |
| Data loss in Kubernetes | Pod killed before flush completes | Set `terminationGracePeriodSeconds` to at least 5 seconds in your pod spec |
| No data flushed on Ctrl+C in script | Script traps signals before Sonda receives them | Ensure SIGTERM/SIGINT propagate to the Sonda process |

!!! warning "SIGKILL bypasses flush"
    `kill -9` (SIGKILL) terminates Sonda immediately with no chance to flush buffered data.
    Always use `kill` (SIGTERM) or Ctrl+C (SIGINT) for a clean shutdown.

```yaml title="Kubernetes: ensure graceful shutdown"
spec:
  terminationGracePeriodSeconds: 10
  containers:
    - name: sonda
      image: ghcr.io/davidban77/sonda:latest
```

```yaml title="Docker Compose: default stop signal is SIGTERM (correct)"
services:
  sonda:
    image: ghcr.io/davidban77/sonda:latest
    # docker compose stop sends SIGTERM by default -- no special config needed
    stop_grace_period: 10s
```

---

**Related pages:**

- [Sinks](../configuration/sinks.md) -- sink types, parameters, and retry configuration
- [Sink Batching](../configuration/sink-batching.md) -- how batching affects data delivery
- [CLI Reference](../configuration/cli-reference.md) -- all flags for `--dry-run`, `--verbose`, and sink options
- [Capacity Planning](capacity-planning.md) -- performance baselines and infrastructure sizing
