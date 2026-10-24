#!/usr/bin/env bash
# A published crate must work for a consumer, not just inside this repository (sp-z1i955): package
# dumpster-kit-conformance, unpack the .crate alone, and run a throwaway crate that depends on the
# unpacked copy and loads every corpus. A crate that reads files above its own directory fails here.
# (Its dependencies are crates.io only, so no tailnet is needed.) Run via `testenv`/CI.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export CARGO_TARGET_DIR="$tmp/target"
cargo package -q -p dumpster-kit-conformance --allow-dirty --no-verify --manifest-path "$here/Cargo.toml"
crate="$(ls "$CARGO_TARGET_DIR"/package/dumpster-kit-conformance-*.crate | head -1)"
mkdir "$tmp/unpacked" "$tmp/consumer"
tar -xzf "$crate" -C "$tmp/unpacked"
pkg="$(ls -d "$tmp"/unpacked/dumpster-kit-conformance-*)"
mkdir "$tmp/consumer/src"
cat >"$tmp/consumer/Cargo.toml" <<TOML
[package]
name = "consumer"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
dumpster-kit-conformance = { path = "$pkg" }

[workspace]
TOML
cat >"$tmp/consumer/src/lib.rs" <<'RS'
#[test]
fn every_corpus_loads_from_the_packaged_crate() {
    assert!(!dumpster_kit_conformance::handle_cases().is_empty());
    assert!(!dumpster_kit_conformance::ulid_cases().is_empty());
    assert!(!dumpster_kit_conformance::email_cases().is_empty());
    assert!(!dumpster_kit_conformance::negative_jwts().is_empty());
}
RS
(cd "$tmp/consumer" && cargo test -q)
echo "ok: the packaged conformance crate works outside the repository"
