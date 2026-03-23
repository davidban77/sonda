# Pipeline Validation

You changed your ingest pipeline, added an encoder, or modified a routing rule. How do you know
nothing broke? Sonda gives you a fast, repeatable way to push known data through your pipeline
and verify it arrives correctly at the other end.

## Smoke Testing With the CLI

The simplest validation: run Sonda with a known metric, check the exit code, and count
the output lines.

```bash
sonda metrics --name smoke_test --rate 5 --duration 2s > /tmp/smoke.txt
echo "Exit code: $?"
wc -l < /tmp/smoke.txt
```

A successful run exits with code `0` and produces approximately `rate * duration` lines
(roughly 10 for rate=5 and duration=2s). A non-zero exit code means something went wrong:

| Exit code | Meaning |
|-----------|---------|
| `0` | Success -- all events emitted. |
| `1` | Error -- missing required flags, bad scenario file, sink connection failure. |

```bash
# Missing required --name flag: exits 1
sonda metrics 2>/dev/null; echo $?
# 1

# Bad scenario path: exits 1
sonda metrics --scenario /nonexistent.yaml 2>/dev/null; echo $?
# 1
```

## Multi-Format Validation

Run the same metric through each encoder to verify that every format arrives at its destination.
This catches encoding regressions and misconfigured parsers.

=== "Prometheus text"

    ```bash
    sonda metrics --name pipeline_test --rate 2 --duration 2s
    ```

    ```
    pipeline_test 0 1700000000000
    pipeline_test 0 1700000000500
    ```

=== "InfluxDB line protocol"

    ```bash
    sonda metrics --name pipeline_test --rate 2 --duration 2s --encoder influx_lp
    ```

    ```
    pipeline_test value=0 1700000000000000000
    pipeline_test value=0 1700000000500000000
    ```

=== "JSON Lines"

    ```bash
    sonda metrics --name pipeline_test --rate 2 --duration 2s --encoder json_lines
    ```

    ```json
    {"name":"pipeline_test","value":0.0,"labels":{},"timestamp":"2026-03-23T12:00:00.000Z"}
    {"name":"pipeline_test","value":0.0,"labels":{},"timestamp":"2026-03-23T12:00:00.500Z"}
    ```

To push each format to a different backend, combine the encoder with an
[`http_push` sink](../configuration/sinks.md#http_push) or a
[file sink](../configuration/sinks.md#file):

```yaml title="multi-format-test.yaml"
name: pipeline_test
rate: 2
duration: 10s

generator:
  type: constant
  value: 42.0

labels:
  env: test

encoder:
  type: influx_lp
sink:
  type: file
  path: /tmp/pipeline-influx.txt
```

```bash
sonda metrics --scenario multi-format-test.yaml
wc -l < /tmp/pipeline-influx.txt
```

See [Encoders](../configuration/encoders.md) and [Sinks](../configuration/sinks.md) for the
full list of supported formats and destinations.

## CI Integration

Add Sonda as a step in your GitHub Actions workflow to validate your pipeline on every push.
The `--duration` flag ensures the step finishes in bounded time.

```yaml title=".github/workflows/pipeline-test.yml"
name: Pipeline Smoke Test
on: [push, pull_request]

jobs:
  smoke-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Install Sonda
        run: cargo install sonda

      - name: Smoke test (Prometheus text)
        run: |
          sonda metrics --name ci_smoke --rate 10 --duration 5s \
            --output /tmp/ci-smoke-prom.txt
          LINES=$(wc -l < /tmp/ci-smoke-prom.txt)
          echo "Produced $LINES lines"
          [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }

      - name: Smoke test (JSON Lines)
        run: |
          sonda metrics --name ci_smoke --rate 10 --duration 5s \
            --encoder json_lines --output /tmp/ci-smoke-json.txt
          LINES=$(wc -l < /tmp/ci-smoke-json.txt)
          echo "Produced $LINES lines"
          [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }
```

!!! tip "Pre-built binaries"
    If a Sonda release binary is available for your platform, download it instead of building
    from source to save CI time. Check the
    [GitHub Releases](https://github.com/davidban77/sonda/releases) page.

## E2E Testing With Docker Compose

For full end-to-end validation, use Docker Compose to spin up Sonda alongside a backend
(VictoriaMetrics, Prometheus, Kafka) and verify data arrives.

The project includes a ready-to-use e2e test suite in `tests/e2e/`. The stack starts
Prometheus, VictoriaMetrics, vmagent, Kafka, Loki, and Grafana:

```bash
# Run the full e2e suite
./tests/e2e/run.sh
```

The script performs these steps:

1. Starts the Docker Compose stack and waits for all services to become healthy.
2. Builds Sonda in release mode.
3. Runs each test scenario for a short duration (5 seconds).
4. Queries VictoriaMetrics (or Kafka) to verify data arrived.
5. Reports PASS/FAIL for each scenario.
6. Tears down the stack and exits with code `0` (all pass) or `1` (any failure).

### Writing Your Own E2E Test

Create a scenario YAML that pushes to your backend, run it with a bounded duration, then
query the backend to verify:

```yaml title="e2e-scenario.yaml"
name: e2e_pipeline_check
rate: 1
duration: 10s

generator:
  type: constant
  value: 99.0

labels:
  test: pipeline
  env: ci

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

```bash
# Push data
sonda metrics --scenario e2e-scenario.yaml

# Wait for ingestion
sleep 5

# Verify the metric exists
curl -s "http://localhost:8428/api/v1/query?query=e2e_pipeline_check" \
  | jq '.data.result | length'
# Expected: 1 (at least one series)
```

### Docker Compose for a Minimal Backend

If you only need VictoriaMetrics for validation, use the provided compose file:

```bash
# Start the stack (includes VictoriaMetrics, vmagent, Grafana, sonda-server)
docker compose -f examples/docker-compose-victoriametrics.yml up -d

# Run your scenario
sonda metrics --scenario e2e-scenario.yaml

# Query and verify
curl -s "http://localhost:8428/api/v1/query?query=e2e_pipeline_check"

# Tear down
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

## Multi-Scenario Validation

Use the `sonda run` subcommand to push metrics and logs concurrently from a single YAML file.
This validates that your pipeline handles multiple signal types at the same time:

```yaml title="multi-pipeline-test.yaml"
scenarios:
  - signal_type: metrics
    name: pipeline_metrics
    rate: 5
    duration: 10s
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: logs
    name: pipeline_logs
    rate: 5
    duration: 10s
    generator:
      type: template
      templates:
        - message: "Pipeline validation event"
      severity_weights:
        info: 1.0
      seed: 42
    encoder:
      type: json_lines
    sink:
      type: file
      path: /tmp/pipeline-logs.json
```

```bash
sonda run --scenario multi-pipeline-test.yaml
echo "Exit: $?"
wc -l < /tmp/pipeline-logs.json
```

See [Scenario Files](../configuration/scenario-file.md) for the full multi-scenario YAML
reference.
