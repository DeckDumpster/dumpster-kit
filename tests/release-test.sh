#!/usr/bin/env bash
# Acceptance for the release path (no network; packaging needs the tailnet registry, so the
# publish job checks it after joining the tailnet): a tag that is not the tree's version is
# refused, and release.yml cannot publish without the whole of ci.yml (gate and both canaries) passing first. Run via `testenv`/CI.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "FAIL: $*"; exit 1; }
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"

"$here/scripts/release-check.sh" "v$version" >/dev/null || fail "the tree's own version tag was refused"
if "$here/scripts/release-check.sh" "v99.0.0" >/dev/null 2>&1; then fail "tag v99.0.0 does not match the tree but was accepted"; fi
if "$here/scripts/release-check.sh" "release-1" >/dev/null 2>&1; then fail "a non-version tag was accepted"; fi

python3 -I - "$here/.github/workflows" <<'PY'
import re, sys
d = sys.argv[1]
rel = open(d + "/release.yml").read()
ci = open(d + "/ci.yml").read()
def die(m): print("FAIL:", m); sys.exit(1)
if not re.search(r"tags:\s*\['v\*'\]", rel) or "branches" in rel.split("jobs:")[0]:
    die("release.yml must trigger on v* tags only")
if "workflow_call:" not in ci: die("ci.yml is not callable, so release.yml cannot require it")
if not re.search(r"^  ci:\n(?:    if: .*\n)?    uses: \./\.github/workflows/ci\.yml", rel, re.M): die("release.yml does not run ci.yml")
pub = rel.split("\n  publish:\n", 1)
if len(pub) != 2: die("release.yml has no publish job")
if not re.search(r"  provision:\n    needs:\s*\[ci\]", rel): die("the runner is provisioned before ci passed")
if not re.search(r"needs:\s*\[provision\]", pub[1]): die("publish does not run on the provisioned runner")
if "cargo publish --workspace --registry tailnet" not in pub[1]: die("publish does not publish the workspace")
if "tailscale/github-action" in rel or "TS_OAUTH" in pub[1].split("\n  # if: always", 1)[0]: die("publish must not join the tailnet itself")
if "DeckDumpster/ephemeral-ci/provision@v2" not in rel or "DeckDumpster/ephemeral-ci/teardown@v2" not in rel: die("publish must run on an ephemeral-ci runner with teardown")
if "workflow_dispatch" not in rel.split("jobs:")[0] or "cargo publish --workspace --dry-run" not in pub[1]: die("no dry-run mode")
if "secrets.GITEA_CARGO_TOKEN" not in pub[1] or re.search(r"token\s*=\s*\S", rel, re.I): die("credential must come from the CI secret")
print("ok")
PY
echo "ok: release path"

# registry-check.sh: passes on a 200 from config.json, fails naming the URL on a 404.
tmp="$(mktemp -d)"; trap 'kill $srv 2>/dev/null; rm -rf "$tmp"' EXIT
mkdir -p "$tmp/www/api/packages/good/cargo" "$tmp/kit/.cargo"
echo '{}' >"$tmp/www/api/packages/good/cargo/config.json"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$tmp/www" >/dev/null 2>&1 & srv=$!
for _ in $(seq 50); do curl -s -o /dev/null "http://127.0.0.1:$port/" && break; sleep 0.1; done
printf '[registries.tailnet]\nindex = "sparse+http://127.0.0.1:%s/api/packages/good/cargo/"\n' "$port" >"$tmp/kit/.cargo/config.toml"
"$here/scripts/registry-check.sh" "$tmp/kit" >/dev/null || fail "registry-check rejected a registry that answers 200"
printf '[registries.tailnet]\nindex = "sparse+http://127.0.0.1:%s/api/packages/nobody/cargo/"\n' "$port" >"$tmp/kit/.cargo/config.toml"
out="$("$here/scripts/registry-check.sh" "$tmp/kit" 2>&1)" && fail "registry-check accepted a 404 owner"
case "$out" in *"/api/packages/nobody/cargo/config.json"*) ;; *) fail "registry-check did not name the URL: $out";; esac
echo "ok: registry check"
