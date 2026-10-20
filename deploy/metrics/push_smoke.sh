#!/usr/bin/env bash
# Pushes one OTLP/HTTP JSON gauge (product_metrics_smoke) straight to VictoriaMetrics, then
# queries it back. Acceptance: the same series appears in the "Product metrics smoke" panel.
set -euo pipefail
addr=${1:-127.0.0.1:8428}
now=$(( $(date +%s) * 1000000000 ))
curl -fsS -X POST "http://$addr/opentelemetry/v1/metrics" -H 'Content-Type: application/json' --data-binary @- <<JSON
{"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"smoke"}},{"key":"host","value":{"stringValue":"$(hostname)"}}]},
"scopeMetrics":[{"metrics":[{"name":"product_metrics_smoke","gauge":{"dataPoints":[{"timeUnixNano":"$now","asDouble":1}]}}]}]}]}
JSON
sleep 2
curl -fsS "http://$addr/api/v1/query" --data-urlencode 'query=product_metrics_smoke' | grep -q product_metrics_smoke
echo "pushed and read back product_metrics_smoke"
