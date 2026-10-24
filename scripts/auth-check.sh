#!/usr/bin/env bash
# Fails unless the registry token authenticates: one read-only GET /api/v1/packages/<owner> (package scope suffices; /api/v1/user needs read:user) on the Gitea that
# hosts the index in .cargo/config.toml. Needs the tailnet and GITEA_CARGO_TOKEN (the bare secret).
# A dry run never authenticates, so this is what makes a bad or misformatted token fail before a release.
set -euo pipefail
kit="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
: "${GITEA_CARGO_TOKEN:?auth-check: GITEA_CARGO_TOKEN is not set}"
index="$(sed -n 's/^index = "\(.*\)"/\1/p' "$kit/.cargo/config.toml" | head -1)"
[ -n "$index" ] || { echo "auth-check: no index in $kit/.cargo/config.toml" >&2; exit 1; }
base="${index#sparse+}"
base="${base%%/api/packages/*}"
owner="${index#*/api/packages/}"
owner="${owner%%/*}"
url="${base}/api/v1/packages/${owner}?type=cargo&limit=1"
code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 \
  -H "Authorization: token ${GITEA_CARGO_TOKEN}" "$url" || true)"
case "$code" in
  200) echo "auth-check: $url authenticates (200)" ;;
  401) echo "auth-check: $url answered 401: the secret is wrong" >&2; exit 1 ;;
  403) echo "auth-check: $url answered 403: the token lacks the package scope" >&2; exit 1 ;;
  *) echo "auth-check: $url answered ${code:-no response}, not 200" >&2; exit 1 ;;
esac
