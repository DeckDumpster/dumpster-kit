#!/usr/bin/env bash
# Only the deploy workflow may target the prod box's self-hosted deploy runner (Ryan, 2026-10-23:
# the mini PC has no runners except as needed to deploy services). Fails any job of any other
# workflow whose runs-on could match it: a `self-hosted` or `deploy` label. Then proves the check
# on fixtures: a non-deploy job on [self-hosted] must fail, the deploy workflow must pass.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
check() {
  python3 -I - "$1" <<'PY'
import os, re, sys

DEPLOY = "deploy.yml"
LABELS = {"self-hosted", "deploy"}

def violations(path):
    out, job, lines = [], None, open(path).read().split("\n")
    in_jobs, i = False, 0
    while i < len(lines):
        ln = lines[i]
        if re.match(r"^jobs:\s*$", ln):
            in_jobs = True
        elif in_jobs and (m := re.match(r"^  ([A-Za-z0-9_-]+):\s*$", ln)):
            job = m.group(1)
        elif job and (m := re.match(r"^    runs-on:\s*(.*)$", ln)):
            text = m.group(1)
            while i + 1 < len(lines) and re.match(r"^      +\S", lines[i + 1]):
                i += 1
                text += " " + lines[i]
            words = set(re.findall(r"[A-Za-z0-9_.-]+", text.split("#")[0]))
            # a runs-on expression cannot be resolved statically, so it is refused too
            if words & LABELS or "${{" in text:
                out.append((job, text.strip()))
        i += 1
    return out

bad = []
for name in sorted(os.listdir(sys.argv[1])):
    if not name.endswith((".yml", ".yaml")) or name in (DEPLOY, "deploy.yaml"):
        continue
    for job, text in violations(os.path.join(sys.argv[1], name)):
        bad.append(f"{name}: job {job!r} has runs-on {text}, which could match the prod deploy runner; "
                   f"only {DEPLOY} may (use a GitHub-hosted runner, or run it through testenv)")
if bad:
    print("\n".join(bad), file=sys.stderr)
    sys.exit(1)
PY
}
check "$here/.github/workflows"
if check "$here/fixtures/runner-targeting/bad" 2>/dev/null; then
  echo "selftest: a non-deploy job on [self-hosted] passed the check" >&2; exit 1
fi
check "$here/fixtures/runner-targeting/good"
echo "runner-targeting: ok"
