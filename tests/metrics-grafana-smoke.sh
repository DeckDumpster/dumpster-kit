#!/usr/bin/env bash
# CI smoke: runs the pinned Grafana image as its real non-root user (472) against the provisioning
# install.sh generates under umask 077, and requires /api/health and the dumpster-kit folder.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/deploy/metrics/stack.conf"
t=$(mktemp -d); chmod 755 "$t"
vol=grafana-smoke-data-$$
# Grafana's data lives in a container-managed volume: files uid 472 writes into a host temp dir cannot be
# removed by the runner user. Cleanup never changes the smoke's exit status.
trap 'docker rm -f grafana-smoke >/dev/null 2>&1; docker volume rm -f "$vol" >/dev/null 2>&1; rm -rf "$t" 2>/dev/null; true' EXIT
export METRICS_STATE_DIR=$t/state METRICS_UNIT_DIR=$t/units METRICS_SECRET_DIR=$t/secrets METRICS_DRY_RUN=1 METRICS_GRAFANA_ADDR=127.0.0.1
(umask 077; METRICS_HOSTNAME_OVERRIDE=$PRODUCT_HOST "$root/deploy/metrics/install.sh" >/dev/null)
# Copy: the checkout path may not be traversable by uid 472 on a CI runner.
cp -r "$root/deploy/dashboards" "$t/dashboards"; chmod -R a+rX "$t/dashboards"
docker volume create "$vol" >/dev/null
docker run -d --name grafana-smoke --user 472 --network=host \
  --env-file "$t/secrets/grafana.env" -e GF_SERVER_HTTP_ADDR=127.0.0.1 -e GF_SERVER_HTTP_PORT=3001 \
  $(for w in $GRAFANA_ACCESS_ENV; do printf -- '-e %s ' "$w"; done) \
  -v "$vol:/var/lib/grafana" -v "$t/state/provisioning:/etc/grafana/provisioning:ro" \
  -v "$t/dashboards:/dashboards/dumpster-kit:ro" "$GRAFANA_IMAGE" >/dev/null
pw=$(sed -n 's/^GF_SECURITY_ADMIN_PASSWORD=//p' "$t/secrets/grafana.env")
base=http://127.0.0.1:3001
for _ in $(seq 60); do curl -fsS http://127.0.0.1:3001/api/health >/dev/null 2>&1 && break; sleep 1; done
curl -fsS http://127.0.0.1:3001/api/health >/dev/null || { docker logs grafana-smoke; echo "FAIL: Grafana not healthy"; exit 1; }
# Dashboard provisioning can finish after /api/health turns green, so retry.
found=
for _ in $(seq 30); do
  curl -fsS -u "admin:$pw" "$base/api/folders" 2>/dev/null | grep -q '"title":"dumpster-kit"' && { found=1; break; }
  sleep 1
done
if [ -z "$found" ]; then
  docker logs grafana-smoke; curl -sS -u "admin:$pw" "$base/api/folders" || true
  echo "FAIL: dumpster-kit folder not provisioned"; exit 1
fi

fail() { docker logs grafana-smoke; echo "FAIL: $1 returned $2, wanted $3"; exit 1; }
# expect <wanted: space-separated codes> <label> <curl args...>: the code is checked, the body discarded.
expect() {
  local want=$1 label=$2 code; shift 2
  code=$(curl -s -o /dev/null -w '%{http_code}' "$@") || true
  case " $want " in *" $code "*) ;; *) fail "$label" "$code" "$want";; esac
}
body='{"dashboard":{"title":"smoke-write","panels":[],"schemaVersion":39},"overwrite":true}'
folder='{"title":"smoke-positive-control"}'

# Positive control: the admin can write, so the refusals below are for lack of permission, not a bad request.
expect "200" "admin POST /api/folders" -u "admin:$pw" -H 'Content-Type: application/json' -d "$folder" "$base/api/folders"

# Anonymous: no -u, no cookie.
expect "200" "GET /api/dashboards/uid/product-metrics-smoke" "$base/api/dashboards/uid/product-metrics-smoke"
expect "200" "GET /d/product-metrics-smoke" "$base/d/product-metrics-smoke"
expect "401 403" "POST /api/dashboards/db" -H 'Content-Type: application/json' -d "$body" "$base/api/dashboards/db"
# A Viewer can read the data source's name and URL (200); it holds no credentials, so none may appear.
for path in /api/datasources /api/datasources/uid/victoriametrics; do
  expect "200" "GET $path" "$base$path"
  if curl -s "$base$path" | grep -Eq 'basicAuthPassword|"password"|secureJsonData'; then
    fail "GET $path" "a credential field in the body" "no basicAuthPassword, password or secureJsonData"
  fi
done
expect "401 403" "GET /api/admin/settings" "$base/api/admin/settings"
expect "401 403" "GET /api/org/users" "$base/api/org/users"
expect "200" "GET /login" "$base/login"
# Admin login still works.
expect "200" "admin GET /api/user" -u "admin:$pw" "$base/api/user"
curl -fsS -u "admin:$pw" "$base/api/user" | grep -q '"isGrafanaAdmin":true' || fail "admin GET /api/user" "no isGrafanaAdmin:true" "isGrafanaAdmin:true"
echo ok
