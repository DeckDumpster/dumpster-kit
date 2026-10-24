#!/usr/bin/env bash
# Fails, naming the URL, unless the registry index in .cargo/config.toml answers 200 at config.json.
# Needs the tailnet, so it runs in a tailnet-joined job (registry-check.yml, release.yml publish), not in `gate`.
set -euo pipefail
kit="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
index="$(sed -n 's/^index = "\(.*\)"/\1/p' "$kit/.cargo/config.toml" | head -1)"
[ -n "$index" ] || { echo "registry-check: no index in $kit/.cargo/config.toml" >&2; exit 1; }
url="${index#sparse+}config.json"
code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 "$url" || true)"
if [ "$code" != "200" ]; then
  echo "registry-check: $url answered ${code:-no response}, not 200 (the owner in the index must exist in Gitea)" >&2
  exit 1
fi
echo "registry-check: $url answers 200"
