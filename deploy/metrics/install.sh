#!/usr/bin/env bash
# Installs the product metrics stack (VictoriaMetrics with direct OTLP ingest + tailnet Grafana)
# as podman containers under user systemd units, with a CPU quota and explicit retention.
# Refuses any host but PRODUCT_HOST (stack.conf). Run via deploy/metrics/deploy.sh from the
# production checkout of this repo, never from a worktree.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
kit=$(cd "$here/../.." && pwd)
# shellcheck source=stack.conf
. "$here/stack.conf"

host=${METRICS_HOSTNAME_OVERRIDE:-$(hostname)}
if [ "$host" != "$PRODUCT_HOST" ]; then
  echo "refusing: this host is '$host', the product metrics stack installs only on '$PRODUCT_HOST'" >&2
  exit 1
fi

state=${METRICS_STATE_DIR:-$HOME/.local/share/product-metrics}
unitdir=${METRICS_UNIT_DIR:-$HOME/.config/systemd/user}
secrets=${METRICS_SECRET_DIR:-$HOME/.config/product-metrics}
dry=${METRICS_DRY_RUN:-}

ip=${METRICS_GRAFANA_ADDR:-$(tailscale ip -4 | head -n1)}
[ -n "$ip" ] || { echo "refusing: no tailnet address; Grafana must not bind to the LAN" >&2; exit 1; }

mkdir -p "$state/vm" "$state/grafana" "$state/provisioning/datasources" "$state/provisioning/dashboards" "$unitdir" "$secrets"
chmod 700 "$secrets"

# Admin credentials: generated once into a 0600 file, never printed.
env_file=$secrets/grafana.env
if [ ! -s "$env_file" ]; then
  umask 077
  printf 'GF_SECURITY_ADMIN_USER=admin\nGF_SECURITY_ADMIN_PASSWORD=%s\n' "$(head -c 24 /dev/urandom | base64 | tr -d '/+=\n')" > "$env_file"
fi
chmod 600 "$env_file"

cp "$here/provisioning/datasource.yml" "$state/provisioning/datasources/datasource.yml"

mounts=""
providers="apiVersion: 1
providers:"
for entry in $DASHBOARDS; do
  name=${entry%%=*}; path=${entry#*=}; path=${path//@KIT@/$kit}
  if [ ! -d "$path" ]; then echo "warning: no dashboards for $name at $path, skipped" >&2; continue; fi
  mounts="$mounts -v $path:/dashboards/$name:ro"
  providers="$providers
  - name: $name
    folder: $name
    type: file
    allowUiUpdates: false
    options:
      path: /dashboards/$name"
done
printf '%s\n' "$providers" > "$state/provisioning/dashboards/dashboards.yml"

cat > "$unitdir/product-victoriametrics.service" <<UNIT
[Unit]
Description=Product metrics: VictoriaMetrics (OTLP at /opentelemetry/v1/metrics)
After=network-online.target

[Service]
CPUQuota=$VM_CPU_QUOTA
Restart=always
ExecStartPre=-/usr/bin/podman rm -f product-victoriametrics
ExecStart=/usr/bin/podman run --rm --name product-victoriametrics --network=host \\
  -v $state/vm:/storage:Z $VM_IMAGE \\
  -storageDataPath=/storage -retentionPeriod=$VM_RETENTION -httpListenAddr=$VM_ADDR
ExecStop=/usr/bin/podman stop -t 20 product-victoriametrics

[Install]
WantedBy=default.target
UNIT

cat > "$unitdir/product-grafana.service" <<UNIT
[Unit]
Description=Product metrics: Grafana (tailnet only)
After=product-victoriametrics.service
Requires=product-victoriametrics.service

[Service]
CPUQuota=$GRAFANA_CPU_QUOTA
Restart=always
EnvironmentFile=$env_file
ExecStartPre=-/usr/bin/podman rm -f product-grafana
ExecStart=/usr/bin/podman run --rm --name product-grafana --network=host \\
  -e GF_SECURITY_ADMIN_USER -e GF_SECURITY_ADMIN_PASSWORD \\
  -e GF_SERVER_HTTP_ADDR=$ip -e GF_SERVER_HTTP_PORT=$GRAFANA_PORT \\
  -e GF_USERS_ALLOW_SIGN_UP=false -e GF_ANALYTICS_REPORTING_ENABLED=false \\
  -v $state/grafana:/var/lib/grafana:Z \\
  -v $state/provisioning:/etc/grafana/provisioning:ro,Z$mounts \\
  $GRAFANA_IMAGE
ExecStop=/usr/bin/podman stop -t 20 product-grafana

[Install]
WantedBy=default.target
UNIT

if [ -n "$dry" ]; then echo "dry run: units written to $unitdir"; exit 0; fi

systemctl --user daemon-reload
systemctl --user enable product-victoriametrics.service product-grafana.service
systemctl --user restart product-victoriametrics.service product-grafana.service
echo "installed; Grafana on http://$ip:$GRAFANA_PORT (admin password in $env_file)"
