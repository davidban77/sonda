# Act 6 — Sonda as a pipeline smoke test

**Time on slide:** ~5 min.

**What this shows:** Same engine, different framing. Instead of "generate test data", sonda is "inject a known signal and verify the pipe carried it". If the heartbeat doesn't arrive in Prometheus, the pipeline is broken — collector down, network partition, ingest backpressure, whatever. This is the case for putting sonda in CI before every deploy.

## The flow

### 1. Start the heartbeat (in one terminal)

```bash
sonda metrics --scenario demo/act-06-smoke-test/scenario.yaml
```

Runs for 30 seconds, emitting one heartbeat per second.

### 2. Verify (in a second terminal)

```bash
./demo/act-06-smoke-test/verify.sh
```

Output (happy path):

```
Polling http://localhost:9090 for pipeline_heartbeat{probe="sonda_smoke"} (timeout 30s)…
PASS — heartbeat metric arrived (1 series).
```

### 3. Simulate a broken pipeline (optional party trick)

Stop Prometheus while the heartbeat is running:

```bash
docker compose -f demo/stack/docker-compose.yml stop prometheus
./demo/act-06-smoke-test/verify.sh
```

Output:

```
Polling http://localhost:9090 for pipeline_heartbeat{probe="sonda_smoke"} (timeout 30s)…
FAIL — no heartbeat metric in Prometheus after 30s.
Pipeline is broken between sonda and Prometheus.
```

Bring Prometheus back:

```bash
docker compose -f demo/stack/docker-compose.yml start prometheus
```

## Talking points

- This is identical to what synthetic monitoring services do — except you run it where YOUR pipeline lives, and the signal shape is exactly what your real telemetry will look like.
- For CI/CD: gate deploys on `verify.sh`. If the smoke fails, don't roll forward.
- For incident triage: when someone says "I'm not seeing data", run this — it tells you if the bottom of the stack is up.
- Same scenario could verify logs (Loki), traces (Tempo), events (Kafka) — anywhere sonda has a sink.

## Talking points to *avoid*

- Don't get pulled into "is this better than blackbox_exporter?" — answer is "different layer". Blackbox checks endpoints; sonda checks the whole pipeline including the data shape.
