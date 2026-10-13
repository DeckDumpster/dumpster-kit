#!/usr/bin/env bash
#
# Consumer canary: build one consumer and run its tier-1 tests (`cargo test`, no containers)
# with every kit crate it depends on replaced by THIS checkout.
#
#   scripts/canary.sh <name> <consumer-checkout>
#
# KIT_URL is the git URL the consumer's Cargo.toml names for the kit.
# The log's first line names the consumer revision tested; the last line is the verdict.
set -euo pipefail

name="${1:?usage: canary.sh <name> <consumer-checkout>}"
consumer="$(cd "${2:?usage: canary.sh <name> <consumer-checkout>}" && pwd)"
kit="$(cd "$(dirname "$0")/.." && pwd)"
KIT_URL="${KIT_URL:-https://github.com/DeckDumpster/dumpster-kit}"

rev="$(git -C "$consumer" rev-parse HEAD)"
kit_rev="$(git -C "$kit" rev-parse HEAD)"
echo "canary: consumer=$name consumer_rev=$rev kit_rev=$kit_rev"

patch="$(cargo metadata --manifest-path "$kit/Cargo.toml" --no-deps --format-version 1 |
  python3 -I -c '
import json, sys
kit = sys.argv[1]
for p in json.load(sys.stdin)["packages"]:
    d = p["manifest_path"].rsplit("/Cargo.toml", 1)[0]
    print("%s = { path = \"%s\" }" % (p["name"], d))
' "$kit")"

cfg="$(mktemp)"
trap 'rm -f "$cfg"' EXIT
printf '[patch."%s"]\n%s\n' "$KIT_URL" "$patch" >"$cfg"

if (cd "$consumer" && cargo test --config "$cfg"); then
  echo "canary: $name GREEN (consumer_rev=$rev kit_rev=$kit_rev)"
else
  echo "canary: $name RED (consumer_rev=$rev kit_rev=$kit_rev)"
  exit 1
fi
