#!/usr/bin/env bash
# Acceptance for scripts/canary.sh, against a throwaway kit and consumer (no network):
# a kit change that breaks the consumer's tests is RED, one that does not is GREEN,
# and the log names the consumer revision. Run it via `testenv`/CI, never against a real consumer.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

kit="$t/kit"
mkdir -p "$kit/scripts" "$kit/crates/fixture-kit/src"
cp "$here/scripts/canary.sh" "$kit/scripts/"
mkdir -p "$kit/.cargo" "$kit/crates/fixture-base/src"
cp "$here/.cargo/config.toml" "$kit/.cargo/"
printf '[workspace]\nresolver = "2"\nmembers = ["crates/fixture-kit", "crates/fixture-base"]\n' >"$kit/Cargo.toml"
# fixture-kit names its sibling through the tailnet registry, as the real kit crates do; the consumer
# has no such registry configured, so the canary must supply it.
printf '[package]\nname = "fixture-kit"\nversion = "0.1.0"\nedition = "2024"\n[dependencies]\nfixture-base = { path = "../fixture-base", version = "0.1.0", registry = "tailnet" }\n' >"$kit/crates/fixture-kit/Cargo.toml"
printf '[package]\nname = "fixture-base"\nversion = "0.1.0"\nedition = "2024"\n' >"$kit/crates/fixture-base/Cargo.toml"
printf 'pub fn base() {}\n' >"$kit/crates/fixture-base/src/lib.rs"
printf 'pub fn answer() -> u32 {\n    42\n}\n' >"$kit/crates/fixture-kit/src/lib.rs"
git -C "$kit" init -q -b main
git -C "$kit" add -A && git -C "$kit" commit -qm base

con="$t/consumer"
mkdir -p "$con/src"
printf '[package]\nname = "consumer"\nversion = "0.1.0"\nedition = "2024"\n[dependencies]\nfixture-kit = { git = "file://%s", rev = "%s" }\n' \
  "$kit" "$(git -C "$kit" rev-parse HEAD)" >"$con/Cargo.toml"
printf '#[cfg(test)]\nmod t {\n    #[test]\n    fn answer() {\n        assert_eq!(fixture_kit::answer(), 42);\n    }\n}\n' >"$con/src/lib.rs"
git -C "$con" init -q -b main
git -C "$con" add -A && git -C "$con" commit -qm base
export KIT_URL="file://$kit" CARGO_TARGET_DIR="$t/target"

fail() { echo "FAIL: $*"; exit 1; }

# breaking PR: must be RED
sed -i 's/42/41/' "$kit/crates/fixture-kit/src/lib.rs"
git -C "$kit" commit -qam breaking
if "$kit/scripts/canary.sh" consumer "$con" >"$t/red.log" 2>&1; then fail "breaking kit change was green"; fi
grep -q "consumer_rev=$(git -C "$con" rev-parse HEAD)" "$t/red.log" || fail "log lacks consumer rev"
grep -q "RED" "$t/red.log" || fail "log lacks RED verdict"

# harmless PR: must be GREEN
git -C "$kit" revert --no-edit HEAD >/dev/null
printf 'pub fn other() {}\n' >>"$kit/crates/fixture-kit/src/lib.rs"
git -C "$kit" commit -qam harmless
"$kit/scripts/canary.sh" consumer "$con" >"$t/green.log" 2>&1 || { cat "$t/green.log"; fail "harmless kit change was red"; }
grep -q "GREEN" "$t/green.log" || fail "log lacks GREEN verdict"
echo "canary-test: ok"
