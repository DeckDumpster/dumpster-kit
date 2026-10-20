#!/usr/bin/env bash
# CI smoke: runs the pinned Grafana image as its real non-root user (472) against the provisioning
# install.sh generates under umask 077, and requires /api/health and the dumpster-kit folder.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/deploy/metrics/stack.conf"
t=$(mktemp -d); chmod 755 "$t"
trap 'docker rm -f grafana-smoke >/dev/null 2>&1 || true; rm -rf "$t"' EXIT
export METRICS_STATE_DIR=$t/state METRICS_UNIT_DIR=$t/units METRICS_SECRET_DIR=$t/secrets METRICS_DRY_RUN=1 METRICS_GRAFANA_ADDR=127.0.0.1
(umask 077; METRICS_HOSTNAME_OVERRIDE=$PRODUCT_HOST "$root/deploy/metrics/install.sh" >/dev/null)
# Copy: the checkout path may not be traversable by uid 472 on a CI runner.
cp -r "$root/deploy/dashboards" "$t/dashboards"; chmod -R a+rX "$t/dashboards"
mkdir -p "$t/data"; chmod 777 "$t/data"
docker run -d --name grafana-smoke --user 472 --network=host \
  --env-file "$t/secrets/grafana.env" -e GF_SERVER_HTTP_ADDR=127.0.0.1 -e GF_SERVER_HTTP_PORT=3001 \
  -v "$t/data:/var/lib/grafana" -v "$t/state/provisioning:/etc/grafana/provisioning:ro" \
  -v "$t/dashboards:/dashboards/dumpster-kit:ro" "$GRAFANA_IMAGE" >/dev/null
pw=$(sed -n 's/^GF_SECURITY_ADMIN_PASSWORD=//p' "$t/secrets/grafana.env")
for _ in $(seq 60); do curl -fsS http://127.0.0.1:3001/api/health >/dev/null 2>&1 && break; sleep 1; done
curl -fsS http://127.0.0.1:3001/api/health >/dev/null || { docker logs grafana-smoke; echo "FAIL: Grafana not healthy"; exit 1; }
# Dashboard provisioning can finish after /api/health turns green, so retry.
for _ in $(seq 30); do
  curl -fsS -u "admin:$pw" "http://127.0.0.1:3001/api/folders" 2>/dev/null | grep -q '"title":"dumpster-kit"' && { echo ok; exit 0; }
  sleep 1
done
docker logs grafana-smoke; curl -sS -u "admin:$pw" "http://127.0.0.1:3001/api/folders" || true
echo "FAIL: dumpster-kit folder not provisioned"; exit 1
