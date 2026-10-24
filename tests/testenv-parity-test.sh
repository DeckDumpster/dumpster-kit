#!/usr/bin/env bash
# Holds .spira/testenv.toml to .github/workflows/ci.yml (law-test-selection-and-plan-are-one-source):
# every CI job has a tier of the same name (matrix jobs get one tier per value, `<job>-<value>`), and
# every `run:` step that names no variable appears verbatim in that tier's argv. A builder who passes
# the tiers has therefore run what CI runs. Run it via `testenv`/CI, never by hand on the host.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 -I - "$here/.github/workflows/ci.yml" "$here/.spira/testenv.toml" <<'PY'
import re, sys, tomllib

def parse_yaml_jobs(path):
    """Just enough of ci.yml: jobs -> (run commands, matrix values). No YAML dependency on the image."""
    jobs, job, in_matrix = {}, None, False
    lines = open(path).read().split("\n")
    in_jobs = False
    i = 0
    while i < len(lines):
        ln = lines[i]
        if re.match(r"^jobs:\s*$", ln):
            in_jobs = True
        elif in_jobs and (m := re.match(r"^  ([A-Za-z0-9_-]+):\s*$", ln)):
            job = jobs.setdefault(m.group(1), {"runs": [], "matrix": []})
        elif job is not None:
            if m := re.match(r"^\s+consumer:\s*\[(.*)\]\s*$", ln):
                job["matrix"] = [v.strip() for v in m.group(1).split(",")]
            elif m := re.match(r"^(\s+)- run:\s*(.*)$", ln) or re.match(r"^(\s+)run:\s*(.*)$", ln):
                indent, rest = len(m.group(1)), m.group(2).strip()
                if rest in ("|", ">"):
                    i += 1
                    block = []
                    while i < len(lines) and (not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent):
                        block.append(lines[i].strip())
                        i += 1
                    job["runs"].append("\n".join(block).strip())
                    continue
                job["runs"].append(rest)
        i += 1
    return jobs

jobs = parse_yaml_jobs(sys.argv[1])
tiers = tomllib.load(open(sys.argv[2], "rb")).get("tier", {})
errors = []
if not jobs:
    errors.append("parsed no jobs from ci.yml")
for name, job in jobs.items():
    for value in job["matrix"] or [None]:
        tier = f"{name}-{value}" if value else name
        if tier not in tiers:
            errors.append(f"CI job {name!r} has no tier {tier!r} in .spira/testenv.toml")
            continue
        argv = " ".join(tiers[tier]["argv"])
        for cmd in job["runs"]:
            if value:
                cmd = cmd.replace("${{ matrix.consumer }}", value)
            if "$" in cmd:
                continue
            if cmd not in argv:
                errors.append(f"tier {tier!r} does not run CI step {cmd!r}")
        if value and f"scripts/canary.sh {value}" not in argv:
            errors.append(f"tier {tier!r} does not run scripts/canary.sh {value}")
if errors:
    print("FAIL: testenv.toml has drifted from ci.yml:")
    for e in errors:
        print("  -", e)
    sys.exit(1)
print(f"ok: {len(jobs)} CI jobs, all mirrored by testenv tiers")
PY
