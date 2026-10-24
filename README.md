# dumpster-kit
Shared, game-agnostic components extracted from pokedumpster and dumpstar (tenancy, tcgcsv, ui, lakehouse)

## Gate and canary

`.github/workflows/ci.yml` runs on GitHub-hosted runners: fmt, clippy, unit tests, then a consumer
canary. `scripts/canary.sh <name> <checkout>` patches every kit crate in the consumer to this
checkout and runs its tier-1 tests (`cargo test`); its log names the consumer and kit revisions.
`tests/canary-test.sh` is its acceptance (a breaking change is red, a harmless one is green).

The kit is public and has no deploy. Consumers depend on tagged releases (see Releases) and move only through their own bump bead.

Access: both consumers are public, so the canary clones them anonymously. Repository secret
`CONSUMER_READ_TOKEN` is an optional override only; no secret is required.

## Releases

Consumers depend on semver releases from the tailnet Cargo registry (Gitea, `[registries.tailnet]` in
`.cargo/config.toml`), not on a git revision. The registry is named `tailnet` and its owner is the Gitea user `rgantt`;
consumers copy it exactly:

    [registries.tailnet]
    index = "sparse+http://ubuntu-mini-0.tailb4a728.ts.net:3000/api/packages/rgantt/cargo/"

`scripts/registry-check.sh` (run by `registry-check.yml` and the publish job, both tailnet-joined) fails, naming the URL,
if that index's `config.json` does not answer 200.
 An ordinary merge publishes nothing; a release is deliberate.

1. Set `[workspace.package] version` and the matching `version` of every entry in
   `[workspace.dependencies]` in `Cargo.toml` to the new `X.Y.Z` (all `dumpster-kit-*` crates share one version).
2. Land that on `main` through the usual queue.
3. Tag the landed commit `vX.Y.Z` and push the tag.
   `.github/workflows/release.yml` runs all of `ci.yml` (gate and both consumer canaries) on the tagged
   commit and only then runs `cargo publish --workspace --registry tailnet`. A red gate or canary publishes
   nothing; `scripts/release-check.sh` also refuses a tag that is not the tree's version.

`tests/release-test.sh` (a step of `gate`) packages every crate for the registry on every PR, offline, so a
crate that cannot publish fails the gate. The registry answers on the tailnet only, so `publish` runs on a
GitHub-hosted runner that joins the tailnet as an ephemeral node (`tailscale/github-action`, org secrets
`TS_OAUTH_CLIENT_ID` and `TS_OAUTH_SECRET`); it never runs on the prod host's deploy runner. The publish token is the
repository secret `GITEA_CARGO_TOKEN` (a Gitea package-write token); no credential is ever in the repo or a log.
Running `release.yml` by hand (`workflow_dispatch`) is a dry run: it joins the tailnet, checks the registry and runs
`cargo publish --dry-run`, publishing nothing.
`tests/release-test.sh` holds the release path (packaging, tag refusal, publish gated on CI).

Semver policy: while 0.x, a breaking change to a public type is a **minor** bump and anything else a patch.
The canaries name the break: a red canary on a change means consumers must adapt, so it ships as a minor bump.
From 1.0 a breaking change is a major bump.

## Crates

