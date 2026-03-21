#!/usr/bin/env bash
# tests/e2e/run.sh — end-to-end integration test runner for sonda's HTTP push sink.
#
# This script:
#   1. Starts docker-compose services (Prometheus, VictoriaMetrics, vmagent)
#   2. Waits for all services to become healthy
#   3. Builds sonda in release mode
#   4. Runs each test scenario for a short duration
#   5. Waits for ingestion to settle
#   6. Queries VictoriaMetrics to verify data arrived
#   7. Reports PASS/FAIL for each scenario
#   8. Tears down docker-compose
#   9. Exits 0 if all scenarios passed, 1 if any failed
#
# Usage:
#   ./tests/e2e/run.sh
#
# Prerequisites:
#   - docker and docker compose (v2) installed and in PATH
#   - curl installed and in PATH
#   - Rust toolchain installed (for cargo build)
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.yml"
SCENARIO_DIR="${SCRIPT_DIR}/scenarios"

# Sonda binary path (release build)
SONDA_BIN="${REPO_ROOT}/target/release/sonda"

# How long each scenario runs
SCENARIO_DURATION=5

# How long to wait for ingestion to settle after a scenario finishes
INGEST_SETTLE_SECS=5

# How long to wait for each service to become healthy (seconds)
HEALTH_WAIT_SECS=60

# VictoriaMetrics query endpoint
VM_QUERY_URL="http://localhost:8428/api/v1/query"

# Kafka container name (must match docker-compose.yml)
KAFKA_CONTAINER="sonda-e2e-kafka"

# Track overall pass/fail state
PASS_COUNT=0
FAIL_COUNT=0

# ---------------------------------------------------------------------------
# Cleanup trap
# ---------------------------------------------------------------------------

cleanup() {
    echo ""
    echo "--- Tearing down docker-compose services ---"
    docker compose -f "${COMPOSE_FILE}" down -v --remove-orphans 2>/dev/null || true
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Helper functions
# ---------------------------------------------------------------------------

info()  { echo "[INFO]  $*"; }
pass()  { echo "[PASS]  $*"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail()  { echo "[FAIL]  $*"; FAIL_COUNT=$((FAIL_COUNT + 1)); }
fatal() { echo "[FATAL] $*" >&2; exit 1; }

# Wait for an HTTP endpoint to return a non-error response.
# Usage: wait_for_health <url> <service_name> <max_seconds>
wait_for_health() {
    local url="$1"
    local name="$2"
    local max_secs="$3"
    local elapsed=0

    info "Waiting for ${name} at ${url} (up to ${max_secs}s)..."
    while true; do
        if curl -sf --max-time 3 "${url}" >/dev/null 2>&1; then
            info "${name} is healthy."
            return 0
        fi
        if [ "${elapsed}" -ge "${max_secs}" ]; then
            fatal "${name} did not become healthy within ${max_secs}s."
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
}

# Query VictoriaMetrics for a metric and return the number of result series.
# Usage: query_vm_count <metric_name>
query_vm_count() {
    local metric="$1"
    local encoded
    # Use {__name__="metric"} form to match series with labels
    encoded="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote('{__name__=\"' + sys.argv[1] + '\"}'))" "${metric}" 2>/dev/null)"
    local result
    # Use /api/v1/series to check if the metric exists (more reliable than instant query)
    local series_url="http://localhost:8428/api/v1/series"
    local match_param
    match_param="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote('{__name__=\"' + sys.argv[1] + '\"}'))" "${metric}" 2>/dev/null)"
    result=$(curl -s --max-time 10 "${series_url}?match[]=${match_param}" 2>/dev/null || echo '{}')
    # Extract the count of result entries from the JSON response.
    # /api/v1/series response shape: {"status":"success","data":[...]}
    echo "${result}" | python3 -c "
import sys, json
data = json.load(sys.stdin)
results = data.get('data', [])
print(len(results))
" 2>/dev/null || echo "0"
}

# Wait for Kafka to accept broker connections by polling kafka-topics.sh inside
# the container. This mirrors the docker-compose healthcheck but runs from the
# host side so the script can gate on it without depending on Docker health state.
# Usage: wait_for_kafka <max_seconds>
wait_for_kafka() {
    local max_secs="$1"
    local elapsed=0

    info "Waiting for Kafka broker to be ready (up to ${max_secs}s)..."
    while true; do
        if docker exec "${KAFKA_CONTAINER}" kafka-topics.sh \
                --bootstrap-server 127.0.0.1:9092 \
                --list >/dev/null 2>&1; then
            info "Kafka is healthy."
            return 0
        fi
        if [ "${elapsed}" -ge "${max_secs}" ]; then
            fatal "Kafka did not become healthy within ${max_secs}s."
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
}

