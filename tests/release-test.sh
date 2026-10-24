#!/usr/bin/env bash
# Acceptance for the release path (no network): every crate packages for the tailnet registry,
# a tag that is not the tree's version is refused, and release.yml cannot publish without the
# whole of ci.yml (gate and both canaries) passing first. Run via `testenv`/CI.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "FAIL: $*"; exit 1; }
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"

"$here/scripts/release-check.sh" "v$version" >/dev/null || fail "the tree's own version tag was refused"
if "$here/scripts/release-check.sh" "v99.0.0" >/dev/null 2>&1; then fail "tag v99.0.0 does not match the tree but was accepted"; fi
if "$here/scripts/release-check.sh" "release-1" >/dev/null 2>&1; then fail "a non-version tag was accepted"; fi

t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT
(cd "$here" && CARGO_TARGET_DIR="$t/target" cargo package --workspace --no-verify --allow-dirty -q >"$t/pkg.log" 2>&1) \
  || { cat "$t/pkg.log"; fail "a crate does not package (internal dependency without a version?)"; }

python3 -I - "$here/.github/workflows" <<'PY'
import re, sys
d = sys.argv[1]
rel = open(d + "/release.yml").read()
ci = open(d + "/ci.yml").read()
def die(m): print("FAIL:", m); sys.exit(1)
if not re.search(r"tags:\s*\['v\*'\]", rel) or "branches" in rel.split("jobs:")[0]:
    die("release.yml must trigger on v* tags only")
if "workflow_call:" not in ci: die("ci.yml is not callable, so release.yml cannot require it")
if not re.search(r"^  ci:\n    uses: \./\.github/workflows/ci\.yml", rel, re.M): die("release.yml does not run ci.yml")
pub = rel.split("\n  publish:\n", 1)
if len(pub) != 2: die("release.yml has no publish job")
if not re.search(r"needs:\s*\[ci\]", pub[1]): die("publish does not need ci")
if "cargo publish --workspace --registry tailnet" not in pub[1]: die("publish does not publish the workspace")
if "secrets.TAILNET_CARGO_TOKEN" not in pub[1] or re.search(r"token\s*=\s*\S", rel, re.I): die("credential must come from the CI secret")
print("ok")
PY
echo "ok: release path"
