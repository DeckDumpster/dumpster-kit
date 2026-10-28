Conformance corpora: data files, one directory per corpus, run by the kit and by each consumer.
The Rust-run corpora (handles, ulids, emails, jwt) live in `crates/dumpster-kit-conformance/corpus/` and are embedded
in that crate, so the published crate carries them; the paths below are relative to that directory. Only the
node check, `no-horizontal-scroll/`, lives here.

## Corpora

| dir | file | rows | run against |
|-----|------|------|-------------|
| `handles/` | `cases.json` | `value`, `valid` | handle validator and the registry `handle` CHECK |
| `ulids/` | `cases.json` | `value`, `valid` | `database_id` validator and the tenant file path |
| `emails/` | `cases.json` | `value`, `valid` | `normalise_email` and the registry `email` CHECK |
| `catalog/` | `cases.json` | `name`, `game`, `steps` | the catalog apply job, ledger, raw landing, alarms, prices, crosswalk and finish keying, over the fake games; run by `dumpster-kit-catalog/tests/conformance.rs` |
| `jwt/` | `negative.json` | `name`, `describes`, `status` | Access verification: every row must be refused |

Load them with `dumpster-kit-conformance` (`handle_cases()`, `ulid_cases()`, `email_cases()`,
`negative_jwts()`); a consumer's CI runs them against whatever validators it uses.

## no-horizontal-scroll (UI tier check)

`no-horizontal-scroll/check.mjs` is the check every site runs in its UI tier (law-no-horizontal-scroll-on-mobile).
It loads every route in the site's route manifest at 390x844 and 768x1024 and fails when
`document.scrollingElement.scrollWidth > innerWidth`, naming the route and the widest offending element.
A route may be `{ "path", "states": [{ "name", "setup(page)" }] }` (JS manifest) to reach expanded states.

    import { assertNoHorizontalScroll } from '<kit>/conformance/no-horizontal-scroll/check.mjs';
    await assertNoHorizontalScroll({ page, baseUrl, routes });          // from a Playwright test
    node <kit>/conformance/no-horizontal-scroll/cli.mjs --base-url URL --routes routes.json  // standalone

`selftest.mjs` proves it fails on a 2000px non-wrapping row (`fixtures/wide.html`) and passes on a wrapping page;
it runs in kit CI (`no-horizontal-scroll` job) and skips where Playwright is absent.