| crate | what it is |
|-------|------------|
| `dumpster-kit-conformance` | locates and loads the corpora under `conformance/` |
| `dumpster-kit-tenancy` | the tenancy kernel: handle/ULID validators, user registry (`schema_registry.sql`), the one tenant-connection opener (read-only `ATTACH`, `assert_isolated`, per-file `user_version`), Cloudflare Access verification, tenant resolution (`TenantId` has no public constructor) |
| `dumpster-kit-ui` | `assets/tokens.css` (two layers: `--pd-*` reference ramp, semantic `--color-*`/`--space-*`/`--text-*`/`--radius-*`/`--shadow-*` roles), the eleven Svelte primitives in `assets/components/`, and the enforcement: `lint` fails on any raw colour, space, type, radius or elevation value (and any `var(--pd-*)`) outside the token file; `contrast` audits `assets/contrast-pairs.json` against WCAG AA and holds the two-layer split |
| `dumpster-kit-lake` | the raw landing zone and the tenant zone's object layout: `ObjectStore`/`ObjectSource`/`ObjectPurge` (dir and S3), `RawLanding` + manifests, `RawZone`, tenant-zone key layout and retention. Configuration names (`<PREFIX>_LAKE_S3_BUCKET`, `~/.config/<app>/lake.env`, …) come from one `LakeEnv::new(&AppPrefix)` |
| `dumpster-kit-keys` | key custody: one master key file, HKDF per-tenant keys, the `tenant_key` tombstone registry, backup and destruction as separate paths (`tests/separation.rs` holds them apart). `KeyEnv::new(&AppPrefix)` roots the file names and every HKDF label at the app, so two apps never share a tenant key |
| `dumpster-kit-ship` | the shipper (outbox -> encrypted Parquet parts in the tenant zone, resumable, gap-detecting) and the zone read-back (`zone::read`/`materialize`). Owns `outbox::{Event, project, SCHEMA_SQL}`; a site writes only the triggers on its own holdings tables and passes a `ShipContext` |
| `dumpster-kit-erase` | the deletion path: tombstone, drop the partition, prove every read path closed (`delete`, `verify`) |
| `dumpster-kit-derive` | `DeriveClock` (one instant, read once) and `finalize_landing` |
| `dumpster-kit-lakehouse` | `partition::choose`/`clock_of`/`provenance` (which raw runs a date derives from), `replay::RawReplay` (a landed partition answers requests by URL), `diff` (row-by-row catalog comparison) |

The lake crates are tested against a **fixture catalog**, not a site: `fixtures/lake/holdings.sql` (two
holdings tables with outbox triggers) and the recorded `fixtures/tcgcsv/` payloads. What stays a site's
part is the catalog mapping: which tables are holdings (and their triggers), the `Landing`/`Fetch`
adapters between its tcgcsv client and the lake, the derive into its own `shared.sqlite`, and the
operator binaries over these libraries.

A site runs both from its own CI: `dumpster-kit-ui lint --tokens <its tokens.css> <its src>` and
`dumpster-kit-ui contrast <tokens.css> <pairs.json>`. There is no budget file; the first hit fails.
The primitives' Svelte render tests (`assets/tests`, node) run in the `ui-primitives` CI job only.

An application passes one `AppPrefix` (`PKDUMP`, `DUMPSTAR`) and one `TenantDbSpec` (its collection
schema and versions); everything else is the kernel's. The `testing` feature exports an in-process
RS256 JWKS server that mints a valid token and every entry of `conformance/jwt/negative.json`.

## datalint (law-no-data-lives-in-code)

`cargo run -q -p dumpster-kit-datalint -- --root <repo> [--data-dir data/remedial] [--min-rows 5]`
refuses tables of reference data written as literals in `.rs/.py/.ts/.js/.svelte`, naming file, line
and the data directory. Remedial datasets are `data/remedial/*.csv|json|yaml` (at the root or inside the crate that owns them: a published crate builds from its own directory only) with a
`<name>.provenance.md` beside each. `tests/` and fixtures are exempt; `.datalint-allow` lists reviewed
exceptions (one path per line) and fails when an entry goes stale, so it only shrinks. Every dumpster
repo runs it in its CI gate.

## Product metrics stack (deploy/metrics)

VictoriaMetrics (OTLP straight to `http://ubuntu-mini-0.tailb4a728.ts.net:8428/opentelemetry/v1/metrics` (tailnet address, so the sites' podman containers reach it; Grafana is on tailnet port 3001), no collector) and a
tailnet-only Grafana, as podman containers under user systemd units with a CPU quota and 90d retention.
`deploy/metrics/deploy.sh` (from the production checkout) pulls and runs `install.sh`, which refuses any
host but `PRODUCT_HOST` in `stack.conf`. Each repo contributes `deploy/dashboards/*.json`; the stack
mounts the directories listed in `DASHBOARDS` read-only. Admin credentials land in
`~/.config/product-metrics/grafana.env` (0600), never printed. `deploy/metrics/push_smoke.sh` pushes a
test gauge shown in the "Product metrics smoke" panel. Offline acceptance: `tests/metrics-install-test.sh`.