# Consume all messages from a Kafka topic (from beginning) using a short
# timeout, then return the line count as a proxy for message count.
# Usage: query_kafka_count <topic>
query_kafka_count() {
    local topic="$1"
    local count
    count=$(docker exec "${KAFKA_CONTAINER}" kafka-console-consumer.sh \
        --bootstrap-server 127.0.0.1:9092 \
        --topic "${topic}" \
        --from-beginning \
        --timeout-ms 5000 2>/dev/null | wc -l)
    # wc -l may include leading whitespace on some platforms; strip it
    echo "${count}" | tr -d ' '
}

# Run a single Kafka scenario: execute sonda, then verify messages arrived in
# the target topic using kafka-console-consumer inside the Kafka container.
# Usage: run_kafka_scenario <scenario_file> <topic> <description>
run_kafka_scenario() {
    local scenario_file="$1"
    local topic="$2"
    local description="$3"

    info "Running Kafka scenario: ${description}"
    info "  File:  ${scenario_file}"
    info "  Topic: ${topic}"

    local timeout_secs=$((SCENARIO_DURATION + 10))
    "${SONDA_BIN}" metrics \
            --scenario "${scenario_file}" \
            --duration "${SCENARIO_DURATION}s" \
            >/dev/null 2>/tmp/sonda-e2e-stderr.log &
    local sonda_pid=$!

    local waited=0
    while kill -0 "${sonda_pid}" 2>/dev/null && [ "${waited}" -lt "${timeout_secs}" ]; do
        sleep 1
        waited=$((waited + 1))
    done

    if kill -0 "${sonda_pid}" 2>/dev/null; then
        kill "${sonda_pid}" 2>/dev/null
        wait "${sonda_pid}" 2>/dev/null
        fail "${description}: sonda timed out after ${timeout_secs}s"
        return
    fi

    wait "${sonda_pid}" 2>/dev/null
    local exit_code=$?

    if [ "${exit_code}" -ne 0 ]; then
        fail "${description}: sonda exited with code ${exit_code}"
        return
    fi

    info "  sonda exited cleanly."
    info "  Waiting ${INGEST_SETTLE_SECS}s for Kafka ingestion to settle..."
    sleep "${INGEST_SETTLE_SECS}"

    local count
    count=$(query_kafka_count "${topic}")
    info "  Kafka message count: ${count}"

    if [ "${count}" -gt 0 ] 2>/dev/null; then
        pass "${description}"
    else
        fail "${description}: no messages found in Kafka topic '${topic}' (count=${count})"
    fi
}

# Run a single scenario and verify data reached VictoriaMetrics.
# Usage: run_scenario <scenario_file> <metric_name> <description>
run_scenario() {
    local scenario_file="$1"
    local metric_name="$2"
    local description="$3"

    info "Running scenario: ${description}"
    info "  File:   ${scenario_file}"
    info "  Metric: ${metric_name}"

    # Run sonda for SCENARIO_DURATION seconds. The scenario YAML also sets duration.
    # Use a background process + wait for portable timeout (macOS has no `timeout`).
    local timeout_secs=$((SCENARIO_DURATION + 10))
    "${SONDA_BIN}" metrics \
            --scenario "${scenario_file}" \
            --duration "${SCENARIO_DURATION}s" \
            >/dev/null 2>/tmp/sonda-e2e-stderr.log &
    local sonda_pid=$!

    # Wait with timeout: kill if still running after timeout_secs
    local waited=0
    while kill -0 "${sonda_pid}" 2>/dev/null && [ "${waited}" -lt "${timeout_secs}" ]; do
        sleep 1
        waited=$((waited + 1))
    done

    if kill -0 "${sonda_pid}" 2>/dev/null; then
        kill "${sonda_pid}" 2>/dev/null
        wait "${sonda_pid}" 2>/dev/null
        fail "${description}: sonda timed out after ${timeout_secs}s"
        return
    fi

    wait "${sonda_pid}" 2>/dev/null
    local exit_code=$?

    if [ "${exit_code}" -eq 0 ]; then
        info "  sonda exited cleanly."
    else
        # Non-zero exit from sonda itself.
        fail "${description}: sonda exited with code ${exit_code}"
        return
    fi

    info "  Waiting ${INGEST_SETTLE_SECS}s for ingestion to settle..."
    sleep "${INGEST_SETTLE_SECS}"

    local count
    count=$(query_vm_count "${metric_name}")
    info "  Query result count: ${count}"

    if [ "${count}" -gt 0 ] 2>/dev/null; then
        pass "${description}"
    else
        fail "${description}: metric '${metric_name}' not found in VictoriaMetrics (count=${count})"
    fi
}

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------

