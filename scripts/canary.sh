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
kit_rev="$(git -C "$kit" rev-parse HEAD 2>/dev/null || echo unknown)"
echo "canary: consumer=$name consumer_rev=$rev kit_rev=$kit_rev"

patch="$(cargo metadata --manifest-path "$kit/Cargo.toml" --no-deps --format-version 1 |
  python3 -I -c '
import json, sys
kit = sys.argv[1]
for p in json.load(sys.stdin)["packages"]:
    d = p["manifest_path"].rsplit("/Cargo.toml", 1)[0]
    print("%s = { path = \"%s\" }" % (p["name"], d))
' "$kit")"

# The kit's manifests name registry = "tailnet" for their internal dependencies; the consumer's
# cargo config does not define it, so supply the index from the kit's own config. Path-patched
# crates never query it.
index="$(sed -n 's/^index = "\(.*\)"/\1/p' "$kit/.cargo/config.toml" | head -1)"
[ -n "$index" ] || { echo "canary: no tailnet index in $kit/.cargo/config.toml" >&2; exit 1; }

cfg="$(mktemp)"
trap 'rm -f "$cfg"' EXIT
# A consumer may name the kit by git URL or through the tailnet registry (pokedumpster, dumpstar):
# patch both, so the canary tests THIS checkout rather than the last published release.
printf '[registries.tailnet]\nindex = "%s"\n\n[patch."%s"]\n%s\n\n[patch.tailnet]\n%s\n' "$index" "$KIT_URL" "$patch" "$patch" >"$cfg"

if (cd "$consumer" && cargo test --config "$cfg"); then
  echo "canary: $name GREEN (consumer_rev=$rev kit_rev=$kit_rev)"
else
  echo "canary: $name RED (consumer_rev=$rev kit_rev=$kit_rev)"
  exit 1
fi
