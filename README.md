# dumpster-kit
Shared, game-agnostic components extracted from pokedumpster and dumpstar (tenancy, tcgcsv, ui, lakehouse)

## Gate and canary

`.github/workflows/ci.yml` runs on GitHub-hosted runners: fmt, clippy, unit tests, then a consumer
canary. `scripts/canary.sh <name> <checkout>` patches every kit crate in the consumer to this
checkout and runs its tier-1 tests (`cargo test`); its log names the consumer and kit revisions.
`tests/canary-test.sh` is its acceptance (a breaking change is red, a harmless one is green).

The kit is public and has no deploy. Consumers pin a tagged revision as a plain git dependency
and move only through their own bump bead.

Access: both consumers are public, so the canary clones them anonymously. Repository secret
`CONSUMER_READ_TOKEN` is an optional override only; no secret is required.

## Crates

| crate | what it is |
|-------|------------|
| `dumpster-kit-conformance` | locates and loads the corpora under `conformance/` |
| `dumpster-kit-tenancy` | the tenancy kernel: handle/ULID validators, user registry (`schema_registry.sql`), the one tenant-connection opener (read-only `ATTACH`, `assert_isolated`, per-file `user_version`), Cloudflare Access verification, tenant resolution (`TenantId` has no public constructor) |

An application passes one `AppPrefix` (`PKDUMP`, `DUMPSTAR`) and one `TenantDbSpec` (its collection
schema and versions); everything else is the kernel's. The `testing` feature exports an in-process
RS256 JWKS server that mints a valid token and every entry of `conformance/jwt/negative.json`.
