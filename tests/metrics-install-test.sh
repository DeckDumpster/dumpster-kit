#!/usr/bin/env bash
# Offline acceptance for deploy/metrics/install.sh: refuses a foreign host; on the product host
# writes quota'd, retention-explicit units, a 0600 secret never printed, and a tailnet-only bind.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
export METRICS_STATE_DIR=$t/state METRICS_UNIT_DIR=$t/units METRICS_SECRET_DIR=$t/secrets METRICS_DRY_RUN=1 METRICS_GRAFANA_ADDR=100.64.0.9

if METRICS_HOSTNAME_OVERRIDE=spira-host "$root/deploy/metrics/install.sh" 2>"$t/err"; then echo "FAIL: foreign host accepted"; exit 1; fi
grep -q refusing "$t/err"
[ ! -e "$t/units" ] || { echo "FAIL: foreign host wrote units"; exit 1; }

out=$(METRICS_HOSTNAME_OVERRIDE=ubuntu-mini-0 "$root/deploy/metrics/install.sh" 2>&1)
pw=$(sed -n 's/^GF_SECURITY_ADMIN_PASSWORD=//p' "$t/secrets/grafana.env")
[ -n "$pw" ]; case "$out" in *"$pw"*) echo "FAIL: password printed"; exit 1;; esac
[ "$(stat -c %a "$t/secrets/grafana.env")" = 600 ]
grep -q 'CPUQuota=' "$t/units/product-victoriametrics.service"
grep -q 'CPUQuota=' "$t/units/product-grafana.service"
grep -q -- '-retentionPeriod=90d' "$t/units/product-victoriametrics.service"
grep -q 'GF_SERVER_HTTP_ADDR=100.64.0.9' "$t/units/product-grafana.service"
grep -q 'dumpster-kit' "$t/state/provisioning/dashboards/dashboards.yml"
grep -q ':ro' "$t/units/product-grafana.service"
echo "ok"
