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

# umask 077 and a stale 0600 file from an earlier install: Grafana's uid must still read provisioning.
mkdir -p "$t/state/provisioning/dashboards"; : > "$t/state/provisioning/dashboards/dashboards.yml"; chmod 600 "$t/state/provisioning/dashboards/dashboards.yml"
out=$(umask 077; METRICS_HOSTNAME_OVERRIDE=ubuntu-mini-0 "$root/deploy/metrics/install.sh" 2>&1)
find "$t/state/provisioning" -type f | grep -q .
while read -r f; do
  m=$(stat -c %a "$f"); [ $((8#$m & 4)) -ne 0 ] || { echo "FAIL: provisioning file not world-readable ($m): $f"; exit 1; }
done < <(find "$t/state/provisioning" -type f)
while read -r d; do
  m=$(stat -c %a "$d"); [ $((8#$m & 5)) -eq 5 ] || { echo "FAIL: provisioning dir not world-traversable ($m): $d"; exit 1; }
done < <(find "$t/state/provisioning" -type d)
pw=$(sed -n 's/^GF_SECURITY_ADMIN_PASSWORD=//p' "$t/secrets/grafana.env")
[ -n "$pw" ]; case "$out" in *"$pw"*) echo "FAIL: password printed"; exit 1;; esac
[ "$(stat -c %a "$t/secrets/grafana.env")" = 600 ]
grep -q 'CPUQuota=' "$t/units/product-victoriametrics.service"
grep -q 'CPUQuota=' "$t/units/product-grafana.service"
grep -q -- '-retentionPeriod=90d' "$t/units/product-victoriametrics.service"
grep -q 'GF_SERVER_HTTP_ADDR=100.64.0.9' "$t/units/product-grafana.service"
grep -q 'dumpster-kit' "$t/state/provisioning/dashboards/dashboards.yml"
grep -q ':ro' "$t/units/product-grafana.service"

# Every pinned image must resolve in its registry: a nonexistent tag fails here, not on the product host.
# Needs network and podman/skopeo; METRICS_SKIP_MANIFEST=1 skips it for offline runs.
if [ "${METRICS_SKIP_MANIFEST:-}" != 1 ]; then
  . "$root/deploy/metrics/stack.conf"
  for img in "$VM_IMAGE" "$GRAFANA_IMAGE"; do
    tag=$(printf %s "$img" | sed 's/@.*//'); dig=$(printf %s "$img" | sed 's/:[^:/@]*@/@/')
    for ref in "$tag" "$dig"; do
      if command -v skopeo >/dev/null; then skopeo inspect --raw "docker://$ref" >/dev/null
      else podman manifest inspect "$ref" >/dev/null; fi || { echo "FAIL: image does not resolve: $ref"; exit 1; }
    done
  done
fi
echo "ok"
