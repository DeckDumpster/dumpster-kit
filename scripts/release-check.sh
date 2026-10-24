#!/usr/bin/env bash
# Refuses a release tag that is not a deliberate release of this tree: the tag must be v<version>
# and every dumpster-kit-* crate must carry that one version, with every internal dependency
# resolving through [workspace.dependencies] at the same version.
#   scripts/release-check.sh <tag>
set -euo pipefail
tag="${1:?usage: release-check.sh <tag>}"
here="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 -I - "$tag" "$here" <<'PY'
import json, re, subprocess, sys
tag, here = sys.argv[1:]
if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
    sys.exit(f"refuse: {tag!r} is not a release tag (v<major>.<minor>.<patch>)")
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--manifest-path", f"{here}/Cargo.toml", "--no-deps", "--format-version", "1"]))
want = tag[1:]
bad = [f"{p['name']} is {p['version']}" for p in meta["packages"] if p["version"] != want]
bad += [f"{p['name']} does not publish to the tailnet registry only" for p in meta["packages"] if p["publish"] != ["tailnet"]]
if bad:
    sys.exit(f"refuse: tag {tag} but " + "; ".join(bad))
print(f"ok: {len(meta['packages'])} crates at {want}")
PY
