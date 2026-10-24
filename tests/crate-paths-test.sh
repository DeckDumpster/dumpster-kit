#!/usr/bin/env bash
# No crate's non-test src may reach above its own directory at run or compile time: a published crate
# contains only its own directory, so a CARGO_MANIFEST_DIR join with `..` or an include_*! that
# resolves outside the crate works in this repository and nowhere else (sp-z1i955). Code after a
# file's first #[cfg(test)] and files named test_support.rs are exempt: consumers never run them.
# Run via `testenv`/CI.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 -I - "$here" <<'PY'
import os, re, sys
root = sys.argv[1]
errors = []
for crate in sorted(os.listdir(root + "/crates")):
    cdir = f"{root}/crates/{crate}"
    for dp, _, fs in os.walk(cdir + "/src"):
        for f in fs:
            if not f.endswith(".rs") or f == "test_support.rs":
                continue
            path = f"{dp}/{f}"
            text = open(path).read().split("#[cfg(test)]")[0]
            for m in re.finditer(r'CARGO_MANIFEST_DIR[^;]*', text):
                if re.search(r'\.\.|\.parent\(\)|ancestors', m.group(0)):
                    errors.append(f"{path}: CARGO_MANIFEST_DIR path leaves the crate: {m.group(0)[:80]}")
            for m in re.finditer(r'include_(?:str|bytes)!\(\s*"([^"]+)"', text):
                target = os.path.normpath(os.path.join(dp, m.group(1)))
                if not target.startswith(cdir + "/"):
                    errors.append(f"{path}: include_*! of {m.group(1)} resolves outside {crate}")
if errors:
    print("FAIL:\n" + "\n".join(errors)); sys.exit(1)
print("ok: no crate src leaves its directory")
PY
