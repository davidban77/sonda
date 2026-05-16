# Pipeline Validation

You shipped a one-line change to a vmagent relabel rule on Friday. By Monday morning,
half the dashboards for `service=payments` are blank. The metrics still arrive, the
counts are normal -- but the rule rewrote `service` to lowercase and the dashboards
filter for `Payments`. Nothing in your pipeline noticed: the data flowed, the writes
succeeded, the only thing that broke was the contract with downstream consumers.

This is the gap CI is supposed to catch. Sonda fills it by giving you a known input
on one end of the pipeline and a check at the other end -- exit code, line count,
backend query -- so any rewrite, drop, or schema drift surfaces as a failed step
before it reaches the dashboards.

---

## Smoke Testing With the CLI

The simplest validation: run a one-entry scenario, check the exit code, count the
output lines. Scaffold a starter file with `sonda new --template`, edit the metric
name to taste, then run it with `-q` to suppress status banners in scripts:

```yaml title="smoke.yaml"
version: 2
kind: runnable
defaults:
  rate: 5
  duration: 2s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: smoke_test
    signal_type: metrics
    name: smoke_test
    generator:
      type: constant
      value: 1.0
```

```bash
sonda -q run smoke.yaml > /tmp/smoke.txt
echo "Exit code: $?"
wc -l < /tmp/smoke.txt
```

A successful run exits with code `0` and produces approximately `rate * duration` lines
(roughly 10 for rate=5 and duration=2s).

| Exit code | Meaning |
|-----------|---------|
| `0` | Success -- all events emitted |
| `1` | Runtime error -- bad scenario file, sink connection failure, validation reject |
| `2` | Argument parse error -- unknown flag, missing argument |

!!! tip "Quick validation in scripts"
    Use the exit code in CI or shell scripts: `sonda -q run smoke.yaml > /dev/null && echo "OK"`.

Now let's verify that every wire format makes it through your pipeline.

---

## Multi-Format Validation

Run the same metric through each encoder to verify that every format arrives at its destination.
This catches encoding regressions and misconfigured parsers. The encoder lives in the YAML; swap the `type:` field to compare formats. Override at the command line with `--encoder` when you need a one-off variant:

=== "Prometheus text"

    ```bash
    sonda run pipeline-test.yaml
    ```

    ```
    pipeline_test 0 1700000000000
    pipeline_test 0 1700000000500
    ```

=== "InfluxDB line protocol"

    ```bash
    sonda run pipeline-test.yaml --encoder influx_lp
    ```

    ```
    pipeline_test value=0 1700000000000000000
    pipeline_test value=0 1700000000500000000
    ```

=== "JSON Lines"

    ```bash
    sonda run pipeline-test.yaml --encoder json_lines
    ```

    ```json
    {"name":"pipeline_test","value":0.0,"labels":{},"timestamp":"2026-03-23T12:00:00.000Z"}
    ```

The starter `pipeline-test.yaml` is two ticks of the constant generator:

```yaml title="pipeline-test.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 2s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: pipeline_test
    signal_type: metrics
    name: pipeline_test
    generator:
      type: constant
      value: 0.0
```

To push a specific format to a file for inspection, use a scenario file:

```bash
sonda run examples/multi-format-test.yaml
wc -l < /tmp/pipeline-influx.txt
```

```yaml title="examples/multi-format-test.yaml"
version: 2

defaults:
  rate: 2
  duration: 10s
  encoder:
    type: influx_lp
  sink:
    type: file
    path: /tmp/pipeline-influx.txt

scenarios:
  - signal_type: metrics
    name: pipeline_test
    generator:
      type: constant
      value: 42.0
    labels:
      env: test
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

The smoke and CI checks above confirm Sonda emitted the expected number of lines and exited
cleanly. They don't prove the data arrived in the shape your backend expects -- only the
backend can answer that.

For full end-to-end validation against a real Prometheus, VictoriaMetrics, Kafka, or Loki
instance -- with backend queries that assert arrival, schema, and labels -- see
[E2E Testing](e2e-testing.md). That guide is the canonical worked example for backend-side
assertions.

Backend assertions cover one signal at a time. For full pipeline coverage, test metrics and logs together.

---

## Multi-Scenario Validation

Use `sonda run` to push metrics and logs concurrently from a single YAML file.
This validates that your pipeline handles multiple signal types at the same time:

```bash
sonda run examples/multi-pipeline-test.yaml
echo "Exit: $?"
wc -l < /tmp/pipeline-logs.json
```

```yaml title="examples/multi-pipeline-test.yaml"
version: 2

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
    log_generator:
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

See [Scenario Fields](../configuration/scenario-fields.md) for the full multi-scenario YAML
reference.

---

## Next Steps

**Testing alert rules?** Start with [Alert Testing](alert-testing.md).

**Verifying recording rules?** Check [Recording Rules](recording-rules.md).

**Running the full e2e suite?** See [E2E Testing](e2e-testing.md).

**Browsing all example scenarios?** See [Example Scenarios](examples.md).