info "=== sonda e2e integration tests ==="
info "Repo root: ${REPO_ROOT}"

# Check docker is available.
if ! command -v docker >/dev/null 2>&1; then
    fatal "docker is not installed or not in PATH."
fi

# Check docker compose v2 is available (docker compose, not docker-compose).
if ! docker compose version >/dev/null 2>&1; then
    fatal "docker compose (v2) is not available. Install Docker Desktop or the compose plugin."
fi

# Check curl is available.
if ! command -v curl >/dev/null 2>&1; then
    fatal "curl is not installed or not in PATH."
fi

# Check python3 is available (used for URL encoding and JSON parsing).
if ! command -v python3 >/dev/null 2>&1; then
    fatal "python3 is not installed or not in PATH (needed for URL encoding and JSON parsing)."
fi

# ---------------------------------------------------------------------------
# Build sonda
# ---------------------------------------------------------------------------

info "Building sonda (release)..."
cargo build --release -p sonda --manifest-path "${REPO_ROOT}/Cargo.toml"
info "Build complete: ${SONDA_BIN}"

# ---------------------------------------------------------------------------
# Start services
# ---------------------------------------------------------------------------

info "Starting docker-compose services..."
docker compose -f "${COMPOSE_FILE}" up -d

# ---------------------------------------------------------------------------
# Wait for services to be healthy
# ---------------------------------------------------------------------------

wait_for_health "http://localhost:9090/-/ready"   "Prometheus"        "${HEALTH_WAIT_SECS}"
wait_for_health "http://localhost:8428/health"    "VictoriaMetrics"   "${HEALTH_WAIT_SECS}"
wait_for_health "http://localhost:8429/health"    "vmagent"           "${HEALTH_WAIT_SECS}"
wait_for_kafka  "${HEALTH_WAIT_SECS}"

# ---------------------------------------------------------------------------
# Run scenarios
# ---------------------------------------------------------------------------

info ""
info "=== Running scenarios ==="

run_scenario \
    "${SCENARIO_DIR}/vm-prometheus-text.yaml" \
    "sonda_e2e_vm_prom_text" \
    "VictoriaMetrics via Prometheus text format"

run_scenario \
    "${SCENARIO_DIR}/vm-influx-lp.yaml" \
    "sonda_e2e_vm_influx_lp_value" \
    "VictoriaMetrics via InfluxDB line protocol"

run_kafka_scenario \
    "${SCENARIO_DIR}/kafka-prometheus-text.yaml" \
    "sonda-e2e-metrics" \
    "Kafka via Prometheus text format"

run_kafka_scenario \
    "${SCENARIO_DIR}/kafka-json-lines.yaml" \
    "sonda-e2e-json" \
    "Kafka via JSON Lines"

# NOTE: vmagent scenario is disabled. vmagent's /api/v1/import/prometheus
# endpoint accepts data (204) but does not relay plain text — it only forwards
# via Prometheus remote write protocol (protobuf). Re-enable when sonda supports
# protobuf remote write encoding.
# run_scenario \
#     "${SCENARIO_DIR}/vmagent-prometheus-text.yaml" \
#     "sonda_e2e_vmagent_prom_text" \
#     "vmagent -> VictoriaMetrics via Prometheus text format"

# ---------------------------------------------------------------------------
# Results
# ---------------------------------------------------------------------------

echo ""
echo "=== Results ==="
echo "  PASS: ${PASS_COUNT}"
echo "  FAIL: ${FAIL_COUNT}"
echo ""

if [ "${FAIL_COUNT}" -gt 0 ]; then
    echo "RESULT: FAIL (${FAIL_COUNT} scenario(s) failed)"
    exit 1
else
    echo "RESULT: PASS (all ${PASS_COUNT} scenario(s) passed)"
    exit 0
fi
