# Pipeline Validation

You changed your ingest pipeline, added an encoder, or modified a routing rule. How do you know
nothing broke? Sonda gives you a fast, repeatable way to push known data through your pipeline
and verify it arrives correctly at the other end.

---

## Smoke Testing With the CLI

The simplest validation: run Sonda with a known metric, check the exit code, and count the
output lines. Use `-q` to suppress status banners in scripts:

```bash
sonda -q metrics --name smoke_test --rate 5 --duration 2s > /tmp/smoke.txt
echo "Exit code: $?"
wc -l < /tmp/smoke.txt
```

A successful run exits with code `0` and produces approximately `rate * duration` lines
(roughly 10 for rate=5 and duration=2s).

| Exit code | Meaning |
|-----------|---------|
| `0` | Success -- all events emitted |
| `1` | Error -- missing required flags, bad scenario file, or sink connection failure |

!!! tip "Quick validation in scripts"
    Use the exit code in CI or shell scripts: `sonda -q metrics --name test --rate 1 --duration 1s > /dev/null && echo "OK"`.

Now let's verify that every wire format makes it through your pipeline.

---

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
    ```

To push a specific format to a file for inspection, use a scenario file:

```bash
sonda metrics --scenario examples/multi-format-test.yaml
wc -l < /tmp/pipeline-influx.txt
```

```yaml title="examples/multi-format-test.yaml"
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

See [Encoders](../configuration/encoders.md) and [Sinks](../configuration/sinks.md) for the
full list of supported formats and destinations.

Individual format checks are good for development. For systematic validation, add Sonda to CI.

---

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
          sonda -q metrics --name ci_smoke --rate 10 --duration 5s \
            --output /tmp/ci-smoke-prom.txt
          LINES=$(wc -l < /tmp/ci-smoke-prom.txt)
          echo "Produced $LINES lines"
          [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }

      - name: Smoke test (JSON Lines)
        run: |
          sonda -q metrics --name ci_smoke --rate 10 --duration 5s \
            --encoder json_lines --output /tmp/ci-smoke-json.txt
          LINES=$(wc -l < /tmp/ci-smoke-json.txt)
          echo "Produced $LINES lines"
          [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }
```

!!! tip "Pre-built binaries"
    If a Sonda release binary is available for your platform, download it instead of building
    from source to save CI time. Check the
    [GitHub Releases](https://github.com/davidban77/sonda/releases) page.

CI catches regressions automatically. For deeper validation against real backends, use Docker Compose.

---

## E2E Testing With Docker Compose

For full end-to-end validation, spin up Sonda alongside a backend and verify data arrives.

The project includes a ready-to-use e2e test suite in `tests/e2e/`. See [E2E Testing](e2e-testing.md)
for the full suite with Prometheus, VictoriaMetrics, Kafka, and Loki.

For a quick single-scenario check against VictoriaMetrics:

```bash
# Start the stack
docker compose -f examples/docker-compose-victoriametrics.yml up -d

# Push data
sonda metrics --scenario examples/e2e-scenario.yaml

# Wait for ingestion, then verify
sleep 5
curl -s "http://localhost:8428/api/v1/query?query=e2e_pipeline_check" \
  | jq '.data.result | length'
# Expected: 1 (at least one series)

# Tear down
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

```yaml title="examples/e2e-scenario.yaml"
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

Single-scenario checks validate one signal type. For full pipeline coverage, test metrics and logs together.

---

## Multi-Scenario Validation

Use `sonda run` to push metrics and logs concurrently from a single YAML file.
This validates that your pipeline handles multiple signal types at the same time:

```bash
sonda run --scenario examples/multi-pipeline-test.yaml
echo "Exit: $?"
wc -l < /tmp/pipeline-logs.json
```

```yaml title="examples/multi-pipeline-test.yaml"
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

Each scenario runs on its own thread. Use different sinks per scenario to keep outputs separate.

See [Scenario Files](../configuration/scenario-file.md) for the full multi-scenario YAML
reference.

---

## Next Steps

**Testing alert rules?** Start with [Alert Testing](alert-testing.md).

**Verifying recording rules?** Check [Recording Rules](recording-rules.md).

**Running the full e2e suite?** See [E2E Testing](e2e-testing.md).

**Browsing all example scenarios?** See [Example Scenarios](examples.md).
