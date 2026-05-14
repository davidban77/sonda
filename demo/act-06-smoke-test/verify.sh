#!/usr/bin/env bash
# Poll Prometheus for the heartbeat metric and exit 0 if it lands within
# the timeout, non-zero otherwise. This is the "did the pipeline work?"
# assertion that pairs with the act-06 scenario.
#
# Usage:
#   ./verify.sh                  # defaults: 30s timeout
#   ./verify.sh 60               # custom timeout

set -euo pipefail

TIMEOUT=${1:-30}
PROM_URL=${PROM_URL:-http://localhost:9090}
QUERY='pipeline_heartbeat{probe="sonda_smoke"}'

echo "Polling ${PROM_URL} for ${QUERY} (timeout ${TIMEOUT}s)…"

deadline=$(( $(date +%s) + TIMEOUT ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  response=$(curl -s "${PROM_URL}/api/v1/query" --data-urlencode "query=${QUERY}" || echo "")
  count=$(echo "$response" | jq -r '.data.result | length // 0' 2>/dev/null || echo "0")
  if [ "$count" != "0" ] && [ -n "$count" ]; then
    echo "PASS — heartbeat metric arrived (${count} series)."
    exit 0
  fi
  sleep 2
done

echo "FAIL — no heartbeat metric in Prometheus after ${TIMEOUT}s."
echo "Pipeline is broken between sonda and Prometheus."
exit 1
