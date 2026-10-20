#!/usr/bin/env bash
# Deploy path for the product metrics stack: runs from the production checkout of dumpster-kit
# (~/dumpster-kit) after pulling main, never from a worktree (law-prod-is-a-different-checkout).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
case "$here" in
  /workspaces/*|*/worktree/*) echo "refusing: deploy from the production checkout, not $here" >&2; exit 1 ;;
esac
git -C "$here" pull --ff-only
exec "$here/install.sh"
