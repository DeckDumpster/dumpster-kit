#!/usr/bin/env bash
# Fails unless the registry token authenticates: one read-only GET /api/v1/user on the Gitea that
# hosts the index in .cargo/config.toml. Needs the tailnet and GITEA_CARGO_TOKEN (the bare secret).
# A dry run never authenticates, so this is what makes a bad or misformatted token fail before a release.
set -euo pipefail
kit="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
: "${GITEA_CARGO_TOKEN:?auth-check: GITEA_CARGO_TOKEN is not set}"
index="$(sed -n 's/^index = "\(.*\)"/\1/p' "$kit/.cargo/config.toml" | head -1)"
[ -n "$index" ] || { echo "auth-check: no index in $kit/.cargo/config.toml" >&2; exit 1; }
url="${index#sparse+}"
url="${url%%/api/packages/*}/api/v1/user"
code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 \
  -H "Authorization: token ${GITEA_CARGO_TOKEN}" "$url" || true)"
if [ "$code" != "200" ]; then
  echo "auth-check: $url answered ${code:-no response} with the registry token, not 200 (a 401 means the secret is wrong)" >&2
  exit 1
fi
echo "auth-check: $url authenticates (200)"
